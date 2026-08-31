#![forbid(unsafe_code)]

mod support;

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use teslatlas_edge::protocol::{GapReasonV2, HubAckV1, HubAckV2, HubBatchV2};
use teslatlas_edge::spool::{EnqueueOutcome, Spool, SpoolConfig, SpoolError, SpoolKey};

use support::{T0, VIN, receiver_envelope};

fn config(temp: &TempDir) -> SpoolConfig {
    SpoolConfig {
        directory: temp.path().join("spool"),
        max_bytes: 1_048_576,
        max_records: 16,
        retention_ms: 60_000,
        batch_max_bytes: 262_144,
        batch_max_records: 8,
    }
}

fn key() -> SpoolKey {
    SpoolKey::from_bytes([0x42; 32])
}

fn expected_v2_batch_id(batch: &HubBatchV2) -> String {
    let mut items = batch
        .records
        .iter()
        .map(|record| (record.spool_seq, b'r', record.record_id.as_str()))
        .chain(
            batch
                .gaps
                .iter()
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

#[test]
fn encrypts_pending_records_and_recovers_after_restart() {
    let temp = TempDir::new().unwrap();
    let spool = Spool::open(config(&temp), key(), T0).unwrap();
    let record = receiver_envelope("tx-encrypted", T0);

    assert_eq!(
        spool.enqueue(record.clone(), T0).unwrap(),
        EnqueueOutcome::Stored(record.record_id())
    );
    let pending_path = fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let encrypted = fs::read(&pending_path).unwrap();
    assert!(
        !encrypted
            .windows(VIN.len())
            .any(|window| window == VIN.as_bytes())
    );
    assert!(!encrypted.windows(3).any(|window| window == b"Soc"));

    drop(spool);
    let recovered = Spool::open(config(&temp), key(), T0 + 1_000).unwrap();
    let batch = recovered.next_batch(T0 + 1_000).unwrap();
    assert_eq!(batch.records.len(), 1);
    assert_eq!(batch.records[0].record_id, record.record_id());
    assert_eq!(batch.records[0].envelope, record);
}

#[test]
fn duplicate_enqueue_is_idempotent_and_ack_deletes_only_named_records() {
    let temp = TempDir::new().unwrap();
    let spool = Spool::open(config(&temp), key(), T0).unwrap();
    let first = receiver_envelope("tx-first", T0);
    let second = receiver_envelope("tx-second", T0 + 1_000);

    assert!(matches!(
        spool.enqueue(first.clone(), T0).unwrap(),
        EnqueueOutcome::Stored(_)
    ));
    assert_eq!(
        spool.enqueue(first.clone(), T0 + 1).unwrap(),
        EnqueueOutcome::AlreadyPresent(first.record_id())
    );
    spool.enqueue(second.clone(), T0 + 2).unwrap();

    let batch = spool.next_batch(T0 + 3).unwrap();
    let result = spool
        .acknowledge(&HubAckV1 {
            version: 1,
            batch_id: batch.batch_id,
            accepted_record_ids: vec![second.record_id()],
        })
        .unwrap();
    assert_eq!(result.acknowledged_record_ids, vec![second.record_id()]);
    assert!(result.unknown_record_ids.is_empty());

    let remaining = spool.next_batch(T0 + 4).unwrap();
    assert_eq!(remaining.records.len(), 1);
    assert_eq!(remaining.records[0].record_id, first.record_id());
}

#[test]
fn retry_with_new_receiver_time_deduplicates_by_stable_identity() {
    let temp = TempDir::new().unwrap();
    let spool = Spool::open(config(&temp), key(), T0).unwrap();
    let first = receiver_envelope("tx-retry", T0);
    let mut retry = first.clone();
    retry.received_at_ms += 5_000;

    assert_eq!(
        spool.enqueue(first.clone(), T0).unwrap(),
        EnqueueOutcome::Stored(first.record_id())
    );
    assert_eq!(
        spool.enqueue(retry, T0 + 5_000).unwrap(),
        EnqueueOutcome::AlreadyPresent(first.record_id())
    );
    assert_eq!(spool.snapshot(T0 + 5_000).pending_records, 1);
}

#[test]
fn queue_record_and_byte_limits_reject_without_partial_files() {
    let temp = TempDir::new().unwrap();
    let mut bounded = config(&temp);
    bounded.max_records = 1;
    bounded.batch_max_records = 1;
    let spool = Spool::open(bounded, key(), T0).unwrap();
    spool.enqueue(receiver_envelope("tx-one", T0), T0).unwrap();
    assert_eq!(
        spool
            .enqueue(receiver_envelope("tx-two", T0 + 1_000), T0 + 1)
            .unwrap_err(),
        SpoolError::CapacityExceeded
    );
    assert_eq!(
        fs::read_dir(temp.path().join("spool/pending"))
            .unwrap()
            .count(),
        1
    );

    let second_temp = TempDir::new().unwrap();
    let mut byte_bounded = config(&second_temp);
    byte_bounded.max_bytes = 32;
    let byte_spool = Spool::open(byte_bounded, key(), T0).unwrap();
    assert_eq!(
        byte_spool
            .enqueue(receiver_envelope("tx-large", T0), T0)
            .unwrap_err(),
        SpoolError::CapacityExceeded
    );
    assert_eq!(
        fs::read_dir(second_temp.path().join("spool/pending"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn corrupt_ciphertext_is_quarantined_while_valid_records_continue() {
    let temp = TempDir::new().unwrap();
    let spool = Spool::open(config(&temp), key(), T0).unwrap();
    spool.enqueue(receiver_envelope("tx-good", T0), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-corrupt", T0 + 1_000), T0 + 1)
        .unwrap();
    drop(spool);

    let mut paths = fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    let corrupt_path = paths.pop().unwrap();
    let length = fs::metadata(&corrupt_path).unwrap().len();
    let mut file = OpenOptions::new().write(true).open(&corrupt_path).unwrap();
    file.seek(SeekFrom::Start(length - 1)).unwrap();
    file.write_all(&[0xff]).unwrap();
    file.sync_all().unwrap();

    let recovered = Spool::open(config(&temp), key(), T0 + 2_000).unwrap();
    let snapshot = recovered.snapshot(T0 + 2_000);
    assert_eq!(snapshot.pending_records, 1);
    assert_eq!(snapshot.corrupt_records, 1);
    assert!(snapshot.degraded);
    let batch = recovered.next_batch_v2(T0 + 2_000).unwrap();
    assert_eq!(batch.records.len(), 1);
    assert_eq!(batch.records[0].envelope.txid, "tx-good");
    assert_eq!(batch.gaps.len(), 1);
    assert_eq!(batch.gaps[0].reason, GapReasonV2::IntegrityQuarantine);
    assert_eq!(
        fs::read_dir(temp.path().join("spool/quarantine"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn retention_expiry_is_visible_and_never_delivered() {
    let temp = TempDir::new().unwrap();
    let mut expiring = config(&temp);
    expiring.retention_ms = 1_000;
    let spool = Spool::open(expiring, key(), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-expired", T0), T0)
        .unwrap();

    assert_eq!(spool.expire_due(T0 + 1_001).unwrap(), 1);
    assert_eq!(
        spool.next_batch(T0 + 1_001).unwrap_err(),
        SpoolError::ProtocolUpgradeRequired
    );
    let batch = spool.next_batch_v2(T0 + 1_001).unwrap();
    assert!(batch.records.is_empty());
    assert_eq!(batch.gaps.len(), 1);
    assert_eq!(batch.gaps[0].reason, GapReasonV2::RetentionExpired);
    let snapshot = spool.snapshot(T0 + 1_001);
    assert_eq!(snapshot.expired_records, 1);
    assert!(snapshot.degraded);
}

#[test]
fn retention_gap_survives_restart_until_exact_v2_acknowledgement() {
    let temp = TempDir::new().unwrap();
    let mut expiring = config(&temp);
    expiring.retention_ms = 1_000;
    let spool = Spool::open(expiring.clone(), key(), T0).unwrap();
    spool.enqueue(receiver_envelope("tx-gap", T0), T0).unwrap();
    let first = spool.next_batch_v2(T0 + 1_001).unwrap();
    assert_eq!(first.gaps.len(), 1);
    let notice_id = first.gaps[0].notice_id.clone();
    drop(spool);

    let restarted = Spool::open(expiring.clone(), key(), T0 + 2_000).unwrap();
    assert_eq!(restarted.snapshot(T0 + 2_000).expired_records, 1);
    let replay = restarted.next_batch_v2(T0 + 2_000).unwrap();
    assert_eq!(replay.gaps[0].notice_id, notice_id);
    let acknowledgement = HubAckV2 {
        version: 2,
        batch_id: replay.batch_id,
        accepted_record_ids: Vec::new(),
        accepted_gap_notice_ids: vec![notice_id.clone()],
    };
    let result = restarted.acknowledge_v2(&acknowledgement).unwrap();
    assert_eq!(result.acknowledged_gap_notice_ids, vec![notice_id]);
    assert!(restarted.next_batch_v2(T0 + 2_001).unwrap().gaps.is_empty());
    let reconciled = restarted.snapshot(T0 + 2_001);
    assert_eq!(reconciled.expired_records, 1);
    assert!(!reconciled.degraded);
    drop(restarted);

    let replayed_receipt = Spool::open(expiring, key(), T0 + 3_000).unwrap();
    assert_eq!(replayed_receipt.snapshot(T0 + 3_000).expired_records, 1);
    assert_eq!(
        replayed_receipt.acknowledge_v2(&acknowledgement).unwrap(),
        result
    );
}

#[test]
fn runtime_corruption_becomes_gap_before_later_sequence_delivery() {
    let temp = TempDir::new().unwrap();
    let spool = Spool::open(config(&temp), key(), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-corrupt-first", T0), T0)
        .unwrap();
    spool
        .enqueue(receiver_envelope("tx-good-later", T0 + 1), T0 + 1)
        .unwrap();

    let mut paths = fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    let corrupt_path = &paths[0];
    let length = fs::metadata(corrupt_path).unwrap().len();
    let mut file = OpenOptions::new().write(true).open(corrupt_path).unwrap();
    file.seek(SeekFrom::Start(length - 1)).unwrap();
    file.write_all(&[0xff]).unwrap();
    file.sync_all().unwrap();

    let batch = spool.next_batch_v2(T0 + 2).unwrap();
    assert_eq!(batch.gaps.len(), 1);
    assert_eq!(batch.gaps[0].reason, GapReasonV2::IntegrityQuarantine);
    assert_eq!(batch.gaps[0].spool_seq, 1);
    assert_eq!(batch.records.len(), 1);
    assert_eq!(batch.records[0].spool_seq, 2);
    assert_eq!(batch.records[0].envelope.txid, "tx-good-later");
    assert_eq!(spool.snapshot(T0 + 2).corrupt_records, 1);
}

#[test]
fn durable_gap_wins_over_reappearing_source_and_sequence_stays_unique() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config.clone(), key(), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-corrupt", T0), T0)
        .unwrap();
    let pending_path = fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let saved_path = temp.path().join("saved-pending.tles");
    fs::copy(&pending_path, &saved_path).unwrap();

    let length = fs::metadata(&pending_path).unwrap().len();
    let mut file = OpenOptions::new().write(true).open(&pending_path).unwrap();
    file.seek(SeekFrom::Start(length - 1)).unwrap();
    file.write_all(&[0xff]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let first = spool.next_batch_v2(T0 + 1).unwrap();
    assert_eq!(first.gaps.len(), 1);
    assert_eq!(first.gaps[0].spool_seq, 1);
    drop(spool);

    fs::copy(&saved_path, &pending_path).unwrap();
    fs::remove_file(temp.path().join("spool/sequence.tlem")).unwrap();
    let recovered = Spool::open(spool_config, key(), T0 + 2).unwrap();
    let replay = recovered.next_batch_v2(T0 + 2).unwrap();
    assert!(replay.records.is_empty());
    assert_eq!(replay.gaps.len(), 1);
    assert_eq!(replay.gaps[0].spool_seq, 1);

    recovered
        .enqueue(receiver_envelope("tx-after-gap", T0 + 2), T0 + 2)
        .unwrap();
    let with_later_record = recovered.next_batch_v2(T0 + 3).unwrap();
    assert_eq!(with_later_record.gaps[0].spool_seq, 1);
    assert_eq!(with_later_record.records[0].spool_seq, 2);
}

#[test]
fn v2_acknowledgement_must_be_a_merged_sequence_prefix() {
    let temp = TempDir::new().unwrap();
    let mut expiring = config(&temp);
    expiring.retention_ms = 1_000;
    let spool = Spool::open(expiring, key(), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-gap-first", T0), T0)
        .unwrap();
    spool
        .enqueue(
            receiver_envelope("tx-record-second", T0 + 1_000),
            T0 + 1_000,
        )
        .unwrap();

    let batch = spool.next_batch_v2(T0 + 1_001).unwrap();
    assert_eq!(batch.gaps[0].spool_seq, 1);
    assert_eq!(batch.records[0].spool_seq, 2);
    assert_eq!(batch.batch_id, expected_v2_batch_id(&batch));
    assert_eq!(
        spool
            .acknowledge_v2(&HubAckV2 {
                version: 2,
                batch_id: batch.batch_id.clone(),
                accepted_record_ids: vec![batch.records[0].record_id.clone()],
                accepted_gap_notice_ids: Vec::new(),
            })
            .unwrap_err(),
        SpoolError::InvalidAcknowledgement
    );
    assert_eq!(spool.snapshot(T0 + 1_001).pending_records, 1);
    assert_eq!(spool.snapshot(T0 + 1_001).pending_gap_notices, 1);

    spool
        .acknowledge_v2(&HubAckV2 {
            version: 2,
            batch_id: batch.batch_id,
            accepted_record_ids: Vec::new(),
            accepted_gap_notice_ids: vec![batch.gaps[0].notice_id.clone()],
        })
        .unwrap();
    let remaining = spool.next_batch_v2(T0 + 1_002).unwrap();
    assert!(remaining.gaps.is_empty());
    assert_eq!(remaining.records[0].spool_seq, 2);
}

#[test]
fn stored_retention_deadline_survives_configuration_change() {
    let temp = TempDir::new().unwrap();
    let mut short = config(&temp);
    short.retention_ms = 1_000;
    let spool = Spool::open(short, key(), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-deadline", T0), T0)
        .unwrap();
    drop(spool);

    let mut longer = config(&temp);
    longer.retention_ms = 60_000;
    let recovered = Spool::open(longer, key(), T0 + 1_001).unwrap();
    let batch = recovered.next_batch_v2(T0 + 1_001).unwrap();
    assert!(batch.records.is_empty());
    assert_eq!(batch.gaps.len(), 1);
    assert_eq!(recovered.snapshot(T0 + 1_001).expired_records, 1);
}

#[test]
fn spool_sequence_remains_monotonic_after_empty_restart() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config.clone(), key(), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-seq-one", T0), T0)
        .unwrap();
    let first_v2 = spool.next_batch_v2(T0 + 1).unwrap();
    assert_eq!(first_v2.records[0].spool_seq, 1);

    let first_v1 = spool.next_batch(T0 + 1).unwrap();
    spool
        .acknowledge(&HubAckV1 {
            version: 1,
            batch_id: first_v1.batch_id,
            accepted_record_ids: vec![first_v1.records[0].record_id.clone()],
        })
        .unwrap();
    drop(spool);

    let restarted = Spool::open(spool_config, key(), T0 + 2).unwrap();
    restarted
        .enqueue(receiver_envelope("tx-seq-two", T0 + 2), T0 + 2)
        .unwrap();
    let second_v2 = restarted.next_batch_v2(T0 + 3).unwrap();
    assert_eq!(second_v2.records[0].spool_seq, 2);
}

#[test]
fn acknowledgement_receipt_survives_restart_and_makes_retry_idempotent() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config.clone(), key(), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-acked", T0), T0)
        .unwrap();
    let batch = spool.next_batch(T0 + 1).unwrap();
    let acknowledgement = HubAckV1 {
        version: 1,
        batch_id: batch.batch_id,
        accepted_record_ids: vec![batch.records[0].record_id.clone()],
    };
    let first = spool.acknowledge(&acknowledgement).unwrap();
    drop(spool);

    let restarted = Spool::open(spool_config, key(), T0 + 2).unwrap();
    let retry = restarted.acknowledge(&acknowledgement).unwrap();
    assert_eq!(retry, first);
    assert!(restarted.next_batch(T0 + 2).unwrap().records.is_empty());
    assert_eq!(
        fs::read_dir(temp.path().join("spool/receipts"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn orphan_temporary_file_is_counted_once_after_quarantine() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    drop(Spool::open(spool_config.clone(), key(), T0).unwrap());
    fs::write(temp.path().join("spool/tmp/orphan.tmp"), b"partial").unwrap();

    let recovered = Spool::open(spool_config, key(), T0 + 1).unwrap();
    let snapshot = recovered.snapshot(T0 + 1);
    assert_eq!(snapshot.corrupt_records, 1);
    assert!(snapshot.degraded);
    assert_eq!(
        recovered.next_batch_v2(T0 + 1).unwrap_err(),
        SpoolError::CorruptRecord
    );
    assert_eq!(
        fs::read_dir(temp.path().join("spool/quarantine"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn v2_spool_format_marker_is_persisted_and_validated() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    drop(Spool::open(spool_config.clone(), key(), T0).unwrap());
    let marker = temp.path().join("spool/FORMAT");
    assert_eq!(fs::read(&marker).unwrap(), b"2\n");

    fs::write(marker, b"1\n").unwrap();
    assert!(Spool::open(spool_config, key(), T0 + 1).is_err());
}
