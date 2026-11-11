// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::{CredentialVaultEntry, auth::{credential_envelope::{CredentialOrigin, CredentialSlot, RefreshOwner}, credential_vault::{CredentialEntry, RedeemedGrant, fixtures::{open_fixture_vault, restart_fixture_context}}}, db::{HubStore, ReplacePolicy}};
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
/// Stores an Owner API pair as the Hub's own login does (fixture path).
fn own(entry: &CredentialVaultEntry, access: &str, refresh: &str) -> Result<uuid::Uuid, CredentialCommandError> {
    let pair = CredentialEntry::own_login_fixture(text(access), text(refresh))?;
    Ok(entry.vault().store_supplied_entry(pair, ReplacePolicy::ReplaceAny)?)
}
fn grant(entry: &CredentialVaultEntry, access: &str, refresh: &str) {
    let session = entry.vault().begin_sign_in(OWNER).expect("begin sign-in");
    let receipt = entry.vault().store_hub_grant(RedeemedGrant::from_validated_redemption_at(session, text(access), text(refresh), 28_800, None, None, 1_800_000_000).expect("validated synthetic issuer answer")).expect("grant barrier");
    entry.vault().finish_hub_grant_aftercare(receipt).expect("aftercare");
}
fn owner(entry: &CredentialVaultEntry) -> crate::auth::credential_vault::CredentialPlain { entry.vault().load(OWNER).expect("read").expect("pair") }

