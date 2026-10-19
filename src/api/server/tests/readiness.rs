// SPDX-License-Identifier: AGPL-3.0-only

// `/readyz` under B2 clarification 3: the body is exactly the supported 1.2.0
// shape whatever the collection attention, cadence or healthy detail say. Only
// the six service reasons ever reach HTTP. Synthetic data in temporary stores.

use super::*;
use crate::config::{CadenceRule, CadenceViolation, CollectorMode};
use crate::db::{
    CollectorAccountRow, CollectorAuthRow, CollectorGapInsert, CollectorRowsWrite,
    CollectorVehicleRow, ReadinessContext, TeslaMateLegacyTokenStore,
};

const NOW_MS: i64 = 1_800_000_000_000;

/// A router over one state, and the state itself so a test can step the
/// readiness clock and drop the cache.
fn readiness_app(
    store: &HubStore,
    required: bool,
    mode: Option<CollectorMode>,
) -> (Router, AppState) {
    let mut state = AppState::new(
        store.clone(),
        required,
        false,
        false,
        None,
        None,
        Some(Sha256Digest::of_bytes(b"configuration digest")),
        None,
    );
    if let Some(mode) = mode {
        state = state.with_readiness_mode(mode);
    }
    (
        router_for_state(state.clone(), CorsPolicy::default()),
        state,
    )
}

fn drop_cache(state: &AppState) {
    *state.readiness_cache.lock().expect("cache") = None;
}

fn step_clock(state: &AppState, now_ms: i64) {
    state
        .readiness_test_now_ms
        .store(now_ms, std::sync::atomic::Ordering::SeqCst);
}

/// One `/readyz` request: status, headers and JSON body.
async fn ready_response(app: &Router) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("readyz response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
        .expect("readyz JSON");
    (status, headers, body)
}

/// The supported 1.2.0 bodies and the headers every answer carries.
fn assert_strict_12(
    status: StatusCode,
    headers: &axum::http::HeaderMap,
    body: &serde_json::Value,
    expected_reason: Option<&str>,
) {
    let object = body.as_object().expect("object body");
    match expected_reason {
        None => {
            assert_eq!(status, StatusCode::OK);
            assert_eq!(*body, serde_json::json!({"status": "ready"}));
            assert_eq!(object.len(), 1, "no member beyond status: {body}");
        }
        Some(reason) => {
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                *body,
                serde_json::json!({"status": "not_ready", "reason": reason})
            );
            assert_eq!(
                object.len(),
                2,
                "no member beyond status and reason: {body}"
            );
        }
    }
    assert!(!object.contains_key("detail"));
    assert!(!object.contains_key("collectorCadenceError"));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(headers.contains_key("x-teslatlas-native-process-id"));
    assert_eq!(
        headers[NATIVE_CONFIG_DIGEST_HEADER],
        Sha256Digest::of_bytes(b"configuration digest")
            .to_string()
            .as_str()
    );
}

fn account(mode: &str, state: &str, reason: &str) -> CollectorAccountRow {
    CollectorAccountRow {
        mode: mode.to_owned(),
        config_fingerprint: "a".repeat(64),
        state: state.to_owned(),
        reason_code: reason.to_owned(),
        since_ms: NOW_MS,
        rate_limited_until_ms: None,
        next_probe_ms: None,
    }
}

fn write_rows(store: &HubStore, write: &CollectorRowsWrite) {
    store
        .collector_write_transaction(
            |transaction| {
                let wrote = HubStore::write_collector_rows_in_transaction(transaction, write)?;
                Ok::<_, crate::db::StoreError>(((), wrote))
            },
            || Ok(()),
        )
        .expect("collector rows");
}

fn auth_row(state: &str, reason: Option<&str>, collecting: bool) -> CollectorAuthRow {
    CollectorAuthRow {
        state: state.to_owned(),
        reason: reason.map(str::to_owned),
        collecting,
        retry_at: None,
        stopped_generation: None,
        stopped_mode: None,
        fuse_consecutive: 0,
        fuse_generation: None,
        fuse_last_rejection_at: None,
        fuse_pause_until: None,
        fuse_rejected_access_digest: None,
        clock_skew: false,
    }
}

