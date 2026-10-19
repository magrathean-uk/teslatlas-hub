// SPDX-License-Identifier: AGPL-3.0-only

//! The CLI halves of the readiness, cadence and Fleet reconnect rules (B2
//! clarification 3): the `status` document assembled by the production helper,
//! the `doctor` command, eligibility, and the managed Serve and bounded Observe
//! entries with an invalid cadence. Synthetic data in temporary Hubs; no network.
//! Fixtures use the library's public surface and plain SQL for the rows that only
//! the collector itself writes.

use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use rusqlite::{Connection, params};
use teslatlas_hub::{
    config::HubConfig,
    credentials::OwnerTokens,
    db::{HubStore, TeslaMateLegacyTokenStore},
    fleet_api::FleetRegion,
    fleet_credentials::{FleetSetupCredentials, persist_fleet_setup_credentials},
    teslamate_credentials::replace_key_and_tokens,
    teslamate_token::encrypt_legacy_owner_tokens,
};
use uuid::Uuid;

use crate::{Cli, Command, build_status_report, collector_can_start, run};

const SCOPED_ACCESS: &str = "e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iLCJ2ZWhpY2xlX2NtZHMiLCJ2ZWhpY2xlX2NoYXJnaW5nX2NtZHMiXX0.sig";
const NOW_MS: i64 = 1_800_000_000_000;
// B2 apply, R3C-04: a back-off at the silence tolerance is clamped now, so the cadence
// fault these tests use is a request timeout above its range.
const FAULT: &str = "interval_seconds = 300\nrequest_timeout_seconds = 121";
const FAULT_MESSAGE: &str = "collector setting collector.request_timeout_seconds = 121 must be \
                              between 1 and 120 seconds inclusive";

fn real_now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("clock range")
}

fn private_hub() -> (tempfile::TempDir, HubStore) {
    let temporary = tempfile::tempdir().expect("temporary Hub");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("private data directory");
    let store = HubStore::initialize(temporary.path()).expect("store");
    (temporary, store)
}

fn write_config(directory: &Path, name: &str, collector: &str) -> std::path::PathBuf {
    let path = directory.join(name);
    fs::write(
        &path,
        format!("data_dir = {directory:?}\nbind = '127.0.0.1:18443'\n\n[collector]\n{collector}\n"),
    )
    .expect("write config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private config");
    path
}

fn load(directory: &Path, name: &str, collector: &str) -> HubConfig {
    HubConfig::load(write_config(directory, name, collector)).expect("config")
}

fn sql(store: &HubStore) -> Connection {
    let connection = Connection::open(store.database_path()).expect("catalogue connection");
    connection
        .busy_timeout(Duration::from_secs(5))
        .expect("busy timeout");
    connection
}

/// A lease row with explicit timing and state.
fn lease(store: &HubStore, started: i64, heartbeat: i64, until: i64, state: &str) {
    let connection = sql(store);
    connection
        .execute("DELETE FROM supervised_collector_lease", [])
        .expect("clear lease");
    connection
        .execute(
            "INSERT INTO supervised_collector_lease(singleton_id, instance_id, state,
                 started_at_ms, heartbeat_at_ms, lease_until_ms)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)",
            params![Uuid::new_v4().to_string(), state, started, heartbeat, until],
        )
        .expect("lease row");
}

fn account(store: &HubStore, mode: &str, state: &str) {
    sql(store)
        .execute(
            "INSERT INTO collector_account_state(singleton_id, mode, config_fingerprint,
                 state, reason_code, since_ms, rate_limited_until_ms, next_probe_ms,
                 updated_at_ms)
             VALUES (1, ?1, ?2, ?3, ?3, ?4, NULL, NULL, ?4)
             ON CONFLICT(singleton_id) DO UPDATE SET mode = ?1, state = ?3,
                 reason_code = ?3",
            params![mode, "a".repeat(64), state, NOW_MS],
        )
        .expect("account row");
}

fn auth_row(
    store: &HubStore,
    state: &str,
    reason: Option<&str>,
    collecting: bool,
    generation: Option<Uuid>,
    mode: Option<&str>,
    skew: bool,
) {
    sql(store)
        .execute(
            "INSERT INTO collector_auth_state(singleton_id, state, reason, collecting,
                 retry_at, stopped_generation, stopped_mode, fuse_consecutive,
                 fuse_generation, fuse_last_rejection_at, fuse_pause_until, clock_skew,
                 updated_at_ms)
             VALUES (1, ?1, ?2, ?3, NULL, ?4, ?5, 0, NULL, NULL, NULL, ?6, ?7)
             ON CONFLICT(singleton_id) DO UPDATE SET state = ?1, reason = ?2,
                 collecting = ?3, stopped_generation = ?4, stopped_mode = ?5,
                 clock_skew = ?6, updated_at_ms = ?7",
            params![
                state,
                reason,
                i64::from(collecting),
                generation.map(|value| value.to_string()),
                mode,
                i64::from(skew),
                NOW_MS
            ],
        )
        .expect("auth row");
}

