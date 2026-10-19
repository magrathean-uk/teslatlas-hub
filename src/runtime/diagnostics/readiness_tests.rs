// SPDX-License-Identifier: AGPL-3.0-only

// Doctor tests for the readiness tiers, the effective credential status and the
// cadence (B2 clarification 3; C5 section 3.5). Synthetic data in temporary
// stores, no network. They share one fixture: a Hub directory, its doctor
// configuration, an optionally configured car, and a real encrypted pair.

const DOCTOR_SCOPED_ACCESS: &str = "e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iLCJ2ZWhpY2xlX2NtZHMiLCJ2ZWhpY2xlX2NoYXJnaW5nX2NtZHMiXX0.sig";
const DOCTOR_UNSCOPED_ACCESS: &str = "e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSJdfQ.sig";

fn doctor_now_ms() -> i64 {
    diagnostic_epoch_ms()
}

/// Write a private configuration for `directory` with the given `[collector]`
/// body and an optional extra TOML tail, and load it.
fn doctor_config(directory: &Path, collector: &str, tail: &str) -> HubConfig {
    let path = directory.join("config.toml");
    fs::write(
        &path,
        format!(
            "data_dir = {directory:?}\nbind = '127.0.0.1:18443'\n\n[collector]\n{collector}\n{tail}\n"
        ),
    )
    .expect("write config");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("private config");
    HubConfig::load(&path).expect("config")
}

/// A Hub with an imported, enabled car, so a collector can start.
fn doctor_seed_car(store: &HubStore) {
    use crate::teslamate_import::{TeslaMateImportRequest, TeslaMateImportScope, publish_history};
    use crate::teslamate_projection::{TeslaMateCar, TeslaMateHistory};
    let history = TeslaMateHistory {
        cars: vec![TeslaMateCar {
            id: 1,
            eid: 4_100_001,
            vid: Some(4_200_001),
            vin: Some("5YJ3E1EA7KF4100001".to_owned()),
            name: Some("Doctor fixture".to_owned()),
            model: Some("3".to_owned()),
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: None,
            settings: crate::hub_pack::ProjectionCarSettings {
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
        &crate::protocol::CursorKey::from_bytes([0xD2; 32]),
        &TeslaMateImportRequest {
            source_key: "doctor-readiness-fixture".to_owned(),
            scope: TeslaMateImportScope::Selected(1),
            imported_at_ms: doctor_now_ms() - 60_000,
        },
        &history,
    )
    .expect("seed car");
}

/// A real encrypted Legacy pair under a real key; returns its generation.
/// `schedule` is `(expires_at, next_refresh_at)` of a refreshed pair.
fn doctor_legacy_pair(
    directory: &Path,
    store: &HubStore,
    access: &str,
    schedule: Option<(i64, i64)>,
) -> Uuid {
    let key = b"doctor readiness key";
    let tokens =
        crate::credentials::OwnerTokens::from_secret_parts(access.to_owned(), "refresh".to_owned())
            .expect("tokens");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key, &tokens).expect("encrypt");
    let stored = match schedule {
        Some((expires_at, next_refresh_at)) => {
            crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, expires_at, next_refresh_at)
        }
        None => crate::db::TeslaMateLegacyTokenStore::imported(access, refresh),
    }
    .expect("stored pair");
    crate::teslamate_credentials::replace_key_and_tokens(directory, store, key, &stored)
        .expect("persist pair");
    store
        .load_teslamate_legacy_tokens()
        .expect("load")
        .expect("row")
        .credential_generation()
        .expect("generation")
}

fn doctor_jwt(exp: i64) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    format!(
        "{}.{}.signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#))
    )
}

/// Store Fleet credentials with the collection scopes (or without them) and
/// return the stored generation.
fn doctor_fleet_pair(directory: &Path, store: &HubStore, access: &str) -> Uuid {
    doctor_fleet_pair_with(directory, store, access, "fleet-refresh")
}

fn doctor_fleet_pair_with(directory: &Path, store: &HubStore, access: &str, refresh: &str) -> Uuid {
    crate::teslamate_credentials::load_or_create_cursor_key(directory).expect("cursor key");
    let credentials = crate::fleet_credentials::FleetSetupCredentials::new(
        access.to_owned(),
        refresh.to_owned(),
        "fleet-client".to_owned(),
        crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("fleet credentials");
    crate::fleet_credentials::persist_fleet_setup_credentials(
        store,
        directory,
        &credentials,
        UNIX_EPOCH + std::time::Duration::from_secs(1_000),
    )
    .expect("persist fleet credentials");
    store
        .load_fleet_tokens()
        .expect("load")
        .expect("row")
        .credential_generation()
        .expect("generation")
}

fn doctor_row(state: &str, reason: Option<&str>, collecting: bool) -> crate::db::CollectorAuthRow {
    crate::db::CollectorAuthRow {
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

fn doctor_write_row(store: &HubStore, row: &crate::db::CollectorAuthRow) {
    store
        .write_collector_auth_row(row, doctor_now_ms())
        .expect("auth row");
}

/// How far past the acquisition a fixture's lease stays unexpired. The doctor judges the
/// lease against the wall clock, and the real lease lasts 30 seconds: a test that a
/// loaded machine delays by more than that (a whole-module run does) would find its
/// collector stale and fail a check that is not what it tests. The tests that mean a
/// stale or absent lease write that state themselves.
const DOCTOR_LEASE_HEADROOM_MS: i64 = 3_600_000;

/// A fresh active lease: a running required collector, whose lease does not run out
/// while the test is delayed (see [`DOCTOR_LEASE_HEADROOM_MS`]).
fn doctor_fresh_lease(store: &HubStore) -> crate::db::SupervisedCollectorLease {
    let now_ms = doctor_now_ms();
    let lease = store
        .acquire_supervised_collector_lease(now_ms)
        .expect("lease");
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE supervised_collector_lease SET lease_until_ms = ?1 WHERE singleton_id = 1",
            rusqlite::params![now_ms + DOCTOR_LEASE_HEADROOM_MS],
        )
        .expect("lease headroom");
    lease
}

fn doctor_account(store: &HubStore, mode: &str, state: &str) {
    store
        .collector_write_transaction(
            |transaction| {
                let wrote = HubStore::write_collector_rows_in_transaction(
                    transaction,
                    &crate::db::CollectorRowsWrite {
                        now_ms: doctor_now_ms(),
                        account: Some(crate::db::CollectorAccountRow {
                            mode: mode.to_owned(),
                            config_fingerprint: "a".repeat(64),
                            state: state.to_owned(),
                            reason_code: state.to_owned(),
                            since_ms: doctor_now_ms(),
                            rate_limited_until_ms: None,
                            next_probe_ms: None,
                        }),
                        ..crate::db::CollectorRowsWrite::default()
                    },
                )?;
                Ok::<_, StoreError>(((), wrote))
            },
            || Ok(()),
        )
        .expect("account row");
}

fn doctor_check<'a>(report: &'a HubDoctorReport, name: &str) -> &'a DoctorCheck {
    report
        .checks
        .iter()
        .find(|check| check.name == name)
        .unwrap_or_else(|| panic!("check {name}"))
}