fn jwt(exp: i64) -> String {
    format!(
        "{}.{}.signature",
        STANDARD_NO_PAD_URL.encode(br#"{"alg":"none"}"#),
        STANDARD_NO_PAD_URL.encode(format!(r#"{{"exp":{exp}}}"#))
    )
}

use base64::engine::general_purpose::URL_SAFE_NO_PAD as STANDARD_NO_PAD_URL;

/// A real pair with the given access token and schedule; returns its generation.
fn real_pair(
    directory: &std::path::Path,
    store: &HubStore,
    access: &str,
    schedule: Option<(i64, i64)>,
) -> Uuid {
    let key = b"http readiness key";
    let tokens =
        crate::credentials::OwnerTokens::from_secret_parts(access.to_owned(), "refresh".to_owned())
            .expect("tokens");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key, &tokens).expect("encrypt");
    let stored = match schedule {
        Some((expires_at, next_refresh_at)) => {
            TeslaMateLegacyTokenStore::refreshed(access, refresh, expires_at, next_refresh_at)
        }
        None => TeslaMateLegacyTokenStore::imported(access, refresh),
    }
    .expect("stored pair");
    crate::teslamate_credentials::replace_key_and_tokens(directory, store, key, &stored)
        .expect("persist");
    store
        .load_teslamate_legacy_tokens()
        .expect("load")
        .expect("row")
        .credential_generation()
        .expect("generation")
}

fn fault() -> CadenceViolation {
    CadenceViolation {
        key: "collector.request_timeout_seconds",
        value: 121,
        rule: CadenceRule::SecondsRange {
            minimum: 1,
            maximum: 120,
        },
    }
}

#[tokio::test]
async fn readyz_keeps_12_bodies_for_attention_and_service_failures() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let generation = real_pair(temporary.path(), &store, "access", None);
    let lease = store
        .acquire_supervised_collector_lease(NOW_MS)
        .expect("lease");
    let (app, state) = readiness_app(&store, true, Some(CollectorMode::Legacy));
    step_clock(&state, NOW_MS + 1_000);
    let check = |expected: Option<&'static str>| {
        let app = app.clone();
        let state = state.clone();
        async move {
            drop_cache(&state);
            let (status, headers, body) = ready_response(&app).await;
            assert_strict_12(status, &headers, &body, expected);
        }
    };

    // Healthy, then every C3 account stop (a zero cap is the same `cap_reached`
    // row), each an attention that never reaches the body.
    check(None).await;
    for (account_state, reason) in [
        ("owner_api_refused", "owner_api_refused"),
        ("provider_refused", "provider_forbidden"),
        ("fleet_access_refused", "fleet_access_refused"),
        ("billing_suspended", "billing_suspended"),
        ("cap_reached", "cap_zero"),
        ("config_error", "fleet_config"),
        ("account_locked", "account_locked"),
    ] {
        write_rows(
            &store,
            &CollectorRowsWrite {
                now_ms: NOW_MS,
                account: Some(account("legacy", account_state, reason)),
                ..CollectorRowsWrite::default()
            },
        );
        check(None).await;
    }
    // A stream audit failure, a paused car, an open outage and an unnotified
    // alert gap: healthy detail and attention, never body members.
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(
                store
                    .register_source(
                        &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
                        NOW_MS,
                    )
                    .expect("source")
                    .source_id,
                "readiness-car",
            ),
            NOW_MS,
        )
        .expect("vehicle")
        .vehicle_id;
    write_rows(
        &store,
        &CollectorRowsWrite {
            now_ms: NOW_MS,
            account: Some(account("legacy", "collecting", "cap_warning")),
            vehicles: vec![CollectorVehicleRow {
                vehicle_id: vehicle,
                phase: "online".to_owned(),
                hold: "paused".to_owned(),
                reason_code: "stream_audit_failed".to_owned(),
                phase_since_ms: NOW_MS,
                suspended_until_ms: None,
                next_probe_ms: None,
                last_wake_read_at_ms: None,
            }],
            gap_inserts: vec![CollectorGapInsert {
                vehicle_id: None,
                kind: "tesla_unreachable".to_owned(),
                started_at_ms: NOW_MS,
                ended_at_ms: None,
                alert: true,
            }],
            ..CollectorRowsWrite::default()
        },
    );
    store.set_stream_audit_failed_cars(1);
    check(None).await;
    // The credential unit's attention states, every one of them.
    for row in [
        auth_row("observer_waiting", Some("token_expired"), false),
        auth_row("clock_invalid", None, false),
        auth_row("retrying", Some("network"), false),
        auth_row("configuration_error", Some("region_conflict"), false),
        CollectorAuthRow {
            stopped_generation: Some(generation),
            ..auth_row("sign_in_required", Some("refresh_token_rejected"), true)
        },
        CollectorAuthRow {
            stopped_mode: Some("legacy".to_owned()),
            ..auth_row("api_refused", Some("owner_api"), false)
        },
    ] {
        store.write_collector_auth_row(&row, NOW_MS).expect("row");
        check(None).await;
    }
    // Signed out is a service failure with its own reason.
    store
        .write_collector_auth_row(
            &CollectorAuthRow {
                stopped_generation: Some(generation),
                ..auth_row("signed_out", Some("access_token_rejected"), false)
            },
            NOW_MS,
        )
        .expect("row");
    check(Some("collector_auth_terminal")).await;
    // A stale lease beside every attention still reads stale.
    step_clock(&state, NOW_MS + SUPERVISED_COLLECTOR_LEASE_MS);
    check(Some("collector_stale")).await;
    step_clock(&state, NOW_MS + 1_000);
    // A terminal lease.
    store
        .heartbeat_supervised_collector_lease(
            lease,
            SupervisedCollectorState::AuthenticationTerminal,
            NOW_MS + 500,
        )
        .expect("terminal lease");
    check(Some("collector_auth_terminal")).await;
    store
        .write_collector_auth_row(&auth_row("ready", None, true), NOW_MS)
        .expect("row");
    store
        .heartbeat_supervised_collector_lease(lease, SupervisedCollectorState::Active, NOW_MS + 600)
        .expect("active lease");
    check(None).await;
    // No lease at all: absent, whatever the attention.
    store
        .release_supervised_collector_lease(lease)
        .expect("release");
    check(Some("collector_absent")).await;
    // A disabled collector demands no lease and reports no attention.
    let (disabled, disabled_state) = readiness_app(&store, false, Some(CollectorMode::Legacy));
    step_clock(&disabled_state, NOW_MS + 1_000);
    let (status, headers, body) = ready_response(&disabled).await;
    assert_strict_12(status, &headers, &body, None);
}

