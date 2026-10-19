// SPDX-License-Identifier: AGPL-3.0-only

//! C6 Part S1: the configured silence tolerance reaches the current summary on
//! the HTTP path (C6 ST-S53, R71).

use super::*;
use crate::{
    current_state::offline_drive_timeout_ms,
    db::{SourceDescriptor, VehicleDescriptor},
    lifecycle::{DEFAULT_OFFLINE_DRIVE_TIMEOUT, stream_observation_payload},
    tesla_stream::StreamUpdate,
};

/// `GET /v1/vehicles/{id}/current` through a router and its JSON body.
async fn current_through(router: Router, vehicle_id: Uuid) -> serde_json::Value {
    let response = router
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/current"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("current response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).expect("current body")
}

#[tokio::test]
async fn c6_st_s53_configured_gap() {
    let temporary = crate::private_tempdir().expect("temporary server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    let mut config = local_plain_server_config(store_path, bind);
    config.collector.offline_drive_timeout_seconds = 120;
    let local_token_path = config.data_dir.join("secrets/local-api-token");
    let store = HubStore::initialize(&config.data_dir).expect("store");

    // (a) The builder holds the configured value in milliseconds; without it the
    // state holds C1's default.
    let state = |store: &HubStore| {
        AppState::new(store.clone(), false, false, false, None, None, None, None)
    };
    let default_ms = i64::try_from(DEFAULT_OFFLINE_DRIVE_TIMEOUT.as_millis()).unwrap();
    assert_eq!(state(&store).offline_drive_timeout_ms, default_ms);
    assert_eq!(default_ms, 600_000);
    let configured = state(&store).with_offline_drive_timeout_ms(offline_drive_timeout_ms(
        config.collector.offline_drive_timeout_seconds,
    ));
    assert_eq!(configured.offline_drive_timeout_ms, 120_000);

    // (b) A stream statement 150 s older than the Hub's clock is not shown as a
    // transient field under the 120 s configuration, and is shown by default.
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api", "c6-s1-http-test"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "9").with_tesla_identity(Some(9), None),
            1_000,
        )
        .expect("vehicle");
    let now_ms = current_epoch_ms().expect("clock");
    let observed_at_ms = now_ms - 150_000;
    let frame = StreamUpdate {
        tag: "9".to_owned(),
        timestamp_ms: observed_at_ms,
        elevation: None,
        est_heading: None,
        est_lat: None,
        est_lng: None,
        est_range: None,
        heading: None,
        odometer: Some(100.0),
        power: None,
        range: None,
        shift_state: None,
        soc: None,
        speed: Some(60.0),
    };
    store
        .accept_stream_observation_and_lifecycle(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms,
                payload: stream_observation_payload(&frame),
            },
            observed_at_ms + 1,
            1,
        )
        .expect("stream observation");
    mark_vehicle_published(&store, vehicle.vehicle_id);

    let by_default = current_through(
        router_for_state(state(&store), CorsPolicy::default()),
        vehicle.vehicle_id,
    )
    .await;
    assert_eq!(by_default["speed"], 97);
    assert_eq!(by_default["observed_at_ms"], observed_at_ms);
    let by_builder = current_through(
        router_for_state(configured, CorsPolicy::default()),
        vehicle.vehicle_id,
    )
    .await;
    assert!(by_builder["speed"].is_null(), "{by_builder}");
    // A persistent value has no age rule.
    assert!(by_builder["odometer"].as_f64().is_some_and(|km| km > 160.0));

    // (c) Serve's own construction applies the configuration: the same read
    // through the real plain-loopback Serve path of this configuration.
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let serve_store = store.clone();
    let server_task = tokio::spawn(async move {
        serve_with_cursor_key(
            serve_store,
            &config,
            Sha256Digest::of_bytes(b"c6 s53 configured gap"),
            None,
            Some(admission),
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });
    wait_for_tcp_listener(bind).await;
    let token = fs::read_to_string(&local_token_path).expect("local bearer file");
    let served = reqwest::Client::new()
        .get(format!(
            "http://{bind}/v1/vehicles/{}/current",
            vehicle.vehicle_id
        ))
        .bearer_auth(token.trim_end())
        .send()
        .await
        .expect("request through Serve");
    assert_eq!(served.status(), StatusCode::OK);
    let served: serde_json::Value =
        serde_json::from_str(&served.text().await.expect("body")).expect("served body");
    assert!(served["speed"].is_null(), "{served}");
    assert_eq!(served["observed_at_ms"], observed_at_ms);
    assert!(served["odometer"].as_f64().is_some_and(|km| km > 160.0));

    shutdown_tx.send(()).expect("signal shutdown");
    server_task
        .await
        .expect("server task")
        .expect("server shuts down");
}
