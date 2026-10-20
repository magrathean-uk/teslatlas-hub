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

// FX-1: the clock the current summary reads is a seam. The default is the
// system clock; a pin makes C6 R28 (c) measure from a fixed time.

/// A fixed past observation time, 2026-09-04: older than the gap at any real
/// time the tests can run.
const FX1_OBSERVED_AT_MS: i64 = 1_788_566_400_000;
const FX1_GAP_MS: i64 = 600_000;

/// A vehicle whose one stream frame, with a speed, was observed at
/// `FX1_OBSERVED_AT_MS`, published so the current route answers for it.
fn fx1_seed(store: &HubStore) -> Uuid {
    let source = store
        .register_source(&SourceDescriptor::new("owner_api", "fx1-test"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "9").with_tesla_identity(Some(9), None),
            1_000,
        )
        .expect("vehicle");
    let frame = StreamUpdate {
        tag: "9".to_owned(),
        timestamp_ms: FX1_OBSERVED_AT_MS,
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
                observed_at_ms: FX1_OBSERVED_AT_MS,
                payload: stream_observation_payload(&frame),
            },
            FX1_OBSERVED_AT_MS + 1,
            1,
        )
        .expect("stream observation");
    mark_vehicle_published(store, vehicle.vehicle_id);
    vehicle.vehicle_id
}

fn fx1_state(store: &HubStore, pin: Option<i64>) -> AppState {
    AppState::new(store.clone(), false, false, false, None, None, None, None)
        .with_summary_now_ms(pin)
}

/// `GET current` through a paired router: a fresh device claims an invitation
/// and reads the vehicle with its bearer.
async fn fx1_current_paired(
    router: Router,
    store: &HubStore,
    vehicle_id: Uuid,
) -> serde_json::Value {
    let invitation = store
        .create_pairing("fx1", 1_000, i64::MAX)
        .expect("pairing invitation");
    let claimed = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/pairings/{}/claim", invitation.pairing_id))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::json!({"secret": invitation.secret(), "device_name": "fx1"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("claim");
    assert_eq!(claimed.status(), StatusCode::OK);
    let bearer = response_json(claimed).await["access_token"]
        .as_str()
        .expect("token")
        .to_owned();
    let served = router
        .oneshot(
            Request::builder()
                .uri(format!("/v1/vehicles/{vehicle_id}/current"))
                .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("current");
    assert_eq!(served.status(), StatusCode::OK);
    response_json(served).await
}

#[tokio::test]
async fn fx1_summary_clock_pin() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let vehicle_id = fx1_seed(&store);
    let speed_through = |pin: Option<i64>| {
        let router = router_for_state(fx1_state(&store, pin), CorsPolicy::default());
        async move { current_through(router, vehicle_id).await }
    };

    // The pin is the evaluation time, exactly: a transient value is served up
    // to and including the gap after its observation (R28 (c)), and not after.
    for pin in [
        FX1_OBSERVED_AT_MS,
        FX1_OBSERVED_AT_MS + 1,
        FX1_OBSERVED_AT_MS + FX1_GAP_MS,
    ] {
        let served = speed_through(Some(pin)).await;
        assert_eq!(served["speed"], 97, "pin {pin}: {served}");
        assert_eq!(served["observed_at_ms"], FX1_OBSERVED_AT_MS);
    }
    let aged = speed_through(Some(FX1_OBSERVED_AT_MS + FX1_GAP_MS + 1)).await;
    assert!(aged["speed"].is_null(), "{aged}");
    // A persistent value has no age rule.
    assert!(aged["odometer"].as_f64().is_some_and(|km| km > 160.0));

    // No pin, a zero pin and a negative pin are the system clock, which is long
    // past the fixed observation: the value has aged out.
    for pin in [None, Some(0), Some(-1), Some(i64::MIN)] {
        let served = speed_through(pin).await;
        assert!(served["speed"].is_null(), "pin {pin:?}: {served}");
    }

    // The ordinary paired router keeps the system clock, and so does the one
    // that is asked for no pin.
    let key = CursorKey::from_bytes([61; 32]);
    for router in [
        paired_router(store.clone(), &key),
        paired_router_at(store.clone(), &key, None),
    ] {
        let served = fx1_current_paired(router, &store, vehicle_id).await;
        assert!(served["speed"].is_null(), "{served}");
    }
}

/// `GET current` through the real plain-loopback Serve path, pinned or not.
async fn fx1_served_through_serve(pin: Option<i64>) -> serde_json::Value {
    let temporary = crate::private_tempdir().expect("temporary server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    let config = local_plain_server_config(store_path, bind);
    let local_token_path = config.data_dir.join("secrets/local-api-token");
    let store = HubStore::initialize(&config.data_dir).expect("store");
    let vehicle_id = fx1_seed(&store);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        serve_with_cursor_key_at(
            store,
            &config,
            Sha256Digest::of_bytes(b"fx1 serve pin"),
            None,
            Some(admission),
            pin,
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });
    wait_for_tcp_listener(bind).await;
    let token = fs::read_to_string(&local_token_path).expect("local bearer file");
    let served = reqwest::Client::new()
        .get(format!("http://{bind}/v1/vehicles/{vehicle_id}/current"))
        .bearer_auth(token.trim_end())
        .send()
        .await
        .expect("request through Serve");
    assert_eq!(served.status(), StatusCode::OK);
    let served = serde_json::from_str(&served.text().await.expect("body")).expect("served body");
    shutdown_tx.send(()).expect("signal shutdown");
    server_task
        .await
        .expect("server task")
        .expect("server shuts down");
    served
}