#[tokio::test]
async fn cadence_readyz_preserves_attention_details_and_service_reasons() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let generation = real_pair(temporary.path(), &store, "access", None);
    let (app, state) = readiness_app(&store, true, Some(CollectorMode::Legacy));
    step_clock(&state, NOW_MS + 1_000);
    // An Owner refusal and an unnotified alert gap beside the cadence fault.
    write_rows(
        &store,
        &CollectorRowsWrite {
            now_ms: NOW_MS,
            account: Some(account("legacy", "owner_api_refused", "owner_api_refused")),
            gap_inserts: vec![CollectorGapInsert {
                vehicle_id: None,
                kind: "hub_down".to_owned(),
                started_at_ms: NOW_MS - 100_000,
                ended_at_ms: Some(NOW_MS - 10_000),
                alert: true,
            }],
            ..CollectorRowsWrite::default()
        },
    );
    let view = |now_ms: i64| {
        store.collector_readiness_view(&ReadinessContext::configured(
            true,
            now_ms,
            CollectorMode::Legacy,
            Some(fault()),
        ))
    };
    let check = |expected: Option<&'static str>| {
        let app = app.clone();
        let state = state.clone();
        async move {
            drop_cache(&state);
            let (status, headers, body) = ready_response(&app).await;
            assert_strict_12(status, &headers, &body, expected);
        }
    };
    // Absent lease: the HTTP body says absent; the shared view has the cadence
    // first, the refusal behind it ignored, and the detail beside it.
    check(Some("collector_absent")).await;
    let absent = view(NOW_MS + 1_000);
    assert_eq!(
        absent.attention,
        Some(crate::db::CollectionAttentionCode::CollectorCadenceInvalid)
    );
    assert_eq!(absent.detail, ["collector_gap_unnotified"]);
    // A fresh wait lease is ready on the wire, the cadence is attention only.
    let lease = store
        .acquire_supervised_collector_lease(NOW_MS)
        .expect("lease");
    check(None).await;
    let fresh = view(NOW_MS + 1_000);
    assert_eq!(fresh.service, Ok(()));
    assert!(fresh.running);
    assert_eq!(
        fresh.attention,
        Some(crate::db::CollectionAttentionCode::CollectorCadenceInvalid)
    );
    // Stale, terminal and signed out keep their service reasons.
    step_clock(&state, NOW_MS + SUPERVISED_COLLECTOR_LEASE_MS);
    check(Some("collector_stale")).await;
    step_clock(&state, NOW_MS + 1_000);
    store
        .heartbeat_supervised_collector_lease(
            lease,
            SupervisedCollectorState::AuthenticationTerminal,
            NOW_MS + 500,
        )
        .expect("terminal lease");
    check(Some("collector_auth_terminal")).await;
    store
        .heartbeat_supervised_collector_lease(lease, SupervisedCollectorState::Active, NOW_MS + 600)
        .expect("active lease");
    store
        .write_collector_auth_row(
            &CollectorAuthRow {
                stopped_generation: Some(generation),
                ..auth_row("signed_out", Some("access_token_rejected"), false)
            },
            NOW_MS,
        )
        .expect("signed out");
    check(Some("collector_auth_terminal")).await;
    let signed_out = view(NOW_MS + 1_000);
    assert_eq!(
        signed_out.service.err().map(|failure| failure.code),
        Some(crate::db::ReadinessReasonCode::CollectorAuthTerminal)
    );
    assert_eq!(
        signed_out.attention,
        Some(crate::db::CollectionAttentionCode::CollectorCadenceInvalid),
        "the cadence is attention beside the signed-out service failure"
    );
    // The fault's own canonical text.
    assert_eq!(
        fault().to_string(),
        "collector setting collector.request_timeout_seconds = 121 must be between 1 and \
         120 seconds inclusive"
    );
}

