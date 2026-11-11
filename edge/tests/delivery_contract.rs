#![forbid(unsafe_code)]

mod support;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use tempfile::TempDir;
use teslatlas_edge::admission::Clock;
use teslatlas_edge::credentials::CredentialStore;
use teslatlas_edge::delivery::DeliveryService;
use teslatlas_edge::protocol::{
    HubAckResultV1, HubAckResultV2, HubAckV1, HubAckV2, HubBatchV1, HubBatchV2,
};
use teslatlas_edge::spool::{Spool, SpoolConfig, SpoolKey};
use tower::ServiceExt;

use support::{T0, receiver_envelope};

struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        T0 + 10_000
    }
}

fn setup(temp: &TempDir) -> (DeliveryService, String) {
    let spool = Spool::open(
        SpoolConfig {
            directory: temp.path().join("spool"),
            max_bytes: 1_048_576,
            max_records: 8,
            retention_ms: 60_000,
            batch_max_bytes: 262_144,
            batch_max_records: 8,
        },
        SpoolKey::from_bytes([0x55; 32]),
        T0,
    )
    .unwrap();
    spool.enqueue(receiver_envelope("tx-one", T0), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-two", T0 + 1_000), T0 + 1)
        .unwrap();
    let credentials = CredentialStore::open(temp.path().join("credentials.json")).unwrap();
    let issued = credentials.enrol("home-hub", T0, 60_000).unwrap();
    (
        DeliveryService::new(spool, credentials, Arc::new(FixedClock)),
        issued.token().to_owned(),
    )
}

fn authorized_get(token: &str) -> Request<Body> {
    Request::get("/v1/hub/batches/next")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn authorized_get_v2(token: &str) -> Request<Body> {
    Request::get("/v2/hub/batches/next")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn disconnect_before_ack_replays_the_same_bounded_batch() {
    let temp = TempDir::new().unwrap();
    let (service, token) = setup(&temp);
    let router = service.router();

    let first = router
        .clone()
        .oneshot(authorized_get(&token))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first: HubBatchV1 =
        serde_json::from_slice(&to_bytes(first.into_body(), 1_048_576).await.unwrap()).unwrap();
    let replay = router.oneshot(authorized_get(&token)).await.unwrap();
    let replay: HubBatchV1 =
        serde_json::from_slice(&to_bytes(replay.into_body(), 1_048_576).await.unwrap()).unwrap();

    assert_eq!(first, replay);
    assert_eq!(first.records.len(), 2);
    assert_eq!(first.records[0].envelope.txid, "tx-one");
    assert_eq!(first.records[1].envelope.txid, "tx-two");
}

#[tokio::test]
async fn reordered_acknowledgements_delete_exact_records_after_hub_commit() {
    let temp = TempDir::new().unwrap();
    let (service, token) = setup(&temp);
    let router = service.router();
    let response = router
        .clone()
        .oneshot(authorized_get(&token))
        .await
        .unwrap();
    let batch: HubBatchV1 =
        serde_json::from_slice(&to_bytes(response.into_body(), 1_048_576).await.unwrap()).unwrap();
    let mut ids = batch
        .records
        .iter()
        .map(|record| record.record_id.clone())
        .collect::<Vec<_>>();
    ids.reverse();
    let acknowledgement = HubAckV1 {
        version: 1,
        batch_id: batch.batch_id,
        accepted_record_ids: ids.clone(),
    };
    let response = router
        .clone()
        .oneshot(
            Request::post("/v1/hub/acks")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&acknowledgement).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: HubAckResultV1 =
        serde_json::from_slice(&to_bytes(response.into_body(), 1_048_576).await.unwrap()).unwrap();
    assert_eq!(result.acknowledged_record_ids, ids);
    assert!(result.unknown_record_ids.is_empty());

    let empty = router.oneshot(authorized_get(&token)).await.unwrap();
    let empty: HubBatchV1 =
        serde_json::from_slice(&to_bytes(empty.into_body(), 1_048_576).await.unwrap()).unwrap();
    assert!(empty.records.is_empty());
}

#[tokio::test]
async fn v2_delivers_stable_ids_sequences_and_exact_acknowledgements() {
    let temp = TempDir::new().unwrap();
    let (service, token) = setup(&temp);
    let router = service.router();
    let response = router
        .clone()
        .oneshot(authorized_get_v2(&token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let batch: HubBatchV2 =
        serde_json::from_slice(&to_bytes(response.into_body(), 1_048_576).await.unwrap()).unwrap();
    assert_eq!(
        batch
            .records
            .iter()
            .map(|record| record.spool_seq)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(
        batch
            .records
            .iter()
            .all(|record| record.record_id != record.legacy_record_id)
    );
    let accepted = batch.records[0].record_id.clone();
    let acknowledgement = HubAckV2 {
        version: 2,
        batch_id: batch.batch_id,
        accepted_record_ids: vec![accepted.clone()],
        accepted_gap_notice_ids: Vec::new(),
    };
    let response = router
        .clone()
        .oneshot(
            Request::post("/v2/hub/acks")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&acknowledgement).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let result: HubAckResultV2 =
        serde_json::from_slice(&to_bytes(response.into_body(), 1_048_576).await.unwrap()).unwrap();
    assert_eq!(result.acknowledged_record_ids, vec![accepted]);

    let remaining = router.oneshot(authorized_get_v2(&token)).await.unwrap();
    let remaining: HubBatchV2 =
        serde_json::from_slice(&to_bytes(remaining.into_body(), 1_048_576).await.unwrap()).unwrap();
    assert_eq!(remaining.records.len(), 1);
    assert_eq!(remaining.records[0].spool_seq, 2);
}

#[tokio::test]
async fn v1_refuses_silent_delivery_when_v2_gap_is_pending() {
    let temp = TempDir::new().unwrap();
    let (service, token) = setup(&temp);
    service.spool().expire_due(T0 + 61_001).unwrap();
    let router = service.router();

    assert_eq!(
        router
            .clone()
            .oneshot(authorized_get(&token))
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let response = router.oneshot(authorized_get_v2(&token)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let batch: HubBatchV2 =
        serde_json::from_slice(&to_bytes(response.into_body(), 1_048_576).await.unwrap()).unwrap();
    assert_eq!(batch.gaps.len(), 2);
}

#[tokio::test]
async fn bad_or_revoked_hub_credentials_are_rejected_without_deletion() {
    let temp = TempDir::new().unwrap();
    let (service, token) = setup(&temp);
    let router = service.router();
    assert_eq!(
        router
            .clone()
            .oneshot(authorized_get(
                "tte1.00000000-0000-0000-0000-000000000000.invalid"
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let credential_id = token.split('.').nth(1).unwrap();
    service
        .credentials()
        .revoke(credential_id, T0 + 1_000)
        .unwrap();
    assert_eq!(
        router
            .oneshot(authorized_get(&token))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(service.spool().snapshot(T0 + 10_000).pending_records, 2);
}

#[tokio::test]
async fn wrong_batch_id_or_duplicate_json_keys_never_delete_records() {
    let temp = TempDir::new().unwrap();
    let (service, token) = setup(&temp);
    let router = service.router();
    let response = router
        .clone()
        .oneshot(authorized_get(&token))
        .await
        .unwrap();
    let batch: HubBatchV1 =
        serde_json::from_slice(&to_bytes(response.into_body(), 1_048_576).await.unwrap()).unwrap();
    let wrong_batch = HubAckV1 {
        version: 1,
        batch_id: "0".repeat(64),
        accepted_record_ids: vec![batch.records[0].record_id.clone()],
    };
    let response = router
        .clone()
        .oneshot(
            Request::post("/v1/hub/acks")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&wrong_batch).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let duplicate_key = format!(
        "{{\"version\":1,\"batch_id\":\"{}\",\"batch_id\":\"{}\",\"accepted_record_ids\":[]}}",
        batch.batch_id, batch.batch_id
    );
    let response = router
        .oneshot(
            Request::post("/v1/hub/acks")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(duplicate_key))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(service.spool().snapshot(T0 + 10_000).pending_records, 2);
}

#[tokio::test]
async fn unreadable_credential_state_is_unavailable_not_bad_auth() {
    let temp = TempDir::new().unwrap();
    let (service, token) = setup(&temp);
    std::fs::write(temp.path().join("credentials.json"), b"invalid").unwrap();

    let response = service
        .router()
        .oneshot(authorized_get(&token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(service.spool().snapshot(T0 + 10_000).pending_records, 2);
}
