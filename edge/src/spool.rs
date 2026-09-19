use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use fs4::fs_std::FileExt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::crypto::{CryptoError, EncryptionKey};
use crate::protocol::{
    GapNoticeV2, GapReasonV2, HubAckResultV1, HubAckResultV2, HubAckV1, HubAckV2, HubBatchRecordV1,
    HubBatchRecordV2, HubBatchV1, HubBatchV2, ReceiverEnvelope, RecordId,
};

const PENDING_SUFFIX: &str = ".tles";
const RECEIPT_SUFFIX: &str = ".tlea";
const GAP_SUFFIX: &str = ".tleg";
const FORMAT_MARKER_FILE: &str = "FORMAT";
const SPOOL_LOCK_FILE: &str = ".lock";
const SEQUENCE_STATE_FILE: &str = "sequence.tlem";
const MAX_ACK_RECEIPTS: usize = 1_024;
const MAX_ENCRYPTED_RECORD_BYTES: u64 = 384 * 1_024;
const MAX_ENCRYPTED_RECEIPT_BYTES: u64 = 160 * 1_024;
const MAX_ENCRYPTED_SEQUENCE_STATE_BYTES: u64 = 16 * 1_024;
const MAX_ENCRYPTED_GAP_BYTES: u64 = 16 * 1_024;
const MAX_FORMAT_MARKER_BYTES: u64 = 8;

pub const SPOOL_FORMAT_VERSION: u16 = 3;
const PREVIOUS_SPOOL_FORMAT_VERSION: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpoolFormat {
    Fresh,
    Previous,
    Current,
}

#[derive(Debug, Clone)]
pub struct SpoolConfig {
    pub directory: PathBuf,
    pub max_bytes: u64,
    pub max_records: usize,
    pub retention_ms: i64,
    pub batch_max_bytes: usize,
    pub batch_max_records: usize,
}

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SpoolKey([u8; 32]);

impl SpoolKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnqueueOutcome {
    Stored(RecordId),
    AlreadyPresent(RecordId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoolSnapshot {
    pub pending_records: usize,
    pub pending_bytes: u64,
    pub corrupt_records: u64,
    pub expired_records: u64,
    pub pending_gap_notices: usize,
    pub pending_gap_bytes: u64,
    pub oldest_age_seconds: u64,
    pub capacity_exhausted: bool,
    pub degraded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpoolError {
    #[error("invalid spool configuration")]
    InvalidConfig,
    #[error("spool capacity exceeded")]
    CapacityExceeded,
    #[error("record cannot fit within the configured delivery batch")]
    BatchItemTooLarge,
    #[error("storage is full")]
    StorageFull,
    #[error("spool key does not match pending records")]
    KeyMismatch,
    #[error("encrypted spool record is corrupt")]
    CorruptRecord,
    #[error("spool input or output failed")]
    Io,
    #[error("spool is already open by another process")]
    AlreadyOpen,
    #[error("established spool is missing sequence state")]
    SequenceStateMissing,
    #[error("spool serialization failed")]
    Serialization,
    #[error(
        "spool format 2 contains acknowledgement receipts without original admission identity; reconcile the receipts before migration"
    )]
    ReceiptRecoveryRequired,
    #[error("v1 acknowledgement history was pruned; use v2 delivery for further acknowledgements")]
    V1AcknowledgementHistoryPruned,
    #[error("invalid acknowledgement")]
    InvalidAcknowledgement,
    #[error("v2 delivery is required while durable gap notices are pending")]
    ProtocolUpgradeRequired,
}

#[derive(Clone)]
pub struct Spool {
    inner: Arc<Inner>,
}

struct Inner {
    config: SpoolConfig,
    key: EncryptionKey,
    _lock: File,
    state: Mutex<State>,
    acknowledgement_lock: Mutex<()>,
}

struct State {
    entries: BTreeMap<RecordId, Entry>,
    stable_index: BTreeMap<RecordId, RecordId>,
    gaps: BTreeMap<RecordId, GapEntry>,
    receipts: BTreeMap<String, ReceiptEntry>,
    next_spool_sequence: u64,
    next_receipt_sequence: u64,
    v1_ack_history_pruned: bool,
    pending_bytes: u64,
    pending_gap_bytes: u64,
    corrupt_records: u64,
    expired_records: u64,
    unresolved_integrity: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            stable_index: BTreeMap::new(),
            gaps: BTreeMap::new(),
            receipts: BTreeMap::new(),
            next_spool_sequence: 1,
            next_receipt_sequence: 0,
            v1_ack_history_pruned: false,
            pending_bytes: 0,
            pending_gap_bytes: 0,
            corrupt_records: 0,
            expired_records: 0,
            unresolved_integrity: false,
        }
    }
}

#[derive(Clone)]
struct Entry {
    record_id: RecordId,
    stable_record_id: RecordId,
    spool_seq: u64,
    admitted_at_ms: i64,
    expires_at_ms: i64,
    encrypted_bytes: u64,
    path: PathBuf,
}

#[derive(Clone)]
struct ReceiptEntry {
    receipt: AckReceipt,
    path: PathBuf,
}

#[derive(Clone)]
struct GapEntry {
    notice: GapNoticeV2,
    encrypted_bytes: u64,
    path: PathBuf,
}

enum BatchItemV2 {
    Record(HubBatchRecordV2),
    Gap(GapNoticeV2),
}

enum BatchCandidateV2 {
    Record(Entry),
    Gap(GapNoticeV2),
}

