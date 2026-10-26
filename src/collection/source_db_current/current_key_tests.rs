// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use std::io::Write;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use tracing::instrument::WithSubscriber;

#[derive(Clone)]
struct CapturedCurrentLog(Arc<Mutex<Vec<u8>>>);
struct CurrentLogWriter(CapturedCurrentLog);
impl Write for CurrentLogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedCurrentLog {
    type Writer = CurrentLogWriter;
    fn make_writer(&'a self) -> Self::Writer {
        CurrentLogWriter(self.clone())
    }
}

#[tokio::test]
async fn mt4_5_unknown_current_key_stops_the_worker_and_keeps_serving() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path()).unwrap();
    store
        .register_import_source("home-source", 1000, false)
        .unwrap();
    let config = ImportSourceCurrentConfig {
        source_url: "postgresql://reader@source.example/sourcedb".into(),
        source_key: "unmatched-private-key".into(),
        vehicle_id: Uuid::new_v4(),
        car_id: 42,
        password_file: temporary.path().join("intentionally-absent-password"),
    };
    let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path()).unwrap();
    let captured = CapturedCurrentLog(Arc::new(Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_writer(captured.clone())
        .finish();
    tokio::time::timeout(
        Duration::from_millis(300),
        run(
            store.clone(),
            config,
            admission,
            std::future::pending::<()>(),
        )
        .with_subscriber(subscriber),
    )
    .await
    .expect("unmatched worker must stop before remote polling");
    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    let required = "The configured import source key for current positions matches no stored import source; stored keys: home-source";
    assert_eq!(log.matches(required).count(), 1, "one diagnostic: {log}");
    assert!(!log.contains("unmatched-private-key"));
    let response = crate::api::server::router(store)
        .oneshot(
            axum::http::Request::builder()
                .uri("/healthz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "the actual health router still serves after worker refusal"
    );
}