/// The started refresh receipt, input fence and binding of an unresolved token
/// refresh, as `begin_*_refresh` writes them.
fn unresolved_refresh(store: &HubStore, transport: &str, generation: Uuid) {
    let (fence, binding) = match transport {
        "legacy_auth" => (
            "legacy_refresh_input_fences",
            "legacy_refresh_receipt_bindings",
        ),
        _ => (
            "fleet_refresh_input_fences",
            "fleet_refresh_receipt_bindings",
        ),
    };
    let connection = sql(store);
    connection
        .execute(
            "INSERT INTO outbound_request_receipts(correlation_id, started_at_ms,
                 vehicle_tesla_id, transport, operation, safety_class, precondition, outcome)
             VALUES (?1, ?2, NULL, ?3, 'token_refresh', 'non_wake_endpoint',
                     'not_required', 'started')",
            params![Uuid::new_v4().to_string(), NOW_MS, transport],
        )
        .expect("receipt");
    let receipt = connection.last_insert_rowid();
    connection
        .execute(
            &format!("INSERT INTO {fence}(input_credential_generation) VALUES (?1)"),
            params![generation.to_string()],
        )
        .expect("fence");
    connection
        .execute(
            &format!(
                "INSERT INTO {binding}(receipt_id, attempt_id, input_credential_generation)
                 VALUES (?1, ?2, ?3)"
            ),
            params![receipt, Uuid::new_v4().to_string(), generation.to_string()],
        )
        .expect("binding");
}

fn gap(store: &HubStore, kind: &str, started: i64, ended: Option<i64>, alert: bool) {
    sql(store)
        .execute(
            "INSERT INTO collection_gaps(vehicle_id, kind, started_at_ms, ended_at_ms,
                 alert, notified_at_ms, created_at_ms)
             VALUES (NULL, ?1, ?2, ?3, ?4, NULL, ?2)",
            params![kind, started, ended, i64::from(alert)],
        )
        .expect("gap row");
}

/// A real encrypted Legacy pair; returns its generation.
fn legacy_pair(
    directory: &Path,
    store: &HubStore,
    access: &str,
    refresh: &str,
    schedule: Option<(i64, i64)>,
) -> Uuid {
    let key = b"cli readiness key";
    let tokens = OwnerTokens::from_file_bytes(
        zeroize::Zeroizing::new(access.as_bytes().to_vec()),
        zeroize::Zeroizing::new(refresh.as_bytes().to_vec()),
    )
    .expect("tokens");
    let (access, refresh) = encrypt_legacy_owner_tokens(key, &tokens).expect("encrypt");
    let stored = match schedule {
        Some((expires_at, next_refresh_at)) => {
            TeslaMateLegacyTokenStore::refreshed(access, refresh, expires_at, next_refresh_at)
        }
        None => TeslaMateLegacyTokenStore::imported(access, refresh),
    }
    .expect("stored pair");
    replace_key_and_tokens(directory, store, key, &stored).expect("persist pair");
    let generation: String = sql(store)
        .query_row(
            "SELECT credential_generation FROM teslamate_legacy_tokens",
            [],
            |row| row.get(0),
        )
        .expect("legacy generation");
    Uuid::parse_str(&generation).expect("uuid")
}

fn fleet_pair(directory: &Path, store: &HubStore) -> Uuid {
    teslatlas_hub::teslamate_credentials::load_or_create_cursor_key(directory).expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        SCOPED_ACCESS.to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("fleet credentials");
    persist_fleet_setup_credentials(
        store,
        directory,
        &credentials,
        std::time::UNIX_EPOCH + Duration::from_secs(1_000),
    )
    .expect("persist fleet");
    let generation: String = sql(store)
        .query_row(
            "SELECT credential_generation FROM fleet_tokens",
            [],
            |row| row.get(0),
        )
        .expect("fleet generation");
    Uuid::parse_str(&generation).expect("uuid")
}

fn jwt(exp: i64) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    format!(
        "{}.{}.signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#))
    )
}