fn doctor_failed_checks(report: &HubDoctorReport) -> Vec<&str> {
    report
        .checks
        .iter()
        .filter(|check| !check.passed)
        .map(|check| check.name.as_str())
        .collect()
}

/// A Legacy Hub ready to collect: a car, a pair, a fresh lease. Returns the
/// directory, the store, the configuration and the pair's generation.
fn doctor_legacy_hub(collector: &str) -> (tempfile::TempDir, HubStore, HubConfig, Uuid) {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    doctor_seed_car(&store);
    let generation = doctor_legacy_pair(temporary.path(), &store, "access", None);
    let config = doctor_config(temporary.path(), collector, "");
    doctor_fresh_lease(&store);
    (temporary, store, config, generation)
}

const DOCTOR_LEGACY: &str = "interval_seconds = 300";
const DOCTOR_FLEET: &str = "provider = 'fleet'\ninterval_seconds = 300";

#[test]
fn doctor_attention_passes_without_claiming_collection_can_start() {
    let (_temporary, store, config, generation) = doctor_legacy_hub(DOCTOR_LEGACY);

    // Quiet: ready, no attention, collection can start.
    let quiet = inspect_hub(&store, &config).expect("report");
    assert_eq!(quiet.status, "ok", "{:?}", doctor_failed_checks(&quiet));
    assert_eq!(quiet.collector.readiness, "ready");
    assert_eq!(quiet.collector.collection_attention, None);
    assert!(quiet.collector.running);
    assert!(quiet.collector.lease.is_some());
    assert!(quiet.collector.can_start);
    assert!(quiet.credentials.selected_credentials_present);

    // C3 refusal with C5 ready: Legacy validity and canStart true, the account
    // refusal is attention, attribution ready/false.
    doctor_account(&store, "legacy", "owner_api_refused");
    doctor_write_row(&store, &doctor_row("ready", None, true));
    let refused = inspect_hub(&store, &config).expect("report");
    assert_eq!(refused.status, "ok", "{:?}", doctor_failed_checks(&refused));
    assert_eq!(
        refused.collector.collection_attention.as_deref(),
        Some("collector_api_refused")
    );
    assert_eq!(refused.collector.readiness, "ready");
    assert!(refused.credentials.legacy.valid_for_collection);
    assert!(refused.collector.can_start);
    assert!(
        doctor_check(&refused, "selectedProviderCredentials")
            .detail
            .ends_with("authState=ready unresolvedRefresh=false")
    );
    assert!(
        doctor_check(&refused, "collectorReadiness")
            .detail
            .contains("attention=collector_api_refused")
    );

    // Applicable C5 API refusal and auth configuration error: Legacy validity
    // false and direct canStart false, yet diagnostic admission passes and the
    // report is ok.
    doctor_account(&store, "legacy", "collecting");
    for (row, state, reason) in [
        (
            crate::db::CollectorAuthRow {
                stopped_mode: Some("legacy".to_owned()),
                ..doctor_row("api_refused", Some("owner_api"), false)
            },
            "api_refused",
            "owner_api",
        ),
        (
            doctor_row("configuration_error", Some("region_conflict"), false),
            "configuration_error",
            "region_conflict",
        ),
    ] {
        doctor_write_row(&store, &row);
        let report = inspect_hub(&store, &config).expect("report");
        assert_eq!(report.status, "ok", "{state}: {:?}", doctor_failed_checks(&report));
        assert!(!report.credentials.legacy.valid_for_collection, "{state}");
        assert!(!report.collector.can_start, "{state}");
        assert!(!report.credentials.selected_credentials_present, "{state}");
        assert_eq!(report.credentials.legacy.auth_state.as_deref(), Some(state));
        assert_eq!(report.credentials.legacy.auth_reason.as_deref(), Some(reason));
        assert!(doctor_check(&report, "selectedProviderCredentials").passed, "{state}");
        assert!(doctor_check(&report, "collectorReadiness").passed, "{state}");
        assert!(
            doctor_check(&report, "selectedProviderCredentials")
                .detail
                .ends_with(&format!("authState={state} unresolvedRefresh=false"))
        );
    }
    // The configuration fault is the credential unit's own attention.
    assert_eq!(
        inspect_hub(&store, &config).expect("report").collector.collection_attention.as_deref(),
        Some("collector_auth_configuration_error")
    );

    // An ordinary credential wait without an unresolved receipt keeps the
    // existing operational flags.
    doctor_write_row(&store, &doctor_row("observer_waiting", Some("token_expired"), false));
    let waiting = inspect_hub(&store, &config).expect("report");
    assert_eq!(waiting.status, "ok");
    assert!(waiting.credentials.legacy.valid_for_collection);
    assert!(waiting.collector.can_start);
    assert_eq!(waiting.collector.collection_attention.as_deref(), Some("collector_auth_waiting"));

    // Initial row-8 sign-in with usable access and a closed receipt: valid,
    // can start, sign-in attention, nothing unresolved.
    doctor_write_row(
        &store,
        &crate::db::CollectorAuthRow {
            stopped_generation: Some(generation),
            ..doctor_row("sign_in_required", Some("refresh_token_rejected"), true)
        },
    );
    let sign_in = inspect_hub(&store, &config).expect("report");
    assert_eq!(sign_in.status, "ok", "{:?}", doctor_failed_checks(&sign_in));
    assert!(sign_in.credentials.legacy.valid_for_collection);
    assert!(sign_in.collector.can_start);
    assert_eq!(sign_in.collector.readiness, "ready");
    assert_eq!(
        sign_in.collector.collection_attention.as_deref(),
        Some("collector_auth_sign_in_required")
    );
    assert!(
        doctor_check(&sign_in, "selectedProviderCredentials")
            .detail
            .ends_with("authState=sign_in_required unresolvedRefresh=false")
    );
    assert!(!store.has_unresolved_legacy_refresh().unwrap());
}

