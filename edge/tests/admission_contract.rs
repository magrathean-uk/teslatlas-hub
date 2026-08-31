#![forbid(unsafe_code)]

mod support;

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use tempfile::TempDir;
use teslatlas_edge::admission::{AdmissionService, Clock, ReceiverBearer};
use teslatlas_edge::spool::{Spool, SpoolConfig, SpoolKey};
use tower::ServiceExt;

use support::{T0, VIN, receiver_envelope};

struct FixedClock(i64);

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0
    }
}

fn spool_config(temp: &TempDir) -> SpoolConfig {
    SpoolConfig {
        directory: temp.path().join("spool"),
        max_bytes: 1_048_576,
        max_records: 8,
        retention_ms: 60_000,
        batch_max_bytes: 262_144,
        batch_max_records: 8,
    }
}

fn service(temp: &TempDir, max_records: usize) -> AdmissionService {
    let mut config = spool_config(temp);
    config.max_records = max_records;
    config.batch_max_records = max_records;
    let spool = Spool::open(config, SpoolKey::from_bytes([7; 32]), T0).unwrap();
    AdmissionService::new(
        spool,
        ReceiverBearer::new("receiver-secret-123").unwrap(),
        Arc::new(FixedClock(T0)),
    )
}

fn post(body: Vec<u8>, bearer: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/internal/fleet-telemetry")
        .header("content-type", "application/json");
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", format!("Bearer {bearer}"));
    }
    builder.body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn receiver_requires_bearer_and_rejects_malformed_or_oversized_input() {
    let temp = TempDir::new().unwrap();
    let router = service(&temp, 8).router();
    let body = serde_json::to_vec(&receiver_envelope("tx-auth", T0)).unwrap();

    assert_eq!(
        router
            .clone()
            .oneshot(post(body.clone(), None))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        router
            .clone()
            .oneshot(post(body, Some("wrong-secret")))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        router
            .clone()
            .oneshot(post(b"not-json".to_vec(), Some("receiver-secret-123")))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        router
            .oneshot(post(
                vec![b' '; teslatlas_edge::protocol::MAX_RECEIVER_BODY_BYTES + 1],
                Some("receiver-secret-123")
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn durable_and_duplicate_admission_return_204_but_store_once() {
    let temp = TempDir::new().unwrap();
    let service = service(&temp, 8);
    let router = service.router();
    let body = serde_json::to_vec(&receiver_envelope("tx-once", T0)).unwrap();

    for _ in 0..2 {
        assert_eq!(
            router
                .clone()
                .oneshot(post(body.clone(), Some("receiver-secret-123")))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
    }
    assert_eq!(service.spool().snapshot(T0).pending_records, 1);
}

#[tokio::test]
async fn receiver_rejects_arrival_time_beyond_clock_skew() {
    let temp = TempDir::new().unwrap();
    let service = service(&temp, 8);
    let router = service.router();
    let body = serde_json::to_vec(&receiver_envelope("tx-future", T0 + 300_001)).unwrap();

    assert_eq!(
        router
            .oneshot(post(body, Some("receiver-secret-123")))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(service.spool().snapshot(T0).pending_records, 0);
}

#[tokio::test]
async fn full_spool_returns_507_without_acknowledging_second_record() {
    let temp = TempDir::new().unwrap();
    let service = service(&temp, 1);
    let router = service.router();
    let first = serde_json::to_vec(&receiver_envelope("tx-first", T0)).unwrap();
    let second = serde_json::to_vec(&receiver_envelope("tx-second", T0 + 1_000)).unwrap();

    assert_eq!(
        router
            .clone()
            .oneshot(post(first, Some("receiver-secret-123")))
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        router
            .oneshot(post(second, Some("receiver-secret-123")))
            .await
            .unwrap()
            .status(),
        StatusCode::INSUFFICIENT_STORAGE
    );
    assert_eq!(service.spool().snapshot(T0).pending_records, 1);
}

#[tokio::test]
async fn corruption_degrades_readiness_without_blocking_liveness() {
    let temp = TempDir::new().unwrap();
    let config = spool_config(&temp);
    let spool = Spool::open(config.clone(), SpoolKey::from_bytes([7; 32]), T0).unwrap();
    spool
        .enqueue(receiver_envelope("tx-corrupt", T0), T0)
        .unwrap();
    drop(spool);
    let path = fs::read_dir(temp.path().join("spool/pending"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let length = fs::metadata(&path).unwrap().len();
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(length - 1)).unwrap();
    file.write_all(&[0xff]).unwrap();
    file.sync_all().unwrap();
    let recovered = Spool::open(config, SpoolKey::from_bytes([7; 32]), T0 + 1).unwrap();
    let service = AdmissionService::new(
        recovered,
        ReceiverBearer::new("receiver-secret-123").unwrap(),
        Arc::new(FixedClock(T0 + 1)),
    );
    let router = service.router();

    assert_eq!(
        router
            .clone()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        router
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn metrics_expose_only_bounded_aggregate_values() {
    let temp = TempDir::new().unwrap();
    let service = service(&temp, 8);
    let router = service.router();
    let secret = "receiver-secret-123";
    let body = serde_json::to_vec(&receiver_envelope("tx-private-id", T0)).unwrap();
    router
        .clone()
        .oneshot(post(body, Some(secret)))
        .await
        .unwrap();
    let response = router
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let text = std::str::from_utf8(&body).unwrap();

    assert!(text.contains("teslatlas_edge_spool_records 1"));
    assert!(text.contains("teslatlas_edge_receiver_admitted_total 1"));
    assert!(text.contains("# TYPE teslatlas_edge_spool_records gauge"));
    assert!(text.contains("# TYPE teslatlas_edge_receiver_admitted_total counter"));
    assert!(text.contains("# TYPE teslatlas_edge_spool_expired_records_total counter"));
    assert!(text.contains("teslatlas_edge_spool_gap_notices 0"));
    for private in [VIN, "tx-private-id", secret, "Soc", "createdAt"] {
        assert!(!text.contains(private), "metrics leaked {private}");
    }
    assert!(!text.contains('{'), "metrics must not use dynamic labels");
}
