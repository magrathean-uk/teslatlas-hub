#![forbid(unsafe_code)]

mod support;

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use teslatlas_edge::protocol::{GapReasonV2, HubAckV1, HubAckV2, HubBatchV2, ReceiverEnvelope};
use teslatlas_edge::spool::{EnqueueOutcome, Spool, SpoolConfig, SpoolError, SpoolKey};

use support::{T0, VIN, receiver_envelope};

fn config(temp: &TempDir) -> SpoolConfig {
    config_for_directory(temp.path().join("spool"))
}

fn config_for_directory(directory: PathBuf) -> SpoolConfig {
    SpoolConfig {
        directory,
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
fn old_v1_ack_cannot_delete_an_identical_legacy_reenqueue() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config, key(), T0).unwrap();
    let event = receiver_envelope("tx-v1-reenqueue", T0);
    spool.enqueue(event.clone(), T0).unwrap();

    let first_batch = spool.next_batch(T0 + 1).unwrap();
    let old_ack = HubAckV1 {
        version: 1,
        batch_id: first_batch.batch_id.clone(),
        accepted_record_ids: vec![first_batch.records[0].record_id.clone()],
    };
    spool.acknowledge(&old_ack).unwrap();

    spool.enqueue(event.clone(), T0 + 2).unwrap();
    assert_eq!(
        spool.acknowledge(&old_ack).unwrap().acknowledged_record_ids,
        vec![event.record_id()]
    );
    let remaining = spool.next_batch(T0 + 3).unwrap();
    assert_eq!(remaining.records.len(), 1);
    assert_eq!(remaining.records[0].record_id, event.record_id());
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
    let sequence_path = temp.path().join("spool/sequence.tlem");
    let saved_sequence = fs::read(&sequence_path).unwrap();
    fs::remove_file(&sequence_path).unwrap();
    assert!(matches!(
        Spool::open(spool_config.clone(), key(), T0 + 2),
        Err(SpoolError::SequenceStateMissing)
    ));
    fs::write(&sequence_path, saved_sequence).unwrap();
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
fn second_spool_opener_fails_before_touching_existing_state() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config.clone(), key(), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-lock-owner", T0), T0)
        .unwrap();
    let pending_before = fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_owned(),
                fs::read(path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let sequence_before = fs::read(temp.path().join("spool/sequence.tlem")).unwrap();
    assert!(matches!(
        Spool::open(spool_config.clone(), key(), T0 + 1),
        Err(SpoolError::AlreadyOpen)
    ));
    assert_eq!(
        fs::read(temp.path().join("spool/sequence.tlem")).unwrap(),
        sequence_before
    );
    let pending_after = fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_owned(),
                fs::read(path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(pending_after, pending_before);
    drop(spool);
    let reopened = Spool::open(spool_config, key(), T0 + 2).unwrap();
    assert_eq!(reopened.snapshot(T0 + 2).pending_records, 1);
}

#[test]
fn second_spool_opener_fails_in_a_separate_process() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let owner = Spool::open(spool_config.clone(), key(), T0).unwrap();
    owner
        .enqueue(receiver_envelope("tx-cross-process-lock", T0), T0)
        .unwrap();

    let ready = temp.path().join("child-ready");
    let release = temp.path().join("child-release");
    let child_exe = std::env::current_exe().unwrap();
    let mut child = Command::new(child_exe)
        .arg("--exact")
        .arg("second_spool_opener_child")
        .arg("--nocapture")
        .env("TESLATLAS_EDGE_LOCK_CHILD", "1")
        .env("TESLATLAS_EDGE_LOCK_SPOOL", &spool_config.directory)
        .env("TESLATLAS_EDGE_LOCK_READY", &ready)
        .env("TESLATLAS_EDGE_LOCK_RELEASE", &release)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut child_ready = false;
    while Instant::now() < deadline {
        if ready.exists() {
            child_ready = true;
            break;
        }
        if let Some(status) = child.try_wait().unwrap() {
            let _ = fs::write(&release, b"stop");
            let _ = child.wait();
            panic!("lock-holder child exited before readiness: {status}");
        }
        thread::sleep(Duration::from_millis(10));
    }
    if !child_ready {
        let _ = fs::write(&release, b"stop");
        let _ = child.wait();
        panic!("lock-holder child did not become ready");
    }

    assert!(matches!(
        Spool::open(spool_config.clone(), key(), T0 + 1),
        Err(SpoolError::AlreadyOpen)
    ));

    drop(owner);
    fs::write(&release, b"stop").unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "lock-holder child failed: {status}");
    assert_eq!(
        Spool::open(spool_config, key(), T0 + 2)
            .unwrap()
            .snapshot(T0 + 2)
            .pending_records,
        1
    );
}

