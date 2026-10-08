#![forbid(unsafe_code)]

mod support;

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use axum::body::Body;
use http::{Request, StatusCode};
use tempfile::TempDir;
use teslatlas_edge::admission::{AdmissionService, Clock, ReceiverBearer};
use teslatlas_edge::credentials::CredentialStore;
use teslatlas_edge::delivery::DeliveryService;
use teslatlas_edge::spool::{Spool, SpoolConfig, SpoolError, SpoolKey};
use tower::ServiceExt;

use support::hub_double::HubDouble;
use support::{T0, receiver_envelope};

fn config(temp: &TempDir) -> SpoolConfig {
    SpoolConfig {
        directory: temp.path().join("spool"),
        max_bytes: 1_048_576,
        max_records: 8,
        retention_ms: 60_000,
        batch_max_bytes: 262_144,
        batch_max_records: 8,
    }
}

fn key() -> SpoolKey {
    SpoolKey::from_bytes([0x6a; 32])
}

#[test]
fn duplicates_and_delayed_input_apply_once_in_stable_admission_order() {
    let temp = TempDir::new().unwrap();
    let spool = Spool::open(config(&temp), key(), T0).unwrap();
    let late = receiver_envelope("tx-late", T0 + 8_000);
    let early = receiver_envelope("tx-early", T0);
    spool.enqueue(late.clone(), T0 + 2).unwrap();
    spool.enqueue(early, T0 + 1).unwrap();
    spool.enqueue(late, T0 + 3).unwrap();

    let batch = spool.next_batch(T0 + 10).unwrap();
    assert_eq!(
        batch
            .records
            .iter()
            .map(|record| record.envelope.txid.as_str())
            .collect::<Vec<_>>(),
        vec!["tx-early", "tx-late"]
    );
    let mut hub = HubDouble::open(temp.path().join("hub-state.json"));
    let acknowledgement = hub.commit_before_ack(&batch);
    spool.acknowledge(&acknowledgement).unwrap();
    assert_eq!(hub.applied_txids(), ["tx-early", "tx-late"]);
    assert!(spool.next_batch(T0 + 11).unwrap().records.is_empty());
}

#[test]
fn lost_ack_edge_restart_and_network_recovery_do_not_duplicate_hub_apply() {
    let temp = TempDir::new().unwrap();
    let spool_config = config(&temp);
    let spool = Spool::open(spool_config.clone(), key(), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-retry", T0), T0)
        .unwrap();
    let first_batch = spool.next_batch(T0 + 1).unwrap();
    let hub_path = temp.path().join("hub-state.json");
    let mut hub = HubDouble::open(&hub_path);
    let _lost_ack = hub.commit_before_ack(&first_batch);
    drop(hub);
    drop(spool);

    let restarted = Spool::open(spool_config, key(), T0 + 2).unwrap();
    let replay = restarted.next_batch(T0 + 2).unwrap();
    assert_eq!(replay, first_batch);
    let mut recovered_hub = HubDouble::open(&hub_path);
    let acknowledgement = recovered_hub.commit_before_ack(&replay);
    restarted.acknowledge(&acknowledgement).unwrap();
    assert_eq!(recovered_hub.applied_txids(), ["tx-retry"]);
    assert!(restarted.next_batch(T0 + 3).unwrap().records.is_empty());
}

#[test]
fn capacity_rejection_and_corrupt_ciphertext_are_visible_and_fail_closed() {
    let temp = TempDir::new().unwrap();
    let mut bounded = config(&temp);
    bounded.max_records = 1;
    bounded.batch_max_records = 1;
    let spool = Spool::open(bounded.clone(), key(), T0).unwrap();
    spool.enqueue(receiver_envelope("tx-kept", T0), T0).unwrap();
    assert_eq!(
        spool
            .enqueue(receiver_envelope("tx-rejected", T0 + 1), T0 + 1)
            .unwrap_err(),
        SpoolError::CapacityExceeded
    );
    drop(spool);

    let path = std::fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    let mut original = [0];
    file.read_exact(&mut original).unwrap();
    let changed = [original[0] ^ 1];
    assert_ne!(changed, original);
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&changed).unwrap();
    file.sync_all().unwrap();

    let recovered = Spool::open(bounded, key(), T0 + 2).unwrap();
    let snapshot = recovered.snapshot(T0 + 2);
    assert_eq!(snapshot.pending_records, 0);
    assert_eq!(snapshot.corrupt_records, 1);
    assert!(snapshot.degraded);
}