#[test]
fn doctor_unresolved_refresh_remains_a_restart_failure() {
    // An unresolved Legacy refresh fails the report whatever the stored status
    // says and whether the access token is usable, and the doctor settles
    // nothing.
    let now_secs = doctor_now_ms() / 1_000;
    for (label, access, schedule) in [
        ("usable", "access".to_owned(), None),
        (
            "unusable",
            doctor_jwt(now_secs - 3_600),
            Some((now_secs + 10_000, now_secs + 5_000)),
        ),
    ] {
        for (state, reason) in [
            ("signed_out", "refresh_outcome_unknown"),
            ("sign_in_required", "refresh_outcome_unknown"),
            ("ready", ""),
        ] {
            let temporary = crate::private_tempdir().expect("temporary Hub");
            let store = HubStore::initialize(temporary.path()).expect("store");
            doctor_seed_car(&store);
            let generation = doctor_legacy_pair(temporary.path(), &store, &access, schedule);
            let config = doctor_config(temporary.path(), DOCTOR_LEGACY, "");
            doctor_fresh_lease(&store);
            store.begin_legacy_refresh(generation).expect("unresolved refresh");
            let mut row = doctor_row(state, (!reason.is_empty()).then_some(reason), state == "ready");
            if state != "ready" {
                row.stopped_generation = Some(generation);
                row.collecting = state == "sign_in_required";
            }
            doctor_write_row(&store, &row);
            let report = inspect_hub(&store, &config).expect("report");
            let case = format!("{label} {state}");
            assert_eq!(report.status, "failed", "{case}");
            assert!(!report.credentials.legacy.valid_for_collection, "{case}");
            assert!(!report.collector.can_start, "{case}");
            let credentials = doctor_check(&report, "selectedProviderCredentials");
            assert!(!credentials.passed, "{case}");
            assert!(credentials.detail.contains("unresolvedRefresh=true"), "{case}");
            // The exact effective state: a sign-in whose token can no longer be
            // used has become a sign-out; nothing else is rewritten.
            let expected = match (state, label) {
                ("sign_in_required", "unusable") => "signed_out",
                _ => state,
            };
            assert_eq!(report.credentials.legacy.auth_state.as_deref(), Some(expected), "{case}");
            assert!(store.has_unresolved_legacy_refresh().expect("read-only"), "{case}");
            let stored = store.load_collector_auth_row().unwrap().expect("row");
            assert_eq!(stored.state, state, "{case}: the stored row is untouched");
        }
    }
}

