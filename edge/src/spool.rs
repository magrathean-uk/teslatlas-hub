use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::crypto::{CryptoError, EncryptionKey};
use crate::protocol::{
    HubAckResultV1, HubAckV1, HubBatchRecordV1, HubBatchV1, ReceiverEnvelope, RecordId,
};

const PENDING_SUFFIX: &str = ".tles";
const RECEIPT_SUFFIX: &str = ".tlea";
const MAX_ACK_RECEIPTS: usize = 1_024;

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
    pub oldest_age_seconds: u64,
    pub degraded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpoolError {
    #[error("invalid spool configuration")]
    InvalidConfig,
    #[error("spool capacity exceeded")]
    CapacityExceeded,
    #[error("storage is full")]
    StorageFull,
    #[error("spool key does not match pending records")]
    KeyMismatch,
    #[error("encrypted spool record is corrupt")]
    CorruptRecord,
    #[error("spool input or output failed")]
    Io,
    #[error("spool serialization failed")]
    Serialization,
    #[error("invalid acknowledgement")]
    InvalidAcknowledgement,
}

#[derive(Clone)]
pub struct Spool {
    inner: Arc<Inner>,
}

struct Inner {
    config: SpoolConfig,
    key: EncryptionKey,
    state: Mutex<State>,
    acknowledgement_lock: Mutex<()>,
}

#[derive(Default)]
struct State {
    entries: BTreeMap<RecordId, Entry>,
    receipts: BTreeMap<String, ReceiptEntry>,
    next_receipt_sequence: u64,
    pending_bytes: u64,
    corrupt_records: u64,
    expired_records: u64,
}

#[derive(Clone)]
struct Entry {
    record_id: RecordId,
    admitted_at_ms: i64,
    encrypted_bytes: u64,
    path: PathBuf,
}