impl BatchCandidateV2 {
    fn spool_seq(&self) -> u64 {
        match self {
            Self::Record(entry) => entry.spool_seq,
            Self::Gap(gap) => gap.spool_seq,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRecord {
    version: u16,
    record_id: RecordId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stable_record_id: Option<RecordId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    spool_seq: Option<u64>,
    admitted_at_ms: i64,
    expires_at_ms: i64,
    envelope: ReceiverEnvelope,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SequenceState {
    version: u16,
    next_spool_sequence: u64,
    #[serde(default)]
    expired_records_total: u64,
    #[serde(default)]
    v1_ack_history_pruned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptAdmission {
    spool_seq: u64,
    stable_record_id: RecordId,
    legacy_record_id: RecordId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AckReceipt {
    version: u16,
    sequence: u64,
    batch_id: String,
    accepted_record_ids: Vec<RecordId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    accepted_stable_record_ids: Vec<RecordId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    accepted_gap_notice_ids: Vec<RecordId>,
    accepted_admissions: Vec<ReceiptAdmission>,
}

impl Spool {
    pub fn open(config: SpoolConfig, key: SpoolKey, now_ms: i64) -> Result<Self, SpoolError> {
        validate_config(&config)?;
        let spool_lock = acquire_spool_lock(&config.directory)?;
        let pending = config.directory.join("pending");
        let temporary = config.directory.join("tmp");
        let quarantine = config.directory.join("quarantine");
        let receipts = config.directory.join("receipts");
        let gaps = config.directory.join("gaps");
        for directory in [
            &config.directory,
            &pending,
            &temporary,
            &quarantine,
            &receipts,
            &gaps,
        ] {
            fs::create_dir_all(directory).map_err(map_io_error)?;
            set_private_directory_permissions(directory)?;
        }
        let spool_format = read_spool_format_marker(&config.directory)?;
        if spool_format == SpoolFormat::Previous {
            reject_ambiguous_previous_receipts(&receipts)?;
        }
        let spool_directory = config.directory.clone();

        let inner = Arc::new(Inner {
            config,
            key: EncryptionKey::from_bytes(key.0),
            _lock: spool_lock,
            state: Mutex::new(State::default()),
            acknowledgement_lock: Mutex::new(()),
        });
        let spool = Self { inner };
        if spool_format == SpoolFormat::Previous {
            // Format 2 has no durable marker distinguishing an empty v1
            // receipt history from one that was already pruned. Preserve the
            // safe boundary across migration instead of allowing a stale v1
            // ACK to match a later legacy-ID admission.
            spool.state()?.v1_ack_history_pruned = true;
        }
        if spool_format == SpoolFormat::Current {
            spool
                .recover_sequence_state()?
                .ok_or(SpoolError::SequenceStateMissing)?;
        }
        spool.recover(now_ms)?;
        if spool_format != SpoolFormat::Current {
            write_current_spool_format_marker(&spool_directory, &temporary, spool_format)?;
        }
        Ok(spool)
    }

    pub fn enqueue(
        &self,
        envelope: ReceiverEnvelope,
        admitted_at_ms: i64,
    ) -> Result<EnqueueOutcome, SpoolError> {
        let record_id = envelope.record_id();
        let stable_record_id = envelope.stable_record_id();
        let mut state = self.state()?;
        if let Some(existing_record_id) = state.stable_index.get(&stable_record_id) {
            return Ok(EnqueueOutcome::AlreadyPresent(existing_record_id.clone()));
        }
        let spool_seq = state.next_spool_sequence;
        let next_spool_sequence = spool_seq.checked_add(1).ok_or(SpoolError::InvalidConfig)?;
        let stored = StoredRecord {
            version: 2,
            record_id: record_id.clone(),
            stable_record_id: Some(stable_record_id.clone()),
            spool_seq: Some(spool_seq),
            admitted_at_ms,
            expires_at_ms: admitted_at_ms.saturating_add(self.inner.config.retention_ms),
            envelope,
        };
        let v1_delivery_bytes = serde_json::to_vec(&HubBatchRecordV1 {
            record_id: stored.record_id.clone(),
            received_at_ms: stored.admitted_at_ms,
            envelope: stored.envelope.clone(),
        })
        .map_err(|_| SpoolError::Serialization)?
        .len();
        let v2_delivery_bytes = serde_json::to_vec(&HubBatchRecordV2 {
            record_id: stable_record_id.clone(),
            legacy_record_id: record_id.clone(),
            spool_seq,
            received_at_ms: stored.admitted_at_ms,
            envelope: stored.envelope.clone(),
        })
        .map_err(|_| SpoolError::Serialization)?
        .len();
        if v1_delivery_bytes > self.inner.config.batch_max_bytes
            || v2_delivery_bytes > self.inner.config.batch_max_bytes
        {
            return Err(SpoolError::BatchItemTooLarge);
        }
        let plaintext = serde_json::to_vec(&stored).map_err(|_| SpoolError::Serialization)?;
        let predicted_bytes = u64::try_from(plaintext.len())
            .map_err(|_| SpoolError::CapacityExceeded)?
            .saturating_add(64);
        if state.entries.len() >= self.inner.config.max_records
            || state.pending_bytes.saturating_add(predicted_bytes) > self.inner.config.max_bytes
        {
            return Err(SpoolError::CapacityExceeded);
        }
        let encrypted = self
            .inner
            .key
            .encrypt(&plaintext)
            .map_err(map_crypto_error)?;
        let encrypted_bytes =
            u64::try_from(encrypted.len()).map_err(|_| SpoolError::CapacityExceeded)?;
        if state.pending_bytes.saturating_add(encrypted_bytes) > self.inner.config.max_bytes {
            return Err(SpoolError::CapacityExceeded);
        }

        let file_name = format!("v2-{:020}-{}{}", spool_seq, record_id, PENDING_SUFFIX);
        let destination = self.pending_dir().join(file_name);
        let temporary = self.temporary_dir().join(format!("{}.tmp", Uuid::new_v4()));
        write_atomic(&temporary, &destination, &encrypted)?;

        state.pending_bytes = state.pending_bytes.saturating_add(encrypted_bytes);
        state.next_spool_sequence = next_spool_sequence;
        state
            .stable_index
            .insert(stable_record_id.clone(), record_id.clone());
        state.entries.insert(
            record_id.clone(),
            Entry {
                record_id: record_id.clone(),
                stable_record_id,
                spool_seq,
                admitted_at_ms,
                expires_at_ms: stored.expires_at_ms,
                encrypted_bytes,
                path: destination,
            },
        );
        self.persist_sequence_state(
            next_spool_sequence,
            state.expired_records,
            state.v1_ack_history_pruned,
        )?;
        Ok(EnqueueOutcome::Stored(record_id))
    }

    pub fn next_batch(&self, now_ms: i64) -> Result<HubBatchV1, SpoolError> {
        self.expire_due(now_ms)?;
        let state = self.state()?;
        if state.unresolved_integrity {
            return Err(SpoolError::CorruptRecord);
        }
        if !state.gaps.is_empty() {
            return Err(SpoolError::ProtocolUpgradeRequired);
        }
        drop(state);
        self.build_batch()
    }

    fn build_batch(&self) -> Result<HubBatchV1, SpoolError> {
        let state = self.state()?;
        if state.unresolved_integrity {
            return Err(SpoolError::CorruptRecord);
        }
        let mut ordered = state.entries.values().cloned().collect::<Vec<_>>();
        ordered.sort_by(|left, right| {
            (left.admitted_at_ms, &left.record_id).cmp(&(right.admitted_at_ms, &right.record_id))
        });
        drop(state);
        let mut records = Vec::new();
        let mut body_bytes = 0_usize;
        for entry in ordered {
            if records.len() >= self.inner.config.batch_max_records {
                break;
            }
            let stored = match self.read_entry(&entry) {
                Ok(stored) => stored,
                Err(SpoolError::CorruptRecord) => {
                    self.quarantine_runtime_entry(&entry)?;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let record = HubBatchRecordV1 {
                record_id: stored.record_id,
                received_at_ms: stored.admitted_at_ms,
                envelope: stored.envelope,
            };
            let encoded_bytes = serde_json::to_vec(&record)
                .map_err(|_| SpoolError::Serialization)?
                .len();
            if !records.is_empty()
                && body_bytes.saturating_add(encoded_bytes) > self.inner.config.batch_max_bytes
            {
                break;
            }
            if encoded_bytes > self.inner.config.batch_max_bytes {
                return Err(SpoolError::BatchItemTooLarge);
            }
            body_bytes = body_bytes.saturating_add(encoded_bytes);
            records.push(record);
        }
        if !self.state()?.gaps.is_empty() {
            return Err(SpoolError::ProtocolUpgradeRequired);
        }
        let batch_id = batch_id(&records);
        Ok(HubBatchV1 {
            version: 1,
            batch_id,
            records,
        })
    }

    pub fn next_batch_v2(&self, now_ms: i64) -> Result<HubBatchV2, SpoolError> {
        self.expire_due(now_ms)?;
        self.build_batch_v2()
    }

    fn build_batch_v2(&self) -> Result<HubBatchV2, SpoolError> {
        let state = self.state()?;
        if state.unresolved_integrity {
            return Err(SpoolError::CorruptRecord);
        }
        let mut ordered = state.entries.values().cloned().collect::<Vec<_>>();
        ordered.sort_by_key(|entry| entry.spool_seq);
        drop(state);

        let pending_gaps = self
            .state()?
            .gaps
            .values()
            .map(|entry| entry.notice.clone())
            .collect::<Vec<_>>();
        let mut candidates = ordered
            .into_iter()
            .map(BatchCandidateV2::Record)
            .chain(pending_gaps.into_iter().map(BatchCandidateV2::Gap))
            .collect::<Vec<_>>();
        candidates.sort_by_key(BatchCandidateV2::spool_seq);
        if candidates
            .windows(2)
            .any(|items| items[0].spool_seq() == items[1].spool_seq())
        {
            return Err(SpoolError::CorruptRecord);
        }

        let mut records = Vec::new();
        let mut gaps = Vec::new();
        let mut body_bytes = 0_usize;
        let mut quarantined = false;
        for candidate in candidates {
            if records.len().saturating_add(gaps.len()) >= self.inner.config.batch_max_records {
                break;
            }
            let candidate = match candidate {
                BatchCandidateV2::Record(entry) => {
                    let stored = match self.read_entry(&entry) {
                        Ok(stored) => stored,
                        Err(SpoolError::CorruptRecord) => {
                            self.quarantine_runtime_entry(&entry)?;
                            quarantined = true;
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    BatchItemV2::Record(HubBatchRecordV2 {
                        record_id: entry.stable_record_id,
                        legacy_record_id: entry.record_id,
                        spool_seq: entry.spool_seq,
                        received_at_ms: stored.admitted_at_ms,
                        envelope: stored.envelope,
                    })
                }
                BatchCandidateV2::Gap(gap) => BatchItemV2::Gap(gap),
            };
            let encoded_bytes = match &candidate {
                BatchItemV2::Record(record) => serde_json::to_vec(record),
                BatchItemV2::Gap(gap) => serde_json::to_vec(gap),
            }
            .map_err(|_| SpoolError::Serialization)?
            .len();
            if (!records.is_empty() || !gaps.is_empty())
                && body_bytes.saturating_add(encoded_bytes) > self.inner.config.batch_max_bytes
            {
                break;
            }
            if encoded_bytes > self.inner.config.batch_max_bytes {
                return Err(SpoolError::BatchItemTooLarge);
            }
            body_bytes = body_bytes.saturating_add(encoded_bytes);
            match candidate {
                BatchItemV2::Record(record) => records.push(record),
                BatchItemV2::Gap(gap) => gaps.push(gap),
            }
        }
        if quarantined {
            return self.build_batch_v2();
        }
        let batch_id = batch_id_v2(&records, &gaps);
        Ok(HubBatchV2 {
            version: 2,
            batch_id,
            records,
            gaps,
        })
    }

    pub fn acknowledge(&self, acknowledgement: &HubAckV1) -> Result<HubAckResultV1, SpoolError> {
        let _acknowledgement_guard = self
            .inner
            .acknowledgement_lock
            .lock()
            .map_err(|_| SpoolError::Io)?;
        let accepted = acknowledgement
            .accepted_record_ids
            .iter()
            .collect::<HashSet<_>>();
        if acknowledgement.version != 1
            || !valid_digest(&acknowledgement.batch_id)
            || accepted.len() != acknowledgement.accepted_record_ids.len()
        {
            return Err(SpoolError::InvalidAcknowledgement);
        }
        let prior_receipt = self
            .state()?
            .receipts
            .get(&acknowledgement.batch_id)
            .cloned();
        if let Some(prior_receipt) = prior_receipt {
            if !same_record_ids(
                &prior_receipt.receipt.accepted_record_ids,
                &acknowledgement.accepted_record_ids,
            ) {
                return Err(SpoolError::InvalidAcknowledgement);
            }
            self.complete_receipt(&prior_receipt.receipt)?;
            return Ok(ack_result(&prior_receipt.receipt));
        }
        if self.state()?.v1_ack_history_pruned {
            return Err(SpoolError::V1AcknowledgementHistoryPruned);
        }
        let delivered = self.build_batch()?;
        let delivered_record_ids = delivered
            .records
            .iter()
            .map(|record| &record.record_id)
            .collect::<HashSet<_>>();
        if delivered.batch_id != acknowledgement.batch_id
            || acknowledgement
                .accepted_record_ids
                .iter()
                .any(|record_id| !delivered_record_ids.contains(record_id))
        {
            return Err(SpoolError::InvalidAcknowledgement);
        }
        let accepted_admissions = self.receipt_admissions_v1(&delivered, acknowledgement)?;
        let sequence = {
            let mut state = self.state()?;
            let sequence = state.next_receipt_sequence;
            state.next_receipt_sequence =
                sequence.checked_add(1).ok_or(SpoolError::InvalidConfig)?;
            sequence
        };
        let receipt = AckReceipt {
            version: 1,
            sequence,
            batch_id: acknowledgement.batch_id.clone(),
            accepted_record_ids: acknowledgement.accepted_record_ids.clone(),
            accepted_stable_record_ids: Vec::new(),
            accepted_gap_notice_ids: Vec::new(),
            accepted_admissions,
        };
        let plaintext = serde_json::to_vec(&receipt).map_err(|_| SpoolError::Serialization)?;
        let encrypted = self
            .inner
            .key
            .encrypt(&plaintext)
            .map_err(map_crypto_error)?;
        if u64::try_from(encrypted.len()).map_err(|_| SpoolError::CapacityExceeded)?
            > MAX_ENCRYPTED_RECEIPT_BYTES
        {
            return Err(SpoolError::CapacityExceeded);
        }
        let destination = self.receipts_dir().join(format!(
            "{sequence:020}-{}{}",
            receipt.batch_id, RECEIPT_SUFFIX
        ));
        let temporary = self.temporary_dir().join(format!("{}.tmp", Uuid::new_v4()));
        write_atomic(&temporary, &destination, &encrypted)?;
        self.state()?.receipts.insert(
            receipt.batch_id.clone(),
            ReceiptEntry {
                receipt: receipt.clone(),
                path: destination,
            },
        );
        self.complete_receipt(&receipt)?;
        self.prune_receipts()?;
        Ok(ack_result(&receipt))
    }

    pub fn acknowledge_v2(&self, acknowledgement: &HubAckV2) -> Result<HubAckResultV2, SpoolError> {
        let _acknowledgement_guard = self
            .inner
            .acknowledgement_lock
            .lock()
            .map_err(|_| SpoolError::Io)?;
        let accepted_records = acknowledgement
            .accepted_record_ids
            .iter()
            .collect::<HashSet<_>>();
        let accepted_gaps = acknowledgement
            .accepted_gap_notice_ids
            .iter()
            .collect::<HashSet<_>>();
        if acknowledgement.version != 2
            || !valid_digest(&acknowledgement.batch_id)
            || accepted_records.len() != acknowledgement.accepted_record_ids.len()
            || accepted_gaps.len() != acknowledgement.accepted_gap_notice_ids.len()
        {
            return Err(SpoolError::InvalidAcknowledgement);
        }
        let prior_receipt = self
            .state()?
            .receipts
            .get(&acknowledgement.batch_id)
            .cloned();
        if let Some(prior_receipt) = prior_receipt {
            if prior_receipt.receipt.version != 2
                || !same_record_ids(
                    &prior_receipt.receipt.accepted_stable_record_ids,
                    &acknowledgement.accepted_record_ids,
                )
                || !same_record_ids(
                    &prior_receipt.receipt.accepted_gap_notice_ids,
                    &acknowledgement.accepted_gap_notice_ids,
                )
            {
                return Err(SpoolError::InvalidAcknowledgement);
            }
            self.complete_receipt(&prior_receipt.receipt)?;
            return Ok(ack_result_v2(&prior_receipt.receipt));
        }
        let delivered = self.build_batch_v2()?;
        let delivered_record_ids = delivered
            .records
            .iter()
            .map(|record| &record.record_id)
            .collect::<HashSet<_>>();
        let delivered_gap_ids = delivered
            .gaps
            .iter()
            .map(|gap| &gap.notice_id)
            .collect::<HashSet<_>>();
        if delivered.batch_id != acknowledgement.batch_id
            || acknowledgement
                .accepted_record_ids
                .iter()
                .any(|record_id| !delivered_record_ids.contains(record_id))
            || acknowledgement
                .accepted_gap_notice_ids
                .iter()
                .any(|notice_id| !delivered_gap_ids.contains(notice_id))
            || !v2_ack_is_merged_prefix(&delivered, acknowledgement)
        {
            return Err(SpoolError::InvalidAcknowledgement);
        }
        let accepted_admissions = self.receipt_admissions_v2(&delivered, acknowledgement)?;
        let sequence = {
            let mut state = self.state()?;
            let sequence = state.next_receipt_sequence;
            state.next_receipt_sequence =
                sequence.checked_add(1).ok_or(SpoolError::InvalidConfig)?;
            sequence
        };
        let receipt = AckReceipt {
            version: 2,
            sequence,
            batch_id: acknowledgement.batch_id.clone(),
            accepted_record_ids: Vec::new(),
            accepted_stable_record_ids: acknowledgement.accepted_record_ids.clone(),
            accepted_gap_notice_ids: acknowledgement.accepted_gap_notice_ids.clone(),
            accepted_admissions,
        };
        let plaintext = serde_json::to_vec(&receipt).map_err(|_| SpoolError::Serialization)?;
        let encrypted = self
            .inner
            .key
            .encrypt(&plaintext)
            .map_err(map_crypto_error)?;
        if u64::try_from(encrypted.len()).map_err(|_| SpoolError::CapacityExceeded)?
            > MAX_ENCRYPTED_RECEIPT_BYTES
        {
            return Err(SpoolError::CapacityExceeded);
        }
        let destination = self.receipts_dir().join(format!(
            "{sequence:020}-{}{}",
            receipt.batch_id, RECEIPT_SUFFIX
        ));
        let temporary = self.temporary_dir().join(format!("{}.tmp", Uuid::new_v4()));
        write_atomic(&temporary, &destination, &encrypted)?;
        self.state()?.receipts.insert(
            receipt.batch_id.clone(),
            ReceiptEntry {
                receipt: receipt.clone(),
                path: destination,
            },
        );
        self.complete_receipt(&receipt)?;
        self.prune_receipts()?;
        Ok(ack_result_v2(&receipt))
    }

    fn receipt_admissions_v1(
        &self,
        delivered: &HubBatchV1,
        acknowledgement: &HubAckV1,
    ) -> Result<Vec<ReceiptAdmission>, SpoolError> {
        let accepted = acknowledgement
            .accepted_record_ids
            .iter()
            .collect::<HashSet<_>>();
        let state = self.state()?;
        delivered
            .records
            .iter()
            .filter(|record| accepted.contains(&record.record_id))
            .map(|record| {
                let entry = state
                    .entries
                    .get(&record.record_id)
                    .ok_or(SpoolError::InvalidAcknowledgement)?;
                Ok(ReceiptAdmission {
                    spool_seq: entry.spool_seq,
                    stable_record_id: entry.stable_record_id.clone(),
                    legacy_record_id: entry.record_id.clone(),
                })
            })
            .collect()
    }

    fn receipt_admissions_v2(
        &self,
        delivered: &HubBatchV2,
        acknowledgement: &HubAckV2,
    ) -> Result<Vec<ReceiptAdmission>, SpoolError> {
        let accepted_records = acknowledgement
            .accepted_record_ids
            .iter()
            .collect::<HashSet<_>>();
        let state = self.state()?;
        delivered
            .records
            .iter()
            .filter(|record| accepted_records.contains(&record.record_id))
            .map(|record| {
                let entry = state
                    .entries
                    .get(&record.legacy_record_id)
                    .ok_or(SpoolError::InvalidAcknowledgement)?;
                if entry.spool_seq != record.spool_seq || entry.stable_record_id != record.record_id
                {
                    return Err(SpoolError::InvalidAcknowledgement);
                }
                Ok(ReceiptAdmission {
                    spool_seq: record.spool_seq,
                    stable_record_id: record.record_id.clone(),
                    legacy_record_id: record.legacy_record_id.clone(),
                })
            })
            .collect()
    }

    fn complete_receipt(&self, receipt: &AckReceipt) -> Result<(), SpoolError> {
        let mut state = self.state()?;
        let mut removed_pending = false;
        for admission in &receipt.accepted_admissions {
            if remove_admitted_record(&mut state, admission)? {
                removed_pending = true;
            }
        }
        for notice_id in &receipt.accepted_gap_notice_ids {
            let Some(entry) = state.gaps.get(notice_id).cloned() else {
                continue;
            };
            match fs::remove_file(&entry.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(map_io_error(error)),
            }
            state.gaps.remove(notice_id);
            state.pending_gap_bytes = state
                .pending_gap_bytes
                .saturating_sub(entry.encrypted_bytes);
        }
        if removed_pending {
            sync_directory(&self.pending_dir())?;
        }
        sync_directory(&self.gaps_dir())?;
        Ok(())
    }

    fn prune_receipts(&self) -> Result<(), SpoolError> {
        loop {
            let oldest = {
                let state = self.state()?;
                if state.receipts.len() <= MAX_ACK_RECEIPTS {
                    return Ok(());
                }
                state
                    .receipts
                    .values()
                    .min_by_key(|entry| entry.receipt.sequence)
                    .cloned()
                    .ok_or(SpoolError::Io)?
            };
            if oldest.receipt.version == 1 {
                let mut state = self.state()?;
                if !state.v1_ack_history_pruned {
                    state.v1_ack_history_pruned = true;
                    self.persist_sequence_state(
                        state.next_spool_sequence,
                        state.expired_records,
                        true,
                    )?;
                }
            }
            match fs::remove_file(&oldest.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(map_io_error(error)),
            }
            self.state()?.receipts.remove(&oldest.receipt.batch_id);
            sync_directory(&self.receipts_dir())?;
        }
    }

    pub fn expire_due(&self, now_ms: i64) -> Result<u64, SpoolError> {
        let mut state = self.state()?;
        let expired = state
            .entries
            .values()
            .filter(|entry| entry.expires_at_ms < now_ms)
            .cloned()
            .collect::<Vec<_>>();
        let mut expired_count = 0_u64;
        for entry in &expired {
            let gap_exists = state
                .gaps
                .values()
                .any(|gap| gap.notice.spool_seq == entry.spool_seq);
            if !gap_exists {
                let expired_records = state
                    .expired_records
                    .checked_add(1)
                    .ok_or(SpoolError::InvalidConfig)?;
                self.persist_sequence_state(
                    state.next_spool_sequence,
                    expired_records,
                    state.v1_ack_history_pruned,
                )?;
                state.expired_records = expired_records;
            }
            let (_, created) = self.persist_gap_locked(
                &mut state,
                entry,
                entry.expires_at_ms,
                GapReasonV2::RetentionExpired,
            )?;
            if gap_exists && created {
                return Err(SpoolError::CorruptRecord);
            }
            match fs::remove_file(&entry.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(map_io_error(error)),
            }
            state.entries.remove(&entry.record_id);
            state.stable_index.remove(&entry.stable_record_id);
            state.pending_bytes = state.pending_bytes.saturating_sub(entry.encrypted_bytes);
            expired_count = expired_count.saturating_add(1);
            sync_directory(&self.pending_dir())?;
        }
        Ok(expired_count)
    }

    fn persist_gap_locked(
        &self,
        state: &mut State,
        entry: &Entry,
        occurred_at_ms: i64,
        reason: GapReasonV2,
    ) -> Result<(RecordId, bool), SpoolError> {
        let reason_label = match reason {
            GapReasonV2::RetentionExpired => b"retention_expired".as_slice(),
            GapReasonV2::IntegrityQuarantine => b"integrity_quarantine".as_slice(),
        };
        let mut evidence_digest = Sha256::new();
        evidence_digest.update(b"teslatlas-edge-gap-evidence-v2\0");
        evidence_digest.update(entry.spool_seq.to_be_bytes());
        evidence_digest.update(entry.stable_record_id.as_str().as_bytes());
        evidence_digest.update(reason_label);
        let evidence_sha256 = hex::encode(evidence_digest.finalize());
        let mut notice_digest = Sha256::new();
        notice_digest.update(b"teslatlas-edge-gap-notice-v2\0");
        notice_digest.update(entry.spool_seq.to_be_bytes());
        notice_digest.update(reason_label);
        notice_digest.update(evidence_sha256.as_bytes());
        let notice_id = RecordId::from_sha256(notice_digest.finalize());
        if state.gaps.contains_key(&notice_id) {
            return Ok((notice_id, false));
        }
        if state
            .gaps
            .values()
            .any(|gap| gap.notice.spool_seq == entry.spool_seq)
        {
            return Err(SpoolError::CorruptRecord);
        }
        if state.entries.values().any(|pending| {
            pending.spool_seq == entry.spool_seq && pending.record_id != entry.record_id
        }) {
            return Err(SpoolError::CorruptRecord);
        }
        if state.gaps.len() >= self.inner.config.max_records {
            return Err(SpoolError::CapacityExceeded);
        }
        let notice = GapNoticeV2 {
            notice_id: notice_id.clone(),
            spool_seq: entry.spool_seq,
            occurred_at_ms,
            reason,
            evidence_sha256,
        };
        let plaintext = serde_json::to_vec(&notice).map_err(|_| SpoolError::Serialization)?;
        let encrypted = self
            .inner
            .key
            .encrypt(&plaintext)
            .map_err(map_crypto_error)?;
        let encrypted_bytes =
            u64::try_from(encrypted.len()).map_err(|_| SpoolError::CapacityExceeded)?;
        if encrypted_bytes > MAX_ENCRYPTED_GAP_BYTES
            || state.pending_gap_bytes.saturating_add(encrypted_bytes) > self.inner.config.max_bytes
        {
            return Err(SpoolError::CapacityExceeded);
        }
        let destination = self.gaps_dir().join(format!(
            "{:020}-{}{}",
            entry.spool_seq, notice_id, GAP_SUFFIX
        ));
        let temporary = self.temporary_dir().join(format!("{}.tmp", Uuid::new_v4()));
        write_atomic(&temporary, &destination, &encrypted)?;
        state.pending_gap_bytes = state.pending_gap_bytes.saturating_add(encrypted_bytes);
        state.gaps.insert(
            notice_id.clone(),
            GapEntry {
                notice,
                encrypted_bytes,
                path: destination,
            },
        );
        Ok((notice_id, true))
    }

    pub fn snapshot(&self, now_ms: i64) -> SpoolSnapshot {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let oldest_age_seconds = state
            .entries
            .values()
            .map(|entry| entry.admitted_at_ms)
            .min()
            .map(|oldest| u64::try_from(now_ms.saturating_sub(oldest).max(0) / 1_000).unwrap_or(0))
            .unwrap_or(0);
        let capacity_exhausted = state.entries.len() >= self.inner.config.max_records
            || state.pending_bytes >= self.inner.config.max_bytes
            || state.gaps.len() >= self.inner.config.max_records
            || state.pending_gap_bytes >= self.inner.config.max_bytes;
        SpoolSnapshot {
            pending_records: state.entries.len(),
            pending_bytes: state.pending_bytes,
            corrupt_records: state.corrupt_records,
            expired_records: state.expired_records,
            pending_gap_notices: state.gaps.len(),
            pending_gap_bytes: state.pending_gap_bytes,
            oldest_age_seconds,
            capacity_exhausted,
            degraded: state.corrupt_records > 0
                || !state.gaps.is_empty()
                || state.unresolved_integrity
                || capacity_exhausted,
        }
    }

    pub fn sync(&self) -> Result<(), SpoolError> {
        sync_directory(&self.pending_dir())?;
        sync_directory(&self.temporary_dir())?;
        sync_directory(&self.quarantine_dir())?;
        sync_directory(&self.receipts_dir())?;
        sync_directory(&self.gaps_dir())?;
        sync_directory(&self.inner.config.directory)
    }

    fn recover(&self, now_ms: i64) -> Result<(), SpoolError> {
        let mut state = self.state()?;
        self.ensure_quarantine_capacity(0, false)?;
        for result in fs::read_dir(self.quarantine_dir()).map_err(map_io_error)? {
            result.map_err(map_io_error)?;
            state.corrupt_records = state.corrupt_records.saturating_add(1);
        }
        for result in fs::read_dir(self.temporary_dir()).map_err(map_io_error)? {
            let path = result.map_err(map_io_error)?.path();
            self.quarantine_file_bounded(&path)?;
            state.corrupt_records = state.corrupt_records.saturating_add(1);
            state.unresolved_integrity = true;
        }
        self.recover_gaps(&mut state)?;
        let mut recovered_records = Vec::new();
        for result in fs::read_dir(self.pending_dir()).map_err(map_io_error)? {
            let entry = result.map_err(map_io_error)?;
            let path = entry.path();
            if !entry.file_type().map_err(map_io_error)?.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("tles")
            {
                self.quarantine_recovered_pending(&mut state, &path, now_ms)?;
                continue;
            }
            let metadata = entry.metadata().map_err(map_io_error)?;
            if metadata.len() > MAX_ENCRYPTED_RECORD_BYTES {
                self.quarantine_recovered_pending(&mut state, &path, now_ms)?;
                continue;
            }
            let encrypted = fs::read(&path).map_err(map_io_error)?;
            let plaintext = match self.inner.key.decrypt(&encrypted) {
                Ok(plaintext) => plaintext,
                Err(CryptoError::KeyMismatch) => return Err(SpoolError::KeyMismatch),
                Err(CryptoError::InvalidCiphertext) => {
                    self.quarantine_recovered_pending(&mut state, &path, now_ms)?;
                    continue;
                }
            };
            let stored: StoredRecord = match serde_json::from_slice(&plaintext) {
                Ok(stored) => stored,
                Err(_) => {
                    self.quarantine_recovered_pending(&mut state, &path, now_ms)?;
                    continue;
                }
            };
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if !valid_stored_record(&stored)
                || !file_name.ends_with(&format!("-{}{}", stored.record_id, PENDING_SUFFIX))
            {
                self.quarantine_recovered_pending(&mut state, &path, now_ms)?;
                continue;
            }
            let encrypted_bytes =
                u64::try_from(encrypted.len()).map_err(|_| SpoolError::CapacityExceeded)?;
            recovered_records.push((stored, path, encrypted_bytes));
        }
        recovered_records.sort_by(|left, right| {
            (left.0.admitted_at_ms, &left.0.record_id)
                .cmp(&(right.0.admitted_at_ms, &right.0.record_id))
        });
        let recovered_record_max = recovered_records
            .iter()
            .filter_map(|(stored, _, _)| stored.spool_seq)
            .max()
            .unwrap_or(0);
        let recovered_gap_max = state
            .gaps
            .values()
            .map(|gap| gap.notice.spool_seq)
            .max()
            .unwrap_or(0);
        let mut next_migration_sequence = recovered_record_max
            .max(recovered_gap_max)
            .checked_add(1)
            .ok_or(SpoolError::InvalidConfig)?;
        for (mut stored, mut path, mut encrypted_bytes) in recovered_records {
            if stored.version == 1 {
                let spool_seq = next_migration_sequence;
                next_migration_sequence = next_migration_sequence
                    .checked_add(1)
                    .ok_or(SpoolError::InvalidConfig)?;
                stored.version = 2;
                stored.stable_record_id = Some(stored.envelope.stable_record_id());
                stored.spool_seq = Some(spool_seq);
                let plaintext =
                    serde_json::to_vec(&stored).map_err(|_| SpoolError::Serialization)?;
                let encrypted = self
                    .inner
                    .key
                    .encrypt(&plaintext)
                    .map_err(map_crypto_error)?;
                let temporary = self.temporary_dir().join(format!("{}.tmp", Uuid::new_v4()));
                write_atomic_replace(&temporary, &path, &encrypted)?;
                encrypted_bytes =
                    u64::try_from(encrypted.len()).map_err(|_| SpoolError::CapacityExceeded)?;
            }
            let stable_record_id = stored
                .stable_record_id
                .clone()
                .ok_or(SpoolError::CorruptRecord)?;
            let spool_seq = stored.spool_seq.ok_or(SpoolError::CorruptRecord)?;
            if state
                .gaps
                .values()
                .any(|gap| gap.notice.spool_seq == spool_seq)
            {
                self.quarantine_file_bounded(&path)?;
                state.corrupt_records = state.corrupt_records.saturating_add(1);
                continue;
            }
            let canonical_path = self.pending_dir().join(format!(
                "v2-{spool_seq:020}-{}{}",
                stored.record_id, PENDING_SUFFIX
            ));
            if path != canonical_path {
                if canonical_path.exists() {
                    return Err(SpoolError::CapacityExceeded);
                }
                fs::rename(&path, &canonical_path).map_err(map_io_error)?;
                sync_directory(&self.pending_dir())?;
                path = canonical_path;
            }
            if state.entries.contains_key(&stored.record_id)
                || state.stable_index.contains_key(&stable_record_id)
                || state
                    .entries
                    .values()
                    .any(|entry| entry.spool_seq == spool_seq)
                || state.entries.len() >= self.inner.config.max_records
                || state.pending_bytes.saturating_add(encrypted_bytes) > self.inner.config.max_bytes
            {
                return Err(SpoolError::CapacityExceeded);
            }
            state.pending_bytes = state.pending_bytes.saturating_add(encrypted_bytes);
            state
                .stable_index
                .insert(stable_record_id.clone(), stored.record_id.clone());
            state.entries.insert(
                stored.record_id.clone(),
                Entry {
                    record_id: stored.record_id,
                    stable_record_id,
                    spool_seq,
                    admitted_at_ms: stored.admitted_at_ms,
                    expires_at_ms: stored.expires_at_ms,
                    encrypted_bytes,
                    path,
                },
            );
        }
        let recovered_sequence = self.recover_sequence_state()?;
        let recovered_next = recovered_sequence
            .as_ref()
            .map(|sequence| sequence.next_spool_sequence)
            .unwrap_or(1);
        state.expired_records = recovered_sequence
            .as_ref()
            .map(|sequence| sequence.expired_records_total)
            .unwrap_or_else(|| {
                u64::try_from(
                    state
                        .gaps
                        .values()
                        .filter(|gap| gap.notice.reason == GapReasonV2::RetentionExpired)
                        .count(),
                )
                .unwrap_or(u64::MAX)
            });
        state.v1_ack_history_pruned = state.v1_ack_history_pruned
            || recovered_sequence
                .as_ref()
                .map(|sequence| sequence.v1_ack_history_pruned)
                .unwrap_or(false);
        let records_next = state
            .entries
            .values()
            .map(|entry| entry.spool_seq)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(SpoolError::InvalidConfig)?;
        let gaps_next = state
            .gaps
            .values()
            .map(|gap| gap.notice.spool_seq)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(SpoolError::InvalidConfig)?;
        state.next_spool_sequence = recovered_next.max(records_next).max(gaps_next);
        self.recover_receipts(&mut state)?;
        let next_spool_sequence = state.next_spool_sequence;
        let expired_records = state.expired_records;
        let v1_ack_history_pruned = state.v1_ack_history_pruned;
        drop(state);
        self.persist_sequence_state(next_spool_sequence, expired_records, v1_ack_history_pruned)?;
        self.expire_due(now_ms)?;
        self.prune_receipts()?;
        Ok(())
    }

    fn recover_gaps(&self, state: &mut State) -> Result<(), SpoolError> {
        for result in fs::read_dir(self.gaps_dir()).map_err(map_io_error)? {
            let entry = result.map_err(map_io_error)?;
            let path = entry.path();
            let valid_file = entry.file_type().map_err(map_io_error)?.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("tleg")
                && entry.metadata().map_err(map_io_error)?.len() <= MAX_ENCRYPTED_GAP_BYTES;
            if !valid_file {
                self.quarantine_file_bounded(&path)?;
                state.corrupt_records = state.corrupt_records.saturating_add(1);
                state.unresolved_integrity = true;
                continue;
            }
            let encrypted = fs::read(&path).map_err(map_io_error)?;
            let plaintext = match self.inner.key.decrypt(&encrypted) {
                Ok(plaintext) => plaintext,
                Err(CryptoError::KeyMismatch) => return Err(SpoolError::KeyMismatch),
                Err(CryptoError::InvalidCiphertext) => {
                    self.quarantine_file_bounded(&path)?;
                    state.corrupt_records = state.corrupt_records.saturating_add(1);
                    state.unresolved_integrity = true;
                    continue;
                }
            };
            let notice: GapNoticeV2 = match serde_json::from_slice(&plaintext) {
                Ok(notice) => notice,
                Err(_) => {
                    self.quarantine_file_bounded(&path)?;
                    state.corrupt_records = state.corrupt_records.saturating_add(1);
                    state.unresolved_integrity = true;
                    continue;
                }
            };
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let encrypted_bytes =
                u64::try_from(encrypted.len()).map_err(|_| SpoolError::CapacityExceeded)?;
            if !valid_gap_notice(&notice)
                || file_name
                    != format!(
                        "{:020}-{}{}",
                        notice.spool_seq, notice.notice_id, GAP_SUFFIX
                    )
                || state.gaps.contains_key(&notice.notice_id)
                || state
                    .gaps
                    .values()
                    .any(|gap| gap.notice.spool_seq == notice.spool_seq)
                || state.gaps.len() >= self.inner.config.max_records
                || state.pending_gap_bytes.saturating_add(encrypted_bytes)
                    > self.inner.config.max_bytes
            {
                self.quarantine_file_bounded(&path)?;
                state.corrupt_records = state.corrupt_records.saturating_add(1);
                state.unresolved_integrity = true;
                continue;
            }
            state.pending_gap_bytes = state.pending_gap_bytes.saturating_add(encrypted_bytes);
            state.gaps.insert(
                notice.notice_id.clone(),
                GapEntry {
                    notice,
                    encrypted_bytes,
                    path,
                },
            );
        }
        Ok(())
    }

    fn quarantine_recovered_pending(
        &self,
        state: &mut State,
        path: &Path,
        now_ms: i64,
    ) -> Result<(), SpoolError> {
        if let Some((spool_seq, record_id)) = parse_v2_pending_file_name(path) {
            let entry = Entry {
                record_id: record_id.clone(),
                stable_record_id: record_id,
                spool_seq,
                admitted_at_ms: now_ms,
                expires_at_ms: now_ms,
                encrypted_bytes: fs::metadata(path).map(|value| value.len()).unwrap_or(0),
                path: path.to_path_buf(),
            };
            self.persist_gap_locked(state, &entry, now_ms, GapReasonV2::IntegrityQuarantine)?;
        } else {
            state.unresolved_integrity = true;
        }
        self.quarantine_file_bounded(path)?;
        state.corrupt_records = state.corrupt_records.saturating_add(1);
        Ok(())
    }

    fn recover_receipts(&self, state: &mut State) -> Result<(), SpoolError> {
        for result in fs::read_dir(self.receipts_dir()).map_err(map_io_error)? {
            let entry = result.map_err(map_io_error)?;
            let path = entry.path();
            if !entry.file_type().map_err(map_io_error)?.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("tlea")
            {
                self.quarantine_file_bounded(&path)?;
                state.corrupt_records = state.corrupt_records.saturating_add(1);
                continue;
            }
            if entry.metadata().map_err(map_io_error)?.len() > MAX_ENCRYPTED_RECEIPT_BYTES {
                self.quarantine_file_bounded(&path)?;
                state.corrupt_records = state.corrupt_records.saturating_add(1);
                continue;
            }
            let encrypted = fs::read(&path).map_err(map_io_error)?;
            let plaintext = match self.inner.key.decrypt(&encrypted) {
                Ok(plaintext) => plaintext,
                Err(CryptoError::KeyMismatch) => return Err(SpoolError::KeyMismatch),
                Err(CryptoError::InvalidCiphertext) => {
                    self.quarantine_file_bounded(&path)?;
                    state.corrupt_records = state.corrupt_records.saturating_add(1);
                    continue;
                }
            };
            let receipt: AckReceipt = match serde_json::from_slice(&plaintext) {
                Ok(receipt) => receipt,
                Err(_) => {
                    self.quarantine_file_bounded(&path)?;
                    state.corrupt_records = state.corrupt_records.saturating_add(1);
                    continue;
                }
            };
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let unique_ids = receipt.accepted_record_ids.iter().collect::<HashSet<_>>();
            let unique_stable_ids = receipt
                .accepted_stable_record_ids
                .iter()
                .collect::<HashSet<_>>();
            let unique_gap_ids = receipt
                .accepted_gap_notice_ids
                .iter()
                .collect::<HashSet<_>>();
            let unique_admissions = receipt.accepted_admissions.iter().collect::<HashSet<_>>();
            let admission_legacy_ids = receipt
                .accepted_admissions
                .iter()
                .map(|admission| &admission.legacy_record_id)
                .collect::<HashSet<_>>();
            let admission_stable_ids = receipt
                .accepted_admissions
                .iter()
                .map(|admission| &admission.stable_record_id)
                .collect::<HashSet<_>>();
            let valid_version_shape = match receipt.version {
                1 => {
                    receipt.accepted_stable_record_ids.is_empty()
                        && receipt.accepted_gap_notice_ids.is_empty()
                }
                2 => receipt.accepted_record_ids.is_empty(),
                _ => false,
            };
            let valid_admission_shape = receipt
                .accepted_admissions
                .iter()
                .all(|admission| admission.spool_seq > 0)
                && match receipt.version {
                    1 => {
                        receipt.accepted_admissions.len() == receipt.accepted_record_ids.len()
                            && admission_legacy_ids
                                == receipt.accepted_record_ids.iter().collect::<HashSet<_>>()
                    }
                    2 => {
                        receipt.accepted_admissions.len()
                            == receipt.accepted_stable_record_ids.len()
                            && admission_stable_ids
                                == receipt
                                    .accepted_stable_record_ids
                                    .iter()
                                    .collect::<HashSet<_>>()
                    }
                    _ => false,
                };
            if !valid_version_shape
                || !valid_admission_shape
                || !valid_digest(&receipt.batch_id)
                || receipt.accepted_record_ids.len() > 256
                || receipt.accepted_stable_record_ids.len() > 256
                || receipt.accepted_gap_notice_ids.len() > 256
                || receipt.sequence == u64::MAX
                || unique_ids.len() != receipt.accepted_record_ids.len()
                || unique_stable_ids.len() != receipt.accepted_stable_record_ids.len()
                || unique_gap_ids.len() != receipt.accepted_gap_notice_ids.len()
                || unique_admissions.len() != receipt.accepted_admissions.len()
                || file_name
                    != format!(
                        "{:020}-{}{}",
                        receipt.sequence, receipt.batch_id, RECEIPT_SUFFIX
                    )
                || state.receipts.contains_key(&receipt.batch_id)
            {
                self.quarantine_file_bounded(&path)?;
                state.corrupt_records = state.corrupt_records.saturating_add(1);
                continue;
            }
            state.next_receipt_sequence = state
                .next_receipt_sequence
                .max(receipt.sequence.saturating_add(1));
            state
                .receipts
                .insert(receipt.batch_id.clone(), ReceiptEntry { receipt, path });
        }
        let receipts = state
            .receipts
            .values()
            .map(|entry| entry.receipt.clone())
            .collect::<Vec<_>>();
        let mut removed_pending = false;
        let mut removed_gaps = false;
        for receipt in receipts {
            for admission in &receipt.accepted_admissions {
                if remove_admitted_record(state, admission)? {
                    removed_pending = true;
                }
            }
            for notice_id in receipt.accepted_gap_notice_ids {
                let Some(entry) = state.gaps.get(&notice_id).cloned() else {
                    continue;
                };
                match fs::remove_file(&entry.path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(map_io_error(error)),
                }
                state.gaps.remove(&notice_id);
                state.pending_gap_bytes = state
                    .pending_gap_bytes
                    .saturating_sub(entry.encrypted_bytes);
                removed_gaps = true;
            }
        }
        if removed_pending {
            sync_directory(&self.pending_dir())?;
        }
        if removed_gaps {
            sync_directory(&self.gaps_dir())?;
        }
        Ok(())
    }

    fn read_entry(&self, entry: &Entry) -> Result<StoredRecord, SpoolError> {
        let metadata = match fs::metadata(&entry.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(SpoolError::CorruptRecord);
            }
            Err(error) => return Err(map_io_error(error)),
        };
        if metadata.len() > MAX_ENCRYPTED_RECORD_BYTES {
            return Err(SpoolError::CorruptRecord);
        }
        let encrypted = match fs::read(&entry.path) {
            Ok(encrypted) => encrypted,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(SpoolError::CorruptRecord);
            }
            Err(error) => return Err(map_io_error(error)),
        };
        let plaintext = self
            .inner
            .key
            .decrypt(&encrypted)
            .map_err(map_crypto_error)?;
        let stored: StoredRecord =
            serde_json::from_slice(&plaintext).map_err(|_| SpoolError::CorruptRecord)?;
        if !valid_stored_record(&stored)
            || stored.record_id != entry.record_id
            || stored.stable_record_id.as_ref() != Some(&entry.stable_record_id)
            || stored.spool_seq != Some(entry.spool_seq)
        {
            return Err(SpoolError::CorruptRecord);
        }
        Ok(stored)
    }

    fn quarantine_runtime_entry(&self, entry: &Entry) -> Result<(), SpoolError> {
        let mut state = self.state()?;
        if !state.entries.contains_key(&entry.record_id) {
            return Ok(());
        }
        self.persist_gap_locked(
            &mut state,
            entry,
            entry.admitted_at_ms,
            GapReasonV2::IntegrityQuarantine,
        )?;
        match self.quarantine_file_bounded(&entry.path) {
            Ok(()) => {}
            Err(SpoolError::Io) if !entry.path.exists() => {
                sync_directory(&self.pending_dir())?;
            }
            Err(error) => return Err(error),
        }
        state.entries.remove(&entry.record_id);
        state.stable_index.remove(&entry.stable_record_id);
        state.pending_bytes = state.pending_bytes.saturating_sub(entry.encrypted_bytes);
        state.corrupt_records = state.corrupt_records.saturating_add(1);
        Ok(())
    }

    fn recover_sequence_state(&self) -> Result<Option<SequenceState>, SpoolError> {
        let path = self.inner.config.directory.join(SEQUENCE_STATE_FILE);
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(map_io_error(error)),
        };
        if !metadata.is_file() || metadata.len() > MAX_ENCRYPTED_SEQUENCE_STATE_BYTES {
            return Err(SpoolError::CorruptRecord);
        }
        let encrypted = fs::read(path).map_err(map_io_error)?;
        let plaintext = self
            .inner
            .key
            .decrypt(&encrypted)
            .map_err(map_crypto_error)?;
        let sequence: SequenceState =
            serde_json::from_slice(&plaintext).map_err(|_| SpoolError::CorruptRecord)?;
        if sequence.version != 1 || sequence.next_spool_sequence == 0 {
            return Err(SpoolError::CorruptRecord);
        }
        Ok(Some(sequence))
    }

    fn persist_sequence_state(
        &self,
        next_spool_sequence: u64,
        expired_records_total: u64,
        v1_ack_history_pruned: bool,
    ) -> Result<(), SpoolError> {
        let plaintext = serde_json::to_vec(&SequenceState {
            version: 1,
            next_spool_sequence,
            expired_records_total,
            v1_ack_history_pruned,
        })
        .map_err(|_| SpoolError::Serialization)?;
        let encrypted = self
            .inner
            .key
            .encrypt(&plaintext)
            .map_err(map_crypto_error)?;
        let destination = self.inner.config.directory.join(SEQUENCE_STATE_FILE);
        let temporary = self.temporary_dir().join(format!("{}.tmp", Uuid::new_v4()));
        write_atomic_replace(&temporary, &destination, &encrypted)
    }

    fn state(&self) -> Result<MutexGuard<'_, State>, SpoolError> {
        self.inner.state.lock().map_err(|_| SpoolError::Io)
    }

    fn pending_dir(&self) -> PathBuf {
        self.inner.config.directory.join("pending")
    }

    fn temporary_dir(&self) -> PathBuf {
        self.inner.config.directory.join("tmp")
    }

    fn quarantine_dir(&self) -> PathBuf {
        self.inner.config.directory.join("quarantine")
    }

    fn receipts_dir(&self) -> PathBuf {
        self.inner.config.directory.join("receipts")
    }

    fn gaps_dir(&self) -> PathBuf {
        self.inner.config.directory.join("gaps")
    }

    fn quarantine_file_bounded(&self, path: &Path) -> Result<(), SpoolError> {
        let source_bytes = fs::symlink_metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        self.ensure_quarantine_capacity(source_bytes, true)?;
        quarantine_file(path, &self.quarantine_dir())
    }

    fn ensure_quarantine_capacity(
        &self,
        additional_bytes: u64,
        adding_file: bool,
    ) -> Result<(), SpoolError> {
        let mut files = 0_usize;
        let mut bytes = 0_u64;
        for result in fs::read_dir(self.quarantine_dir()).map_err(map_io_error)? {
            let entry = result.map_err(map_io_error)?;
            files = files.saturating_add(1);
            bytes = bytes.saturating_add(entry.metadata().map_err(map_io_error)?.len());
        }
        let resulting_files = files.saturating_add(usize::from(adding_file));
        if resulting_files > self.inner.config.max_records
            || bytes.saturating_add(additional_bytes) > self.inner.config.max_bytes
        {
            return Err(SpoolError::CapacityExceeded);
        }
        Ok(())
    }
}

fn validate_config(config: &SpoolConfig) -> Result<(), SpoolError> {
    if config.max_bytes == 0
        || config.max_records == 0
        || config.retention_ms <= 0
        || config.batch_max_bytes == 0
        || config.batch_max_records == 0
        || config.batch_max_records > config.max_records
    {
        return Err(SpoolError::InvalidConfig);
    }
    Ok(())
}

fn valid_stored_record(stored: &StoredRecord) -> bool {
    if stored.envelope.validate().is_err()
        || stored.record_id != stored.envelope.record_id()
        || stored.expires_at_ms < stored.admitted_at_ms
    {
        return false;
    }
    match stored.version {
        1 => stored.stable_record_id.is_none() && stored.spool_seq.is_none(),
        2 => {
            stored.stable_record_id.as_ref() == Some(&stored.envelope.stable_record_id())
                && stored.spool_seq.is_some_and(|sequence| sequence > 0)
        }
        _ => false,
    }
}

fn valid_gap_notice(notice: &GapNoticeV2) -> bool {
    if notice.spool_seq == 0 || !valid_digest(&notice.evidence_sha256) {
        return false;
    }
    let reason_label = match notice.reason {
        GapReasonV2::RetentionExpired => b"retention_expired".as_slice(),
        GapReasonV2::IntegrityQuarantine => b"integrity_quarantine".as_slice(),
    };
    let mut digest = Sha256::new();
    digest.update(b"teslatlas-edge-gap-notice-v2\0");
    digest.update(notice.spool_seq.to_be_bytes());
    digest.update(reason_label);
    digest.update(notice.evidence_sha256.as_bytes());
    notice.notice_id == RecordId::from_sha256(digest.finalize())
}

fn parse_v2_pending_file_name(path: &Path) -> Option<(u64, RecordId)> {
    let file_name = path.file_name()?.to_str()?;
    let body = file_name
        .strip_prefix("v2-")?
        .strip_suffix(PENDING_SUFFIX)?;
    let (sequence, record_id) = body.split_once('-')?;
    if sequence.len() != 20 {
        return None;
    }
    let spool_seq = sequence.parse::<u64>().ok()?;
    if spool_seq == 0 {
        return None;
    }
    Some((spool_seq, RecordId::parse(record_id).ok()?))
}

fn batch_id(records: &[HubBatchRecordV1]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"teslatlas-edge-batch-v1\0");
    for record in records {
        digest.update(record.record_id.as_str().as_bytes());
        digest.update([0]);
    }
    hex::encode(digest.finalize())
}

fn batch_id_v2(records: &[HubBatchRecordV2], gaps: &[GapNoticeV2]) -> String {
    let mut items = records
        .iter()
        .map(|record| (record.spool_seq, b'r', record.record_id.as_str()))
        .chain(
            gaps.iter()
                .map(|gap| (gap.spool_seq, b'g', gap.notice_id.as_str())),
        )
        .collect::<Vec<_>>();
    items.sort_by_key(|item| item.0);

    let mut digest = Sha256::new();
    digest.update(b"teslatlas-edge-batch-v2\0");
    for (spool_seq, kind, id) in items {
        digest.update([kind]);
        digest.update(spool_seq.to_be_bytes());
        digest.update(id.as_bytes());
        digest.update([0]);
    }
    hex::encode(digest.finalize())
}

fn v2_ack_is_merged_prefix(batch: &HubBatchV2, acknowledgement: &HubAckV2) -> bool {
    let accepted_records = acknowledgement
        .accepted_record_ids
        .iter()
        .collect::<HashSet<_>>();
    let accepted_gaps = acknowledgement
        .accepted_gap_notice_ids
        .iter()
        .collect::<HashSet<_>>();
    let mut items = batch
        .records
        .iter()
        .map(|record| (record.spool_seq, true, &record.record_id))
        .chain(
            batch
                .gaps
                .iter()
                .map(|gap| (gap.spool_seq, false, &gap.notice_id)),
        )
        .collect::<Vec<_>>();
    items.sort_by_key(|item| item.0);

    let mut matched = 0_usize;
    let mut saw_unaccepted = false;
    for (_, is_record, id) in items {
        let accepted = if is_record {
            accepted_records.contains(id)
        } else {
            accepted_gaps.contains(id)
        };
        if accepted {
            if saw_unaccepted {
                return false;
            }
            matched = matched.saturating_add(1);
        } else {
            saw_unaccepted = true;
        }
    }
    matched == accepted_records.len().saturating_add(accepted_gaps.len())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn same_record_ids(left: &[RecordId], right: &[RecordId]) -> bool {
    left.len() == right.len()
        && left.iter().collect::<HashSet<_>>() == right.iter().collect::<HashSet<_>>()
}

fn remove_admitted_record(
    state: &mut State,
    admission: &ReceiptAdmission,
) -> Result<bool, SpoolError> {
    let Some(entry) = state.entries.get(&admission.legacy_record_id).cloned() else {
        return Ok(false);
    };
    if entry.spool_seq != admission.spool_seq
        || entry.stable_record_id != admission.stable_record_id
        || entry.record_id != admission.legacy_record_id
    {
        return Ok(false);
    }
    match fs::remove_file(&entry.path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(map_io_error(error)),
    }
    state.entries.remove(&entry.record_id);
    state.stable_index.remove(&entry.stable_record_id);
    state.pending_bytes = state.pending_bytes.saturating_sub(entry.encrypted_bytes);
    Ok(true)
}

fn ack_result(receipt: &AckReceipt) -> HubAckResultV1 {
    HubAckResultV1 {
        version: 1,
        acknowledged_record_ids: receipt.accepted_record_ids.clone(),
        unknown_record_ids: Vec::new(),
    }
}

fn ack_result_v2(receipt: &AckReceipt) -> HubAckResultV2 {
    HubAckResultV2 {
        version: 2,
        acknowledged_record_ids: receipt.accepted_stable_record_ids.clone(),
        acknowledged_gap_notice_ids: receipt.accepted_gap_notice_ids.clone(),
        unknown_record_ids: Vec::new(),
        unknown_gap_notice_ids: Vec::new(),
    }
}

fn read_spool_format_marker(root: &Path) -> Result<SpoolFormat, SpoolError> {
    let marker = root.join(FORMAT_MARKER_FILE);
    match fs::symlink_metadata(&marker) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.len() > MAX_FORMAT_MARKER_BYTES {
                return Err(SpoolError::CorruptRecord);
            }
            let contents = fs::read(&marker).map_err(map_io_error)?;
            if contents == format!("{SPOOL_FORMAT_VERSION}\n").as_bytes() {
                return Ok(SpoolFormat::Current);
            }
            if contents == format!("{PREVIOUS_SPOOL_FORMAT_VERSION}\n").as_bytes() {
                return Ok(SpoolFormat::Previous);
            }
            Err(SpoolError::CorruptRecord)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SpoolFormat::Fresh),
        Err(error) => Err(map_io_error(error)),
    }
}

fn reject_ambiguous_previous_receipts(receipts_dir: &Path) -> Result<(), SpoolError> {
    let mut entries = fs::read_dir(receipts_dir).map_err(map_io_error)?;
    if entries.next().transpose().map_err(map_io_error)?.is_some() {
        return Err(SpoolError::ReceiptRecoveryRequired);
    }
    Ok(())
}

fn write_current_spool_format_marker(
    root: &Path,
    temporary_dir: &Path,
    previous_format: SpoolFormat,
) -> Result<(), SpoolError> {
    let marker = root.join(FORMAT_MARKER_FILE);
    let current_format_marker = format!("{SPOOL_FORMAT_VERSION}\n");
    let temporary = temporary_dir.join(format!("{}.tmp", Uuid::new_v4()));
    match previous_format {
        SpoolFormat::Fresh => write_atomic(&temporary, &marker, current_format_marker.as_bytes()),
        SpoolFormat::Previous => {
            write_atomic_replace(&temporary, &marker, current_format_marker.as_bytes())
        }
        SpoolFormat::Current => Ok(()),
    }
}

fn write_atomic(temporary: &Path, destination: &Path, bytes: &[u8]) -> Result<(), SpoolError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(temporary).map_err(map_io_error)?;
    file.write_all(bytes).map_err(map_io_error)?;
    file.sync_all().map_err(map_io_error)?;
    drop(file);
    fs::rename(temporary, destination).map_err(map_io_error)?;
    sync_directory(destination.parent().ok_or(SpoolError::Io)?)
}

fn write_atomic_replace(
    temporary: &Path,
    destination: &Path,
    bytes: &[u8],
) -> Result<(), SpoolError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(temporary).map_err(map_io_error)?;
    file.write_all(bytes).map_err(map_io_error)?;
    file.sync_all().map_err(map_io_error)?;
    drop(file);
    fs::rename(temporary, destination).map_err(map_io_error)?;
    sync_directory(destination.parent().ok_or(SpoolError::Io)?)
}

fn quarantine_file(path: &Path, quarantine: &Path) -> Result<(), SpoolError> {
    let destination = quarantine.join(format!("{}.quarantine", Uuid::new_v4()));
    fs::rename(path, &destination).map_err(map_io_error)?;
    sync_directory(quarantine)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), SpoolError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(map_io_error)
}

fn map_crypto_error(error: CryptoError) -> SpoolError {
    match error {
        CryptoError::KeyMismatch => SpoolError::KeyMismatch,
        CryptoError::InvalidCiphertext => SpoolError::CorruptRecord,
    }
}

fn map_io_error(error: io::Error) -> SpoolError {
    if matches!(error.raw_os_error(), Some(28) | Some(112)) {
        SpoolError::StorageFull
    } else {
        SpoolError::Io
    }
}

fn acquire_spool_lock(directory: &Path) -> Result<File, SpoolError> {
    fs::create_dir_all(directory).map_err(map_io_error)?;
    let path = directory.join(SPOOL_LOCK_FILE);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    options.mode(0o600);
    let file = options.open(&path).map_err(map_io_error)?;
    // Keep an advisory non-blocking lock for the lifetime of Inner so another
    // process cannot enter recovery or mutate the same spool.
    match file.try_lock_exclusive() {
        Ok(true) => {
            set_private_file_permissions(&file)?;
            Ok(file)
        }
        Ok(false) => Err(SpoolError::AlreadyOpen),
        Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock) => {
            Err(SpoolError::AlreadyOpen)
        }
        Err(error) => Err(map_io_error(error)),
    }
}