#[tokio::test]
async fn fx1_serve_applies_the_pinned_summary_clock() {
    let pinned = fx1_served_through_serve(Some(FX1_OBSERVED_AT_MS + FX1_GAP_MS)).await;
    assert_eq!(pinned["speed"], 97, "{pinned}");
    let unpinned = fx1_served_through_serve(None).await;
    assert!(unpinned["speed"].is_null(), "{unpinned}");
}

#[tokio::test]
async fn fx1_tls_router_applies_the_pinned_summary_clock() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let vehicle_id = fx1_seed(&store);
    let source = "data_dir = '/var/lib/teslatlas-test'\nbind = '127.0.0.1:8443'\n";
    let (config, _) = HubConfig::from_exact_bytes(source.as_bytes()).expect("configuration");
    for (pin, served_speed) in [
        (Some(FX1_OBSERVED_AT_MS + FX1_GAP_MS), true),
        (Some(FX1_OBSERVED_AT_MS + FX1_GAP_MS + 1), false),
        (None, false),
    ] {
        let router = tls_router_for_config(
            store.clone(),
            false,
            &CursorKey::from_bytes([62; 32]),
            Sha256Digest::of_bytes(b"fx1 tls pin"),
            &config,
            pin,
        );
        let served = fx1_current_paired(router, &store, vehicle_id).await;
        assert_eq!(served["speed"].is_null(), !served_speed, "{pin:?} {served}");
    }
}

/// `GET current` through the admitted TLS Serve path a Fixture-mode source run
/// takes (`serve_for_admitted_user_at`, then the TLS router), pinned or not: a
/// device claims a pairing and reads the vehicle over HTTPS.
#[cfg(target_os = "macos")]
async fn fx1_served_through_admitted_tls_serve(pin: Option<i64>) -> serde_json::Value {
    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary admitted TLS server root");
    let (admission, store_path) = admitted_server_fixture(&temporary);
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    let config = local_tls_server_config(&temporary, store_path, bind);
    let certificate = fs::read(
        &config
            .tls
            .as_ref()
            .expect("TLS configuration")
            .certificate_path,
    )
    .expect("certificate");
    let store = HubStore::initialize(&config.data_dir).expect("store");
    let vehicle_id = fx1_seed(&store);
    let invitation = store
        .create_pairing("fx1", 1_000, i64::MAX)
        .expect("pairing invitation");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        serve_for_admitted_user_at(
            store,
            &config,
            Sha256Digest::of_bytes(b"fx1 admitted tls pin"),
            admission,
            None,
            pin,
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });
    wait_for_tcp_listener(bind).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .add_root_certificate(reqwest::Certificate::from_pem(&certificate).expect("certificate"))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .expect("HTTPS client");
    let claimed = client
        .post(format!(
            "https://{bind}/v1/pairings/{}/claim",
            invitation.pairing_id
        ))
        .json(&serde_json::json!({"secret": invitation.secret(), "device_name": "fx1"}))
        .send()
        .await
        .expect("claim over HTTPS");
    assert_eq!(claimed.status(), StatusCode::OK);
    let claimed: serde_json::Value =
        serde_json::from_str(&claimed.text().await.expect("claim body")).expect("claim JSON");
    let bearer = claimed["access_token"].as_str().expect("token").to_owned();
    let served = client
        .get(format!("https://{bind}/v1/vehicles/{vehicle_id}/current"))
        .bearer_auth(bearer)
        .send()
        .await
        .expect("current over HTTPS");
    assert_eq!(served.status(), StatusCode::OK);
    let served = serde_json::from_str(&served.text().await.expect("body")).expect("served body");
    shutdown_tx.send(()).expect("signal shutdown");
    server_task
        .await
        .expect("server task")
        .expect("server shuts down");
    served
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn fx1_admitted_tls_serve_applies_the_pinned_summary_clock() {
    let pinned = fx1_served_through_admitted_tls_serve(Some(FX1_OBSERVED_AT_MS + FX1_GAP_MS)).await;
    assert_eq!(pinned["speed"], 97, "{pinned}");
    let aged =
        fx1_served_through_admitted_tls_serve(Some(FX1_OBSERVED_AT_MS + FX1_GAP_MS + 1)).await;
    assert!(aged["speed"].is_null(), "{aged}");
    let unpinned = fx1_served_through_admitted_tls_serve(None).await;
    assert!(unpinned["speed"].is_null(), "{unpinned}");
}
