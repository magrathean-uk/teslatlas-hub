// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use super::fixtures::{FixtureExclusion, seed_entry, seed_legacy_supplied_pair, open_fixture_vault};

include!("tests/admitted.rs");
include!("tests/power.rs");

const OWNER: CredentialSlot = CredentialSlot::OwnerApi;
fn entry(access: &str, refresh: &str) -> CredentialEntry {
    CredentialEntry::direct_entry(Zeroizing::new(access.to_owned()), Zeroizing::new(refresh.to_owned())).expect("entry")
}
fn seeded() -> (tempfile::TempDir, Uuid) {
    let root = crate::private_tempdir().expect("fixture root");
    let generation = seed_legacy_supplied_pair(root.path(), "access-0001", "refresh-0001", None);
    (root, generation)
}
fn open(data: &Path) -> Arc<CredentialVault> { open_fixture_vault(HubStore::initialize(data).expect("store"), data) }
fn pair(vault: &CredentialVault) -> CredentialPlain { vault.load(OWNER).expect("load").expect("present") }
fn unchanged(vault: &CredentialVault, generation: Uuid) {
    let row = pair(vault);
    assert!(row.generation == generation && row.access.as_str() == "access-0001" && row.refresh.as_str() == "refresh-0001", "pair retained");
}
fn admitted_migration_fixture(fleet: bool, outcome: Option<&str>, fenced: bool, candidate: usize) -> tempfile::TempDir {
    credential_migration::g24_m2_m::seed_admitted_open_fixture(fleet, outcome, fenced, candidate)
}
fn open_fake_migration(data: &Path) -> Arc<CredentialVault> {
    credential_migration::seam::reset();
    credential_migration::seam::with(|state| { state.fake_pre_upgrade = true; state.fake_generation = true; });
    let vault = open(data); credential_migration::seam::reset(); vault
}

#[test]
fn ct045_admitted_open() {
    let root = crate::private_tempdir().expect("root"); let store = HubStore::initialize(root.path()).expect("store");
    let observer = store.open().expect("observer");
    for admitted in [false, true] {
        if admitted { drop(open(root.path())); }
        for immutable in [false, true] {
            if immutable { observer.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").expect("immutable fixture checkpoint"); }
            let version: i64 = observer.query_row("PRAGMA data_version", [], |row| row.get(0)).expect("data version");
            let lane = if immutable { HubStore::open_immutable_read_only(root.path()) } else { HubStore::open_read_only(root.path()) }.expect("read-only lane");
            let state = lane.credential_format_state().expect("format").expect("state");
            assert!(state.owner_api_state == CredentialSlotFormatState::None && state.fleet_api_state == CredentialSlotFormatState::None && state.completed_at_ms.is_some() && !state.purge_required && state.wal_scrubbed, "CT45.fresh_status_source");
            assert!(lane.credential_pair_presence(OWNER).expect("presence").is_none(), "read-only empty slot");
            if immutable { lane.verify_immutable_snapshot_unchanged().expect("immutable bytes"); }
            drop(lane);
            let after: i64 = observer.query_row("PRAGMA data_version", [], |row| row.get(0)).expect("data version after");
            assert!(after == version, "read-only lane writes nothing");
        }
    }
    assert!(!root.path().join("secrets").join("credential-keys").exists(), "fresh admitted open is lazy");
}

#[test]
fn ct053_blocked_slot_vault() {
    let root = credential_migration::g24_m2_m::seed_admitted_blocked_owner(); let vault = open_fake_migration(root.path());
    assert!(matches!(vault.load(OWNER), Err(CredentialStoreError::SourceCredentialDisabled(CredentialSlot::OwnerApi))), "blocked Owner read");
    assert!(matches!(vault.load(CredentialSlot::FleetApi), Err(CredentialStoreError::SourceCredentialDisabled(CredentialSlot::FleetApi))), "pre-upgrade Fleet refused unread");
    let state = HubStore::initialize(root.path()).expect("status").credential_format_state().expect("format").expect("state");
    assert!(state.owner_api_state == CredentialSlotFormatState::Pending && state.owner_api_blocked.is_some() && !state.old_tables_dropped, "CT53.blocked_status_source");
    let session = vault.begin_sign_in(OWNER).expect("recovery sign-in session");
    let grant = RedeemedGrant::from_validated_redemption(session, Zeroizing::new("access-0003".into()), Zeroizing::new("refresh-0003".into()), 28_800, None, None).expect("validated redemption");
    let stored = vault.store_hub_grant(grant).expect("bound recovery grant");
    let generation = stored.generation(); assert!(pair(&vault).generation == generation && pair(&vault).origin == Some(CredentialOrigin::HubLogin), "recovery pair installed");
    credential_migration::seam::with(|state| { state.fake_pre_upgrade = true; state.fake_generation = true; });
    vault.finish_hub_grant_aftercare(stored).expect("recovery aftercare"); credential_migration::seam::reset();
    let state = HubStore::initialize(root.path()).expect("status").credential_format_state().expect("format").expect("state");
    // M02-T02: the pre-upgrade Fleet row stays refused unread, so the store does
    // not complete; the Owner slot is discarded once the own login replaces it.
    assert!(state.owner_api_state == CredentialSlotFormatState::Discarded && state.fleet_api_blocked.is_some() && state.completed_at_ms.is_none() && !state.old_tables_dropped, "CT53.recovery_status_source");
}

#[test]
fn ct055_admitted_cleanup() {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt}; use std::io::Write;
    let root = crate::private_tempdir().expect("root"); let store = HubStore::initialize(root.path()).expect("store");
    let completed = store.credential_format_state().expect("format").expect("state").completed_at_ms;
    let secrets = private_directory::open_validated(&root.path().join("secrets"), true).expect("secrets");
    let names = ["pre-upgrade-owner-api.relocated.key", "pre-upgrade-owner-api.relocated-previous.key", "pre-upgrade-owner-api.key", "pre-upgrade-fleet-api.key", "fleet-credentials.key", ".fleet-credentials.pending.key", ".fleet-credentials-key-migration"];
    for name in names { std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(secrets.path().join(name)).expect("candidate fixture").write_all(b"synthetic-residue").expect("fixture bytes"); }
    let encoded = crate::runtime::name_test_support::forbidden_name(); let retained = secrets.path().join(format!("{encoded}-kept.key"));
    std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&retained).expect("encoded name fixture").write_all(b"synthetic-retained").expect("fixture bytes");
    let metadata = std::fs::metadata(&retained).expect("metadata");
    let vault = open(root.path()); assert!(vault.test_commit_counts() == (0, 0), "complete cleanup no counted commit");
    assert!(names.iter().all(|name| !secrets.path().join(name).exists()), "all neutral candidates removed");
    let after = std::fs::metadata(&retained).expect("retained metadata");
    assert!(metadata.ino() == after.ino() && metadata.mtime() == after.mtime() && std::fs::read(&retained).expect("retained bytes") == b"synthetic-retained", "encoded name untouched");
    assert!(store.credential_format_state().expect("format").expect("state").completed_at_ms == completed, "CT55.completion_status_source");
}

#[test]
fn ct013_lazy_vault_open() {
    let root = crate::private_tempdir().expect("root");
    let vault = open(root.path());
    assert!(!root.path().join("secrets").join("credential-keys").exists(), "lazy keyring");
    assert!(vault.load(OWNER).expect("empty read").is_none(), "empty row");
    vault.clear_credential_pair(OWNER).expect("empty clear");
    assert!(vault.lock().expect("state").commits_issued == 0, "no counted fresh commit");
}

#[test]
fn ct014_custody_through_vault() {
    use std::os::unix::fs::PermissionsExt;
    assert_fixture_vault_lifecycle();
    #[cfg(target_os = "macos")]
    assert_acl_descriptor_recovery();
    let (root, generation) = seeded();
    let path = root.path().join("secrets").join("credential-keys");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode fixture");
    let vault = open(root.path()); unchanged(&vault, generation);
    assert!(std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777 == 0o600, "mode repaired");
    assert!(vault.keys.read().expect("keys").as_ref().expect("keyring").exposure_suspected, "exposure recorded");
    drop(vault);
    let (directory_root, _) = seeded();
    let directory = directory_root.path().join("secrets");
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).expect("directory mode fixture");
    drop(open(directory_root.path()));
    assert!(std::fs::metadata(&directory).expect("directory metadata").permissions().mode() & 0o777 == 0o700, "CT14 directory custody repaired");
    #[cfg(target_os = "macos")]
    {
        let (acl_root, _) = seeded(); let key = acl_root.path().join("secrets").join("credential-keys");
        assert!(std::process::Command::new("/bin/chmod").args(["+a", "everyone allow read"]).arg(&key).status().expect("ACL fixture").success(), "CT14 ACL installed");
        let vault = open(acl_root.path());
        assert!(vault.keys.read().expect("keys").as_ref().expect("ring").exposure_suspected, "CT14 ACL exposure recorded through the vault");
    }
    for fault in ["symlink", "hardlink", "owner", "fifo", "directory"] {
        let (root, _) = seeded(); let key = root.path().join("secrets").join("credential-keys");
        let retained = root.path().join("fixture-ring");
        if fault != "owner" { std::fs::rename(&key, &retained).expect("fault fixture"); }
        let expected = match fault {
            "symlink" => { std::os::unix::fs::symlink(&retained, &key).expect("link fixture"); credential_keys::UnsafeReason::Symlink },
            "hardlink" => { std::fs::hard_link(&retained, &key).expect("hard link fixture"); credential_keys::UnsafeReason::LinkCount },
            "fifo" => { assert!(std::process::Command::new("/usr/bin/mkfifo").arg(&key).status().expect("FIFO fixture").success(), "FIFO created"); credential_keys::UnsafeReason::NotRegular },
            "directory" => { std::fs::create_dir(&key).expect("directory fixture"); credential_keys::UnsafeReason::NotRegular },
            _ => credential_keys::UnsafeReason::Owner,
        };
        let before = (fault != "owner").then(|| std::fs::read(&retained).expect("target bytes"));
        let result = credential_keys::with_key_faults(credential_keys::KeyFaults { owner: (fault == "owner").then_some(u32::MAX), ..Default::default() }, || CredentialVault::open(HubStore::initialize(root.path()).expect("store"), AdmittedUserHub::for_test(root.path()).expect("admission"), root.path(), VaultOptions { exclusion: Some(Arc::new(FixtureExclusion)), now_ms: Some(1_800_000_000_000), probe_failure: false }));
        assert!(matches!(result, Err(CredentialStoreError::Key(CredentialKeyError::KeyringUnsafe(reason))) if reason == expected), "CT14.custody refusal");
        if let Some(before) = before { assert!(std::fs::read(&retained).expect("retained bytes") == before, "CT14.target unchanged"); }
    }
}