/// An imported, enabled car so a collector can start.
fn seed_car(store: &HubStore) {
    use teslatlas_hub::{
        hub_pack::ProjectionCarSettings,
        protocol::CursorKey,
        teslamate_import::{TeslaMateImportRequest, TeslaMateImportScope, publish_history},
        teslamate_projection::{TeslaMateCar, TeslaMateHistory},
    };
    let history = TeslaMateHistory {
        cars: vec![TeslaMateCar {
            id: 1,
            eid: 5_100_001,
            vid: Some(5_200_001),
            vin: Some("5YJ3E1EA7KF510001".to_owned()),
            name: Some("CLI fixture".to_owned()),
            model: Some("3".to_owned()),
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: None,
            settings: ProjectionCarSettings {
                enabled: true,
                use_streaming_api: true,
                ..Default::default()
            },
        }],
        drives: Vec::new(),
        positions: Vec::new(),
        charging_processes: Vec::new(),
        charges: Vec::new(),
        addresses: Vec::new(),
        geofences: Vec::new(),
        states: Vec::new(),
        updates: Vec::new(),
    };
    publish_history(
        store,
        &CursorKey::from_bytes([0xD3; 32]),
        &TeslaMateImportRequest {
            source_key: "cli-readiness-fixture".to_owned(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: real_now_ms() - 60_000,
        },
        &history,
    )
    .expect("seed car");
}

fn status(directory: &Path, config: &HubConfig, now_ms: i64) -> serde_json::Value {
    let store = HubStore::open_read_only(directory).expect("read-only store");
    build_status_report(&store, config, now_ms).expect("status report")
}

#[test]
fn status_readiness_snapshot_preserves_attention_and_lease_timing() {
    let (temporary, store) = private_hub();
    let config = load(temporary.path(), "on.toml", "interval_seconds = 300");
    account(&store, "legacy", "cap_reached");
    gap(
        &store,
        "hub_down",
        NOW_MS - 100_000,
        Some(NOW_MS - 10_000),
        true,
    );
    let at = |ms: i64| status(temporary.path(), &config, ms);
    let timing = |report: &serde_json::Value| report["collector"].clone();
    let expected_timing = serde_json::json!({
        "instanceId": serde_json::Value::Null,
        "startedAtMs": NOW_MS - 10_000,
        "heartbeatAtMs": NOW_MS - 1_000,
        "leaseUntilMs": NOW_MS + 29_000,
    });

    // No lease: absent, attention and detail beside it, nothing running.
    let absent = at(NOW_MS);
    assert_eq!(absent["ready"], false);
    assert_eq!(absent["readinessReason"], "collector_absent");
    assert_eq!(absent["collectionAttention"], "collector_cap_reached");
    assert_eq!(
        absent["readinessDetail"],
        serde_json::json!(["collector_gap_unnotified"])
    );
    assert_eq!(absent["collectorRunning"], false);
    assert_eq!(absent["collector"], serde_json::Value::Null);
    assert!(absent.get("collectorCadenceError").is_none());

    // Fresh: ready, running, the cap stop is attention only, timing retained.
    lease(
        &store,
        NOW_MS - 10_000,
        NOW_MS - 1_000,
        NOW_MS + 29_000,
        "active",
    );
    let fresh = at(NOW_MS);
    assert_eq!(fresh["ready"], true);
    assert_eq!(fresh["readinessReason"], serde_json::Value::Null);
    assert_eq!(fresh["collectionAttention"], "collector_cap_reached");
    assert_eq!(fresh["collectorRunning"], true);
    let mut lease_timing = timing(&fresh);
    lease_timing["instanceId"] = serde_json::Value::Null;
    assert_eq!(lease_timing, expected_timing);
    // The lease witness has exactly the four redacted members: no invented stop.
    assert_eq!(timing(&fresh).as_object().expect("object").len(), 4);

    // Terminal: not ready, still witnessing a running process.
    lease(
        &store,
        NOW_MS - 10_000,
        NOW_MS - 1_000,
        NOW_MS + 29_000,
        "auth_terminal",
    );
    let terminal = at(NOW_MS);
    assert_eq!(terminal["ready"], false);
    assert_eq!(terminal["readinessReason"], "collector_auth_terminal");
    assert_eq!(terminal["collectorRunning"], true);
    assert_eq!(terminal["collectionAttention"], "collector_cap_reached");

    // Expired at equality: stale, not running, the timing retained as stored.
    lease(&store, NOW_MS - 40_000, NOW_MS - 30_000, NOW_MS, "active");
    let expired = at(NOW_MS);
    assert_eq!(expired["ready"], false);
    assert_eq!(expired["readinessReason"], "collector_stale");
    assert_eq!(expired["collectorRunning"], false);
    assert_eq!(expired["collector"]["leaseUntilMs"], NOW_MS);
    assert_eq!(expired["collector"]["heartbeatAtMs"], NOW_MS - 30_000);
    assert_eq!(at(NOW_MS - 1)["readinessReason"], serde_json::Value::Null);

    // Interval zero: collection is not required, so no lease is demanded and no
    // attention, detail or running witness is reported.
    let off = load(temporary.path(), "off.toml", "interval_seconds = 0");
    let disabled = status(temporary.path(), &off, NOW_MS);
    assert_eq!(disabled["ready"], true);
    assert_eq!(disabled["collectionAttention"], serde_json::Value::Null);
    assert_eq!(disabled["readinessDetail"], serde_json::json!([]));
    assert_eq!(disabled["collectorRunning"], false);
}

#[test]
fn status_readiness_read_errors_are_reported_in_json() {
    let (temporary, store) = private_hub();
    let config = load(temporary.path(), "on.toml", "interval_seconds = 300");
    account(&store, "legacy", "cap_reached");
    // An added reader fails: the synthetic gap table is gone. The command still
    // answers, and says the catalogue is unavailable with no attention or detail,
    // whatever the lease says: a catalogue fault is never hidden behind an absent,
    // expired or terminal lease.
    sql(&store)
        .execute("DROP TABLE collection_gaps", [])
        .expect("drop gaps");
    type LeaseRow = (i64, i64, i64, &'static str);
    let cases: [(&str, Option<LeaseRow>); 4] = [
        ("absent", None),
        (
            "expired",
            Some((NOW_MS - 40_000, NOW_MS - 30_000, NOW_MS, "active")),
        ),
        (
            "terminal",
            Some((
                NOW_MS - 10_000,
                NOW_MS - 1_000,
                NOW_MS + 29_000,
                "auth_terminal",
            )),
        ),
        (
            "fresh",
            Some((NOW_MS - 10_000, NOW_MS - 1_000, NOW_MS + 29_000, "active")),
        ),
    ];
    for (label, row) in cases {
        match row {
            Some((started, heartbeat, until, state)) => {
                lease(&store, started, heartbeat, until, state);
            }
            None => {
                sql(&store)
                    .execute("DELETE FROM supervised_collector_lease", [])
                    .expect("no lease");
            }
        }
        let report = status(temporary.path(), &config, NOW_MS);
        assert_eq!(report["status"], "ok", "{label}");
        assert_eq!(report["ready"], false, "{label}");
        assert_eq!(
            report["readinessReason"], "catalogue_unavailable",
            "{label}"
        );
        assert_eq!(
            report["collectionAttention"],
            serde_json::Value::Null,
            "{label}"
        );
        assert_eq!(report["readinessDetail"], serde_json::json!([]), "{label}");
    }
}

#[tokio::test]
async fn cadence_doctor_dispatch_passes_a_stopped_invalid_setup() {
    // The actual Doctor dispatch on a valid stopped setup (no lease, so the
    // writer is not running) under a cadence the managed Serve would wait on:
    // attention only, so the command succeeds. The same setup with a stale lease
    // is an independent service failure, and the dispatch fails.
    let (temporary, store) = private_hub();
    seed_car(&store);
    legacy_pair(temporary.path(), &store, "access", "refresh", None);
    let config_path = write_config(temporary.path(), "config.toml", FAULT);
    store
        .checkpoint_catalogue_for_immutable_read()
        .expect("checkpoint");
    let doctor = |config: std::path::PathBuf| async move {
        run(Cli {
            config: Some(config),
            command: Command::Doctor,
        })
        .await
    };
    doctor(config_path.clone())
        .await
        .expect("a stopped invalid cadence setup passes doctor");
    lease(&store, 1, 1, 2, "active");
    store
        .checkpoint_catalogue_for_immutable_read()
        .expect("checkpoint");
    let failure = doctor(config_path)
        .await
        .expect_err("a stale lease fails doctor beside the cadence");
    assert!(
        failure.to_string().contains("doctor found failures"),
        "{failure}"
    );
}

#[tokio::test]
async fn doctor_cli_attention_succeeds_but_unresolved_refresh_fails() {
    let (temporary, store) = private_hub();
    seed_car(&store);
    let generation = legacy_pair(temporary.path(), &store, "access", "refresh", None);
    let config_path = write_config(temporary.path(), "config.toml", "interval_seconds = 300");
    lease(
        &store,
        real_now_ms() - 1_000,
        real_now_ms(),
        real_now_ms() + 29_000,
        "active",
    );
    account(&store, "legacy", "owner_api_refused");
    store
        .checkpoint_catalogue_for_immutable_read()
        .expect("checkpoint");
    let doctor = |config: std::path::PathBuf| async move {
        run(Cli {
            config: Some(config),
            command: Command::Doctor,
        })
        .await
    };
    // An Owner refusal is attention: the actual dispatch succeeds.
    doctor(config_path.clone())
        .await
        .expect("attention alone passes doctor");
    // A sign-in required with usable access is attention too.
    auth_row(
        &store,
        "sign_in_required",
        Some("refresh_token_rejected"),
        true,
        Some(generation),
        None,
        false,
    );
    store
        .checkpoint_catalogue_for_immutable_read()
        .expect("checkpoint");
    doctor(config_path.clone())
        .await
        .expect("a usable sign-in passes doctor");
    // An unresolved Legacy refresh fails the dispatch and is not settled.
    unresolved_refresh(&store, "legacy_auth", generation);
    store
        .checkpoint_catalogue_for_immutable_read()
        .expect("checkpoint");
    let failure = doctor(config_path)
        .await
        .expect_err("an unresolved refresh fails doctor");
    assert!(
        failure.to_string().contains("doctor found failures"),
        "{failure}"
    );
    assert!(
        store
            .has_unresolved_legacy_refresh()
            .expect("read-only doctor")
    );
}

#[tokio::test]
async fn fleet_unresolved_refresh_is_attention_not_service_failure() {
    let (temporary, store) = private_hub();
    seed_car(&store);
    let generation = fleet_pair(temporary.path(), &store);
    let collector = "provider = 'fleet'\ninterval_seconds = 300";
    let config_path = write_config(temporary.path(), "config.toml", collector);
    let config = HubConfig::load(&config_path).expect("config");
    lease(
        &store,
        real_now_ms() - 1_000,
        real_now_ms(),
        real_now_ms() + 29_000,
        "active",
    );
    account(&store, "fleet_polling", "collecting");
    unresolved_refresh(&store, "fleet_api", generation);
    store
        .checkpoint_catalogue_for_immutable_read()
        .expect("checkpoint");
    // Status: ready, the unknown refresh is attention, nothing duplicated in the
    // healthy detail, the lease witnesses a running collector.
    let report = status(temporary.path(), &config, real_now_ms());
    assert_eq!(report["ready"], true);
    assert_eq!(report["readinessReason"], serde_json::Value::Null);
    assert_eq!(
        report["collectionAttention"],
        "collector_fleet_refresh_outcome_unknown"
    );
    assert_eq!(report["readinessDetail"], serde_json::json!([]));
    assert_eq!(report["collectorRunning"], true);
    // Doctor passes the actual dispatch; operational validity stays with the
    // fenced input (the doctor's own tests pin the flags).
    run(Cli {
        config: Some(config_path),
        command: Command::Doctor,
    })
    .await
    .expect("an identified unresolved Fleet refresh is attention for doctor");
    assert!(
        store
            .has_unresolved_fleet_refresh()
            .expect("nothing settled")
    );
}

#[test]
fn readiness_context_uses_loaded_mode_with_stale_or_missing_account_rows() {
    let (temporary, store) = private_hub();
    let now_secs = NOW_MS / 1_000;
    let usable_until = now_secs + 100;
    let generation = legacy_pair(
        temporary.path(),
        &store,
        &jwt(usable_until + 300),
        "refresh",
        Some((usable_until + 1_000, usable_until - 500)),
    );
    lease(
        &store,
        NOW_MS - 10_000,
        NOW_MS - 1_000,
        NOW_MS + 29_000,
        "active",
    );
    auth_row(
        &store,
        "sign_in_required",
        Some("refresh_token_rejected"),
        true,
        Some(generation),
        None,
        false,
    );
    let legacy = load(temporary.path(), "legacy.toml", "interval_seconds = 300");
    let fleet = load(
        temporary.path(),
        "fleet.toml",
        "provider = 'fleet'\ninterval_seconds = 300",
    );
    // The account row says Legacy, says Fleet, or is missing: the loaded
    // configuration decides which credential status applies.
    for stored in [Some("legacy"), Some("fleet_polling"), None] {
        match stored {
            Some(mode) => account(&store, mode, "collecting"),
            None => {
                sql(&store)
                    .execute("DELETE FROM collector_account_state", [])
                    .expect("delete the account row");
                let rows: i64 = sql(&store)
                    .query_row("SELECT COUNT(*) FROM collector_account_state", [], |row| {
                        row.get(0)
                    })
                    .expect("count");
                assert_eq!(rows, 0, "the missing-row pass has no account row");
            }
        }
        let legacy_view = status(temporary.path(), &legacy, NOW_MS);
        assert_eq!(
            legacy_view["collectionAttention"], "collector_auth_sign_in_required",
            "Legacy over {stored:?}"
        );
        let fleet_view = status(temporary.path(), &fleet, NOW_MS);
        assert_eq!(
            fleet_view["collectionAttention"],
            serde_json::Value::Null,
            "Fleet over {stored:?}"
        );
    }
    // The authentication time is the evaluation time: the same retained status is
    // signed out once the usable bound has passed, with no write between.
    let later = status(temporary.path(), &legacy, usable_until * 1_000 + 1);
    assert_eq!(later["readinessReason"], "collector_stale");
    lease(
        &store,
        usable_until * 1_000 - 5_000,
        usable_until * 1_000,
        usable_until * 1_000 + 30_000,
        "active",
    );
    let before = status(temporary.path(), &legacy, usable_until * 1_000 - 1);
    assert_eq!(before["ready"], true);
    let at_bound = status(temporary.path(), &legacy, usable_until * 1_000);
    assert_eq!(at_bound["readinessReason"], "collector_auth_terminal");
    // A corrected local configuration and a Serve that kept its faulty snapshot
    // answer from their own loaded cadence and the same dynamic status.
    let faulty = load(temporary.path(), "faulty.toml", FAULT);
    let corrected = load(temporary.path(), "corrected.toml", "interval_seconds = 300");
    let faulty_view = status(temporary.path(), &faulty, usable_until * 1_000 - 1);
    let corrected_view = status(temporary.path(), &corrected, usable_until * 1_000 - 1);
    assert_eq!(
        faulty_view["collectionAttention"],
        "collector_cadence_invalid"
    );
    assert_eq!(
        corrected_view["collectionAttention"],
        "collector_auth_sign_in_required"
    );
    assert_eq!(faulty_view["ready"], corrected_view["ready"]);
}

#[test]
fn cadence_status_json_matches_the_shared_readiness_view() {
    let (temporary, store) = private_hub();
    let faulty = load(temporary.path(), "faulty.toml", FAULT);
    let corrected = load(temporary.path(), "corrected.toml", "interval_seconds = 300");
    let expected_error = serde_json::json!({
        "key": "collector.request_timeout_seconds",
        "value": 121,
        "rule": "seconds_range",
        "message": FAULT_MESSAGE,
    });
    // The lease matrix, with the expiry boundary: the faulty local view carries
    // the cadence diagnostic and attention beside each service reason, and the
    // corrected one carries neither.
    type LeaseRow = (i64, i64, i64, &'static str);
    type LeaseCase = (&'static str, Option<LeaseRow>, Option<&'static str>, bool);
    let cases: [LeaseCase; 5] = [
        ("absent", None, Some("collector_absent"), false),
        (
            "fresh",
            Some((NOW_MS - 10_000, NOW_MS - 1_000, NOW_MS + 29_000, "active")),
            None,
            true,
        ),
        (
            "terminal",
            Some((
                NOW_MS - 10_000,
                NOW_MS - 1_000,
                NOW_MS + 29_000,
                "auth_terminal",
            )),
            Some("collector_auth_terminal"),
            true,
        ),
        (
            "expiring",
            Some((NOW_MS - 40_000, NOW_MS - 30_000, NOW_MS, "active")),
            Some("collector_stale"),
            false,
        ),
        (
            "just fresh",
            Some((NOW_MS - 40_000, NOW_MS - 30_000, NOW_MS + 1, "active")),
            None,
            true,
        ),
    ];
    for (label, row, reason, running) in cases {
        if let Some((started, heartbeat, until, state)) = row {
            lease(&store, started, heartbeat, until, state);
        }
        let local = status(temporary.path(), &faulty, NOW_MS);
        assert_eq!(
            local["collectionAttention"], "collector_cadence_invalid",
            "{label}"
        );
        assert_eq!(local["collectorCadenceError"], expected_error, "{label}");
        assert_eq!(
            local["readinessReason"],
            serde_json::json!(reason),
            "{label}"
        );
        assert_eq!(local["ready"], reason.is_none(), "{label}");
        assert_eq!(local["collectorRunning"], running, "{label}");
        let fixed = status(temporary.path(), &corrected, NOW_MS);
        assert!(fixed.get("collectorCadenceError").is_none(), "{label}");
        assert_eq!(
            fixed["collectionAttention"],
            serde_json::Value::Null,
            "{label}"
        );
        assert_eq!(
            fixed["readinessReason"],
            serde_json::json!(reason),
            "{label}"
        );
    }
    // Disabled collection omits the diagnostic although the setting is faulty.
    let disabled = load(
        temporary.path(),
        "disabled.toml",
        "interval_seconds = 0\nrequest_timeout_seconds = 121",
    );
    let report = status(temporary.path(), &disabled, NOW_MS);
    assert!(report.get("collectorCadenceError").is_none());
    assert_eq!(report["collectionAttention"], serde_json::Value::Null);
}

#[test]
fn cadence_invalid_keeps_otherwise_eligible_serve_supervised() {
    for fleet in [false, true] {
        let (temporary, store) = private_hub();
        seed_car(&store);
        if fleet {
            fleet_pair(temporary.path(), &store);
        } else {
            legacy_pair(temporary.path(), &store, "access", "refresh", None);
        }
        let provider = if fleet { "provider = 'fleet'\n" } else { "" };
        let faulty = load(
            temporary.path(),
            "faulty.toml",
            &format!("{provider}{FAULT}"),
        );
        let corrected = load(
            temporary.path(),
            "corrected.toml",
            &format!("{provider}interval_seconds = 300"),
        );
        assert!(faulty.collector.cadence_violation().is_some());
        // Eligibility ignores the cadence, so both start a supervised collector.
        assert!(
            collector_can_start(&store, &faulty).expect("eligibility"),
            "fleet {fleet}"
        );
        assert!(
            collector_can_start(&store, &corrected).expect("eligibility"),
            "fleet {fleet}"
        );
        // Collection off, no car, and no credentials stay ineligible.
        let off = load(
            temporary.path(),
            "off.toml",
            &format!("{provider}interval_seconds = 0"),
        );
        assert!(!collector_can_start(&store, &off).expect("eligibility"));
        let (empty_dir, empty) = private_hub();
        let no_car = load(
            empty_dir.path(),
            "faulty.toml",
            &format!("{provider}{FAULT}"),
        );
        assert!(!collector_can_start(&empty, &no_car).expect("eligibility"));
        // The same store without tokens: a car but no credentials.
        let (bare_dir, bare) = private_hub();
        seed_car(&bare);
        let no_tokens = load(
            bare_dir.path(),
            "faulty.toml",
            &format!("{provider}{FAULT}"),
        );
        assert!(!collector_can_start(&bare, &no_tokens).expect("eligibility"));
    }
}

#[tokio::test]
async fn cadence_serve_wait_hands_over_cursor_before_server_start() {
    use teslatlas_hub::{
        hub_user_process::AdmittedUserHub,
        protocol::{CursorClaims, HUB_PROJECTION_SCHEMA_V3, OpaqueCursor, PROTOCOL_V1},
        teslamate_credentials::load_or_create_cursor_key,
    };
    let (temporary, store) = private_hub();
    seed_car(&store);
    legacy_pair(temporary.path(), &store, "access", "refresh", None);
    let mut config = load(temporary.path(), "faulty.toml", FAULT);
    assert!(config.collector.cadence_violation().is_some());
    // This runs the public entry, with its production client factories: its data,
    // token and stream endpoints are a provider's unroutable local ones, so a
    // regression that sent a request from the wait could never reach a Tesla host.
    config.collector.owner_api_base_url = Some("https://127.0.0.1:9/".to_owned());
    config.collector.legacy_auth.token_url = Some("https://127.0.0.1:9/oauth2/v3/token".to_owned());
    config.collector.stream_endpoint_override = Some("wss://127.0.0.1:9/streaming/".to_owned());
    let admission = AdmittedUserHub::admit(temporary.path()).expect("admission");
    let (cursor_tx, cursor_rx) = tokio::sync::oneshot::channel();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let collector_store = store.clone();
    let collector_config = config.clone();
    let supervisor = tokio::spawn(crate::run_macos_serve_supervisor(
        true,
        move |ready, shutdown| async move {
            teslatlas_hub::collector::run_supervised_for_admitted_user(
                &collector_store,
                &collector_config,
                admission,
                ready,
                async move {
                    let _ = shutdown.await;
                },
            )
            .await
            .map_err(std::io::Error::other)
        },
        move |cursor_key, shutdown| async move {
            // The listener is built only now: the collector has handed over its
            // exact cursor key.
            let _ = cursor_tx.send(cursor_key);
            let _ = shutdown.await;
            Ok(())
        },
        async move {
            let _ = stop_rx.await;
            crate::MacServeControl::Shutdown
        },
    ));
    let handed = tokio::time::timeout(Duration::from_secs(20), cursor_rx)
        .await
        .expect("the wait hands over before the deadline")
        .expect("the server factory ran")
        .expect("a non-null cursor key");
    // The key is the data directory's own: both issue the same opaque cursor.
    let claims = || CursorClaims {
        protocol: PROTOCOL_V1,
        schema: HUB_PROJECTION_SCHEMA_V3,
        installation_id: Uuid::from_u128(0x11111111_1111_4111_8111_111111111111),
        account_id: Uuid::from_u128(0x22222222_2222_4222_8222_222222222222),
        vehicle_id: Uuid::from_u128(0x33333333_3333_4333_8333_333333333333),
        generation: 7,
        sequence: 11,
    };
    let own = load_or_create_cursor_key(temporary.path()).expect("cursor key");
    assert_eq!(
        OpaqueCursor::issue(&handed, claims())
            .expect("handed cursor")
            .as_str(),
        OpaqueCursor::issue(&own, claims())
            .expect("own cursor")
            .as_str()
    );
    // The supervisor lives until shutdown, with the wait's lease and its open
    // stop gap in place.
    assert!(!supervisor.is_finished());
    let held = store
        .supervised_collector_lease_status()
        .expect("lease")
        .expect("the wait holds the lease");
    assert!(held.lease_until_ms > held.heartbeat_at_ms);
    let stops: i64 = sql(&store)
        .query_row(
            "SELECT COUNT(*) FROM collection_gaps WHERE kind = 'hub_stopped' AND ended_at_ms IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("stop gaps");
    assert_eq!(stops, 1);
    let requests: i64 = sql(&store)
        .query_row(
            "SELECT COUNT(*) FROM outbound_request_receipts",
            [],
            |row| row.get(0),
        )
        .expect("receipts");
    assert_eq!(requests, 0, "the wait sends nothing");
    stop_tx.send(()).expect("shutdown");
    tokio::time::timeout(Duration::from_secs(20), supervisor)
        .await
        .expect("the supervisor stops")
        .expect("join")
        .expect("an orderly shutdown");
    assert!(
        store
            .supervised_collector_lease_status()
            .expect("lease")
            .is_none()
    );
    // The stop stays open for the next ordinary start to close.
    let stops: i64 = sql(&store)
        .query_row(
            "SELECT COUNT(*) FROM collection_gaps WHERE kind = 'hub_stopped' AND ended_at_ms IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("stop gaps");
    assert_eq!(stops, 1);
}

#[tokio::test]
async fn cadence_observe_refuses_before_handoff_and_cannot_finish_as_a_wait() {
    use teslatlas_hub::hub_user_process::AdmittedUserHub;
    let (temporary, store) = private_hub();
    seed_car(&store);
    legacy_pair(temporary.path(), &store, "access", "refresh", None);
    // An unused loopback port: after the refusal nothing may listen on it.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("probe port");
        listener.local_addr().expect("address").port()
    };
    let path = temporary.path().join("faulty.toml");
    fs::write(
        &path,
        format!(
            "data_dir = {:?}\nbind = '127.0.0.1:{port}'\n\n[collector]\n{FAULT}\n",
            temporary.path()
        ),
    )
    .expect("config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("mode");
    let config = HubConfig::load(&path).expect("config");
    let admission = AdmittedUserHub::admit(temporary.path()).expect("admission");
    // A control deadline far beyond the test: a refusal must not wait for it.
    let started = tokio::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        crate::run_observe_orchestration(
            store.clone(),
            config,
            teslatlas_hub::protocol::Sha256Digest::of_bytes(b"observe digest"),
            admission,
            async {
                tokio::time::sleep(Duration::from_secs(600)).await;
                crate::MacServeControl::Shutdown
            },
        ),
    )
    .await
    .expect("the refusal arrives before the control deadline");
    assert!(started.elapsed() < Duration::from_secs(20));
    let error = outcome.expect_err("Observe cannot finish as a wait");
    assert_eq!(error.to_string(), FAULT_MESSAGE);
    // No handoff, no listener, no lease and no gap.
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "nothing listens"
    );
    assert!(
        store
            .supervised_collector_lease_status()
            .expect("lease")
            .is_none()
    );
    let gaps: i64 = sql(&store)
        .query_row("SELECT COUNT(*) FROM collection_gaps", [], |row| row.get(0))
        .expect("gaps");
    assert_eq!(gaps, 0);
}