#[test]
fn doctor_signed_out_and_integrity_failures_remain_failures() {
    // A signed-out unit fails the credential check and the readiness check.
    let (temporary, store, config, generation) = doctor_legacy_hub(DOCTOR_LEGACY);
    doctor_write_row(
        &store,
        &crate::db::CollectorAuthRow {
            stopped_generation: Some(generation),
            ..doctor_row("signed_out", Some("access_token_rejected"), false)
        },
    );
    let signed_out = inspect_hub(&store, &config).expect("report");
    assert_eq!(signed_out.status, "failed");
    assert_eq!(signed_out.collector.readiness, "collector_auth_terminal");
    assert_eq!(
        doctor_failed_checks(&signed_out),
        ["selectedProviderCredentials", "collectorReadiness"]
    );
    assert_eq!(signed_out.credentials.legacy.auth_state.as_deref(), Some("signed_out"));
    assert!(
        doctor_check(&signed_out, "selectedProviderCredentials")
            .detail
            .ends_with("authState=signed_out unresolvedRefresh=false")
    );

    // Missing key.
    doctor_write_row(&store, &doctor_row("ready", None, true));
    let key_path = crate::teslamate_credentials::key_path(temporary.path());
    let key = fs::read(&key_path).expect("key");
    fs::remove_file(&key_path).expect("remove key");
    let missing = inspect_hub(&store, &config).expect("report");
    assert_eq!(missing.status, "failed");
    assert!(doctor_failed_checks(&missing).contains(&"selectedProviderCredentials"));
    // Wrong key.
    fs::write(&key_path, b"another key entirely").expect("wrong key");
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).expect("key mode");
    let wrong = inspect_hub(&store, &config).expect("report");
    assert!(doctor_failed_checks(&wrong).contains(&"selectedProviderCredentials"));
    // Damaged ciphertext under the right key.
    fs::write(&key_path, &key).expect("restore key");
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).expect("key mode");
    assert_eq!(inspect_hub(&store, &config).expect("report").status, "ok");
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE teslamate_legacy_tokens SET access = X'0102030405060708091011121314151617'",
            [],
        )
        .expect("damage ciphertext");
    let cipher = inspect_hub(&store, &config).expect("report");
    assert!(doctor_failed_checks(&cipher).contains(&"selectedProviderCredentials"));
    assert!(!cipher.credentials.legacy.valid_for_collection);

    // A malformed TLS identity and a damaged published pack fail their own checks
    // beside a healthy credential.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    doctor_seed_car(&store);
    doctor_legacy_pair(temporary.path(), &store, "access", None);
    doctor_fresh_lease(&store);
    let certificate = temporary.path().join("certificate.pem");
    let private_key = temporary.path().join("private-key.pem");
    fs::write(&certificate, b"not a certificate").expect("certificate");
    fs::write(&private_key, b"not a key").expect("private key");
    fs::set_permissions(&certificate, fs::Permissions::from_mode(0o600)).expect("mode");
    fs::set_permissions(&private_key, fs::Permissions::from_mode(0o600)).expect("mode");
    let config = doctor_config(
        temporary.path(),
        DOCTOR_LEGACY,
        &format!(
            "[tls]\ncertificate_path = {certificate:?}\nprivate_key_path = {private_key:?}\npublic_url = 'https://hub.example.test/'"
        ),
    );
    let tls = inspect_hub(&store, &config).expect("report");
    assert_eq!(tls.status, "failed");
    assert_eq!(doctor_failed_checks(&tls), ["tlsIdentity"]);
    let packs = temporary.path().join("packs");
    let mut damaged = 0;
    for entry in walk_files(&packs) {
        fs::write(&entry, b"damaged pack").expect("damage pack");
        damaged += 1;
    }
    assert!(damaged > 0, "the seeded car published a pack");
    let pack = inspect_hub(&store, &config).expect("report");
    assert!(doctor_failed_checks(&pack).contains(&"catalogueIntegrity"));
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk_files(&path));
        } else {
            found.push(path);
        }
    }
    found
}

#[test]
fn doctor_disabled_collection_suppresses_attention() {
    // Interval zero (no collector), a valid setup, stored stops and a stale stream
    // witness: no attention, no detail, nothing running, and otherwise OK.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    doctor_seed_car(&store);
    doctor_legacy_pair(temporary.path(), &store, "access", None);
    let config = doctor_config(temporary.path(), "interval_seconds = 0", "");
    doctor_fresh_lease(&store);
    doctor_account(&store, "legacy", "cap_reached");
    doctor_write_row(&store, &doctor_row("observer_waiting", Some("token_expired"), false));
    store.set_stream_audit_failed_cars(1);
    let report = inspect_hub(&store, &config).expect("report");
    assert_eq!(report.status, "ok", "{:?}", doctor_failed_checks(&report));
    assert_eq!(report.collector.collection_attention, None);
    assert!(report.collector.readiness_detail.is_empty());
    assert!(!report.collector.running);
    assert_eq!(report.collector.cadence_error, None);
    assert_eq!(report.collector.readiness, "ready");
    // The init-only control: an empty Hub with collection off.
    let empty = crate::private_tempdir().expect("temporary Hub");
    let empty_store = HubStore::initialize(empty.path()).expect("store");
    let empty_config = doctor_config(empty.path(), "interval_seconds = 0", "");
    let init_only = inspect_hub(&empty_store, &empty_config).expect("report");
    assert_eq!(init_only.status, "ok");
    assert!(init_only.collector.init_only);
    assert_eq!(init_only.collector.collection_attention, None);
    assert!(!init_only.collector.running);
    let json = serde_json::to_value(&init_only.collector).expect("JSON");
    assert_eq!(json["collectionAttention"], serde_json::Value::Null);
    assert_eq!(json["running"], false);
    assert_eq!(json["cadenceError"], serde_json::Value::Null);
    assert_eq!(json["lease"], serde_json::Value::Null);
}