#[test]
fn second_spool_opener_child() {
    let Some(spool_directory) = std::env::var_os("TESLATLAS_EDGE_LOCK_SPOOL") else {
        return;
    };
    let ready = PathBuf::from(std::env::var_os("TESLATLAS_EDGE_LOCK_READY").unwrap());
    let release = PathBuf::from(std::env::var_os("TESLATLAS_EDGE_LOCK_RELEASE").unwrap());
    let attempted = Spool::open(
        config_for_directory(PathBuf::from(spool_directory)),
        key(),
        T0 + 1,
    );
    assert!(matches!(attempted, Err(SpoolError::AlreadyOpen)));
    fs::write(ready, b"ready").unwrap();
    while !release.exists() {
        thread::sleep(Duration::from_millis(10));
    }
    let reopened = Spool::open(
        config_for_directory(PathBuf::from(
            std::env::var_os("TESLATLAS_EDGE_LOCK_SPOOL").unwrap(),
        )),
        key(),
        T0 + 2,
    )
    .unwrap();
    assert_eq!(reopened.snapshot(T0 + 2).pending_records, 1);
}

#[test]
fn established_spool_missing_or_corrupt_sequence_state_fails_without_recovery_mutation() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config.clone(), key(), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-sequence-state", T0), T0)
        .unwrap();
    drop(spool);
    let sequence_path = temp.path().join("spool/sequence.tlem");
    let sequence_backup = fs::read(&sequence_path).unwrap();
    let pending_before = fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_owned(),
                fs::read(path).unwrap(),
            )
        })
        .collect::<Vec<_>>();

    fs::remove_file(&sequence_path).unwrap();
    assert!(matches!(
        Spool::open(spool_config.clone(), key(), T0 + 1),
        Err(SpoolError::SequenceStateMissing)
    ));
    assert!(!sequence_path.exists());
    assert_eq!(
        fs::read_dir(temp.path().join("spool/pending"))
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.file_name().unwrap().to_owned(),
                    fs::read(path).unwrap(),
                )
            })
            .collect::<Vec<_>>(),
        pending_before
    );

    fs::write(&sequence_path, b"corrupt-sequence-state").unwrap();
    assert!(matches!(
        Spool::open(spool_config.clone(), key(), T0 + 2),
        Err(SpoolError::CorruptRecord)
    ));
    assert_eq!(fs::read(&sequence_path).unwrap(), b"corrupt-sequence-state");
    fs::write(&sequence_path, sequence_backup).unwrap();
    assert_eq!(
        Spool::open(spool_config, key(), T0 + 3)
            .unwrap()
            .snapshot(T0 + 3)
            .pending_records,
        1
    );
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
fn old_v2_ack_cannot_delete_a_reenqueued_stable_event() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config, key(), T0).unwrap();
    let first = receiver_envelope("tx-reenqueued", T0);
    spool.enqueue(first.clone(), T0).unwrap();

    let first_batch = spool.next_batch_v2(T0 + 1).unwrap();
    let old_ack = HubAckV2 {
        version: 2,
        batch_id: first_batch.batch_id.clone(),
        accepted_record_ids: vec![first_batch.records[0].record_id.clone()],
        accepted_gap_notice_ids: Vec::new(),
    };
    spool.acknowledge_v2(&old_ack).unwrap();

    let mut retry = first;
    retry.received_at_ms += 5_000;
    assert!(matches!(
        spool.enqueue(retry.clone(), T0 + 5_000).unwrap(),
        EnqueueOutcome::Stored(_)
    ));
    let requeued = spool.next_batch_v2(T0 + 5_001).unwrap();
    assert_eq!(requeued.records.len(), 1);
    assert_eq!(requeued.records[0].spool_seq, 2);
    assert_eq!(requeued.records[0].record_id, retry.stable_record_id());
    assert_ne!(
        requeued.records[0].legacy_record_id,
        old_ack.accepted_record_ids[0]
    );

    assert_eq!(
        spool
            .acknowledge_v2(&old_ack)
            .unwrap()
            .acknowledged_record_ids,
        vec![old_ack.accepted_record_ids[0].clone()]
    );
    let after_old_retry = spool.next_batch_v2(T0 + 5_002).unwrap();
    assert_eq!(after_old_retry.records.len(), 1);
    assert_eq!(after_old_retry.records[0].spool_seq, 2);
    assert_eq!(
        after_old_retry.records[0].record_id,
        retry.stable_record_id()
    );
}