/// Every supplied-token input is refused with one stable code, before it is
/// parsed, and nothing is stored or created: no pair, no keyring.
#[test]
fn source_credential_disabled_entry_refuses_every_supplied_input() {
    let (root, _store, entry) = fixture();
    for kind in [OwnerEntryKind::Direct, OwnerEntryKind::Provider, OwnerEntryKind::SourceToken] {
        for replace in [false, true] {
            assert_eq!(prepare_owner_entry(kind, text("access-0001"), Some(text("refresh-0001")), replace).unwrap_err().code(), "source_credential_disabled");
            let tokens = crate::credentials::OwnerTokens::from_secret_parts("access-0001".into(), "refresh-0001".into()).expect("synthetic pair");
            assert_eq!(prepare_owner_tokens(kind, tokens, replace).unwrap_err().code(), "source_credential_disabled");
            let refused = decode_owner_entry(br#"{"accessToken":"access-0001","refreshToken":"refresh-0001"}"#, kind, replace).unwrap_err();
            assert!(matches!(&refused, OwnerInputError::Credential(error) if error.code() == "source_credential_disabled"), "{refused}");
            let refused = decode_owner_entry(b"not even a document", kind, replace).unwrap_err();
            assert!(matches!(&refused, OwnerInputError::Credential(error) if error.code() == "source_credential_disabled"), "refused before parsing");
            let refused = decode_owner_files(Zeroizing::new(b"access-0001".to_vec()), Zeroizing::new(b"refresh-0001".to_vec()), kind).unwrap_err();
            assert!(matches!(&refused, OwnerInputError::Credential(error) if error.code() == "source_credential_disabled"));
        }
    }
    assert_eq!(prepare_fleet_entry(text("access-0002"), text("refresh-0002"), "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).unwrap_err().code(), "source_credential_disabled");
    let refused = decode_fleet_entry(br#"{"accessToken":"a","refreshToken":"r","clientId":"c","region":"eu","expiresInSeconds":28800}"#).unwrap_err().to_string();
    assert!(refused.starts_with("source_credential_disabled: ") && !refused.contains("access-0002"), "{refused}");
    for slot in [OWNER, FLEET] { assert!(entry.vault().credential_pair_presence(slot).expect("presence").is_none(), "nothing stored"); }
    assert!(!root.path().join("secrets").join("credential-keys").exists(), "no keyring created");
    assert!(SOURCE_CREDENTIAL_DISABLED_TEXT.contains("own login"));
    assert_eq!(credential_owner_message("source_credential_disabled", None, None, None).as_deref(), Some(SOURCE_CREDENTIAL_DISABLED_TEXT));
}

/// A stored pair the Hub did not get from its own login is never opened: the
/// origin refusal precedes key selection and decryption, so a damaged envelope
/// still reports the policy code. The Hub's own login then replaces it.
#[test]
fn source_credential_disabled_load_refuses_every_foreign_origin() {
    for foreign in [
        CredentialEntry::source_token(text("access-0001"), text("refresh-0001")).expect("source row"),
        CredentialEntry::direct_entry(text("access-0001"), text("refresh-0001")).expect("typed row"),
        CredentialEntry::provider_entry(text("access-0001"), None).expect("provider row"),
    ] {
        let (root, store, entry) = fixture();
        drop(entry);
        crate::auth::credential_vault::fixtures::seed_entry(root.path(), foreign);
        let store = restart_fixture_context(store, root.path()).expect("new process");
        store.open().expect("fault SQL").execute("UPDATE credential_pairs SET access_envelope=zeroblob(4) WHERE slot='owner_api'", []).expect("damage the envelope");
        let entry = CredentialVaultEntry::for_test(open_fixture_vault(store.clone(), root.path()));
        assert_eq!(entry.vault().load(OWNER).unwrap_err().code(), "source_credential_disabled");
        let error = CredentialCommandError::from(entry.vault().load(OWNER).unwrap_err());
        assert!(error.to_string().contains("own login") && !error.to_string().contains("access-0001"));
        grant(&entry, "access-0002", "refresh-0002");
        let pair = owner(&entry);
        assert_eq!((pair.origin, pair.refresh_owner, pair.access.as_str()), (Some(CredentialOrigin::HubLogin), RefreshOwner::Hub, "access-0002"));
    }
}

#[test]
fn ct075_entry_caps() {
    let (root, _store, entry) = fixture();
    let access = "a".repeat(16_384);
    own(&entry, &access, "refresh-0001").expect("at cap");
    let generation = owner(&entry).generation;
    for (access, refresh) in [("a".repeat(16_385), "refresh-0002".to_owned()), ("access-0002".into(), "r".repeat(16_385))] {
        assert_eq!(own(&entry, &access, &refresh).unwrap_err().code(), "token_too_large");
        assert_eq!(owner(&entry).generation, generation);
    }
    assert!(root.path().join("secrets").join("credential-keys").exists());
}

#[test]
fn ct079_keyring_lost_at_runtime() {
    let (root, store, entry) = fixture();
    own(&entry, "access-0001", "refresh-0001").expect("entry");
    let pair = owner(&entry);
    entry.vault().begin_refresh(OWNER, pair.generation).expect("spent fence");
    std::fs::rename(root.path().join("secrets").join("credential-keys"), root.path().join("retained-keyring")).expect("simulate missing keyring");
    assert_eq!(credential_status(&store, root.path(), 1_800_000_000_000).expect("status").credential_store.keyring, "keyring_missing");
    drop(entry);
    let store = restart_fixture_context(store, root.path()).expect("new process");
    let entry = CredentialVaultEntry::for_test(open_fixture_vault(store, root.path()));
    assert_eq!(entry.vault().load(OWNER).unwrap_err().code(), "keyring_missing");
    assert_eq!(own(&entry, "access-0002", "refresh-0001").unwrap_err().code(), "refresh_token_spent");
}

#[test]
fn ct080_foreign_key_row() {
    let (_root, store, entry) = fixture();
    own(&entry, "access-0001", "refresh-0001").expect("entry");
    entry.vault().store_supplied_entry(CredentialEntry::fleet_supplied(text("access-0002"), text("refresh-0002"), "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).expect("Fleet"), ReplacePolicy::ReplaceAny).expect("Fleet barrier");
    store.open().expect("fault SQL").execute("UPDATE credential_pairs SET access_envelope=CAST(substr(access_envelope,1,6)||zeroblob(8)||substr(access_envelope,15) AS BLOB) WHERE slot='owner_api'", []).expect("foreign key header");
    assert_eq!(entry.vault().load(OWNER).unwrap_err().code(), "unknown_key");
    assert_eq!(entry.vault().load(FLEET).expect("independent slot").expect("Fleet").access.as_str(), "access-0002");
}

#[test]
fn ct081_binding_swaps() {
    let (_root, store, entry) = fixture();
    own(&entry, "access-0001", "refresh-0001").expect("entry");
    store.open().expect("fault SQL").execute("UPDATE credential_pairs SET access_envelope=refresh_envelope, refresh_envelope=access_envelope WHERE slot='owner_api'", []).expect("swap fields");
    assert_eq!(entry.vault().load(OWNER).unwrap_err().code(), "binding_mismatch");
}

#[test]
fn ct082_copy_back() {
    let (_root, store, entry) = fixture();
    own(&entry, "access-0001", "refresh-0001").expect("entry");
    let pair = owner(&entry);
    let sql = store.open().expect("fixture SQL");
    sql.execute_batch("CREATE TEMP TABLE retained_pair AS SELECT * FROM credential_pairs").expect("retained image");
    entry.vault().begin_refresh(OWNER, pair.generation).expect("first presentation fence");
    own(&entry, "access-0002", "refresh-0002").expect("new own pair");
    sql.execute_batch("DELETE FROM credential_pairs; INSERT INTO credential_pairs SELECT * FROM retained_pair").expect("simulate copy back");
    assert_eq!(owner(&entry).access.as_str(), "access-0001");
    assert!(entry.vault().begin_refresh(OWNER, pair.generation).is_err());
    assert_eq!(own(&entry, "access-0003", "refresh-0001").unwrap_err().code(), "refresh_token_spent");
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
    let refused = prepare_owner_entry(OwnerEntryKind::Direct, text("access-0001"), Some(text("refresh-0001")), false).unwrap_err();
    own(&entry, "access-0001", "refresh-0001").expect("entry");
    let pair = owner(&entry);
    entry.vault().begin_refresh(OWNER, pair.generation).expect("spent");
    let error = own(&entry, "access-0002", "refresh-0001").unwrap_err();
    for rendered in [refused.to_string(), format!("{refused:?}"), error.to_string(), format!("{error:?}"), format!("{:?}", owner(&entry))] {
        for secret in ["access-0001", "access-0002", "refresh-0001"] { assert!(!rendered.contains(secret)); }
    }
    let logs = logs.lock().expect("captured log");
    let text = std::str::from_utf8(&logs).expect("log UTF-8");
    for secret in ["access-0001", "access-0002", "refresh-0001"] { assert!(!text.contains(secret), "credential in a log"); }
}

#[test]
fn ct084_restore_without_secrets() {
    let (root, store, entry) = fixture();
    own(&entry, "access-0001", "refresh-0001").expect("entry");
    std::fs::rename(root.path().join("secrets"), root.path().join("retained-custody")).expect("data-only restore image");
    let status = credential_status(&store, root.path(), 1_800_000_000_000).expect("status");
    assert!(status.legacy_credentials.present && status.credential_store.keyring == "keyring_missing");
    assert!(credential_store_unhealthy(&status, true, false));
}


/// A fresh store holds no sign-in; the Hub's own login is the one write path
/// whose pair is admitted, with the own-login origin and the Hub as refresh
/// owner.
#[test]
fn own_login_default_sign_in_is_the_only_admitted_store_path() {
    let (root, store, entry) = fixture();
    let fresh = credential_status(&store, root.path(), 1_800_000_000_000).expect("fresh status");
    assert!(!fresh.legacy_credentials.present && !fresh.fleet_credentials.present, "a fresh store holds no sign-in");
    assert!(entry.vault().load(OWNER).expect("fresh read").is_none());
    grant(&entry, "access-0001", "refresh-0001");
    let pair = owner(&entry);
    assert_eq!((pair.origin, pair.refresh_owner), (Some(CredentialOrigin::HubLogin), RefreshOwner::Hub));
    let status = credential_status(&store, root.path(), 1_800_000_000_000).expect("status");
    assert_eq!((status.legacy_credentials.origin, status.legacy_credentials.store_state), (Some("hub_login"), "ok"));
}

/// The status projection reports a stored pair that is not the Hub's own
/// login by its refusal code, without opening it, so no caller treats it as
/// usable and the configured service is unhealthy until the owner signs in.
#[test]
fn source_credential_disabled_status_reports_refused_pairs_unusable() {
    for (foreign, code) in [
        (CredentialEntry::source_token(text("access-0001"), text("refresh-0001")).expect("source row"), "source_credential_disabled"),
        (CredentialEntry::direct_entry(text("access-0001"), text("refresh-0001")).expect("typed row"), "source_credential_disabled"),
    ] {
        let (root, store, entry) = fixture();
        drop(entry);
        crate::auth::credential_vault::fixtures::seed_entry(root.path(), foreign);
        let status = credential_status(&store, root.path(), 1_800_000_000_000).expect("status");
        assert!(status.legacy_credentials.present);
        assert_eq!(status.legacy_credentials.store_state, code);
        assert!(credential_store_unhealthy(&status, true, false), "the configured Owner slot is unusable");
        assert_eq!(credential_owner_message(code, None, None, None).as_deref(), Some(SOURCE_CREDENTIAL_DISABLED_TEXT));
    }
    assert!(credential_owner_message("unknown_origin", None, None, None).is_some_and(|text| text.contains("own login")));
}