#[tokio::test]
async fn cadence_readyz_disabled_and_edge_have_no_operational_cadence_error() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    write_rows(
        &store,
        &CollectorRowsWrite {
            now_ms: NOW_MS,
            account: Some(account("legacy", "cap_reached", "cap_reached")),
            ..CollectorRowsWrite::default()
        },
    );
    // Disabled collection: no lease is demanded, the body is the healthy one,
    // and no attention or detail exists, even when a fault is supplied.
    let (disabled, state) = readiness_app(&store, false, Some(CollectorMode::Legacy));
    step_clock(&state, NOW_MS + 1_000);
    let (status, headers, body) = ready_response(&disabled).await;
    assert_strict_12(status, &headers, &body, None);
    let disabled_view = store.collector_readiness_view(&ReadinessContext::configured(
        false,
        NOW_MS + 1_000,
        CollectorMode::Legacy,
        Some(fault()),
    ));
    assert_eq!(disabled_view.attention, None);
    assert!(disabled_view.detail.is_empty());
    assert!(!disabled_view.running);
    // `/readyz` is never given a cadence (the production context sets none), so
    // a Fleet-mode context and a corrected Legacy context alike carry no cadence
    // fault: the attention is the C3 stop, never the cadence. The Edge exclusion
    // itself is proven where the cadence is derived, in the doctor test
    // `cadence_doctor_describes_inactive_settings_without_attention`.
    for context in [
        ReadinessContext::configured(true, NOW_MS + 1_000, CollectorMode::FleetPolling, None),
        ReadinessContext::configured(true, NOW_MS + 1_000, CollectorMode::Legacy, None),
    ] {
        let view = store.collector_readiness_view(&context);
        assert_eq!(
            view.attention,
            Some(crate::db::CollectionAttentionCode::CollectorCapReached)
        );
    }
    // The HTTP service checks of a required collector are unchanged by it.
    let (required, required_state) = readiness_app(&store, true, Some(CollectorMode::FleetPolling));
    step_clock(&required_state, NOW_MS + 1_000);
    let (status, headers, body) = ready_response(&required).await;
    assert_strict_12(status, &headers, &body, Some("collector_absent"));
}