#[cfg(target_os = "macos")]
fn assert_acl_descriptor_recovery() {
    use std::os::fd::AsFd;
    let root = crate::private_tempdir().expect("ACL retry directory");
    let directory = private_directory::open_validated(root.path(), false).expect("ACL retry custody");
    let descriptor = directory.as_fd().try_clone_to_owned().expect("ACL retry descriptor");
    for refusals in [1, 3] {
        assert!(std::process::Command::new("/bin/chmod").args(["+a", "everyone allow search"])
            .arg(root.path()).status().expect("ACL retry fixture").success(), "CT14 ACL retry fixture installed");
        let (result, used) = private_directory::with_acl_descriptor_refusals(refusals,
            || private_directory::clear_extended_acl(&descriptor, root.path()));
        assert!(used == refusals, "CT14 descriptor refusal attempts counted");
        if refusals == 1 {
            assert!(result.is_ok(), "CT14 transient descriptor refusal recovered");
            assert!(private_directory::verify_no_extended_acl(&descriptor, root.path()).is_ok(), "CT14 recovered clear removes actual ACL");
        } else {
            assert!(result.is_err(), "CT14 persistent descriptor refusal fails closed at three attempts");
            assert!(private_directory::verify_no_extended_acl(&descriptor, root.path()).is_err(), "CT14 refused clear leaves actual ACL detectable");
            private_directory::clear_extended_acl(&descriptor, root.path()).expect("ACL retry fixture cleanup");
        }
    }
}

fn assert_fixture_vault_lifecycle() {
    let (root, generation) = seeded();
    let store = HubStore::initialize(root.path()).expect("fixture context store");
    let gate = Arc::new(std::sync::Barrier::new(8));
    let workers: Vec<_> = (0..8).map(|_| {
        let store = store.clone();
        let path = root.path().to_owned();
        let gate = Arc::clone(&gate);
        std::thread::spawn(move || {
            gate.wait();
            open_fixture_vault(store, &path)
        })
    }).collect();
    let vaults: Vec<_> = workers.into_iter().map(|worker| worker.join().expect("fixture opener")).collect();
    assert!(vaults.iter().all(|vault| Arc::ptr_eq(vault, &vaults[0])), "CT14 atomic fixture open");
    let shared = fixtures::open_admitted_fixture_vault(store.clone(), root.path(), vaults[0].test_admission()).expect("CT14 explicit fixture reuse");
    assert!(Arc::ptr_eq(&shared, &vaults[0]), "CT14 fixture sharing is explicit");
    drop(shared);
    assert!(CredentialVault::open(store.clone(), vaults[0].test_admission(), root.path(), VaultOptions::default()).is_err(), "CT14 second vault refused");
    assert!(matches!(CredentialVault::open_for_runtime(store.clone(), vaults[0].test_admission(), root.path(), None), Err(CredentialStoreError::Store(StoreError::CredentialConnectionTaken))), "CT14 runtime refuses fixture reuse");
    let independent_handle = HubStore::initialize(root.path()).expect("independent store handle");
    assert!(CredentialVault::open(independent_handle, vaults[0].test_admission(), root.path(), VaultOptions::default()).is_err(), "CT14 admission owns one vault across store handles");
    assert!(AdmittedUserHub::admit(root.path()).is_err(), "CT14 second admission refused");
    let weak = Arc::downgrade(&vaults[0]);
    let retained_admission = Arc::downgrade(&vaults[0].test_admission());
    drop(vaults);
    assert!(weak.upgrade().is_none(), "CT14 fixture releases vault");
    assert!(retained_admission.upgrade().is_some(), "CT14 store retains admission without a live vault");
    assert!(AdmittedUserHub::admit(root.path()).is_err(), "CT14 store lifetime excludes re-admission");
    drop(store.take_credential_connection().expect("CT14 released credential connection"));
    let reopened = open_fixture_vault(store.clone(), root.path());
    assert!(Arc::ptr_eq(&retained_admission.upgrade().expect("retained admission"), &reopened.test_admission()), "CT14 reopen reuses admission");
    assert!(reopened.options.now_ms.is_none(), "CT14 fixture uses one runtime clock");
    unchanged(&reopened, generation);
    let previous = Arc::downgrade(&reopened);
    let finishing_store = store.clone();
    let finishing_vault = Arc::clone(&reopened);
    let finishing = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        // A finishing holder must still be able to enter the registry while the
        // restart waits. Holding current during the poll would block this exit.
        drop(finishing_store.fixture_vault.current.lock().expect("CT14 finishing lookup"));
        drop(finishing_vault);
        drop(finishing_store);
    });
    drop(reopened);
    let store = fixtures::restart_fixture_context(store, root.path()).expect("CT14 fresh process context");
    finishing.join().expect("CT14 holder finished during restart");
    assert!(previous.upgrade().is_none() && retained_admission.upgrade().is_none(), "CT14 process released before restart");
    let reopened = open_fixture_vault(store, root.path());
    assert_ne!(previous.as_ptr(), Arc::as_ptr(&reopened), "CT14 restarted vault is new");
    let (other_root, _) = seeded();
    let other_store = HubStore::initialize(other_root.path()).expect("independent fixture store");
    let independent = open_fixture_vault(other_store, other_root.path());
    assert!(!Arc::ptr_eq(&reopened, &independent), "CT14 fixture contexts isolated");
}

#[test]
fn ct015_missing_keyring_beside_rows() {
    let (root, _) = seeded();
    std::fs::rename(root.path().join("secrets").join("credential-keys"), root.path().join("keyring.fixture")).expect("missing fixture");
    let vault = open(root.path());
    assert!(matches!(vault.load(OWNER), Err(CredentialStoreError::Key(CredentialKeyError::KeyringMissing))), "missing key read");
    assert!(!root.path().join("secrets").join("credential-keys").exists(), "no replacement");
}

struct FailedExclusion;
impl BackupExclusion for FailedExclusion {
    fn ensure_excluded(&self, _: &Path) -> Result<ExclusionState, CredentialKeyError> { Err(CredentialKeyError::BackupExclusionFailed) }
}

fn prepare_rotation(data: &Path) {
    let secrets = private_directory::open_validated(&data.join("secrets"), false).expect("secrets");
    let keys = credential_keys::load_keyring(&secrets).expect("keyring").expect("present");
    credential_keys::begin_rotation(&secrets, &FixtureExclusion, &keys, 1_800_086_400).expect("rotation publication");
}