#[test]
fn receipt_recovery_preserves_reenqueued_stable_event_and_sequence() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config.clone(), key(), T0).unwrap();
    let first = receiver_envelope("tx-recovered-reenqueue", T0);
    spool.enqueue(first.clone(), T0).unwrap();

    let first_batch = spool.next_batch_v2(T0 + 1).unwrap();
    let old_ack = HubAckV2 {
        version: 2,
        batch_id: first_batch.batch_id.clone(),
        accepted_record_ids: vec![first_batch.records[0].record_id.clone()],
        accepted_gap_notice_ids: Vec::new(),
    };
    spool.acknowledge_v2(&old_ack).unwrap();

    let mut retry = first;
    retry.received_at_ms += 5_000;
    spool.enqueue(retry.clone(), T0 + 5_000).unwrap();
    drop(spool);

    let restarted = Spool::open(spool_config, key(), T0 + 5_001).unwrap();
    let second = receiver_envelope("tx-distinct-after-restart", T0 + 5_001);
    restarted.enqueue(second.clone(), T0 + 5_001).unwrap();
    assert_eq!(
        restarted
            .acknowledge_v2(&old_ack)
            .unwrap()
            .acknowledged_record_ids,
        vec![old_ack.accepted_record_ids[0].clone()]
    );

    let remaining = restarted.next_batch_v2(T0 + 5_002).unwrap();
    assert_eq!(remaining.records.len(), 2);
    assert_eq!(remaining.records[0].spool_seq, 2);
    assert_eq!(remaining.records[0].legacy_record_id, retry.record_id());
    assert_eq!(remaining.records[1].spool_seq, 3);
    assert_eq!(remaining.records[1].legacy_record_id, second.record_id());
}

#[test]
fn v2_receipt_binding_handles_identical_legacy_reenqueue() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config, key(), T0).unwrap();
    let event = receiver_envelope("tx-identical-reenqueue", T0);
    spool.enqueue(event.clone(), T0).unwrap();

    let first_batch = spool.next_batch_v2(T0 + 1).unwrap();
    let old_ack = HubAckV2 {
        version: 2,
        batch_id: first_batch.batch_id.clone(),
        accepted_record_ids: vec![first_batch.records[0].record_id.clone()],
        accepted_gap_notice_ids: Vec::new(),
    };
    spool.acknowledge_v2(&old_ack).unwrap();

    spool.enqueue(event.clone(), T0 + 2).unwrap();
    let requeued = spool.next_batch_v2(T0 + 3).unwrap();
    assert_eq!(requeued.records[0].spool_seq, 2);
    assert_eq!(requeued.records[0].legacy_record_id, event.record_id());

    spool.acknowledge_v2(&old_ack).unwrap();
    let remaining = spool.next_batch_v2(T0 + 4).unwrap();
    assert_eq!(remaining.records.len(), 1);
    assert_eq!(remaining.records[0].spool_seq, 2);
    assert_eq!(remaining.records[0].legacy_record_id, event.record_id());
}