#[tokio::test]
async fn cadence_readyz_does_not_cache_readiness_past_token_usability() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let usable_until: i64 = 1_800_000_100;
    let generation = real_pair(
        temporary.path(),
        &store,
        &jwt(usable_until + 300),
        Some((usable_until + 1_000, usable_until - 500)),
    );
    // A lease that is fresh across the whole probe, so only the token can change.
    store
        .acquire_supervised_collector_lease(usable_until * 1_000 - 10_000)
        .expect("lease");
    store
        .write_collector_auth_row(
            &CollectorAuthRow {
                stopped_generation: Some(generation),
                ..auth_row("sign_in_required", Some("refresh_token_rejected"), true)
            },
            NOW_MS,
        )
        .expect("row");
    let before = {
        let connection = store.open().expect("catalogue");
        let count = |sql: &str| -> i64 { connection.query_row(sql, [], |row| row.get(0)).unwrap() };
        (
            count("SELECT COUNT(*) FROM outbound_request_receipts"),
            count("SELECT updated_at_ms FROM collector_auth_state"),
        )
    };
    let (app, state) = readiness_app(&store, true, Some(CollectorMode::Legacy));
    // Warm the cache with a 200 one millisecond before the bound.
    step_clock(&state, usable_until * 1_000 - 1);
    let (status, headers, body) = ready_response(&app).await;
    assert_strict_12(status, &headers, &body, None);
    // Still inside the ordinary one-second TTL, the clock reaches the bound: the
    // cache is not served, the answer is terminal, and later is too.
    step_clock(&state, usable_until * 1_000);
    let (status, headers, body) = ready_response(&app).await;
    assert_strict_12(status, &headers, &body, Some("collector_auth_terminal"));
    step_clock(&state, usable_until * 1_000 + 250);
    let (status, headers, body) = ready_response(&app).await;
    assert_strict_12(status, &headers, &body, Some("collector_auth_terminal"));
    // Nothing was exchanged or written by the answers.
    let connection = store.open().expect("catalogue");
    let count = |sql: &str| -> i64 { connection.query_row(sql, [], |row| row.get(0)).unwrap() };
    assert_eq!(
        (
            count("SELECT COUNT(*) FROM outbound_request_receipts"),
            count("SELECT updated_at_ms FROM collector_auth_state"),
        ),
        before
    );
    assert_eq!(
        store.load_collector_auth_row().unwrap().unwrap().state,
        "sign_in_required"
    );
}

#[tokio::test]
async fn fleet_unresolved_refresh_is_attention_not_service_failure() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    // A real encrypted Fleet pair with the collection scopes, under a real key:
    // its pair, key, generation, claims and scopes are independently valid.
    const SCOPED_ACCESS: &str = "e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iLCJ2ZWhpY2xlX2NtZHMiLCJ2ZWhpY2xlX2NoYXJnaW5nX2NtZHMiXX0.sig";
    crate::teslamate_credentials::load_or_create_cursor_key(temporary.path()).expect("cursor key");
    crate::fleet_credentials::persist_fleet_setup_credentials(
        &store,
        temporary.path(),
        &crate::fleet_credentials::FleetSetupCredentials::new(
            SCOPED_ACCESS.to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("credentials"),
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist pair");
    let generation = store
        .load_fleet_tokens()
        .expect("store")
        .expect("pair")
        .credential_generation()
        .expect("generation");
    store
        .begin_fleet_refresh(generation)
        .expect("unresolved refresh");
    // Independently valid, and the unresolved refresh is the identified one.
    let inspection =
        crate::fleet_credentials::inspect_stored_fleet_credentials(&store, temporary.path())
            .expect("the pair, key, claims and scopes are independently valid");
    assert!(inspection.unresolved_refresh);
    write_rows(
        &store,
        &CollectorRowsWrite {
            now_ms: NOW_MS,
            account: Some(account("fleet_polling", "collecting", "collecting")),
            ..CollectorRowsWrite::default()
        },
    );
    store
        .acquire_supervised_collector_lease(NOW_MS)
        .expect("lease");
    let (app, state) = readiness_app(&store, true, Some(CollectorMode::FleetPolling));
    step_clock(&state, NOW_MS + 1_000);
    let (status, headers, body) = ready_response(&app).await;
    assert_strict_12(status, &headers, &body, None);
    let view = store.collector_readiness_view(&ReadinessContext::configured(
        true,
        NOW_MS + 1_000,
        CollectorMode::FleetPolling,
        None,
    ));
    assert_eq!(
        view.attention,
        Some(crate::db::CollectionAttentionCode::CollectorFleetRefreshOutcomeUnknown)
    );
    assert!(view.detail.is_empty(), "no duplicate healthy detail");
}