#[derive(Clone)]
struct ReceiptEntry {
    receipt: AckReceipt,
    path: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRecord {
    version: u16,
    record_id: RecordId,
    admitted_at_ms: i64,
    expires_at_ms: i64,
    envelope: ReceiverEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AckReceipt {
    version: u16,
    sequence: u64,
    batch_id: String,
    accepted_record_ids: Vec<RecordId>,
}

impl Spool {
    pub fn open(config: SpoolConfig, key: SpoolKey, now_ms: i64) -> Result<Self, SpoolError> {
        validate_config(&config)?;
        let pending = config.directory.join("pending");
        let temporary = config.directory.join("tmp");
        let quarantine = config.directory.join("quarantine");
        let receipts = config.directory.join("receipts");
        for directory in [
            &config.directory,
            &pending,
            &temporary,
            &quarantine,
            &receipts,
        ] {
            fs::create_dir_all(directory).map_err(map_io_error)?;
            set_private_directory_permissions(directory)?;
        }

        let inner = Arc::new(Inner {
            config,
            key: EncryptionKey::from_bytes(key.0),
            state: Mutex::new(State::default()),
            acknowledgement_lock: Mutex::new(()),
        });
        let spool = Self { inner };
        spool.recover(now_ms)?;
        Ok(spool)
    }

    pub fn enqueue(
        &self,
        envelope: ReceiverEnvelope,
        admitted_at_ms: i64,
    ) -> Result<EnqueueOutcome, SpoolError> {
        let record_id = envelope.record_id();
        let mut state = self.state()?;
        if state.entries.contains_key(&record_id) {
            return Ok(EnqueueOutcome::AlreadyPresent(record_id));
        }
        let stored = StoredRecord {
            version: 1,
            record_id: record_id.clone(),
            admitted_at_ms,
            expires_at_ms: admitted_at_ms.saturating_add(self.inner.config.retention_ms),
            envelope,
        };
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

        let file_name = format!(
            "{:020}-{}{}",
            admitted_at_ms.max(0),
            record_id,
            PENDING_SUFFIX
        );
        let destination = self.pending_dir().join(file_name);
        let temporary = self.temporary_dir().join(format!("{}.tmp", Uuid::new_v4()));
        write_atomic(&temporary, &destination, &encrypted)?;

        state.pending_bytes = state.pending_bytes.saturating_add(encrypted_bytes);
        state.entries.insert(
            record_id.clone(),
            Entry {
                record_id: record_id.clone(),
                admitted_at_ms,
                encrypted_bytes,
                path: destination,
            },
        );
        Ok(EnqueueOutcome::Stored(record_id))
    }

    pub fn next_batch(&self, now_ms: i64) -> Result<HubBatchV1, SpoolError> {
        self.expire_due(now_ms)?;
        self.build_batch()
    }

    fn build_batch(&self) -> Result<HubBatchV1, SpoolError> {
        let state = self.state()?;
        let mut ordered = state.entries.values().cloned().collect::<Vec<_>>();
        ordered.sort_by(|left, right| {
            (left.admitted_at_ms, &left.record_id).cmp(&(right.admitted_at_ms, &right.record_id))
        });
        let mut records = Vec::new();
        let mut body_bytes = 0_usize;
        for entry in ordered {
            if records.len() >= self.inner.config.batch_max_records {
                break;
            }
            let stored = self.read_entry(&entry)?;
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
                return Err(SpoolError::InvalidConfig);
            }
            body_bytes = body_bytes.saturating_add(encoded_bytes);
            records.push(record);
        }
        let batch_id = batch_id(&records);
        Ok(HubBatchV1 {
            version: 1,
            batch_id,
            records,
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
        };
        let plaintext = serde_json::to_vec(&receipt).map_err(|_| SpoolError::Serialization)?;
        let encrypted = self
            .inner
            .key
            .encrypt(&plaintext)
            .map_err(map_crypto_error)?;
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

    fn complete_receipt(&self, receipt: &AckReceipt) -> Result<(), SpoolError> {
        let mut state = self.state()?;
        for record_id in &receipt.accepted_record_ids {
            let Some(entry) = state.entries.get(record_id).cloned() else {
                continue;
            };
            match fs::remove_file(&entry.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(map_io_error(error)),
            }
            state.entries.remove(record_id);
            state.pending_bytes = state.pending_bytes.saturating_sub(entry.encrypted_bytes);
        }
        sync_directory(&self.pending_dir())?;
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
            .filter(|entry| {
                entry
                    .admitted_at_ms
                    .saturating_add(self.inner.config.retention_ms)
                    < now_ms
            })
            .cloned()
            .collect::<Vec<_>>();
        for entry in &expired {
            match fs::remove_file(&entry.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(map_io_error(error)),
            }
            state.entries.remove(&entry.record_id);
            state.pending_bytes = state.pending_bytes.saturating_sub(entry.encrypted_bytes);
        }
        let expired_count = u64::try_from(expired.len()).unwrap_or(u64::MAX);
        state.expired_records = state.expired_records.saturating_add(expired_count);
        if !expired.is_empty() {
            sync_directory(&self.pending_dir())?;
        }
        Ok(expired_count)
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
        SpoolSnapshot {
            pending_records: state.entries.len(),
            pending_bytes: state.pending_bytes,
            corrupt_records: state.corrupt_records,
            expired_records: state.expired_records,
            oldest_age_seconds,
            degraded: state.corrupt_records > 0 || state.expired_records > 0,
        }
    }

    pub fn sync(&self) -> Result<(), SpoolError> {
        sync_directory(&self.pending_dir())?;
        sync_directory(&self.temporary_dir())?;
        sync_directory(&self.quarantine_dir())?;
        sync_directory(&self.receipts_dir())?;
        sync_directory(&self.inner.config.directory)
    }

    fn recover(&self, now_ms: i64) -> Result<(), SpoolError> {
        let mut state = self.state()?;
        for result in fs::read_dir(self.quarantine_dir()).map_err(map_io_error)? {
            result.map_err(map_io_error)?;
            state.corrupt_records = state.corrupt_records.saturating_add(1);
        }
        for result in fs::read_dir(self.temporary_dir()).map_err(map_io_error)? {
            let path = result.map_err(map_io_error)?.path();
            quarantine_file(&path, &self.quarantine_dir())?;
            state.corrupt_records = state.corrupt_records.saturating_add(1);
        }
        for result in fs::read_dir(self.pending_dir()).map_err(map_io_error)? {
            let entry = result.map_err(map_io_error)?;
            let path = entry.path();
            if !entry.file_type().map_err(map_io_error)?.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("tles")
            {
                quarantine_file(&path, &self.quarantine_dir())?;
                state.corrupt_records = state.corrupt_records.saturating_add(1);
                continue;
            }
            let encrypted = fs::read(&path).map_err(map_io_error)?;
            let plaintext = match self.inner.key.decrypt(&encrypted) {
                Ok(plaintext) => plaintext,
                Err(CryptoError::KeyMismatch) => return Err(SpoolError::KeyMismatch),
                Err(CryptoError::InvalidCiphertext) => {
                    quarantine_file(&path, &self.quarantine_dir())?;
                    state.corrupt_records = state.corrupt_records.saturating_add(1);
                    continue;
                }
            };
            let stored: StoredRecord = match serde_json::from_slice(&plaintext) {
                Ok(stored) => stored,
                Err(_) => {
                    quarantine_file(&path, &self.quarantine_dir())?;
                    state.corrupt_records = state.corrupt_records.saturating_add(1);
                    continue;
                }
            };
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if stored.version != 1
                || stored.record_id != stored.envelope.record_id()
                || !file_name.ends_with(&format!("-{}{}", stored.record_id, PENDING_SUFFIX))
            {
                quarantine_file(&path, &self.quarantine_dir())?;
                state.corrupt_records = state.corrupt_records.saturating_add(1);
                continue;
            }
            let encrypted_bytes =
                u64::try_from(encrypted.len()).map_err(|_| SpoolError::CapacityExceeded)?;
            if state.entries.contains_key(&stored.record_id)
                || state.entries.len() >= self.inner.config.max_records
                || state.pending_bytes.saturating_add(encrypted_bytes) > self.inner.config.max_bytes
            {
                return Err(SpoolError::CapacityExceeded);
            }
            state.pending_bytes = state.pending_bytes.saturating_add(encrypted_bytes);
            state.entries.insert(
                stored.record_id.clone(),
                Entry {
                    record_id: stored.record_id,
                    admitted_at_ms: stored.admitted_at_ms,
                    encrypted_bytes,
                    path,
                },
            );
        }
        self.recover_receipts(&mut state)?;
        drop(state);
        self.expire_due(now_ms)?;
        self.prune_receipts()?;
        Ok(())
    }

    fn recover_receipts(&self, state: &mut State) -> Result<(), SpoolError> {
        for result in fs::read_dir(self.receipts_dir()).map_err(map_io_error)? {
            let entry = result.map_err(map_io_error)?;
            let path = entry.path();
            if !entry.file_type().map_err(map_io_error)?.is_file()
                || path.extension().and_then(|extension| extension.to_str()) != Some("tlea")
            {
                quarantine_file(&path, &self.quarantine_dir())?;
                state.corrupt_records = state.corrupt_records.saturating_add(1);
                continue;
            }
            let encrypted = fs::read(&path).map_err(map_io_error)?;
            let plaintext = match self.inner.key.decrypt(&encrypted) {
                Ok(plaintext) => plaintext,
                Err(CryptoError::KeyMismatch) => return Err(SpoolError::KeyMismatch),
                Err(CryptoError::InvalidCiphertext) => {
                    quarantine_file(&path, &self.quarantine_dir())?;
                    state.corrupt_records = state.corrupt_records.saturating_add(1);
                    continue;
                }
            };
            let receipt: AckReceipt = match serde_json::from_slice(&plaintext) {
                Ok(receipt) => receipt,
                Err(_) => {
                    quarantine_file(&path, &self.quarantine_dir())?;
                    state.corrupt_records = state.corrupt_records.saturating_add(1);
                    continue;
                }
            };
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let unique_ids = receipt.accepted_record_ids.iter().collect::<HashSet<_>>();
            if receipt.version != 1
                || !valid_digest(&receipt.batch_id)
                || receipt.accepted_record_ids.len() > 256
                || unique_ids.len() != receipt.accepted_record_ids.len()
                || file_name
                    != format!(
                        "{:020}-{}{}",
                        receipt.sequence, receipt.batch_id, RECEIPT_SUFFIX
                    )
                || state.receipts.contains_key(&receipt.batch_id)
            {
                quarantine_file(&path, &self.quarantine_dir())?;
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
        for receipt in receipts {
            for record_id in receipt.accepted_record_ids {
                let Some(entry) = state.entries.get(&record_id).cloned() else {
                    continue;
                };
                match fs::remove_file(&entry.path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(map_io_error(error)),
                }
                state.entries.remove(&record_id);
                state.pending_bytes = state.pending_bytes.saturating_sub(entry.encrypted_bytes);
                removed_pending = true;
            }
        }
        if removed_pending {
            sync_directory(&self.pending_dir())?;
        }
        Ok(())
    }

    fn read_entry(&self, entry: &Entry) -> Result<StoredRecord, SpoolError> {
        let encrypted = fs::read(&entry.path).map_err(map_io_error)?;
        let plaintext = self
            .inner
            .key
            .decrypt(&encrypted)
            .map_err(map_crypto_error)?;
        let stored: StoredRecord =
            serde_json::from_slice(&plaintext).map_err(|_| SpoolError::CorruptRecord)?;
        if stored.version != 1
            || stored.record_id != entry.record_id
            || stored.record_id != stored.envelope.record_id()
        {
            return Err(SpoolError::CorruptRecord);
        }
        Ok(stored)
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

fn batch_id(records: &[HubBatchRecordV1]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"teslatlas-edge-batch-v1\0");
    for record in records {
        digest.update(record.record_id.as_str().as_bytes());
        digest.update([0]);
    }
    hex::encode(digest.finalize())
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

fn ack_result(receipt: &AckReceipt) -> HubAckResultV1 {
    HubAckResultV1 {
        version: 1,
        acknowledged_record_ids: receipt.accepted_record_ids.clone(),
        unknown_record_ids: Vec::new(),
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

    #[test]
    fn storage_full_errors_are_classified_without_retrying() {
        assert_eq!(
            map_io_error(io::Error::from_raw_os_error(28)),
            SpoolError::StorageFull
        );
    }
}