fn set_private_file_permissions(file: &File) -> Result<(), SpoolError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(map_io_error)?;
    }
    Ok(())
}

fn set_private_directory_permissions(path: &Path) -> Result<(), SpoolError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(map_io_error)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn storage_full_errors_are_classified_without_retrying() {
        assert_eq!(
            map_io_error(io::Error::from_raw_os_error(28)),
            SpoolError::StorageFull
        );
    }

    #[test]
    fn pruned_v1_receipt_history_fails_closed_across_format2_migration_and_restart() {
        let temp = TempDir::new().unwrap();
        let config = SpoolConfig {
            directory: temp.path().join("spool"),
            max_bytes: 1_048_576,
            max_records: 8,
            retention_ms: 60_000,
            batch_max_bytes: 262_144,
            batch_max_records: 8,
        };
        let spool = Spool::open(
            config.clone(),
            SpoolKey::from_bytes([0x54; 32]),
            1_800_000_000_000,
        )
        .unwrap();
        let event = ReceiverEnvelope::parse(
            &serde_json::to_vec(&json!({
                "version": 1,
                "vin": "5YJ3E1EA7KF000001",
                "txid": "v1-history-pruned",
                "tx_type": "V",
                "received_at_ms": 1_800_000_000_100_i64,
                "timestamp_ms": 1_800_000_000_000_i64,
                "payload": {}
            }))
            .unwrap(),
        )
        .unwrap();
        spool.enqueue(event, 1_800_000_000_000).unwrap();
        {
            let mut state = spool.state().unwrap();
            for sequence in 0..=MAX_ACK_RECEIPTS {
                let batch_id = format!("{sequence:064x}");
                let path = spool
                    .receipts_dir()
                    .join(format!("{sequence:020}-{batch_id}{RECEIPT_SUFFIX}"));
                fs::write(&path, b"synthetic").unwrap();
                state.receipts.insert(
                    batch_id.clone(),
                    ReceiptEntry {
                        receipt: AckReceipt {
                            version: 1,
                            sequence: sequence as u64,
                            batch_id,
                            accepted_record_ids: Vec::new(),
                            accepted_stable_record_ids: Vec::new(),
                            accepted_gap_notice_ids: Vec::new(),
                            accepted_admissions: Vec::new(),
                        },
                        path,
                    },
                );
            }
        }
        spool.prune_receipts().unwrap();
        assert!(spool.state().unwrap().v1_ack_history_pruned);
        for entry in fs::read_dir(spool.receipts_dir()).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        drop(spool);

        // A legacy format-2 spool may have already pruned every receipt and
        // has no authoritative bit recording that boundary. The migration
        // must conservatively preserve the v1 fail-closed behavior.
        fs::remove_file(temp.path().join("spool/sequence.tlem")).unwrap();
        fs::write(temp.path().join("spool/FORMAT"), b"2\n").unwrap();

        let restarted =
            Spool::open(config, SpoolKey::from_bytes([0x54; 32]), 1_800_000_000_001).unwrap();
        assert!(restarted.state().unwrap().v1_ack_history_pruned);
        assert_eq!(fs::read(temp.path().join("spool/FORMAT")).unwrap(), b"3\n");
        let batch = restarted.next_batch(1_800_000_000_001).unwrap();
        assert_eq!(
            restarted
                .acknowledge(&HubAckV1 {
                    version: 1,
                    batch_id: batch.batch_id,
                    accepted_record_ids: batch
                        .records
                        .iter()
                        .map(|record| record.record_id.clone())
                        .collect(),
                })
                .unwrap_err(),
            SpoolError::V1AcknowledgementHistoryPruned
        );
        assert_eq!(restarted.snapshot(1_800_000_000_001).pending_records, 1);
    }

    #[test]
    fn legacy_v1_record_migrates_without_changing_v1_delivery_id() {
        let temp = TempDir::new().unwrap();
        let config = SpoolConfig {
            directory: temp.path().join("spool"),
            max_bytes: 1_048_576,
            max_records: 8,
            retention_ms: 60_000,
            batch_max_bytes: 262_144,
            batch_max_records: 8,
        };
        let key_bytes = [0x33; 32];
        drop(
            Spool::open(
                config.clone(),
                SpoolKey::from_bytes(key_bytes),
                1_800_000_000_000,
            )
            .unwrap(),
        );
        let envelope = ReceiverEnvelope::parse(
            &serde_json::to_vec(&json!({
                "version": 1,
                "vin": "5YJ3E1EA7KF000001",
                "txid": "legacy-tx",
                "tx_type": "V",
                "received_at_ms": 1_800_000_000_100_i64,
                "timestamp_ms": 1_800_000_000_000_i64,
                "payload": {}
            }))
            .unwrap(),
        )
        .unwrap();
        let legacy_id = envelope.record_id();
        let stored = StoredRecord {
            version: 1,
            record_id: legacy_id.clone(),
            stable_record_id: None,
            spool_seq: None,
            admitted_at_ms: 1_800_000_000_000,
            expires_at_ms: 1_800_000_060_000,
            envelope,
        };
        let encrypted = EncryptionKey::from_bytes(key_bytes)
            .encrypt(&serde_json::to_vec(&stored).unwrap())
            .unwrap();
        let path = config.directory.join("pending").join(format!(
            "{:020}-{}{}",
            stored.admitted_at_ms, legacy_id, PENDING_SUFFIX
        ));
        fs::write(path, encrypted).unwrap();

        let migrated =
            Spool::open(config, SpoolKey::from_bytes(key_bytes), 1_800_000_000_001).unwrap();
        assert_eq!(
            migrated.next_batch(1_800_000_000_001).unwrap().records[0].record_id,
            legacy_id
        );
        assert_eq!(
            migrated.next_batch_v2(1_800_000_000_001).unwrap().records[0].spool_seq,
            1
        );
        assert!(
            fs::read_dir(temp.path().join("spool/pending"))
                .unwrap()
                .all(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("v2-"))
        );
    }

    #[test]
    fn authenticated_stored_envelope_with_invalid_receiver_shape_is_quarantined() {
        let temp = TempDir::new().unwrap();
        let config = SpoolConfig {
            directory: temp.path().join("spool"),
            max_bytes: 1_048_576,
            max_records: 8,
            retention_ms: 60_000,
            batch_max_bytes: 262_144,
            batch_max_records: 8,
        };
        let key_bytes = [0x71; 32];
        let spool = Spool::open(
            config.clone(),
            SpoolKey::from_bytes(key_bytes),
            1_800_000_000_000,
        )
        .unwrap();
        let valid = ReceiverEnvelope::parse(
            &serde_json::to_vec(&json!({
                "version": 1,
                "vin": "5YJ3E1EA7KF000001",
                "txid": "authenticated-invalid-envelope",
                "tx_type": "V",
                "received_at_ms": 1_800_000_000_100_i64,
                "timestamp_ms": 1_800_000_000_000_i64,
                "payload": {}
            }))
            .unwrap(),
        )
        .unwrap();
        spool.enqueue(valid, 1_800_000_000_000).unwrap();
        drop(spool);

        let pending = fs::read_dir(config.directory.join("pending"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let encrypted = fs::read(&pending).unwrap();
        let plaintext = EncryptionKey::from_bytes(key_bytes)
            .decrypt(&encrypted)
            .unwrap();
        let mut stored: StoredRecord = serde_json::from_slice(&plaintext).unwrap();
        stored.envelope.tx_type = "unsupported".to_owned();
        stored.record_id = stored.envelope.record_id();
        stored.stable_record_id = Some(stored.envelope.stable_record_id());
        let replacement = config.directory.join("pending").join(format!(
            "v2-{:020}-{}{}",
            stored.spool_seq.unwrap(),
            stored.record_id,
            PENDING_SUFFIX
        ));
        fs::remove_file(&pending).unwrap();
        let replacement_ciphertext = EncryptionKey::from_bytes(key_bytes)
            .encrypt(&serde_json::to_vec(&stored).unwrap())
            .unwrap();
        fs::write(replacement, replacement_ciphertext).unwrap();

        let recovered =
            Spool::open(config, SpoolKey::from_bytes(key_bytes), 1_800_000_000_001).unwrap();
        let batch = recovered.next_batch_v2(1_800_000_000_001).unwrap();
        assert!(batch.records.is_empty());
        assert_eq!(batch.gaps.len(), 1);
        assert_eq!(batch.gaps[0].reason, GapReasonV2::IntegrityQuarantine);
        assert_eq!(recovered.snapshot(1_800_000_000_001).pending_records, 0);
    }
}