#[test]
fn cs_p1_fleet_diagnostics_ignores_retained_c5() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    doctor_seed_car(&store);
    let generation = doctor_legacy_pair(temporary.path(), &store, "access", None);
    doctor_fleet_pair(temporary.path(), &store, DOCTOR_SCOPED_ACCESS);
    doctor_fresh_lease(&store);
    let retained = crate::db::CollectorAuthRow {
        stopped_generation: Some(generation),
        clock_skew: true,
        ..doctor_row("signed_out", Some("access_token_rejected"), false)
    };
    doctor_write_row(&store, &retained);
    let fleet_config = doctor_config(temporary.path(), DOCTOR_FLEET, "");
    // The account row says Legacy, then there is no account row: Fleet governs.
    for stored_mode in [Some("legacy"), None] {
        match stored_mode {
            Some(mode) => doctor_account(&store, mode, "collecting"),
            None => {
                store
                    .open()
                    .expect("catalogue")
                    .execute("DELETE FROM collector_account_state", [])
                    .expect("delete the account row");
                let rows: i64 = store
                    .open()
                    .expect("catalogue")
                    .query_row("SELECT COUNT(*) FROM collector_account_state", [], |row| row.get(0))
                    .expect("count");
                assert_eq!(rows, 0, "the missing-row pass has no account row");
            }
        }
        let report = inspect_hub(&store, &fleet_config).expect("report");
        assert_eq!(report.status, "ok", "{:?}", doctor_failed_checks(&report));
        let legacy = &report.credentials.legacy;
        assert_eq!(legacy.auth_state, None);
        assert_eq!(legacy.auth_reason, None);
        assert_eq!(legacy.retry_at, None);
        assert!(!legacy.collecting);
        assert!(!legacy.clock_skew);
        assert_eq!(report.collector.readiness, "ready");
        assert_eq!(report.collector.collection_attention, None);
        assert!(
            doctor_check(&report, "selectedProviderCredentials")
                .detail
                .ends_with("authState=not_applicable unresolvedRefresh=not_applicable")
        );
        assert!(report.credentials.fleet.valid_for_collection);
        assert!(report.collector.can_start);
    }
    // Fleet never reads the Legacy key material: with the key gone the Fleet
    // report is exactly what it was, and the retained pair is present but not
    // valid for collection in a Fleet configuration.
    let with_key = inspect_hub(&store, &fleet_config).expect("report");
    assert!(with_key.credentials.legacy.present);
    assert!(!with_key.credentials.legacy.valid_for_collection);
    let key = crate::teslamate_credentials::key_path(temporary.path());
    let key_bytes = fs::read(&key).expect("Legacy key");
    fs::remove_file(&key).expect("remove the Legacy key");
    let without_key = inspect_hub(&store, &fleet_config).expect("report");
    assert_eq!(without_key.status, "ok", "{:?}", doctor_failed_checks(&without_key));
    assert_eq!(without_key.credentials, with_key.credentials);
    assert_eq!(without_key.collector, with_key.collector);
    assert_eq!(
        doctor_check(&without_key, "selectedProviderCredentials"),
        doctor_check(&with_key, "selectedProviderCredentials")
    );
    fs::write(&key, key_bytes).expect("restore the Legacy key");
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).expect("private key");
    // Reading did not write: the retained row is exactly as stored.
    assert_eq!(store.load_collector_auth_row().unwrap(), Some(retained.clone()));
    // Switching back to Legacy exposes the applicable state, and the signed-out
    // unit fails the Legacy report.
    let legacy_config = doctor_config(temporary.path(), DOCTOR_LEGACY, "");
    let back = inspect_hub(&store, &legacy_config).expect("report");
    assert_eq!(back.credentials.legacy.auth_state.as_deref(), Some("signed_out"));
    assert!(back.credentials.legacy.clock_skew);
    assert_eq!(back.status, "failed");
    assert_eq!(store.load_collector_auth_row().unwrap(), Some(retained));
}

#[test]
fn fleet_unresolved_refresh_is_attention_not_service_failure() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    doctor_seed_car(&store);
    let generation = doctor_fleet_pair(temporary.path(), &store, DOCTOR_SCOPED_ACCESS);
    let config = doctor_config(temporary.path(), DOCTOR_FLEET, "");
    doctor_fresh_lease(&store);
    store.begin_fleet_refresh(generation).expect("unresolved Fleet refresh");
    let report = inspect_hub(&store, &config).expect("report");
    // Doctor passes; the service is ready; the unknown refresh is attention.
    assert_eq!(report.status, "ok", "{:?}", doctor_failed_checks(&report));
    assert_eq!(report.collector.readiness, "ready");
    assert_eq!(
        report.collector.collection_attention.as_deref(),
        Some("collector_fleet_refresh_outcome_unknown")
    );
    assert!(report.collector.readiness_detail.is_empty(), "no duplicate healthy detail");
    // Operational validity and canStart stay false; the pair, claims and scopes
    // are independently sound, so diagnostic admission passes.
    assert!(!report.credentials.fleet.valid_for_collection);
    assert!(!report.credentials.selected_credentials_present);
    assert!(!report.collector.can_start);
    assert_eq!(report.credentials.fleet.scope_status.as_deref(), Some("ready"));
    assert!(doctor_check(&report, "selectedProviderCredentials").passed);
    assert!(doctor_check(&report, "fleetCollectionScopes").passed);
    assert!(doctor_check(&report, "collectorReadiness").passed);
    assert!(store.has_unresolved_fleet_refresh().expect("nothing settled"));
    // A valid Fleet cap, billing or Fleet configuration stop is attention with
    // true operational flags (no unresolved receipt).
    let clean = crate::private_tempdir().expect("temporary Hub");
    let clean_store = HubStore::initialize(clean.path()).expect("store");
    doctor_seed_car(&clean_store);
    doctor_fleet_pair(clean.path(), &clean_store, DOCTOR_SCOPED_ACCESS);
    let clean_config = doctor_config(clean.path(), DOCTOR_FLEET, "");
    doctor_fresh_lease(&clean_store);
    for (state, attention) in [
        ("cap_reached", "collector_cap_reached"),
        ("billing_suspended", "collector_billing_suspended"),
        ("config_error", "collector_config_error"),
    ] {
        doctor_account(&clean_store, "fleet_polling", state);
        let stopped = inspect_hub(&clean_store, &clean_config).expect("report");
        assert_eq!(stopped.status, "ok", "{state}: {:?}", doctor_failed_checks(&stopped));
        assert_eq!(stopped.collector.collection_attention.as_deref(), Some(attention));
        assert!(stopped.credentials.fleet.valid_for_collection, "{state}");
        assert!(stopped.collector.can_start, "{state}");
        assert!(
            doctor_check(&stopped, "selectedProviderCredentials")
                .detail
                .ends_with("authState=not_applicable unresolvedRefresh=not_applicable")
        );
    }
}

