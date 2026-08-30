#![forbid(unsafe_code)]

mod support;

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};

use tempfile::TempDir;
use teslatlas_edge::protocol::HubAckV1;
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
    let batch = recovered.next_batch(T0 + 2_000).unwrap();
    assert_eq!(batch.records.len(), 1);
    assert_eq!(batch.records[0].envelope.txid, "tx-good");
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
    assert!(spool.next_batch(T0 + 1_001).unwrap().records.is_empty());
    let snapshot = spool.snapshot(T0 + 1_001);
    assert_eq!(snapshot.expired_records, 1);
    assert!(snapshot.degraded);
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
    assert_eq!(recovered.snapshot(T0 + 1).corrupt_records, 1);
    assert_eq!(
        fs::read_dir(temp.path().join("spool/quarantine"))
            .unwrap()
            .count(),
        1
    );
}