#[test]
fn v2_receipt_with_maximum_admissions_stays_within_encrypted_limit() {
    let temp = TempDir::new().unwrap();
    let mut spool_config = config(&temp);
    spool_config.max_records = 256;
    spool_config.batch_max_records = 256;
    spool_config.batch_max_bytes = 512 * 1024;
    let spool = Spool::open(spool_config, key(), T0).unwrap();
    for index in 0..256 {
        let timestamp = T0 + i64::from(index);
        spool
            .enqueue(
                receiver_envelope(&format!("tx-receipt-size-{index}"), timestamp),
                timestamp,
            )
            .unwrap();
    }
    let batch = spool.next_batch_v2(T0 + 1_000).unwrap();
    assert_eq!(batch.records.len(), 256);
    spool
        .acknowledge_v2(&HubAckV2 {
            version: 2,
            batch_id: batch.batch_id,
            accepted_record_ids: batch
                .records
                .iter()
                .map(|record| record.record_id.clone())
                .collect(),
            accepted_gap_notice_ids: Vec::new(),
        })
        .unwrap();
    let receipt = fs::read_dir(temp.path().join("spool/receipts"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(fs::metadata(receipt).unwrap().len() <= 160 * 1_024);
}

#[test]
fn v2_queue_over_ack_cap_drains_in_valid_prefixes() {
    let temp = TempDir::new().unwrap();
    let mut spool_config = config(&temp);
    spool_config.max_bytes = 8 * 1_024 * 1_024;
    spool_config.max_records = 1_024;
    spool_config.batch_max_bytes = 4 * 1_024 * 1_024;
    spool_config.batch_max_records = 257;
    let spool = Spool::open(spool_config, key(), T0).unwrap();
    for index in 0..1_024 {
        let timestamp = T0 + i64::from(index);
        spool
            .enqueue(
                receiver_envelope(&format!("tx-boundary-{index}"), timestamp),
                timestamp,
            )
            .unwrap();
    }

    let mut acknowledged = 0_usize;
    loop {
        let batch = spool.next_batch_v2(T0 + 2_000).unwrap();
        if batch.records.is_empty() && batch.gaps.is_empty() {
            break;
        }
        assert!(batch.records.len() <= 257);
        assert!(batch.gaps.is_empty());
        let accepted_count = batch.records.len().min(256);
        spool
            .acknowledge_v2(&HubAckV2 {
                version: 2,
                batch_id: batch.batch_id,
                accepted_record_ids: batch
                    .records
                    .iter()
                    .take(accepted_count)
                    .map(|record| record.record_id.clone())
                    .collect(),
                accepted_gap_notice_ids: Vec::new(),
            })
            .unwrap();
        acknowledged = acknowledged.saturating_add(accepted_count);
    }
    assert_eq!(acknowledged, 1_024);
    assert_eq!(spool.snapshot(T0 + 2_000).pending_records, 0);
}

#[test]
fn legal_receiver_input_is_rejected_before_storage_when_batch_item_cannot_fit() {
    let temp = TempDir::new().unwrap();
    let mut spool_config = config(&temp);
    spool_config.batch_max_bytes = 64;
    let spool = Spool::open(spool_config, key(), T0).unwrap();
    assert_eq!(
        spool.enqueue(receiver_envelope("tx-too-large-for-batch", T0), T0),
        Err(SpoolError::BatchItemTooLarge)
    );
    assert_eq!(spool.snapshot(T0).pending_records, 0);
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
    assert_eq!(fs::read(&marker).unwrap(), b"3\n");

    fs::write(&marker, b"2\n").unwrap();
    drop(Spool::open(spool_config.clone(), key(), T0 + 1).unwrap());
    assert_eq!(fs::read(&marker).unwrap(), b"3\n");

    fs::write(marker, b"1\n").unwrap();
    assert!(Spool::open(spool_config, key(), T0 + 1).is_err());
}

#[test]
fn format2_receipt_migration_fails_before_marker_change_or_receipt_recovery() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config.clone(), key(), T0).unwrap();
    let first = receiver_envelope("tx-format2-receipt", T0);
    spool.enqueue(first.clone(), T0).unwrap();
    let batch = spool.next_batch_v2(T0 + 1).unwrap();
    spool
        .acknowledge_v2(&HubAckV2 {
            version: 2,
            batch_id: batch.batch_id,
            accepted_record_ids: vec![batch.records[0].record_id.clone()],
            accepted_gap_notice_ids: Vec::new(),
        })
        .unwrap();
    let mut later = first;
    later.received_at_ms += 5_000;
    spool.enqueue(later, T0 + 5_000).unwrap();
    drop(spool);

    let marker = temp.path().join("spool/FORMAT");
    fs::write(&marker, b"2\n").unwrap();
    let pending_before = fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_owned(),
                fs::read(path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        Spool::open(spool_config.clone(), key(), T0 + 5_001),
        Err(SpoolError::ReceiptRecoveryRequired)
    ));
    assert_eq!(fs::read(&marker).unwrap(), b"2\n");
    assert_eq!(
        fs::read_dir(temp.path().join("spool/receipts"))
            .unwrap()
            .count(),
        1
    );
    let pending_after = fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_owned(),
                fs::read(path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(pending_after, pending_before);
}