#[test]
fn fleet_unknown_refresh_does_not_mask_independent_credential_failures() {
    // Each independent fault, with an identified unresolved refresh beside it,
    // still fails admission.
    #[allow(clippy::type_complexity)]
    let faults: [(&str, fn(&Path, &HubStore, Uuid)); 4] = [
        ("missing key", |directory, _, _| {
            fs::remove_file(crate::fleet_credentials::fleet_key_path(directory)).expect("remove key");
        }),
        ("bad ciphertext", |_, store, _| {
            store
                .open()
                .expect("catalogue")
                .execute("UPDATE fleet_tokens SET access = X'0102030405060708090a0b0c0d0e0f1011'", [])
                .expect("damage ciphertext");
        }),
        ("generation mismatch", |_, store, generation| {
            // The binding keeps naming the old generation: not identified.
            store
                .open()
                .expect("catalogue")
                .execute(
                    "UPDATE fleet_tokens SET credential_generation = ?1",
                    rusqlite::params![Uuid::from_u128(generation.as_u128() ^ 1).to_string()],
                )
                .expect("mismatch");
        }),
        ("missing collection scopes", |directory, store, _| {
            // Replace the pair with one that lacks `vehicle_location`: the new
            // pair carries its own generation, so fence it afresh.
            store.clear_fleet_tokens().expect("clear");
            let generation =
                doctor_fleet_pair_with(directory, store, DOCTOR_UNSCOPED_ACCESS, "fleet-refresh-two");
            store.begin_fleet_refresh(generation).expect("unresolved refresh");
        }),
    ];
    for (label, fault) in faults {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("store");
        doctor_seed_car(&store);
        let generation = doctor_fleet_pair(temporary.path(), &store, DOCTOR_SCOPED_ACCESS);
        let config = doctor_config(temporary.path(), DOCTOR_FLEET, "");
        doctor_fresh_lease(&store);
        store.begin_fleet_refresh(generation).expect("unresolved refresh");
        fault(temporary.path(), &store, generation);
        let report = inspect_hub(&store, &config).expect("report");
        assert_eq!(report.status, "failed", "{label}");
        assert!(!doctor_check(&report, "selectedProviderCredentials").passed, "{label}");
        assert!(!report.credentials.fleet.valid_for_collection, "{label}");
        assert!(!report.collector.can_start, "{label}");
        if label == "missing collection scopes" {
            assert_eq!(
                report.credentials.fleet.scope_status.as_deref(),
                Some("ready"),
                "the claims parse, the scopes are short"
            );
            assert!(!doctor_check(&report, "fleetCollectionScopes").passed);
        }
    }
    // A generation mismatch with no unresolved receipt at all is the same error
    // and fails admission too: the unknown-outcome error is never accepted
    // blindly.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    doctor_seed_car(&store);
    let generation = doctor_fleet_pair(temporary.path(), &store, DOCTOR_SCOPED_ACCESS);
    let config = doctor_config(temporary.path(), DOCTOR_FLEET, "");
    doctor_fresh_lease(&store);
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE fleet_tokens SET credential_generation = ?1",
            rusqlite::params![Uuid::from_u128(generation.as_u128() ^ 1).to_string()],
        )
        .expect("mismatch");
    assert!(matches!(
        crate::fleet_credentials::inspect_stored_fleet_credentials(&store, temporary.path()),
        Err(FleetCredentialError::RotationOutcomeUnknown)
    ));
    let report = inspect_hub(&store, &config).expect("report");
    assert_eq!(report.status, "failed");
    assert!(!doctor_check(&report, "selectedProviderCredentials").passed);
}

