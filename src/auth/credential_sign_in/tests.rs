// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::{CredentialVaultEntry, auth::{credential_envelope::{CredentialOrigin, CredentialSlot, RefreshOwner}, credential_vault::{RedeemedGrant, fixtures::{open_fixture_vault, restart_fixture_context}}}, db::{HubStore, LegacyCandidates}};
use zeroize::Zeroizing;

const OWNER: CredentialSlot = CredentialSlot::OwnerApi;
const FLEET: CredentialSlot = CredentialSlot::FleetApi;
fn text(value: &str) -> Zeroizing<String> { Zeroizing::new(value.to_owned()) }
fn fixture() -> (tempfile::TempDir, HubStore, CredentialVaultEntry) {
    let root = crate::private_tempdir().expect("synthetic root");
    let store = HubStore::initialize(root.path()).expect("store");
    let entry = CredentialVaultEntry::for_test(open_fixture_vault(store.clone(), root.path()));
    (root, store, entry)
}
fn supplied(entry: &CredentialVaultEntry, kind: OwnerEntryKind, access: &str, refresh: Option<&str>, replace: bool) -> Result<CanonicalOwnerTokens, CredentialCommandError> {
    persist_owner_entry(entry, prepare_owner_entry(kind, text(access), refresh.map(text), replace)?, false)
}
fn grant(entry: &CredentialVaultEntry, access: &str, refresh: &str) {
    let session = entry.vault().begin_sign_in(OWNER).expect("begin sign-in");
    let receipt = entry.vault().store_hub_grant(RedeemedGrant::from_validated_redemption_at(session, text(access), text(refresh), 28_800, None, None, 1_800_000_000).expect("validated synthetic issuer answer")).expect("grant barrier");
    entry.vault().finish_hub_grant_aftercare(receipt).expect("aftercare");
}
fn owner(entry: &CredentialVaultEntry) -> crate::auth::credential_vault::CredentialPlain { entry.vault().load(OWNER).expect("read").expect("pair") }
fn opaque(store: &HubStore) {
    store.open().expect("fixture SQL").execute("INSERT INTO credential_legacy_fences(legacy_generation,identity,fenced_at_ms) VALUES(?1,'opaque',1)", [uuid::Uuid::new_v4().to_string()]).expect("opaque fence");
}

