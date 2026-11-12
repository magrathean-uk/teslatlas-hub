// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::auth::credential_sign_in::{credential_status, credential_store_unhealthy, status_fixtures};

fn config(root: &Path) -> HubConfig {
    HubConfig::from_exact_bytes(format!("data_dir = {root:?}\nbind = '127.0.0.1:18443'\n[collector]\ninterval_seconds = 0\n").as_bytes()).expect("synthetic config").0
}
fn status(root: &Path, store: &HubStore) -> crate::auth::credential_sign_in::CredentialStatus {
    credential_status(store, root, 1_800_000_000_000).expect("status projection")
}
fn read_only_routes(root: &Path, store: &HubStore) {
    let connection = store.open().expect("observer");
    let version: i64 = connection.query_row("PRAGMA data_version", [], |row| row.get(0)).expect("data version");
    let rows: i64 = connection.query_row("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0)).expect("schema count");
    for _ in 0..2 {
        let lane = HubStore::open_read_only(root).expect("non-admitted lane");
        let _ = credential_status(&lane, root, 1_800_000_000_000).expect("status");
        let _ = inspect_hub(&lane, &config(root)).expect("doctor and preflight projection");
    }
    assert_eq!(connection.query_row::<i64, _, _>("PRAGMA data_version", [], |row| row.get(0)).expect("version after"), version);
    assert_eq!(connection.query_row::<i64, _, _>("SELECT count(*) FROM sqlite_schema", [], |row| row.get(0)).expect("schema after"), rows);
    assert!(!root.join("secrets").join("credential-keys").exists(), "read-only routes publish no keyring");
}

#[test]
fn ct045_status_routes() {
    let root = crate::private_tempdir().expect("fresh store");
    let store = HubStore::initialize(root.path()).expect("store");
    read_only_routes(root.path(), &store);
    let observer = store.open().expect("fixture SQL");
    observer.execute_batch(&format!("PRAGMA user_version={}", crate::db::SCHEMA_VERSION - 1)).expect("below-rung image");
    let before = status(root.path(), &store);
    assert_eq!(before.credential_store.format_state, "pending");
    assert_eq!(before.credential_store.keyring, "ok");
    assert_eq!(before.credential_store.key_age_days, None);
    read_only_routes(root.path(), &store);
    assert_eq!(observer.query_row::<i64, _, _>("PRAGMA user_version", [], |row| row.get(0)).expect("rung unchanged"), i64::from(crate::db::SCHEMA_VERSION - 1));
}

#[test]
fn ct057_status_non_admitted() {
    let (root, store) = status_fixtures::phase_b("pending");
    let admission = crate::hub_user_process::AdmittedUserHub::admit(root.path()).expect("separate process holds admission");
    let lane = HubStore::open_read_only(root.path()).expect("status lane");
    assert_eq!(status(root.path(), &lane).legacy_credentials.store_state, "migration_pending");
    drop(inspect_hub(&lane, &config(root.path())).expect("doctor without admission"));
    assert!(admission.assert_sensitive_access().is_ok());
    assert_eq!(status(root.path(), &store).credential_store.format_state, "pending");
}

#[test]
fn ct078_status_keys() {
    let root = crate::private_tempdir().expect("root");
    let store = HubStore::initialize(root.path()).expect("store");
    let value = serde_json::to_value(status(root.path(), &store)).expect("status JSON");
    for slot in ["legacyCredentials", "fleetCredentials"] { for key in ["origin", "refreshOwner", "storeState"] { assert!(value[slot].get(key).is_some()); } }
    for key in ["formatState", "blockedReason", "cleanupNeededBytes", "keyring", "keyAgeDays", "exposureSuspected", "secretsBackupExcluded", "durability", "legacyFences", "opaqueLegacyFences", "tokenEntryRequiresSignIn", "backupsWaiting"] { assert!(value["credentialStore"].get(key).is_some(), "{key}"); }
    assert_eq!(value["credentialStore"]["keyring"], "ok");
    assert!(value["credentialStore"]["keyAgeDays"].is_null());
    assert_eq!(value["legacyCredentials"]["storeState"], "ok");
    assert!(!root.path().join("secrets").exists());
}

#[test]
fn ct087_status() {
    let (root, store) = status_fixtures::phase_b("mapping");
    let projection = credential_status(&store, root.path(), diagnostic_epoch_ms()).expect("status at the doctor clock");
    // M02-T02: the pre-upgrade pair is refused unread, so the store stays blocked.
    assert_eq!(projection.credential_store.format_state, "blocked");
    assert_eq!(projection.credential_store.legacy_fences, 2);
    assert!(!projection.legacy_credentials.present);
    let doctor = inspect_hub(&HubStore::open_read_only(root.path()).expect("lane"), &config(root.path())).expect("consolidated doctor");
    assert_eq!(doctor.credential_store, projection.credential_store);
}

#[test]
fn ct091_status() {
    // M02-T02: a pre-upgrade pair is refused unread, so a store holding one
    // never reaches cleanup or completion; it stays blocked.
    for (kind, expected) in [("pending", "pending"), ("purge_wait", "blocked"), ("blocked", "blocked")] {
        let (root, store) = status_fixtures::phase_b(kind);
        let projection = status(root.path(), &store);
        assert_eq!(projection.credential_store.format_state, expected, "{kind}");
        assert_eq!(projection.credential_store.backups_waiting, expected != "complete");
        if kind == "blocked" { assert_eq!(projection.credential_store.blocked_reason.owner_api, Some("source_credential_disabled")); }
    }
}

#[test]
fn ct103_durability_readiness() {
    use tower::ServiceExt;
    let (root, store) = status_fixtures::durability_failure();
    assert_eq!(status(root.path(), &store).credential_store.durability, Some("unavailable"));
    assert!(credential_store_unhealthy(&status(root.path(), &store), false, false));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    let query = |store: &HubStore| runtime.block_on(async {
        let response = crate::server::router(store.clone()).oneshot(axum::http::Request::builder().uri("/readyz").body(axum::body::Body::empty()).expect("request")).await.expect("response");
        let code = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 4096).await.expect("readiness body");
        (code, serde_json::from_slice::<serde_json::Value>(&bytes).expect("JSON"))
    });
    let (code, value) = query(&store);
    assert_eq!(code, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(value["reason"], "credential_store_error");
    assert_eq!(query(&store).0, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    let store = status_fixtures::recover(store, root.path());
    assert_eq!(status(root.path(), &store).credential_store.durability, Some("ok"));
    assert_eq!(query(&store).0, axum::http::StatusCode::OK);
}