#[test]
fn readiness_context_uses_loaded_mode_with_stale_or_missing_account_rows() {
    // Fleet configured over a Legacy account row and a retained signed-out
    // status, and the inverse: the configured mode governs the doctor, and the
    // authentication time is the evaluation time.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    doctor_seed_car(&store);
    let now_secs = doctor_now_ms() / 1_000;
    let usable_until = now_secs + 3_600;
    let generation = doctor_legacy_pair(
        temporary.path(),
        &store,
        &doctor_jwt(usable_until + 300),
        Some((usable_until + 1_000, usable_until - 500)),
    );
    doctor_fleet_pair(temporary.path(), &store, DOCTOR_SCOPED_ACCESS);
    doctor_fresh_lease(&store);
    doctor_account(&store, "legacy", "collecting");
    doctor_write_row(
        &store,
        &crate::db::CollectorAuthRow {
            stopped_generation: Some(generation),
            ..doctor_row("sign_in_required", Some("refresh_token_rejected"), true)
        },
    );
    let fleet_config = doctor_config(temporary.path(), DOCTOR_FLEET, "");
    let legacy_config = doctor_config(temporary.path(), DOCTOR_LEGACY, "");
    let fleet = inspect_hub(&store, &fleet_config).expect("report");
    assert_eq!(fleet.credentials.legacy.auth_state, None);
    assert_eq!(fleet.collector.collection_attention, None);
    let legacy = inspect_hub(&store, &legacy_config).expect("report");
    assert_eq!(legacy.credentials.legacy.auth_state.as_deref(), Some("sign_in_required"));
    assert_eq!(
        legacy.collector.collection_attention.as_deref(),
        Some("collector_auth_sign_in_required")
    );
    // No account row at all: the loaded configuration governs both ways.
    store
        .open()
        .expect("catalogue")
        .execute("DELETE FROM collector_account_state", [])
        .expect("delete the account row");
    let rows: i64 = store
        .open()
        .expect("catalogue")
        .query_row("SELECT COUNT(*) FROM collector_account_state", [], |row| row.get(0))
        .expect("count");
    assert_eq!(rows, 0, "no account row");
    let fleet_no_row = inspect_hub(&store, &fleet_config).expect("report");
    assert_eq!(fleet_no_row.credentials.legacy.auth_state, None);
    assert_eq!(fleet_no_row.collector.collection_attention, None);
    let legacy_no_row = inspect_hub(&store, &legacy_config).expect("report");
    assert_eq!(legacy_no_row.credentials.legacy.auth_state.as_deref(), Some("sign_in_required"));
    assert_eq!(
        legacy_no_row.collector.collection_attention.as_deref(),
        Some("collector_auth_sign_in_required")
    );
    // The inverse: a Fleet account row under a Legacy configuration still reads
    // the status, because the loaded mode decides.
    doctor_account(&store, "fleet_polling", "collecting");
    let inverse = inspect_hub(&store, &legacy_config).expect("report");
    assert_eq!(inverse.credentials.legacy.auth_state.as_deref(), Some("sign_in_required"));
    // Past the usability bound the same stored row is effectively signed out,
    // with no write between.
    let later = store
        .effective_collector_auth(crate::config::CollectorMode::Legacy, (usable_until + 1) * 1_000)
        .expect("effective")
        .expect("status");
    assert_eq!(later.row.state, "signed_out");
    assert_eq!(later.row.reason.as_deref(), Some("refresh_token_rejected"));
    assert_eq!(store.load_collector_auth_row().unwrap().unwrap().state, "sign_in_required");
}

#[test]
fn cadence_doctor_stopped_writer_is_informational() {
    // A valid stopped setup (no lease) under a cadence the managed Serve would
    // wait on: the cadence check passes and describes the fault, the absent
    // collector is still allowed, and the report is ok.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    doctor_seed_car(&store);
    doctor_legacy_pair(temporary.path(), &store, "access", None);
    let config = doctor_config(
        temporary.path(),
        "interval_seconds = 300\nrequest_timeout_seconds = 121",
        "",
    );
    let report = inspect_hub(&store, &config).expect("report");
    assert_eq!(report.status, "ok", "{:?}", doctor_failed_checks(&report));
    assert_eq!(report.collector.readiness, "collector_absent");
    assert!(!report.collector.running);
    assert_eq!(report.collector.lease, None);
    assert_eq!(
        report.collector.collection_attention.as_deref(),
        Some("collector_cadence_invalid")
    );
    let expected = "collector setting collector.request_timeout_seconds = 121 must be between 1 and 120 seconds inclusive";
    let cadence = report.collector.cadence_error.as_ref().expect("cadence error");
    assert_eq!(cadence.key, "collector.request_timeout_seconds");
    assert_eq!(cadence.value, 121);
    assert_eq!(cadence.rule, "seconds_range");
    assert_eq!(cadence.message, expected);
    let check = doctor_check(&report, "collectorCadence");
    assert!(check.passed);
    assert!(check.detail.contains(expected), "{}", check.detail);
    assert!(doctor_check(&report, "collectorReadiness").passed);
    let json = serde_json::to_value(&report.collector).expect("JSON");
    assert_eq!(
        json["cadenceError"],
        serde_json::json!({
            "key": "collector.request_timeout_seconds",
            "value": 121,
            "rule": "seconds_range",
            "message": expected,
        })
    );
    // Corrected: no cadence error, the check says valid.
    let corrected = doctor_config(temporary.path(), "interval_seconds = 300", "");
    let ok = inspect_hub(&store, &corrected).expect("report");
    assert_eq!(ok.collector.cadence_error, None);
    assert_eq!(ok.collector.collection_attention, None);
    assert_eq!(doctor_check(&ok, "collectorCadence").detail, "valid");
}

#[test]
fn doctor_does_not_pass_an_unreadable_collection_fact_behind_an_absent_lease() {
    // Doctor tolerates a stopped writer (no lease). That allowance must not hide
    // a fault in an added read: the readiness is the catalogue fault, and the
    // report fails.
    let (_temporary, store, config, _) = doctor_legacy_hub(DOCTOR_LEGACY);
    store
        .open()
        .expect("catalogue")
        .execute("DELETE FROM supervised_collector_lease", [])
        .expect("stopped writer");
    let stopped = inspect_hub(&store, &config).expect("report");
    assert_eq!(stopped.status, "ok", "{:?}", doctor_failed_checks(&stopped));
    assert_eq!(stopped.collector.readiness, "collector_absent");
    store
        .open()
        .expect("catalogue")
        .execute("DROP TABLE collection_gaps", [])
        .expect("drop an added reader's table");
    let broken = inspect_hub(&store, &config).expect("report");
    assert_eq!(broken.collector.readiness, "catalogue_unavailable");
    assert_eq!(broken.collector.collection_attention, None);
    assert!(broken.collector.readiness_detail.is_empty());
    assert_eq!(broken.status, "failed");
    assert!(!doctor_check(&broken, "collectorReadiness").passed);
}