struct ManualClock(AtomicI64);

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn normalized_numeric_expansion_is_rejected_before_http_acceptance() {
    let temp = TempDir::new().unwrap();
    let mut limits = config(&temp);
    limits.max_bytes = 2_097_152;
    limits.batch_max_bytes = 1_048_576;
    let spool = Spool::open(limits, key(), T0).unwrap();
    let service = AdmissionService::new(
        spool.clone(),
        ReceiverBearer::new("synthetic-bearer-0123456789").unwrap(),
        Arc::new(ManualClock(AtomicI64::new(T0))),
    );
    let floats = vec!["1e9"; 50_000].join(",");
    let body = format!(
        r#"{{"version":1,"vin":"5YJ3E1EA7KF000001","txid":"float-expansion","tx_type":"V","received_at_ms":{T0},"timestamp_ms":{T0},"payload":{{"values":[{floats}]}}}}"#
    );
    assert!(body.len() < 262_144);
    let parsed = teslatlas_edge::protocol::ReceiverEnvelope::parse(body.as_bytes()).unwrap();
    assert!(serde_json::to_vec(&parsed).unwrap().len() > 384 * 1_024);
    let sequence_path = temp.path().join("spool/sequence.tlem");
    let sequence = std::fs::read(&sequence_path).unwrap();
    let response = service
        .router()
        .oneshot(
            Request::post("/v1/internal/fleet-telemetry")
                .header("authorization", "Bearer synthetic-bearer-0123456789")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(spool.snapshot(T0).pending_records, 0);
    assert_eq!(std::fs::read(sequence_path).unwrap(), sequence);
    let batch = spool.next_batch_v2(T0).unwrap();
    assert!(batch.records.is_empty() && batch.gaps.is_empty());
    assert_eq!(
        std::fs::read_dir(temp.path().join("spool/tmp"))
            .unwrap()
            .count(),
        0
    );
}

fn get(token: &str) -> Request<Body> {
    Request::get("/v1/hub/batches/next")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn credential_overlap_and_revocation_take_effect_without_spool_loss() {
    let temp = TempDir::new().unwrap();
    let spool = Spool::open(config(&temp), key(), T0).unwrap();
    spool.enqueue(receiver_envelope("tx-auth", T0), T0).unwrap();
    let credentials = CredentialStore::open(temp.path().join("credentials.json")).unwrap();
    let old = credentials.enrol("home-hub", T0, 60_000).unwrap();
    let replacement = credentials
        .rotate(old.credential_id(), T0 + 100, 5_000, 60_000)
        .unwrap();
    let clock = Arc::new(ManualClock(AtomicI64::new(T0 + 200)));
    let service = DeliveryService::new(spool, credentials, clock.clone());
    let router = service.router();
    assert_eq!(
        router
            .clone()
            .oneshot(get(old.token()))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        router
            .clone()
            .oneshot(get(replacement.token()))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    service
        .credentials()
        .revoke(replacement.credential_id(), T0 + 300)
        .unwrap();
    clock.0.store(T0 + 300, Ordering::SeqCst);
    assert_eq!(
        router
            .clone()
            .oneshot(get(replacement.token()))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    clock.0.store(T0 + 5_100, Ordering::SeqCst);
    assert_eq!(
        router.oneshot(get(old.token())).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(service.spool().snapshot(T0 + 5_100).pending_records, 1);
}