#[test]
fn ct020_rotation_crash_points() {
    // A stopped process after step 1, and after step 2's unproved COMMIT.
    for after_commit in [false, true] {
        let (root, input) = seeded();
        if after_commit {
            let vault = open(root.path());
            vault.lock().expect("state").connection.set_test_barrier_failure_at(Some(2));
            assert!(matches!(vault.rotate(), Err(CredentialStoreError::DurabilityUnavailable)), "step 2 barrier fault");
            assert!(vault.keys.read().expect("keys").as_ref().expect("keyring").keyring().previous.is_some(), "previous retained");
            drop(vault);
        } else { prepare_rotation(root.path()); }
        let vault = open(root.path()); unchanged(&vault, input);
        assert!(vault.keys.read().expect("keys").as_ref().expect("keyring").keyring().previous.is_none(), "resume stable");
        assert!(!vault.lock().expect("state").read_only, "resume writable");
    }
    {
        let (root, input) = seeded(); let vault = open(root.path());
        let db = HubStore::initialize(root.path()).expect("rollback fixture store").open().expect("rollback fixture");
        db.execute_batch("CREATE TRIGGER refuse_reseal BEFORE UPDATE ON credential_pairs BEGIN SELECT RAISE(ABORT,'reseal rollback'); END;").expect("step 2 rollback");
        assert!(vault.rotate().is_err(), "CT20 step 2 statement rolled back"); unchanged(&vault, input);
        assert!(vault.keys.read().expect("keys").as_ref().expect("ring").keyring().previous.is_some(), "CT20 rollback retains both keys");
        drop(vault); db.execute_batch("DROP TRIGGER refuse_reseal").expect("lift rollback fault");
        let vault = open(root.path()); unchanged(&vault, input);
        assert!(vault.keys.read().expect("keys").as_ref().expect("ring").keyring().previous.is_none(), "CT20 retry completes rolled-back reseal");
    }
    // A failing resume statement is retained as a diagnostic, never an open failure.
    let (root, input) = seeded(); prepare_rotation(root.path());
    let store = HubStore::initialize(root.path()).expect("fault store");
    let db = store.open().expect("fault connection");
    db.execute_batch("CREATE TRIGGER fixture_reservation_refusal BEFORE INSERT ON credential_key_usage BEGIN SELECT RAISE(ABORT, 'fixture reservation fault'); END;").expect("fault trigger");
    let vault = open(root.path()); unchanged(&vault, input);
    assert!(vault.lock().expect("state").resume_error.is_some(), "resume failure recorded");
    assert!(vault.keys.read().expect("keys").as_ref().expect("keyring").keyring().previous.is_some(), "rotation retained");
    drop(vault); db.execute_batch("DROP TRIGGER fixture_reservation_refusal").expect("disarm fixture");
    let vault = open(root.path()); unchanged(&vault, input);
    assert!(vault.keys.read().expect("keys").as_ref().expect("keyring").keyring().previous.is_none(), "next open resumes");
    drop(vault);
    let (root, input) = seeded(); prepare_rotation(root.path());
    OPEN_CONTROL.with(|slot| *slot.borrow_mut() = Some(Box::new(|connection| connection.set_test_barrier_failure_at(Some(1)))));
    let vault = open(root.path()); OPEN_CONTROL.with(|slot| *slot.borrow_mut() = None);
    assert!(vault.lock().expect("state").read_only && vault.keys.read().expect("keys").as_ref().expect("ring").keyring().previous.is_some(), "CT20.failed resume barrier retains readable rotating vault");
    unchanged(&vault, input); drop(vault);
    let vault = open(root.path()); unchanged(&vault, input); drop(vault);
    // An undecodable slot records a resume failure while another slot remains
    // usable, including at subsequent opens.
    let (root, _) = seeded();
    seed_entry(root.path(), CredentialEntry::fleet_supplied(Zeroizing::new("fleet-access-0001".into()), Zeroizing::new("fleet-refresh-0001".into()), "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28800).expect("Fleet fixture"));
    prepare_rotation(root.path());
    let db = rusqlite::Connection::open(root.path().join("hub.sqlite")).expect("fault fixture");
    db.execute("UPDATE credential_pairs SET access_envelope=CAST(substr(access_envelope,1,6)||zeroblob(8)||substr(access_envelope,15) AS BLOB) WHERE slot='owner_api'", []).expect("unknown key fixture");
    drop(db);
    for _ in 0..2 {
        let vault = open(root.path());
        assert!(vault.lock().expect("state").resume_error.is_some(), "CT20.undecodable resume recorded");
        // M02-T02: the Fleet slot is refused at load, not by the Owner decode failure.
        assert!(vault.load(OWNER).is_err() && matches!(vault.load(CredentialSlot::FleetApi), Err(CredentialStoreError::SourceCredentialDisabled(CredentialSlot::FleetApi))), "CT20.independent slot refusal");
    }
}

#[test]
fn ct021_rotation_disk_full() {
    let (root, input) = seeded(); let vault = open(root.path());
    let before = std::fs::read(root.path().join("secrets").join("credential-keys")).expect("keyring bytes");
    let error = credential_keys::with_key_faults(credential_keys::KeyFaults { write_errno: Some(rustix::io::Errno::NOSPC.raw_os_error()), ..Default::default() }, || vault.rotate());
    assert!(matches!(error, Err(CredentialStoreError::Key(CredentialKeyError::Io { .. }))), "disk full reported");
    assert!(std::fs::read(root.path().join("secrets").join("credential-keys")).expect("retained keyring") == before, "keyring unchanged");
    unchanged(&vault, input);
    drop(vault);
    prepare_rotation(root.path());
    RESEAL_CONTROL.with(|control| *control.borrow_mut() = Some(Box::new(|connection| {
        connection.set_test_query_only(true).expect("SQLite read-only control");
    })));
    let vault = open(root.path());
    {
        let mut state = vault.lock().expect("state");
        assert!(matches!(&state.resume_error, Some(CredentialStoreError::Store(StoreError::Begin(error) | StoreError::CredentialPairStore(error))) if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ReadOnly)), "CT21.SQLITE_READONLY recorded");
        let retained: (Vec<u8>, Vec<u8>) = state.connection.test_connection().query_row(
            "SELECT access_envelope,refresh_envelope FROM credential_pairs WHERE slot='owner_api'", [], |row| Ok((row.get(0)?, row.get(1)?))).expect("CT21 retained envelopes");
        let preceding = vault.keys.read().expect("keys").as_ref().expect("ring").keyring().previous.as_ref().expect("preceding key").id;
        assert!([retained.0, retained.1].iter().all(|bytes| parse_header(bytes[..ENVELOPE_HEADER_BYTES].try_into().expect("header")).expect("valid header").key_id == preceding), "CT21 pair stays under the preceding key");
        state.connection.set_test_query_only(false).expect("restore SQLite writes");
    }
    unchanged(&vault, input);
    assert!(vault.keys.read().expect("keys").as_ref().expect("keyring").keyring().previous.is_some(), "rotating after refusal");
    drop(vault);
    let vault = open(root.path()); unchanged(&vault, input);
    assert!(vault.keys.read().expect("keys").as_ref().expect("keyring").keyring().previous.is_none(), "resume after refusal");
}

#[test]
fn ct022_forced_rotation_vault() {
    for at_limit in [false, true] {
        let (root, input) = seeded(); let vault = open(root.path());
        let id = vault.keys.read().expect("keys").as_ref().expect("keyring").keyring().current.id;
        { let state = vault.lock().expect("state"); state.connection.test_connection().execute("UPDATE credential_key_usage SET reserved_invocations=?1 WHERE key_id=?2", rusqlite::params![if at_limit { 1_i64 << 31 } else { (1_i64 << 31) - 2 }, id.0.as_slice()]).expect("usage fixture"); }
        let receipt = vault.begin_refresh(OWNER, input).expect("begin"); let output = Uuid::new_v4();
        vault.persist_refreshed(OWNER, receipt, input, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000).expect("sealing write");
        let current={let keys = vault.keys.read().expect("keys"); let keys = keys.as_ref().expect("keyring");
            assert!(keys.keyring().previous.is_none(), "stable after trigger");
            assert!((keys.keyring().current.id != id) == at_limit, "rotation threshold");keys.keyring().current.id};
        let mut state = vault.lock().expect("state");
        let used = state.connection.credential_reserved_invocations(current).expect("reservation count");
        assert!(used == if at_limit { 4 } else { 1_u64 << 31 }, "reserved before seals");
        assert!(state.connection.credential_refresh_is_complete(OWNER, receipt, input, output).expect("receipt proof"), "receipt preserved");
        drop(state);
        assert!(pair(&vault).generation == output, "successor opens");
    }
}

/// A source-imported pair (external refresh owner) and a pair migrated with no
/// recorded origin are never loaded, never handed over and never refreshed.
#[test]
fn source_credential_disabled_vault_refuses_source_and_unknown_origin() {
    let root = crate::private_tempdir().expect("root");
    let source = CredentialEntry::source_token(Zeroizing::new("access-0001".into()), Zeroizing::new("refresh-0001".into())).expect("source entry");
    seed_entry(root.path(), source); let vault = open(root.path());
    assert!(matches!(vault.load(OWNER), Err(CredentialStoreError::SourceCredentialDisabled(OWNER))), "source pair refused");
    drop(vault);
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(crate::credentials::assert_external_refresh_owner_contract());
    // A pair with no recorded origin (an Owner row migrated by an older Hub)
    // is refused by the same header check; the vault no longer converts
    // pre-upgrade rows, so the header is checked directly.
    let unknown = admit_stored_origin(OWNER, None, RefreshOwner::Hub).expect_err("unknown origin refused");
    assert!(matches!(unknown, CredentialStoreError::UnknownOrigin(OWNER)), "unknown origin refused");
    assert_eq!(unknown.code(), "unknown_origin");
}

#[test]
fn ct097_successor_legacy_forms() {
    for suffix in ["\n", "\r\n"] {
        assert!(matches!(crate::credentials::OwnerTokens::from_secret_parts("access-0002".into(), format!("refresh-0002{suffix}")), Err(crate::credentials::CredentialError::InvalidTokenBytes)), "line-ended successor refused");
    }
    for slot in [OWNER, CredentialSlot::FleetApi] { for fenced in [true, false] {
        let root = crate::private_tempdir().expect("slot fixture");
        let input = if slot == OWNER { seed_legacy_supplied_pair(root.path(), "access-0001", "refresh-0001", None) } else {
            seed_entry(root.path(), CredentialEntry::fleet_supplied(Zeroizing::new("access-0001".into()), Zeroizing::new("refresh-0001".into()), "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).expect("Fleet fixture"))
        };
        let generation_for = |access: &[u8], refresh: &[u8]| {
            crate::credentials::OwnerTokens::from_secret_parts(std::str::from_utf8(access).ok()?.to_owned(), std::str::from_utf8(refresh).ok()?.to_owned()).ok()?;
            Some(Uuid::new_v5(&Uuid::from_u128(0x40008000000000000003), refresh))
        };
        let generation = generation_for(b"access-0002", if fenced { b"refresh-0002".as_slice() } else { b" refresh-0002".as_slice() }).expect("fixture identity");
        let candidates = LegacyCandidates::for_successor_with_old_g("access-0002", "refresh-0002", generation_for).expect("legacy candidate");
        assert!(candidates.0.len() == 1, "single kept-constructor form");
        let store = HubStore::initialize(root.path()).expect("fixture store");
        store.open().expect("fixture connection").execute("INSERT INTO credential_legacy_fences(legacy_generation, identity, fenced_at_ms) VALUES(?1, 'opaque', 1)", [generation.to_string()]).expect("legacy fence fixture");
        let vault = open(root.path()); let receipt = vault.begin_refresh(slot, input).expect("begin");
        let issued = vault.test_commit_counts().0;
        let result = with_successor_generation_fixture(candidates.0[0], || vault.persist_refreshed(slot, receipt, input, Uuid::new_v4(), "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000));
        if fenced {
            assert!(matches!(result, Err(CredentialStoreError::RefreshTokenSpent)), "legacy successor fence");
            if slot == OWNER {
                let row = vault.load(slot).expect("retained load").expect("pair");
                assert!(row.generation == input && row.access.as_str() == "access-0001" && row.refresh.as_str() == "refresh-0001", "CT97 pair retained");
            } else {
                // M02-T02: the retained Fleet pair is refused at load.
                assert!(matches!(vault.load(slot), Err(CredentialStoreError::SourceCredentialDisabled(CredentialSlot::FleetApi))), "CT97 Fleet pair refused");
                assert!(vault.stored_generation(slot).expect("generation") == Some(input), "CT97 Fleet pair retained");
            }
            assert!(vault.has_unresolved_refresh(slot).expect("unresolved"), "input consumed");
            assert!(vault.test_commit_counts().0 == issued + 1, "no completion or closing commit");
        } else { result.expect("leading space is a separate legacy form"); }
    } }
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("CT97 manager runtime")
        .block_on(crate::credentials::assert_vault_fenced_successor_contract());
}
fn open_exclusion(data: &Path, fail: bool) -> Arc<CredentialVault> {
    let options = VaultOptions { exclusion: Some(if fail { Arc::new(FailedExclusion) } else { Arc::new(FixtureExclusion) }), now_ms: Some(1_800_000_000_000), probe_failure: false };
    CredentialVault::open(HubStore::initialize(data).expect("store"), AdmittedUserHub::for_test(data).expect("admission"), data, options).expect("open")
}
#[test]
fn ct018_failed_exclusion_vault() {
    let (root, generation) = seeded(); let vault = open_exclusion(root.path(), true);
    unchanged(&vault, generation);
    assert!(matches!(vault.begin_refresh(OWNER, generation), Err(CredentialStoreError::BackupExclusionFailed)), "begin refused");
    assert!(matches!(vault.rotate(), Err(CredentialStoreError::BackupExclusionFailed)), "rotation refused");
    assert!(matches!(vault.store_supplied_entry(entry("access-0002", "refresh-0002"), ReplacePolicy::ReplaceAny), Err(CredentialStoreError::BackupExclusionFailed)), "entry refused");
    drop(vault);
    let (vault, calls) = private_directory::with_sync_faults(true, || open_exclusion(root.path(), true));
    assert!(calls.contains(&"regular"), "CT18.keyring proof runs despite failed exclusion");
    assert!(vault.lock().expect("state").read_only, "CT18.failed proof makes read only");
    unchanged(&vault, generation);
}

#[test]
fn ct064_persist_before_use_seeded() {
    let (root, input) = seeded(); let vault = open(root.path());
    let receipt = vault.begin_refresh(OWNER, input).expect("begin");
    let output = Uuid::new_v4();
    vault.lock().expect("state").connection.set_test_barrier_failure_at(Some(2));
    assert!(matches!(vault.persist_refreshed(OWNER, receipt, input, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000), Err(CredentialStoreError::DurabilityUnavailable)), "completion barrier");
    { let mut state = vault.lock().expect("state"); assert!(state.commits_proven < state.commits_issued, "commit unproven"); state.connection.set_test_barrier_failure(true); }
    assert!(matches!(vault.load(OWNER), Err(CredentialStoreError::DurabilityUnavailable)), "no unproven handover");
    vault.lock().expect("state").connection.set_test_barrier_failure(false);
    assert!(vault.stored_generation(OWNER).expect("proved generation") == Some(output), "read proves commit");
}

#[test]
fn ct065_persistence_retry() {
    let (root, input) = seeded(); let vault = open(root.path()); let receipt = vault.begin_refresh(OWNER, input).expect("begin"); let output = Uuid::new_v4();
    let persist = || vault.persist_refreshed(OWNER, receipt, input, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000);
    assert!(matches!(persist(), Ok(RefreshCompletion::Completed)), "complete");
    let issued = vault.lock().expect("state").commits_issued;
    assert!(matches!(persist(), Ok(RefreshCompletion::AlreadyComplete)), "idempotent");
    assert!(vault.lock().expect("state").commits_issued == issued, "no retry reservation");
}

#[test]
fn ct067_refresh_restart_non_reuse() {
    let (root, input) = seeded(); let vault = open(root.path()); vault.begin_refresh(OWNER, input).expect("begin"); drop(vault);
    let vault = open(root.path());
    assert!(vault.has_unresolved_refresh(OWNER).expect("unresolved"), "restart fence");
    assert!(vault.begin_refresh(OWNER, input).is_err(), "no resend");
    assert!(matches!(vault.store_supplied_entry(entry("different-access", "refresh-0001"), ReplacePolicy::ReplaceAny), Err(CredentialStoreError::RefreshTokenSpent)), "no entry reuse");
}

#[test]
fn ct070_rotation_during_refresh() {
    let (root, input) = seeded(); let vault = open(root.path()); let receipt = vault.begin_refresh(OWNER, input).expect("begin");
    let seam = Arc::new(VaultLockSeam::default()); *vault.lock_seam.lock().unwrap() = Some(Arc::clone(&seam));
    let rotation = { let vault = Arc::clone(&vault); std::thread::spawn(move || vault.rotate().expect("rotation")) };
    let rotation_id = rotation.thread().id(); seam.wait_for(Some(rotation_id), "rotation_step1");
    let output = Uuid::new_v4();
    let completion = { let vault = Arc::clone(&vault); std::thread::spawn(move || vault.persist_refreshed(OWNER, receipt, input, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000).expect("completion")) };
    let completion_id = completion.thread().id(); seam.wait_for(Some(completion_id), "request");
    assert!(!seam.events.lock().unwrap().iter().any(|(id, event)| *id == completion_id && *event == "acquire"), "completion waits inside rotation");
    seam.release_rotation(); rotation.join().expect("rotation thread"); completion.join().expect("completion thread");
    *vault.lock_seam.lock().unwrap() = None;
    let events = seam.events.lock().unwrap();
    let index = |id, name| events.iter().position(|(thread, event)| *thread == id && *event == name).expect("ordered event");
    assert!(index(rotation_id, "acquire") < index(completion_id, "request") && index(completion_id, "request") < index(rotation_id, "release") && index(rotation_id, "release") < index(completion_id, "acquire"), "rotation lock order");
    assert!(index(rotation_id, "rotation_finished") < index(completion_id, "acquire"), "CT70 completion cannot acquire between rotation steps");
    drop(events);
    assert!(pair(&vault).generation == output, "successor after rotation");
    let keys = vault.keys.read().expect("keys"); let keys = keys.as_ref().expect("keyring");
    assert!(keys.keyring().previous.is_none(), "rotation finished");
    let row = vault.lock().expect("state").connection.load_credential_pair(OWNER).expect("sealed row").expect("pair");
    let header = parse_header(row.access_envelope[..ENVELOPE_HEADER_BYTES].try_into().expect("header")).expect("parsed header");
    assert!(header.key_id == keys.keyring().current.id, "successor uses current key");
}

#[test]
fn ct071_both_slots() {
    // M02-T04: one origin refreshes at a time, so the Fleet refresh cannot begin
    // while the Owner one is unresolved; M02-T02: the Fleet pair is never loaded.
    let (root, owner_input) = seeded();
    let fleet = CredentialEntry::fleet_supplied(Zeroizing::new("fleet-access-0001".into()), Zeroizing::new("fleet-refresh-0001".into()), "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28800).expect("Fleet entry");
    let fleet_input = seed_entry(root.path(), fleet);
    let vault = open(root.path());
    let owner_receipt = vault.begin_refresh(OWNER, owner_input).expect("Owner begin");
    let refused = vault.begin_refresh(CredentialSlot::FleetApi, fleet_input).expect_err("Fleet begin refused while Owner is unresolved");
    assert!(matches!(refused, CredentialStoreError::Store(StoreError::WriteRefused("refresh_fence_other_origin"))), "{refused:?}");
    let output = Uuid::new_v4();
    vault.persist_refreshed(OWNER, owner_receipt, owner_input, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000).expect("persisted");
    assert!(pair(&vault).generation == output, "Owner successor readable");
    assert!(matches!(vault.load(CredentialSlot::FleetApi), Err(CredentialStoreError::SourceCredentialDisabled(CredentialSlot::FleetApi))), "Fleet pair refused at load");
    let state = vault.lock().expect("state");
    let counts: (i64, i64) = state.connection.test_connection().query_row("SELECT (SELECT count(*) FROM credential_refresh_fences), (SELECT count(*) FROM outbound_request_receipts WHERE outcome='success')", [], |row| Ok((row.get(0)?, row.get(1)?))).expect("completion evidence");
    assert!(counts == (1, 1), "one spent fence and one success");
}

#[test]
fn ct099_guarded_read_interleavings() {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("CT99 manager runtime")
        .block_on(crate::credentials::assert_vault_read_interleaving_contract());
    for point in ["after_check", "after_snapshot", "after_barrier"] {
        assert_read_interleaving(point, OWNER, false);
    }
    // M02-T02: a Fleet pair is refused at load before any guarded read, so only
    // Owner reads interleave with a completion.
    assert_read_interleaving("after_check", OWNER, true);
    let (root, input) = seeded(); let vault = open(root.path()); let receipt = vault.begin_refresh(OWNER, input).expect("begin"); let output = Uuid::new_v4();
    vault.lock().expect("state").connection.set_test_barrier_failure_at(Some(2));
    assert!(vault.persist_refreshed(OWNER, receipt, input, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000).is_err(), "unproven completion");
    vault.lock().expect("state").connection.set_test_barrier_failure(true);
    assert!(vault.stored_generation(OWNER).is_err(), "generation fenced");
    assert!(vault.is_completed_output(OWNER, output).is_err(), "output fenced");
    assert!(vault.is_completed_output_for_receipt(OWNER, receipt, input, output).is_err(), "receipt fenced");
    assert!(vault.load(OWNER).is_err(), "pair fenced");
    let present = vault.credential_pair_presence(OWNER).expect("presence"); assert!(present.is_some(), "presence unguarded");
    vault.lock().expect("state").connection.set_test_barrier_failure(false);
    assert!(vault.is_completed_output_for_receipt(OWNER, receipt, input, output).expect("proved"), "receipt after proof");
    assert!(pair(&vault).generation == output, "pair after proof");
}

#[test]
fn ct096_c5_retry_barrier() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
        .block_on(crate::credentials::assert_vault_retry_barrier_contract());
    assert_retry_restart_probe();
}

#[test]
fn ct103_durability_recovery() {
    let _controls=controls();
    {
        let (root, _) = seeded(); let store = HubStore::initialize(root.path()).expect("probe fixture store");
        let mut connection = store.take_credential_connection().expect("probe fixture connection");
        connection.set_test_barrier_failure(true);
        assert!(connection.probe_credential_durability().is_err(), "CT103 fixture unavailable");
        connection.set_test_barrier_failure(false); let mut cut_points = 0;
        connection.probe_credential_durability_with_cut_point(&mut || {
            cut_points += 1;
            assert!(status(root.path()).durability == Some(crate::db::CredentialDurabilityState::Unavailable), "CT103 cut point precedes durability proof");
        }).expect("CT103 cut point probe");
        assert!(cut_points == 1 && status(root.path()).durability == Some(crate::db::CredentialDurabilityState::Ok), "CT103 single cut point and recovered diagnostic");
    }
    let (root, generation) = seeded();
    let options = VaultOptions { exclusion: Some(Arc::new(FixtureExclusion)), now_ms: Some(1_800_000_000_000), probe_failure: true };
    let vault = CredentialVault::open(HubStore::initialize(root.path()).expect("store"), AdmittedUserHub::for_test(root.path()).expect("admission"), root.path(), options).expect("read-only open");
    unchanged(&vault, generation);
    assert!(matches!(vault.begin_refresh(OWNER, generation), Err(CredentialStoreError::DurabilityUnavailable)), "read-only begin");
    assert!(matches!(vault.clear_credential_pair(OWNER), Err(CredentialStoreError::DurabilityUnavailable)), "read-only clear");
    assert!(status(root.path()).durability==Some(crate::db::CredentialDurabilityState::Unavailable),"CT103.unavailable_status_source");
    drop(vault);
    let options = VaultOptions { exclusion: Some(Arc::new(FixtureExclusion)), now_ms: Some(1_800_000_000_000), probe_failure: true };
    drop(CredentialVault::open(HubStore::initialize(root.path()).expect("store"),AdmittedUserHub::for_test(root.path()).expect("admission"),root.path(),options).expect("repeated read-only open"));
    PROBE_CONTROL.with(|s|*s.borrow_mut()=Some(Box::new(|connection|connection.set_test_barrier_failure_at(Some(2)))));
    let vault=open(root.path());
    assert!(vault.lock().expect("state").read_only && status(root.path()).durability==Some(crate::db::CredentialDurabilityState::Unavailable),"CT103.failed ok barrier preserves unavailable");
    drop(vault);
    PROBE_CONTROL.with(|s|*s.borrow_mut()=Some(Box::new(|connection|{connection.test_connection().execute_batch("CREATE TEMP TRIGGER refuse_probe_ok BEFORE UPDATE OF durability ON credential_format_state WHEN NEW.durability='ok' BEGIN SELECT RAISE(ABORT,'probe record refused'); END;").expect("probe record fault");})));
    let vault=open(root.path());
    let refused={let state=vault.lock().expect("state");state.read_only && state.resume_error.is_some()};
    assert!(refused && status(root.path()).durability==Some(crate::db::CredentialDurabilityState::Unavailable),"CT103.failed ok record preserves store error");
    drop(vault); PROBE_CONTROL.with(|s|*s.borrow_mut()=None);
    PROBE_CONTROL.with(|slot| *slot.borrow_mut() = Some(Box::new(|connection| {
        connection.test_connection().commit_hook(Some(|| true)).expect("install native COMMIT refusal");
    })));
    let vault = open(root.path());
    let refused = { let state = vault.lock().expect("state"); state.read_only && state.resume_error.is_some() };
    assert!(refused
        && status(root.path()).durability == Some(crate::db::CredentialDurabilityState::Unavailable), "CT103 native COMMIT refusal preserves unavailable");
    vault.lock().expect("state").connection.test_connection().commit_hook(None::<fn() -> bool>).expect("remove native COMMIT refusal");
    drop(vault); PROBE_CONTROL.with(|slot| *slot.borrow_mut() = None);
    let observed=Arc::new(Mutex::new(Vec::new()));let events=Arc::clone(&observed);let data=root.path().to_owned();
    fixtures::probe_observer(Some(Box::new(move |point|{
        assert!(status(&data).durability==Some(crate::db::CredentialDurabilityState::Unavailable),"CT103.probe holds retain unavailable_status_source");
        events.lock().expect("probe log").push(point.to_owned());
    })));
    let vault = open(root.path());fixtures::probe_observer(None);
    assert!(observed.lock().expect("probe log").as_slice()==["after_probe_checkpoint","after_probe_barrier"],"CT103.probe boundary order");
    let trace=vault.lock().expect("state").connection.test_events();
    let position=|name|trace.iter().position(|event|*event==name).expect("probe event");
    assert!(position("probe_barrier")<position("probe_ok_commit") && position("probe_ok_commit")<position("probe_ok_barrier"),"CT103.probe ok commit and barrier order");
    assert!(status(root.path()).durability==Some(crate::db::CredentialDurabilityState::Ok),"CT103.recovered_status_source");
    vault.begin_refresh(OWNER, generation).expect("recovered begin");
    drop(vault);
    assert!(status(root.path()).durability==Some(crate::db::CredentialDurabilityState::Ok),"CT103.persisted before next open_status_source");
    let vault=open(root.path());assert!(!vault.lock().expect("state").connection.test_events().contains(&"probe_ok_commit"),"CT103.already proven no ok rewrite");
}

#[test]
fn ct088_barriers_open_refresh_rotation_conversion() {
    let (root, input) = seeded();
    let options = VaultOptions { exclusion: Some(Arc::new(FixtureExclusion)), now_ms: Some(1_800_000_000_000), probe_failure: true };
    let vault = CredentialVault::open(HubStore::initialize(root.path()).expect("store"), AdmittedUserHub::for_test(root.path()).expect("admission"), root.path(), options).expect("read-only open");
    let counts = vault.test_commit_counts(); unchanged(&vault, input);
    assert!(matches!(vault.store_supplied_entry(entry("access-0002", "refresh-0002"), ReplacePolicy::ReplaceAny), Err(CredentialStoreError::DurabilityUnavailable)), "entry blocked after failed probe");
    assert!(matches!(vault.begin_refresh(OWNER, input), Err(CredentialStoreError::DurabilityUnavailable)), "begin blocked after failed probe");
    assert!(vault.test_commit_counts() == counts, "read-only issues no counted commit");
    drop(vault);
    let vault = open(root.path());
    vault.lock().expect("state").connection.set_test_barrier_failure_at(Some(1));
    assert!(matches!(vault.begin_refresh(OWNER, input), Err(CredentialStoreError::DurabilityUnavailable)), "begin barrier failure");
    let mut state = vault.lock().expect("state");
    let evidence: (String, i64) = state.connection.test_connection().query_row("SELECT (SELECT outcome FROM outbound_request_receipts WHERE operation='token_refresh' ORDER BY id DESC LIMIT 1), (SELECT count(*) FROM credential_refresh_fences)", [], |row| Ok((row.get(0)?, row.get(1)?))).expect("begin cleanup evidence");
    assert!(evidence.0 == "cancelled" && evidence.1 == 0, "unsent begin cancelled and released");
    assert!(state.commits_issued == 2 && state.commits_proven < state.commits_issued, "cancel commit awaits proof");
    state.connection.set_test_barrier_failure_at(None); drop(state);
    let receipt = vault.begin_refresh(OWNER, input).expect("new begin after proof"); let output = Uuid::new_v4();
    vault.lock().expect("state").connection.set_test_barrier_failure_at(Some(2));
    assert!(matches!(vault.persist_refreshed(OWNER, receipt, input, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000), Err(CredentialStoreError::DurabilityUnavailable)), "complete unproved");
    let counts = vault.test_commit_counts();
    vault.lock().expect("state").connection.set_test_barrier_failure_at(None);
    assert!(matches!(vault.persist_refreshed(OWNER, receipt, input, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000), Ok(RefreshCompletion::AlreadyComplete)), "retry proves complete");
    assert!(vault.test_commit_counts().0 == counts.0, "retry reserves and commits nothing");
    let fault = credential_keys::with_key_faults(credential_keys::KeyFaults { file_barrier_fail: true, ..Default::default() }, || vault.rotate());
    assert!(fault.is_err(), "temporary key file barrier failure");
    assert!(vault.lock().expect("state").read_only, "failed key proof forbids mutation");
    drop(vault);
    // M02-T02: no pre-upgrade pair is converted, so the conversion barrier case is gone.
    use std::os::unix::fs::MetadataExt;
    let (root, input) = seeded(); let catalogue = fs::metadata(root.path().join("hub.sqlite")).expect("catalogue identity");
    let (_, syncs) = private_directory::with_sync_observations(|| {
        let vault = open(root.path()); let receipt = vault.begin_refresh(OWNER, input).expect("observed begin");
        vault.persist_refreshed(OWNER, receipt, input, Uuid::new_v4(), "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000).expect("observed completion");
        vault.rotate().expect("observed rotation");
    });
    let operation = if cfg!(target_os = "macos") { "F_FULLFSYNC" } else { "fsync" };
    assert!(syncs.iter().all(|(op, _, dev, ino)| *op == operation && (*dev, *ino) != (catalogue.dev(), catalogue.ino())), "CT88.native barriers never target the catalogue descriptor");
    assert!(syncs.iter().filter(|(_, regular, _, _)| *regular).count() >= 3 && syncs.iter().any(|(_, regular, _, _)| !regular), "CT88.keyring temporary file and directory descriptors observed");
}

#[test]
fn ct103_read_only_complete_cleanup() {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::io::Write;
    let (root, generation) = seeded();
    let status = HubStore::initialize(root.path()).expect("status store");
    let completed = status.credential_format_state().expect("state").expect("format").completed_at_ms;
    let names = ["pre-upgrade-owner-api.relocated.key", "pre-upgrade-fleet-api.key", ".fleet-credentials.pending.key"];
    let paths: Vec<_> = names.iter().map(|name| root.path().join("secrets").join(name)).collect();
    for path in &paths { std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).expect("leftover fixture").write_all(b"synthetic-key-residue").expect("fixture bytes"); }
    let metadata: Vec<_> = paths.iter().map(|path| std::fs::metadata(path).expect("fixture metadata")).collect();
    let options = VaultOptions { exclusion: Some(Arc::new(FixtureExclusion)), now_ms: Some(1_800_000_000_000), probe_failure: true };
    let vault = CredentialVault::open(HubStore::initialize(root.path()).expect("store"), AdmittedUserHub::for_test(root.path()).expect("admission"), root.path(), options).expect("read-only open");
    unchanged(&vault, generation);
    for (path, before) in paths.iter().zip(&metadata) {
        let after = std::fs::metadata(path).expect("retained metadata");
        assert!(before.ino() == after.ino() && before.mtime() == after.mtime() && std::fs::read(path).expect("retained bytes") == b"synthetic-key-residue", "CT103.no_cleanup");
    }
    assert!(status.credential_format_state().expect("read-only status").expect("format").completed_at_ms == completed, "CT103.complete_status_source");
    drop(vault);
    let vault = open(root.path()); unchanged(&vault, generation);
    assert!(paths.iter().all(|path| !path.exists()), "CT103.cleanup_after_probe");
    assert!(status.credential_format_state().expect("completed status").expect("format").completed_at_ms == completed, "CT103.completion_unchanged");
}

#[test]
fn ct069_scrub_empty_secret() {
    let access = "provider-access-0001";
    let secrets = crate::owner_api::ProviderSecrets::new(&[access, ""]);
    let mut fields = serde_json::Map::new();
    fields.insert("empty".into(), serde_json::Value::String(String::new()));
    fields.insert("echo".into(), serde_json::Value::String(access.into()));
    let paths = secrets.scrub_fields(&mut fields);
    assert!(paths.len() == 1 && paths[0] == "echo", "scrub list");
    assert!(fields["empty"].as_str() == Some(""), "empty field retained");
    assert!(fields["echo"].is_null(), "access echo removed");
    assert!(!secrets.reflects(""), "empty secret ignored");
}

#[test]
fn ct095_wire_secret_drops() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
        .block_on(crate::fleet_api::assert_fleet_wire_contract());
}

fn store_status(data: &Path) -> crate::auth::credential_sign_in::CredentialStatus {
    crate::auth::credential_sign_in::credential_status(&HubStore::initialize(data).expect("status store"), data, 1_800_000_000_000).expect("status projection")
}

#[test]
fn m01_t15_keyring_exposure_survives_restart_until_rotation() {
    use std::os::unix::fs::PermissionsExt;
    let (root, generation) = seeded();
    let path = root.path().join("secrets").join("credential-keys");
    let mode = |path: &Path| std::fs::metadata(path).expect("keyring metadata").permissions().mode() & 0o777;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("exposed keyring fixture");
    assert!(store_status(root.path()).credential_store.exposure_suspected && !status(root.path()).keyring_exposure, "live check before any repair");
    let vault = open(root.path()); unchanged(&vault, generation);
    assert!(mode(&path) == 0o600, "admitted open makes the keyring private");
    assert!(status(root.path()).keyring_exposure && store_status(root.path()).credential_store.exposure_suspected, "repaired exposure recorded");
    drop(vault);
    let vault = open(root.path()); unchanged(&vault, generation);
    assert!(store_status(root.path()).credential_store.exposure_suspected, "warning survives a restart");
    vault.rotate().expect("rotation");
    assert!(pair(&vault).generation == generation && mode(&path) == 0o600, "rotation keeps the pair");
    assert!(!status(root.path()).keyring_exposure && !store_status(root.path()).credential_store.exposure_suspected, "rotation clears the warning");
    drop(vault);
    drop(open(root.path()));
    assert!(!store_status(root.path()).credential_store.exposure_suspected, "cleared after the next restart");
}

#[test]
fn m01_t15_resumed_rotation_keeps_exposure() {
    use std::os::unix::fs::PermissionsExt;
    let (root, generation) = seeded();
    prepare_rotation(root.path());
    let path = root.path().join("secrets").join("credential-keys");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("exposed rotating keyring fixture");
    let vault = open(root.path()); unchanged(&vault, generation);
    assert!(vault.keys.read().expect("keys").as_ref().expect("keyring").keyring().previous.is_none(), "rotation resumed at open");
    assert!(status(root.path()).keyring_exposure && store_status(root.path()).credential_store.exposure_suspected, "a resumed rotation's key was in the exposed file");
    vault.rotate().expect("fresh rotation");
    assert!(!status(root.path()).keyring_exposure && !store_status(root.path()).credential_store.exposure_suspected, "a rotation begun after the finding clears it");
}

#[test]
fn m01_t15_keyring_barrier_failure_records_cause() {
    let (root, generation) = seeded();
    let (vault, calls) = private_directory::with_sync_faults(true, || open(root.path()));
    assert!(calls.contains(&"regular"), "keyring barrier reached");
    {
        let state = vault.lock().expect("state");
        assert!(state.read_only && matches!(state.resume_error, Some(CredentialStoreError::DurabilityUnavailable)), "cause recorded at open");
    }
    let projection = store_status(root.path());
    assert!(projection.credential_store.durability == Some("unavailable"), "status reports unavailable durability");
    assert!(crate::auth::credential_sign_in::credential_store_unhealthy(&projection, true, false), "readiness reports the credential store error");
    assert!(matches!(vault.begin_refresh(OWNER, generation), Err(CredentialStoreError::DurabilityUnavailable)), "read-only refusal");
    drop(vault);
    let vault = open(root.path());
    assert!(!vault.lock().expect("state").read_only && store_status(root.path()).credential_store.durability == Some("ok"), "next proven open recovers");
}

#[test]
fn m01_t15_keyring_exposure_column_migrates_in_place() {
    let root = crate::private_tempdir().expect("root");
    drop(HubStore::initialize(root.path()).expect("v79 store"));
    {
        let raw = HubStore::initialize(root.path()).expect("store").open().expect("raw connection");
        raw.execute_batch("ALTER TABLE credential_format_state DROP COLUMN keyring_exposure").expect("pre-column v79 fixture");
        let present: bool = raw.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('credential_format_state') WHERE name='keyring_exposure')", [], |row| row.get(0)).expect("column check");
        assert!(!present, "fixture lacks the column");
    }
    let store = HubStore::initialize(root.path()).expect("migrating open");
    assert!(!store.credential_format_state().expect("format").expect("state").keyring_exposure, "column added with no exposure");
    let raw = store.open().expect("raw connection");
    assert!(raw.execute("UPDATE credential_format_state SET keyring_exposure=2", []).is_err(), "column keeps its check");
}

/// Finding 1 (M02-T02 round 3): the raw-token Fleet bridge refuses before the
/// vault stores anything, and a stored Fleet pair is refused at load. A
/// supplied row and a row migrated by an older Hub carry the same header
/// (`fleet_oauth`, Hub-refreshed), so both are refused until an authenticated
/// own-exchange origin exists.
#[test]
fn source_credential_disabled_fleet_bridge_and_stored_fleet_rows_are_refused() {
    let root = crate::private_tempdir().expect("root");
    let input = crate::fleet_credentials::FleetSetupCredentials::new("fleet-access-0001".into(), "fleet-refresh-0001".into(),
        "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).expect("Fleet input");
    let scope = crate::auth::credential_sign_in::SignInScope::open_for_test(root.path(), true).expect("scope");
    let refused = crate::fleet_credentials::persist_fleet_setup_credentials(scope.entry(), root.path(), &input, std::time::SystemTime::now())
        .expect_err("raw Fleet tokens are refused");
    assert!(matches!(refused, crate::fleet_credentials::FleetCredentialError::Store(StoreError::WriteRefused("source_credential_disabled"))), "{refused:?}");
    assert!(scope.entry().vault().stored_generation(CredentialSlot::FleetApi).expect("generation").is_none(), "the bridge stored nothing");
    scope.release().expect("release");
    for owner in [RefreshOwner::Hub, RefreshOwner::External] {
        assert!(matches!(admit_stored_origin(CredentialSlot::FleetApi, Some(CredentialOrigin::FleetOauth), owner),
            Err(CredentialStoreError::SourceCredentialDisabled(CredentialSlot::FleetApi))), "supplied or migrated Fleet header refused");
    }
    seed_entry(root.path(), CredentialEntry::fleet_supplied(Zeroizing::new("fleet-access-0002".into()), Zeroizing::new("fleet-refresh-0002".into()),
        "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).expect("Fleet fixture"));
    let vault = open(root.path());
    assert!(matches!(vault.load(CredentialSlot::FleetApi), Err(CredentialStoreError::SourceCredentialDisabled(CredentialSlot::FleetApi))), "stored Fleet pair refused at load");
    drop(vault);
    let store = HubStore::initialize(root.path()).expect("store");
    let status = crate::auth::credential_sign_in::credential_status(&store, root.path(), 1_800_000_000_000).expect("status");
    assert_eq!(status.fleet_credentials.store_state, "source_credential_disabled", "status reports the refusal");
}

/// Finding 2 (M02-T02 round 3): pair seeding writes a pair without an exchange,
/// so the own-login fixture constructor, its seeding store path and the
/// exported seeding helpers are compiled only into test and debug builds,
/// never into a release build, with or without `interop-fixture`.
#[test]
fn source_credential_disabled_fixture_seeding_is_absent_from_release_builds() {
    fn gate(source: &str, item: &str) -> String {
        let at = source.find(item).unwrap_or_else(|| panic!("{item} present"));
        let line_start = source[..at].rfind('\n').unwrap_or(0);
        let mut gates = Vec::new();
        for line in source[..line_start].lines().rev() {
            let line = line.trim();
            if line.starts_with("#[cfg(") { gates.push(line.to_owned()); }
            else if !(line.starts_with("#[") || line.starts_with("///") || line.starts_with("//")) { break; }
        }
        assert_eq!(gates.len(), 1, "{item} has exactly one cfg gate: {gates:?}");
        gates.remove(0)
    }
    let allowed = ["#[cfg(any(test, debug_assertions))]", "#[cfg(debug_assertions)]"];
    for (source, items) in [
        (include_str!("types.rs"), &["fn own_login_fixture", "fn own_login_grant_secret", "fn own_login_binding", "fn fleet_supplied"][..]),
        (include_str!("../credential_vault.rs"), &["fn store_supplied_entry"][..]),
        (include_str!("../credential_sign_in/manager_fixture.rs"), &["fn seed_owner_pair_for_test", "fn seed_fleet_pair_for_test"][..]),
        (include_str!("../credential_sign_in.rs"), &["pub use manager_fixture::{seed_owner_pair_for_test"][..]),
    ] {
        for item in items {
            let gate = gate(source, item);
            assert!(allowed.contains(&gate.as_str()), "{item} is gated to test or debug builds only: {gate}");
        }
    }
}

/// Finding 3 (M02-T02 round 3): opening the vault over a pre-upgrade store
/// refuses each pending slot before any candidate key file is inspected or
/// read, before any decryption and before anything is sealed or stored, even
/// with every key that would open the old pair present.
#[test]
fn source_credential_disabled_vault_open_refuses_pre_upgrade_pairs_unread() {
    for fleet in [false, true] {
        let root = admitted_migration_fixture(fleet, None, false, 0);
        credential_migration::seam::reset();
        credential_migration::seam::with(|state| { state.fake_pre_upgrade = true; state.fake_generation = true; });
        let vault = open(root.path());
        let (events, decrypted) = credential_migration::seam::with(|state| (state.events.clone(), state.decrypt_log.len()));
        credential_migration::seam::reset();
        let slot = if fleet { CredentialSlot::FleetApi } else { OWNER };
        assert_eq!(decrypted, 0, "no pre-upgrade pair was decrypted");
        assert!(!events.iter().any(|event| event.starts_with("read:") || event.starts_with("inspect:")), "no candidate key inspected or read: {events:?}");
        assert!(events.iter().any(|event| *event == format!("refused:{}", slot.as_str())), "slot refused: {events:?}");
        assert!(matches!(vault.load(slot), Err(CredentialStoreError::SourceCredentialDisabled(refused)) if refused == slot), "refused at load");
        assert!(vault.credential_pair_presence(slot).expect("presence").is_none(), "nothing was sealed or stored");
    }
}

/// Finding 4 (M02-T02 round 3): key rotation never decrypts or re-seals a row
/// the load refuses; it carries the row unchanged and re-seals only the
/// admitted own-login pair.
#[test]
fn source_credential_disabled_rotation_never_opens_refused_rows() {
    let (root, owner) = seeded();
    seed_entry(root.path(), CredentialEntry::fleet_supplied(Zeroizing::new("fleet-access-0001".into()), Zeroizing::new("fleet-refresh-0001".into()),
        "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).expect("Fleet fixture"));
    let vault = open(root.path());
    let raw = |slot: CredentialSlot| {
        let mut state = vault.lock().expect("state");
        let row = state.connection.load_credential_pair(slot).expect("row").expect("present");
        (row.access_envelope.clone(), row.refresh_envelope.clone(), row.generation)
    };
    let (fleet_before, owner_before) = (raw(CredentialSlot::FleetApi), raw(OWNER));
    vault.rotate().expect("rotation");
    assert!(raw(CredentialSlot::FleetApi) == fleet_before, "refused Fleet row carried unopened");
    assert!(raw(OWNER).0 != owner_before.0, "admitted Owner pair re-sealed under the new key");
    unchanged(&vault, owner);
}

// ---------------------------------------------------------------- M02-T11
// The refused-credential purge (DEC-083): names carry `refused_credential_purge`.

const FLEET_KEY_FILE: &str = "fleet-credentials.key";

/// An upgraded store whose pre-upgrade Owner row the Hub's own login has
/// replaced, while its pre-upgrade Fleet row stays refused, with the old Fleet
/// key file and the old tables still on disk. Returns the own-login generation.
fn refused_purge_blocked_fixture() -> (tempfile::TempDir, Uuid) {
    credential_migration::seam::reset();
    let root = credential_migration::g24_m2_m::seed_admitted_blocked_owner();
    let vault = open(root.path());
    let session = vault.begin_sign_in(OWNER).expect("own sign-in session");
    let grant = RedeemedGrant::from_validated_redemption(session, Zeroizing::new("access-0003".into()),
        Zeroizing::new("refresh-0003".into()), 28_800, None, None).expect("validated redemption");
    let stored = vault.store_hub_grant(grant).expect("own-login pair");
    let generation = stored.generation();
    vault.finish_hub_grant_aftercare(stored).expect("aftercare");
    (root, generation)
}

/// A current store holding the Hub's own login and a stored Fleet pair.
fn refused_purge_stored_fleet_fixture() -> (tempfile::TempDir, Uuid) {
    let root = crate::private_tempdir().expect("root");
    let owner = seed_entry(root.path(), CredentialEntry::own_login_fixture(Zeroizing::new("access-0001".into()), Zeroizing::new("refresh-0001".into())).expect("own login"));
    seed_entry(root.path(), CredentialEntry::fleet_supplied(Zeroizing::new("fleet-access-0002".into()), Zeroizing::new("fleet-refresh-0002".into()),
        "client-0001".into(), crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica, 28_800).expect("Fleet fixture"));
    (root, owner)
}

fn refused_purge_contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}

fn refused_purge_table_present(data: &Path, table: &str) -> bool {
    HubStore::initialize(data).expect("store").open().expect("raw").query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)", [table], |row| row.get(0)).expect("table query")
}

/// Acceptance 1: the listing names the refused slot, its origin, the old key
/// file and the old tables, never the own-login pair or a secret value, and
/// writes nothing.
#[test]
fn refused_credential_purge_listing_names_what_it_removes_and_deletes_nothing() {
    let (root, _) = refused_purge_blocked_fixture();
    let bytes = |name: &str| std::fs::read(root.path().join(name)).ok();
    let store = HubStore::open_read_only(root.path()).expect("read-only store");
    let before = (bytes("hub.sqlite"), bytes("hub.sqlite-wal"), format!("{:?}", store.credential_format_state().expect("format")));
    let report = refused_credential_listing(&store, root.path()).expect("listing");
    assert_eq!(report.mode, "list");
    assert_eq!(report.refused_slots, vec![RefusedSlotView { slot: "fleet_api", origin: "pre_upgrade", table: "pre_upgrade_table", refusal: "source_credential_disabled" }],
        "only the refused Fleet slot; the admitted own-login pair is not listed");
    assert!(report.old_key_files.contains(&FLEET_KEY_FILE), "{:?}", report.old_key_files);
    assert!(report.old_tables.contains(&"fleet_tokens") && report.old_tables.contains(&"legacy_owner_tokens"), "{:?}", report.old_tables);
    assert_eq!(report.confirm_command.as_deref(), Some("teslatlas-hub purge-refused-credentials --confirm"));
    let text = serde_json::to_string(&report).expect("json");
    for secret in ["fleet-access-0001", "fleet-refresh-0001", "access-0003", "refresh-0003", "access-0001"] {
        assert!(!text.contains(secret), "no secret value in the listing: {text}");
    }
    let after = (bytes("hub.sqlite"), bytes("hub.sqlite-wal"), format!("{:?}", store.credential_format_state().expect("format")));
    assert!(before == after, "the listing wrote nothing");
    assert!(root.path().join("secrets").join(FLEET_KEY_FILE).exists(), "the listing removed no key file");
    assert!(refused_purge_table_present(root.path(), "fleet_tokens"), "the listing dropped no table");
}

/// Acceptance 1 and 2: the purge removes the refused pre-upgrade row, the old
/// tables and the old key file and keeps the own-login pair; afterwards the
/// store reports no refused slot, cleanup completes and backups are clean.
#[test]
fn refused_credential_purge_removes_refused_rows_tables_and_key_files_and_keeps_the_own_login_pair() {
    let (root, generation) = refused_purge_blocked_fixture();
    let vault = open(root.path());
    let report = vault.purge_refused_credentials().expect("purge");
    assert_eq!(report.mode, "purge");
    assert_eq!(report.refused_slots.iter().map(|slot| (slot.slot, slot.origin)).collect::<Vec<_>>(), vec![("fleet_api", "pre_upgrade")]);
    assert!(report.old_key_files.contains(&FLEET_KEY_FILE), "{report:?}");
    assert!(report.old_tables.contains(&"fleet_tokens"), "{report:?}");
    assert_eq!(report.old_key_files_remaining, Some(Vec::new()));
    assert_eq!(report.cleanup_complete, Some(true));
    let owner = pair(&vault);
    assert!(owner.generation == generation && owner.origin == Some(CredentialOrigin::HubLogin) && owner.access.as_str() == "access-0003",
        "the admitted own-login pair is untouched");
    drop(vault);
    assert!(!root.path().join("secrets").join(FLEET_KEY_FILE).exists(), "the old key file is removed");
    for table in ["fleet_tokens", "legacy_owner_tokens", "fleet_refresh_receipt_bindings", "legacy_refresh_input_fences"] {
        assert!(!refused_purge_table_present(root.path(), table), "{table} dropped");
    }
    let store = HubStore::initialize(root.path()).expect("store");
    let state = store.credential_format_state().expect("format").expect("state");
    assert!(state.fleet_api_state == CredentialSlotFormatState::Discarded && state.fleet_api_blocked.is_none()
        && state.old_tables_dropped && state.completed_at_ms.is_some() && !state.purge_required && state.wal_scrubbed && state.old_files_removed,
        "cleanup completes: {state:?}");
    let status = crate::auth::credential_sign_in::credential_status(&store, root.path(), 1_800_000_000_000).expect("status");
    assert_eq!(status.credential_store.format_state, "complete");
    assert_eq!((status.legacy_credentials.store_state, status.fleet_credentials.store_state), ("ok", "ok"), "no refused slot");
    assert!(status.credential_store.blocked_reason.owner_api.is_none() && status.credential_store.blocked_reason.fleet_api.is_none());
    assert_eq!(store.begin_backup_snapshot().expect("snapshot").credential_backup_admission(), crate::db::CredentialBackupAdmission::Clean,
        "backups are no longer blocked");
    assert!(store.refused_credential_inventory().expect("inventory").rows.is_empty());
    drop(store);
    let vault = open(root.path());
    let again = vault.purge_refused_credentials().expect("second purge");
    assert!(again.refused_slots.is_empty() && again.cleanup_complete.is_none(), "a second purge removes nothing: {again:?}");
    assert_eq!(pair(&vault).generation, generation);
}

/// Acceptance 1: a stored Fleet pair is refused, so the purge removes it with
/// its ciphertext; the own-login pair stays.
#[test]
fn refused_credential_purge_removes_a_stored_fleet_pair_and_its_ciphertext() {
    let (root, owner) = refused_purge_stored_fleet_fixture();
    let vault = open(root.path());
    let envelopes = {
        let mut state = vault.lock().expect("state");
        let row = state.connection.load_credential_pair(CredentialSlot::FleetApi).expect("row").expect("present");
        (row.access_envelope.clone(), row.refresh_envelope.clone())
    };
    let report = vault.purge_refused_credentials().expect("purge");
    assert_eq!(report.refused_slots, vec![RefusedSlotView { slot: "fleet_api", origin: "fleet_oauth", table: "credential_pairs", refusal: "source_credential_disabled" }]);
    assert!(report.old_tables.is_empty(), "a current store has no old tables");
    assert_eq!(report.cleanup_complete, Some(true));
    assert!(vault.credential_pair_presence(CredentialSlot::FleetApi).expect("presence").is_none(), "the Fleet pair is gone");
    assert_eq!(pair(&vault).generation, owner, "the own-login pair stays");
    drop(vault);
    for name in ["hub.sqlite", "hub.sqlite-wal"] {
        if let Ok(bytes) = std::fs::read(root.path().join(name)) {
            assert!(!refused_purge_contains(&bytes, &envelopes.0) && !refused_purge_contains(&bytes, &envelopes.1), "{name} keeps no Fleet ciphertext");
        }
    }
}

/// Acceptance 1: rows, ciphertext and old tables go in one counted commit,
/// and no key file is removed until that commit is durable.
#[test]
fn refused_credential_purge_commits_once_and_removes_key_files_after_the_commit() {
    let (root, _) = refused_purge_blocked_fixture();
    let vault = open(root.path());
    let key = root.path().join("secrets").join(FLEET_KEY_FILE);
    let mut commits = 0;
    {
        let mut state = vault.lock().expect("state");
        let removed = state.connection.purge_refused_credentials_with_commit(&mut || {
            commits += 1;
            assert!(key.exists(), "no key file is removed before the commit");
        }).expect("purge commit");
        assert_eq!(removed.rows.len(), 1);
    }
    assert_eq!(commits, 1, "the rows and the old tables go in one transaction");
    assert!(!refused_purge_table_present(root.path(), "fleet_tokens"), "the commit dropped the old tables");
    assert!(key.exists(), "the key file outlives the store commit");
    vault.finish_clear_aftercare().expect("Phase B after the commit");
    assert!(!key.exists(), "the key file is removed after the commit");
}

/// Acceptance 1: the purge is refused while a refresh of the own-login pair
/// is unresolved or a collector holds its lease, and removes nothing then.
#[test]
fn refused_credential_purge_refuses_while_a_refresh_or_a_collector_is_active() {
    let (root, owner) = refused_purge_stored_fleet_fixture();
    let vault = open(root.path());
    let present = |vault: &CredentialVault| vault.credential_pair_presence(CredentialSlot::FleetApi).expect("presence").is_some();
    let receipt = vault.begin_refresh(OWNER, owner).expect("own-login refresh begins");
    let refused = vault.purge_refused_credentials().expect_err("refresh active");
    assert!(matches!(refused, CredentialStoreError::Store(StoreError::WriteRefused("refresh_active"))), "{refused:?}");
    assert!(present(&vault), "nothing removed during a refresh");
    vault.cancel_refresh(receipt, owner).expect("refresh resolved");
    let raw = HubStore::initialize(root.path()).expect("store").open().expect("raw");
    raw.execute("INSERT INTO supervised_collector_lease(singleton_id, instance_id, state, started_at_ms, heartbeat_at_ms, lease_until_ms)
        VALUES (1, ?1, 'active', 0, 1, ?2)", rusqlite::params![Uuid::new_v4().to_string(), i64::MAX]).expect("live lease");
    let refused = vault.purge_refused_credentials().expect_err("collector running");
    assert!(matches!(refused, CredentialStoreError::Store(StoreError::WriteRefused("collector_running"))), "{refused:?}");
    assert!(present(&vault), "nothing removed while a collector runs");
    raw.execute("DELETE FROM supervised_collector_lease", []).expect("lease released");
    vault.purge_refused_credentials().expect("purge once idle");
    assert!(!present(&vault));
    assert_eq!(pair(&vault).generation, owner);
}

/// Acceptance 2: before the purge, status and the doctor (one hint) and the
/// backup refusal name the command; afterwards none does.
#[test]
fn refused_credential_purge_status_doctor_and_backup_name_the_command_until_the_purge() {
    let (root, _) = refused_purge_blocked_fixture();
    let store = HubStore::initialize(root.path()).expect("store");
    let hint = crate::runtime::operator::purge::hint(&store).expect("hint").expect("refused credentials are kept");
    assert_eq!(hint["command"], "teslatlas-hub purge-refused-credentials");
    assert_eq!(hint["refusedSlots"], serde_json::json!(["fleet_api"]));
    assert!(hint["message"].as_str().is_some_and(|text| text.contains("teslatlas-hub purge-refused-credentials --confirm")), "{hint}");
    let destination = crate::private_tempdir().expect("destination");
    let error = crate::data_recovery::create_data_backup(&store, &destination.path().join("backup")).expect_err("backup refused");
    let message = crate::data_recovery::refused_credential_backup_message(&store, &error).expect("the blocked refusal names the command");
    assert!(message.contains("teslatlas-hub purge-refused-credentials --confirm"), "{message}");
    let vault = open(root.path());
    vault.purge_refused_credentials().expect("purge");
    drop(vault);
    assert!(crate::runtime::operator::purge::hint(&store).expect("hint").is_none(), "nothing to name after the purge");
    let waiting = crate::data_recovery::DataRecoveryError::Store(StoreError::BackupWaitsForCredentialCleanup);
    assert!(crate::data_recovery::refused_credential_backup_message(&store, &waiting).is_none(), "no blocked refusal after the purge");
}