#[test]
fn cadence_doctor_retains_credentials_scopes_and_service_failures() {
    let fault = "interval_seconds = 300\nrequest_timeout_seconds = 121";
    // A stale lease is an independent service failure beside the cadence.
    let (temporary, store, config, generation) = doctor_legacy_hub(fault);
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE supervised_collector_lease SET started_at_ms = 1, heartbeat_at_ms = 1, lease_until_ms = 2",
            [],
        )
        .expect("stale lease");
    let stale = inspect_hub(&store, &config).expect("report");
    assert_eq!(stale.status, "failed");
    assert_eq!(stale.collector.readiness, "collector_stale");
    assert_eq!(doctor_failed_checks(&stale), ["collectorReadiness"]);
    assert!(doctor_check(&stale, "collectorCadence").passed);
    assert!(stale.collector.cadence_error.is_some());
    // A sign-in whose token expired is signed out: the credential and readiness
    // checks fail, the cadence check still passes, and the projection writes
    // nothing.
    let now_secs = doctor_now_ms() / 1_000;
    let expired = doctor_legacy_pair(
        temporary.path(),
        &store,
        &doctor_jwt(now_secs - 3_600),
        Some((now_secs + 10_000, now_secs + 5_000)),
    );
    let _ = generation;
    let row = crate::db::CollectorAuthRow {
        stopped_generation: Some(expired),
        ..doctor_row("sign_in_required", Some("refresh_token_rejected"), true)
    };
    doctor_write_row(&store, &row);
    let report = inspect_hub(&store, &config).expect("report");
    assert_eq!(report.credentials.legacy.auth_state.as_deref(), Some("signed_out"));
    assert_eq!(report.status, "failed");
    assert!(!doctor_check(&report, "selectedProviderCredentials").passed);
    assert!(doctor_check(&report, "collectorCadence").passed);
    assert_eq!(store.load_collector_auth_row().unwrap(), Some(row));
    // A damaged pair is a credential failure beside the cadence.
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE teslamate_legacy_tokens SET access = X'0102030405060708091011121314151617'",
            [],
        )
        .expect("damage ciphertext");
    let damaged = inspect_hub(&store, &config).expect("report");
    assert!(!doctor_check(&damaged, "selectedProviderCredentials").passed);
    assert!(doctor_check(&damaged, "collectorCadence").passed);
    // Missing credentials (no stored pair for the selected provider) remain a
    // failure beside the cadence, for Legacy and for Fleet.
    for provider in [
        "interval_seconds = 300\nrequest_timeout_seconds = 121",
        "provider = 'fleet'\ninterval_seconds = 300\nrequest_timeout_seconds = 121",
    ] {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("store");
        doctor_seed_car(&store);
        let config = doctor_config(temporary.path(), provider, "");
        doctor_fresh_lease(&store);
        let missing = inspect_hub(&store, &config).expect("report");
        assert_eq!(missing.status, "failed", "{provider}");
        assert!(!missing.credentials.selected_credentials_present);
        assert!(!doctor_check(&missing, "selectedProviderCredentials").passed, "{provider}");
        assert!(!missing.collector.can_start, "{provider}");
        assert!(doctor_check(&missing, "collectorCadence").passed, "{provider}");
        assert!(missing.collector.cadence_error.is_some(), "{provider}");
    }
    // Missing Fleet scopes remain a failure beside the cadence.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    doctor_seed_car(&store);
    doctor_fleet_pair(temporary.path(), &store, DOCTOR_UNSCOPED_ACCESS);
    let config = doctor_config(
        temporary.path(),
        "provider = 'fleet'\ninterval_seconds = 300\noffline_drive_timeout_seconds = 20",
        "",
    );
    doctor_fresh_lease(&store);
    let scopes = inspect_hub(&store, &config).expect("report");
    assert_eq!(scopes.status, "failed");
    assert!(!doctor_check(&scopes, "fleetCollectionScopes").passed);
    assert!(doctor_check(&scopes, "collectorCadence").passed);
    assert!(scopes.collector.cadence_error.is_some());
}

#[test]
fn cadence_doctor_describes_inactive_settings_without_attention() {
    // Interval zero: collection is not required, so a faulty cadence is
    // described as inactive and raises nothing.
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("store");
    doctor_seed_car(&store);
    doctor_legacy_pair(temporary.path(), &store, "access", None);
    let config = doctor_config(
        temporary.path(),
        "interval_seconds = 0\nrequest_timeout_seconds = 121",
        "",
    );
    let off = inspect_hub(&store, &config).expect("report");
    assert_eq!(off.status, "ok");
    assert_eq!(off.collector.cadence_error, None);
    assert_eq!(off.collector.collection_attention, None);
    let check = doctor_check(&off, "collectorCadence");
    assert!(check.passed);
    assert!(check.detail.starts_with("inactive"), "{}", check.detail);
    // The same cadence with collection required faults and raises attention.
    let direct_dir = crate::private_tempdir().expect("temporary Hub");
    let direct_store = HubStore::initialize(direct_dir.path()).expect("store");
    doctor_seed_car(&direct_store);
    let direct_config = doctor_config(
        direct_dir.path(),
        "provider = 'fleet'\ninterval_seconds = 300\nrequest_timeout_seconds = 121",
        "",
    );
    let direct = inspect_hub(&direct_store, &direct_config).expect("report");
    assert!(direct.collector.cadence_error.is_some());
    assert_eq!(
        direct.collector.collection_attention.as_deref(),
        Some("collector_cadence_invalid")
    );
}