#[test]
fn ct072_origins() {
    for (kind, origin, refresh_owner) in [(OwnerEntryKind::Direct, CredentialOrigin::DirectEntry, RefreshOwner::Hub), (OwnerEntryKind::Provider, CredentialOrigin::ProviderEntry, RefreshOwner::Hub), (OwnerEntryKind::SourceToken, CredentialOrigin::SourceToken, RefreshOwner::External)] {
        let (_root, _store, entry) = fixture();
        drop(supplied(&entry, kind, "access-0001", Some("refresh-0001"), false).expect("entry barrier"));
        let pair = owner(&entry);
        assert_eq!((pair.origin, pair.refresh_owner, pair.expires_at, pair.next_refresh_at), (Some(origin), refresh_owner, 0, 0));
        if kind == OwnerEntryKind::SourceToken { assert_eq!(supplied(&entry, kind, "access-0002", Some("refresh-0002"), false).unwrap_err().code(), "slot_occupied"); }
    }
    let (_root, _store, entry) = fixture();
    let prepared = prepare_fleet_entry(text("access-0003"), text("refresh-0003"), "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).expect("Fleet input");
    persist_fleet_entry(&entry, prepared, false).expect("Fleet barrier");
    let pair = entry.vault().load(FLEET).expect("read").expect("Fleet pair");
    assert_eq!((pair.origin, pair.refresh_owner), (Some(CredentialOrigin::FleetOauth), RefreshOwner::Hub));
    assert!(pair.expires_at > pair.next_refresh_at && pair.next_refresh_at > 0);
}

#[test]
fn ct073_empty_refresh() {
    for refresh in [None, Some(""), Some(" \t\r\n")] {
        let (_root, store, entry) = fixture();
        drop(supplied(&entry, OwnerEntryKind::Provider, "access-0001", refresh, false).expect("access-only provider"));
        let pair = owner(&entry);
        assert!(!pair.refresh_present && pair.refresh.is_empty());
        assert_eq!((pair.expires_at, pair.next_refresh_at), (0, 0));
        let ciphertext: Vec<u8> = store.open().expect("fixture SQL").query_row("SELECT refresh_envelope FROM credential_pairs WHERE slot='owner_api'", [], |row| row.get(0)).expect("empty envelope");
        assert_eq!(ciphertext.len(), 67, "CT73: authenticated empty plaintext");
        assert_eq!(prepare_owner_entry(OwnerEntryKind::Direct, text("access-0002"), refresh.map(text), false).unwrap_err().code(), "empty_refresh_token");
        assert_eq!(owner(&entry).access.as_str(), "access-0001");
    }
}

#[test]
fn ct074_trimming() {
    let (_root, _store, entry) = fixture();
    let prepared = decode_owner_entry(br#"{"accessToken":" \taccess-0001\r\n","refreshToken":"\trefresh-0001\n"}"#, OwnerEntryKind::Direct, false).expect("received forms");
    drop(persist_owner_entry(&entry, prepared, false).expect("entry barrier"));
    let pair = owner(&entry);
    assert_eq!((pair.access.as_str(), pair.refresh.as_str()), ("access-0001", "refresh-0001"));
    entry.vault().begin_refresh(OWNER, pair.generation).expect("spend input");
    assert_eq!(supplied(&entry, OwnerEntryKind::Direct, "access-0002", Some(" refresh-0001\r\n"), false).unwrap_err().code(), "refresh_token_spent");
}

#[test]
fn ct075_entry_caps() {
    let (root, _store, entry) = fixture();
    let access = "a".repeat(16_384);
    drop(supplied(&entry, OwnerEntryKind::Direct, &access, Some("refresh-0001"), false).expect("at cap"));
    let generation = owner(&entry).generation;
    for (access, refresh) in [("a".repeat(16_385), "refresh-0002".into()), ("access-0002".into(), "r".repeat(16_385))] {
        assert_eq!(supplied(&entry, OwnerEntryKind::Direct, &access, Some(&refresh), false).unwrap_err().code(), "token_too_large");
        assert_eq!(owner(&entry).generation, generation);
    }
    assert!(root.path().join("secrets").join("credential-keys").exists());
}

#[test]
fn ct076_coexistence() {
    let (_root, _store, entry) = fixture();
    drop(supplied(&entry, OwnerEntryKind::SourceToken, "access-0001", Some("refresh-0001"), false).expect("import"));
    grant(&entry, "access-0002", "refresh-0002");
    assert_eq!((owner(&entry).origin, owner(&entry).refresh_owner), (Some(CredentialOrigin::HubLogin), RefreshOwner::Hub));
    assert_eq!(supplied(&entry, OwnerEntryKind::SourceToken, "access-0003", Some("refresh-0003"), false).unwrap_err().code(), "slot_occupied");
    assert_eq!(owner(&entry).access.as_str(), "access-0002");
    let prepared = prepare_owner_entry(OwnerEntryKind::SourceToken, text("access-0003"), Some(text("refresh-0003")), true).expect("explicit replacement");
    struct RefusingNotice;
    impl std::io::Write for RefusingNotice {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> { Err(std::io::Error::other("notice refused")) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    assert!(persist_owner_entry_with_notice(&entry, prepared, false, &mut RefusingNotice).is_err());
    assert_eq!(owner(&entry).access.as_str(), "access-0002", "the warning must precede the replacement");
    let prepared = prepare_owner_entry(OwnerEntryKind::SourceToken, text("access-0003"), Some(text("refresh-0003")), true).expect("replacement retry");
    let mut notice = Vec::new();
    drop(persist_owner_entry_with_notice(&entry, prepared, false, &mut notice).expect("explicit replacement"));
    assert_eq!(notice, format!("{REPLACE_IMPORT_WARNING}\n").as_bytes());
    assert_eq!(owner(&entry).refresh_owner, RefreshOwner::External);
    assert!(REPLACE_IMPORT_WARNING.contains("fresh sign-in"));
}

#[test]
fn ct079_keyring_lost_at_runtime() {
    let (root, store, entry) = fixture();
    drop(supplied(&entry, OwnerEntryKind::Direct, "access-0001", Some("refresh-0001"), false).expect("entry"));
    let pair = owner(&entry);
    entry.vault().begin_refresh(OWNER, pair.generation).expect("spent fence");
    std::fs::rename(root.path().join("secrets").join("credential-keys"), root.path().join("retained-keyring")).expect("simulate missing keyring");
    assert_eq!(credential_status(&store, root.path(), 1_800_000_000_000).expect("status").credential_store.keyring, "keyring_missing");
    drop(entry);
    let store = restart_fixture_context(store, root.path()).expect("new process");
    let entry = CredentialVaultEntry::for_test(open_fixture_vault(store, root.path()));
    assert_eq!(entry.vault().load(OWNER).unwrap_err().code(), "keyring_missing");
    assert_eq!(supplied(&entry, OwnerEntryKind::Direct, "access-0002", Some("refresh-0001"), false).unwrap_err().code(), "refresh_token_spent");
}

#[test]
fn ct080_foreign_key_row() {
    let (_root, store, entry) = fixture();
    drop(supplied(&entry, OwnerEntryKind::Direct, "access-0001", Some("refresh-0001"), false).expect("entry"));
    persist_fleet_entry(&entry, prepare_fleet_entry(text("access-0002"), text("refresh-0002"), "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).expect("Fleet"), false).expect("Fleet barrier");
    store.open().expect("fault SQL").execute("UPDATE credential_pairs SET access_envelope=CAST(substr(access_envelope,1,6)||zeroblob(8)||substr(access_envelope,15) AS BLOB) WHERE slot='owner_api'", []).expect("foreign key header");
    assert_eq!(entry.vault().load(OWNER).unwrap_err().code(), "unknown_key");
    assert_eq!(entry.vault().load(FLEET).expect("independent slot").expect("Fleet").access.as_str(), "access-0002");
}

#[test]
fn ct081_binding_swaps() {
    let (_root, store, entry) = fixture();
    drop(supplied(&entry, OwnerEntryKind::Direct, "access-0001", Some("refresh-0001"), false).expect("entry"));
    store.open().expect("fault SQL").execute("UPDATE credential_pairs SET access_envelope=refresh_envelope, refresh_envelope=access_envelope WHERE slot='owner_api'", []).expect("swap fields");
    assert_eq!(entry.vault().load(OWNER).unwrap_err().code(), "binding_mismatch");
}

#[test]
fn ct082_copy_back() {
    let (_root, store, entry) = fixture();
    drop(supplied(&entry, OwnerEntryKind::Direct, "access-0001", Some("refresh-0001"), false).expect("entry"));
    let pair = owner(&entry);
    let sql = store.open().expect("fixture SQL");
    sql.execute_batch("CREATE TEMP TABLE retained_pair AS SELECT * FROM credential_pairs").expect("retained image");
    entry.vault().begin_refresh(OWNER, pair.generation).expect("first presentation fence");
    drop(supplied(&entry, OwnerEntryKind::Direct, "access-0002", Some("refresh-0002"), false).expect("new supplied pair"));
    sql.execute_batch("DELETE FROM credential_pairs; INSERT INTO credential_pairs SELECT * FROM retained_pair").expect("simulate copy back");
    assert_eq!(owner(&entry).access.as_str(), "access-0001");
    assert!(entry.vault().begin_refresh(OWNER, pair.generation).is_err());
    assert_eq!(supplied(&entry, OwnerEntryKind::Direct, "access-0003", Some("refresh-0001"), false).unwrap_err().code(), "refresh_token_spent");
}

#[test]
fn ct083_no_secrets() {
    struct LogWriter(std::sync::Arc<std::sync::Mutex<Zeroizing<Vec<u8>>>>);
    impl std::io::Write for LogWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.lock().expect("log capture").extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let logs = std::sync::Arc::new(std::sync::Mutex::new(Zeroizing::new(Vec::new())));
    let sink = std::sync::Arc::clone(&logs);
    let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false).with_max_level(tracing::Level::TRACE)
        .with_writer(move || LogWriter(std::sync::Arc::clone(&sink))).finish();
    // With a single registered dispatcher, tracing evaluates callsite interest and the
    // global level on whichever thread registers or rebuilds, so a parallel test thread
    // with no subscriber can switch this capture off. A second, inert dispatcher keeps
    // every thread on the shared path (the anchor of `tesla_stream::recovery_tests`).
    let _anchor = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let _capture = tracing::subscriber::set_default(subscriber);
    let (_root, _store, entry) = fixture();
    tracing::callsite::rebuild_interest_cache();
    let prepared = prepare_owner_entry(OwnerEntryKind::Direct, text("access-0001"), Some(text("refresh-0001")), false).expect("prepared");
    let debug = format!("{prepared:?}");
    assert!(!debug.contains("access-0001") && !debug.contains("refresh-0001"));
    drop(persist_owner_entry(&entry, prepared, false).expect("entry"));
    let pair = owner(&entry);
    entry.vault().begin_refresh(OWNER, pair.generation).expect("spent");
    let error = supplied(&entry, OwnerEntryKind::Direct, "access-0002", Some("refresh-0001"), false).unwrap_err();
    for rendered in [error.to_string(), format!("{error:?}"), format!("{:?}", owner(&entry))] {
        for secret in ["access-0001", "access-0002", "refresh-0001"] { assert!(!rendered.contains(secret)); }
    }
    let logs = logs.lock().expect("captured log");
    let text = std::str::from_utf8(&logs).expect("log UTF-8");
    assert!(text.contains("provenance=") && text.contains("supplied"), "credential write provenance bytes={}", text.len());
    for secret in ["access-0001", "access-0002", "refresh-0001"] { assert!(!text.contains(secret), "credential in a log"); }
}

#[test]
fn ct084_restore_without_secrets() {
    let (root, store, entry) = fixture();
    drop(supplied(&entry, OwnerEntryKind::Direct, "access-0001", Some("refresh-0001"), false).expect("entry"));
    std::fs::rename(root.path().join("secrets"), root.path().join("retained-custody")).expect("data-only restore image");
    let status = credential_status(&store, root.path(), 1_800_000_000_000).expect("status");
    assert!(status.legacy_credentials.present && status.credential_store.keyring == "keyring_missing");
    assert!(credential_store_unhealthy(&status, true, false));
}

#[test]
fn ct085_canonical_migration_entry() {
    let (_root, _store, entry) = fixture();
    drop(supplied(&entry, OwnerEntryKind::SourceToken, " access-0001\n", Some("\trefresh-0001\r\n"), false).expect("source entry"));
    let pair = owner(&entry);
    assert_eq!((pair.access.as_str(), pair.refresh.as_str(), pair.refresh_owner), ("access-0001", "refresh-0001", RefreshOwner::External));
    entry.vault().handover_owner_api_refresh_to_hub().expect("explicit handover");
    entry.vault().begin_refresh(OWNER, pair.generation).expect("consume canonical form");
    assert_eq!(supplied(&entry, OwnerEntryKind::Direct, "access-0002", Some("refresh-0001"), false).unwrap_err().code(), "refresh_token_spent");
}

#[test]
fn ct086_legacy_fences() {
    for received in ["refresh-0001", "refresh-0001\n", "refresh-0001\r\n", " refresh-0001 "] {
        let (root, store, entry) = fixture();
        let candidates = LegacyCandidates::for_secret_parts("access-0001", received).expect("received candidates");
        assert!(!candidates.0.is_empty());
        for generation in candidates.0 {
            store.open().expect("fixture SQL").execute("INSERT OR IGNORE INTO credential_legacy_fences(legacy_generation,identity,fenced_at_ms) VALUES(?1,'opaque',1)", [generation.to_string()]).expect("recognised fence");
        }
        assert_eq!(supplied(&entry, OwnerEntryKind::Direct, "access-0001", Some(received), false).unwrap_err().code(), "refresh_token_spent");
        assert!(!root.path().join("secrets").join("credential-keys").exists());
    }
}

#[test]
fn ct098_continuity_supplied_paths() {
    let (root, store, entry) = fixture();
    opaque(&store);
    for kind in [OwnerEntryKind::Direct, OwnerEntryKind::Provider, OwnerEntryKind::SourceToken] {
        for replace in [false, true] { assert_eq!(supplied(&entry, kind, "access-0001", Some("refresh-0001"), replace).unwrap_err().code(), "fresh_sign_in_required"); }
    }
    let fleet = prepare_fleet_entry(text("access-0002"), text("refresh-0002"), "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).expect("Fleet input");
    assert_eq!(persist_fleet_entry(&entry, fleet, false).unwrap_err().code(), "fresh_sign_in_required");
    assert_eq!(check_token_import_continuity(&store).unwrap_err().code(), "fresh_sign_in_required");
    assert!(!root.path().join("secrets").join("credential-keys").exists());
    drop(supplied(&entry, OwnerEntryKind::Provider, "access-0003", None, false).expect("access-only exception"));
    grant(&entry, "access-0004", "refresh-0004");
    assert_eq!(owner(&entry).origin, Some(CredentialOrigin::HubLogin));
}