#[test]
fn v2_matches_protocol_owned_literal_record_gap_batch_and_ack_vectors() {
    let temp = TempDir::new().unwrap();
    let mut long_lived = config(&temp);
    long_lived.retention_ms = 120_000;
    long_lived.batch_max_records = 16;
    let spool = Spool::open(long_lived.clone(), key(), T0 - 20_000).unwrap();

    // Advance the durable sequence with ordinary acknowledged records so the
    // public vector can exercise u64 big-endian sequence values 10, 11 and 12.
    for index in 1..=9 {
        let timestamp = T0 - 20_000 + i64::from(index);
        spool
            .enqueue(
                receiver_envelope(&format!("vector-seed-{index}"), timestamp),
                timestamp + 100,
            )
            .unwrap();
    }
    let seed_batch = spool.next_batch_v2(T0 - 10_000).unwrap();
    let seed_result = spool
        .acknowledge_v2(&HubAckV2 {
            version: 2,
            batch_id: seed_batch.batch_id,
            accepted_record_ids: seed_batch
                .records
                .iter()
                .map(|record| record.record_id.clone())
                .collect(),
            accepted_gap_notice_ids: Vec::new(),
        })
        .unwrap();
    assert_eq!(seed_result.acknowledged_record_ids.len(), 9);

    let projected = ReceiverEnvelope::parse(
        &serde_json::to_vec(&json!({
            "version": 1,
            "vin": VIN,
            "txid": "edge-projected-0001",
            "tx_type": "V",
            "received_at_ms": 1_800_000_000_100_i64,
            "timestamp_ms": 1_800_000_000_000_i64,
            "payload": {
                "vin": VIN,
                "createdAt": "2027-01-15T08:00:00Z",
                "data": {"Soc": {"intValue": "80"}}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    spool.enqueue(projected, 1_800_000_000_100).unwrap();
    drop(spool);

    let mut short_lived = long_lived;
    short_lived.retention_ms = 60_000;
    let spool = Spool::open(short_lived, key(), T0).unwrap();
    let expired = ReceiverEnvelope::parse(
        &serde_json::to_vec(&json!({
            "version": 1,
            "vin": VIN,
            "txid": "edge-expired-0002",
            "tx_type": "V",
            "received_at_ms": 1_800_000_000_200_i64,
            "timestamp_ms": 1_800_000_000_100_i64,
            "payload": {
                "vin": VIN,
                "createdAt": "2027-01-15T08:00:00.100Z",
                "data": {"Soc": {"intValue": "79"}}
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let alert = ReceiverEnvelope::parse(
        &serde_json::to_vec(&json!({
            "version": 1,
            "vin": VIN,
            "txid": "edge-alert-0003",
            "tx_type": "alerts",
            "received_at_ms": 1_800_000_000_300_i64,
            "timestamp_ms": 1_800_000_000_200_i64,
            "payload": {
                "name": "userPresent",
                "createdAt": "2027-01-15T08:00:00.200Z"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    spool.enqueue(expired, 1_800_000_000_200).unwrap();
    spool.enqueue(alert, 1_800_000_000_300).unwrap();

    let batch = spool.next_batch_v2(1_800_000_060_201).unwrap();
    assert_eq!(
        batch
            .records
            .iter()
            .map(|record| (
                record.spool_seq,
                record.record_id.as_str(),
                record.legacy_record_id.as_str(),
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                10,
                "8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9",
                "ac89a19968e0d88fe632e2cf59046dd333213da1e4ecd34f97430341bc70a0bd",
            ),
            (
                12,
                "42424c8baa6532299915b8026625a0dc271769fd97e19647b6366df523866d1f",
                "e9e9bee52f0bda000acdd63a1d79c245e72bdfeca9c3b9c4932c6f5ec0e8e618",
            ),
        ]
    );
    assert_eq!(batch.gaps.len(), 1);
    assert_eq!(batch.gaps[0].spool_seq, 11);
    assert_eq!(batch.gaps[0].reason, GapReasonV2::RetentionExpired);
    assert_eq!(
        batch.gaps[0].evidence_sha256,
        "c2fceaa38e73c41b78386cad195a383e5ae4505db9f71c0e43e7f669a8380e53"
    );
    assert_eq!(
        batch.gaps[0].notice_id.as_str(),
        "73002ffc20a769d62ab2800675b51e8fc3a895ff762b370e5edf11185b864ab2"
    );
    assert_eq!(
        batch.batch_id,
        "01d91f49aa9e06ae80070976797616a9ec74cf6ef22c6862f615b5a28371a0ef"
    );

    let acknowledgement = HubAckV2 {
        version: 2,
        batch_id: batch.batch_id,
        accepted_record_ids: vec![batch.records[0].record_id.clone()],
        accepted_gap_notice_ids: vec![batch.gaps[0].notice_id.clone()],
    };
    assert_eq!(
        serde_json::to_string(&acknowledgement).unwrap(),
        "{\"version\":2,\"batch_id\":\"01d91f49aa9e06ae80070976797616a9ec74cf6ef22c6862f615b5a28371a0ef\",\"accepted_record_ids\":[\"8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9\"],\"accepted_gap_notice_ids\":[\"73002ffc20a769d62ab2800675b51e8fc3a895ff762b370e5edf11185b864ab2\"]}"
    );
    let result = spool.acknowledge_v2(&acknowledgement).unwrap();
    assert_eq!(
        serde_json::to_string(&result).unwrap(),
        "{\"version\":2,\"acknowledged_record_ids\":[\"8284fe7aea66b79f09cfa5b2fe3ca99fc79fdac24631e06b8365aa8a0c64e5c9\"],\"acknowledged_gap_notice_ids\":[\"73002ffc20a769d62ab2800675b51e8fc3a895ff762b370e5edf11185b864ab2\"],\"unknown_record_ids\":[],\"unknown_gap_notice_ids\":[]}"
    );
    let remaining = spool.next_batch_v2(1_800_000_060_202).unwrap();
    assert_eq!(remaining.records.len(), 1);
    assert_eq!(remaining.records[0].spool_seq, 12);
    assert!(remaining.gaps.is_empty());
}
