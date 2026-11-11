// SPDX-License-Identifier: AGPL-3.0-only

// M06-T02: authenticated generations, secret exclusion and restore that does
// not resurrect access. Synthetic data only, in temporary stores; no network,
// no collector run and no real credential. The canaries below are random
// strings planted where a secret would be.

use std::time::Duration;

use rusqlite::params;

use super::*;
use crate::data_recovery::{
    EncryptedBackupReport, EncryptedBackupRequest,
    archive_crypto::KdfParams,
    backup_admission::{VolumeTrust, test_seam},
    create_encrypted_backup,
    encrypted_backup::{HookAction, archive_plaintext, clear_hook, set_hook},
    restore_encrypted_backup, verify_encrypted_backup,
};

const PASSWORD: &str = "correct horse battery staple";
const TEST_KDF: KdfParams = KdfParams {
    memory_kib: 64,
    passes: 1,
    lanes: 1,
};

struct Hub {
    temporary: tempfile::TempDir,
    store: HubStore,
    destination: PathBuf,
}

fn hub() -> Hub {
    clear_hook();
    // Removals happen only where ownership is enforced; the scratch volume
    // may ignore ownership, so the tests say which volume they play.
    test_seam::force(Some(VolumeTrust::Posix));
    let (temporary, store) = create_fixture();
    publish_schema_22_fixture(&store);
    let destination = temporary.path().join("destination");
    fs::create_dir(&destination).expect("destination");
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).expect("private");
    Hub {
        temporary,
        store,
        destination,
    }
}

impl Hub {
    fn request_to<'a>(&self, destination: &'a Path) -> EncryptedBackupRequest<'a> {
        let mut request = EncryptedBackupRequest::new(destination, PASSWORD);
        request.kdf = TEST_KDF;
        request.chunk_bytes = 4096;
        request
    }

    fn backup(&self) -> EncryptedBackupReport {
        create_encrypted_backup(&self.store, &self.request_to(&self.destination))
            .expect("encrypted backup")
    }

    fn backup_keeping(&self, keep: usize) -> EncryptedBackupReport {
        let mut request = self.request_to(&self.destination);
        request.keep = std::num::NonZeroUsize::new(keep);
        create_encrypted_backup(&self.store, &request).expect("rotating backup")
    }

    fn data(&self) -> PathBuf {
        self.store.database_path().parent().unwrap().to_path_buf()
    }

    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.destination)
            .expect("destination listing")
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A plain connection on the live catalogue with `secure_delete` off, as
    /// an earlier writer without it would have deleted rows.
    fn raw(&self) -> Connection {
        let connection = Connection::open(self.store.database_path()).expect("raw catalogue");
        connection
            .execute_batch("PRAGMA secure_delete = OFF; PRAGMA foreign_keys = ON;")
            .expect("raw pragmas");
        connection
    }
}

fn tick() {
    std::thread::sleep(Duration::from_millis(3));
}

fn holds(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// Whether the live catalogue's current image (checkpointed into the file)
/// holds `needle`, in a row or in a free page.
fn live_files_hold(data: &Path, needle: &[u8]) -> bool {
    let connection = Connection::open(data.join("hub.sqlite")).expect("raw catalogue");
    let (busy, _, _): (i64, i64, i64) = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("checkpoint");
    assert_eq!(busy, 0, "checkpoint completed");
    drop(connection);
    fs::read(data.join("hub.sqlite")).is_ok_and(|bytes| holds(&bytes, needle))
}

fn name_of(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

fn canary() -> Vec<u8> {
    format!("m06t02-secret-canary-{}", Uuid::new_v4()).into_bytes()
}

/// An envelope-sized blob made of the canary, large enough to spill into
/// overflow pages that a delete returns to the freelist.
fn canary_blob(canary: &[u8]) -> Vec<u8> {
    canary.iter().copied().cycle().take(16_000).collect()
}

/// Insert a credential pair row whose envelopes are `blob`.
fn insert_pair(connection: &Connection, slot: &str, blob: &[u8]) {
    let fleet = slot == "fleet_api";
    connection
        .execute(
            "INSERT INTO credential_pairs(account_id, slot, origin, refresh_owner, generation,
                 access_envelope, refresh_envelope, refresh_present, refresh_fingerprint,
                 expires_at, next_refresh_at, client_id, region, migrated, written_at_ms)
             VALUES (1, ?1, ?2, 'hub', ?3, ?4, ?4, 1, ?5, 2000, 1000, ?6, ?7, 0, 1)",
            params![
                slot,
                if fleet { "fleet_oauth" } else { "hub_login" },
                Uuid::new_v4().to_string(),
                blob,
                vec![7_u8; 32],
                fleet.then_some("synthetic-client"),
                fleet.then_some("eu"),
            ],
        )
        .expect("synthetic credential pair");
}

/// Plant refresh-token bytes that the live store deleted earlier without
/// `secure_delete` (old Owner API and Fleet pairs), a current Owner API pair,
/// a credential-unit status row and key files in the data directory. Returns
/// the canaries of the deleted pairs, the live pair and the key files.
fn plant_secrets(hub: &Hub) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let deleted = canary();
    let live = canary();
    let files = canary();
    let connection = hub.raw();
    insert_pair(&connection, "owner_api", &canary_blob(&deleted));
    insert_pair(&connection, "fleet_api", &canary_blob(&deleted));
    connection
        .execute_batch("DELETE FROM credential_pairs;")
        .expect("delete old pairs");
    insert_pair(&connection, "owner_api", &live.iter().copied().cycle().take(256).collect::<Vec<u8>>());
    connection
        .execute(
            "INSERT INTO collector_auth_state(singleton_id, state, reason, collecting, retry_at,
                 stopped_generation, stopped_mode, fuse_consecutive, fuse_generation,
                 fuse_last_rejection_at, fuse_pause_until, clock_skew, updated_at_ms,
                 fuse_rejected_access_digest)
             VALUES (1, 'ready', NULL, 1, NULL, NULL, NULL, 1, ?1, 5, NULL, 0, 10, ?2)",
            params![Uuid::new_v4().to_string(), Uuid::new_v4().to_string()],
        )
        .expect("credential status row");
    drop(connection);
    let data = hub.data();
    let secrets = data.join("secrets");
    fs::create_dir_all(&secrets).unwrap();
    for name in CAR_CONTROL_SECRET_FILES {
        fs::write(data.join(name), &files).unwrap();
        fs::write(secrets.join(name), &files).unwrap();
    }
    for name in ["hub-cursor.key", "tls-key.pem"] {
        fs::write(secrets.join(name), &files).unwrap();
    }
    assert!(
        live_files_hold(&data, &deleted),
        "precondition: the deleted pair's bytes are still in the live catalogue files"
    );
    (deleted, live, files)
}

fn table_rows(data: &Path, table: &str) -> i64 {
    let connection = open_immutable_catalogue(data).expect("catalogue");
    if !catalogue_has_table(&connection, table) {
        return 0;
    }
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
        .expect("row count")
}

fn device_digest(store: &HubStore, device_id: Uuid) -> Vec<u8> {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT token_sha256 FROM paired_devices WHERE device_id = ?1",
            [device_id.to_string()],
            |row| row.get(0),
        )
        .expect("device digest")
}

fn pair_device(store: &HubStore, name: &str, at_ms: i64) -> (Uuid, String) {
    let invitation = store
        .create_pairing(name, at_ms, at_ms + 100_000)
        .expect("pairing");
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), name, at_ms + 1)
        .expect("claim");
    (access.device_id, access.access_token.as_bearer().to_owned())
}

// ---------------------------------------------------------------------------
// backup_secret_exclusion
// ---------------------------------------------------------------------------

#[test]
fn backup_secret_exclusion_no_refresh_token_or_key_bytes_reach_an_encrypted_archive() {
    let hub = hub();
    let (deleted, live, files) = plant_secrets(&hub);
    let report = hub.backup();
    let plaintext = archive_plaintext(&report.path, PASSWORD).expect("archive plaintext");
    assert!(
        !holds(&plaintext, &deleted),
        "free pages copied from the live catalogue carried a deleted refresh token"
    );
    assert!(!holds(&plaintext, &live), "the current owner pair is excluded");
    assert!(!holds(&plaintext, &files), "no key file is a member");
    // The members are the catalogue, packs and signed no-ops only.
    let restored = hub.temporary.path().join("restored");
    let restored_report =
        restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("restore");
    assert_eq!(restored_report.member_count, report.member_count);
    for name in CAR_CONTROL_SECRET_FILES {
        assert!(!restored.join(name).exists(), "{name} is not restored");
    }
    assert!(!restored.join("secrets").exists());
    let image = fs::read(restored.join("hub.sqlite")).unwrap();
    assert!(!holds(&image, &deleted) && !holds(&image, &live));
}

#[test]
fn backup_secret_exclusion_directory_generation_holds_no_deleted_secret_bytes() {
    let hub = hub();
    let (deleted, live, files) = plant_secrets(&hub);
    let backup = hub.temporary.path().join("directory-generation");
    create_data_backup(&hub.store, &backup).expect("directory backup");
    let image = fs::read(backup.join("data/hub.sqlite")).unwrap();
    assert!(!holds(&image, &deleted), "a deleted refresh token survived in a free page");
    assert!(!holds(&image, &live));
    assert!(!backup.join("data/secrets").exists());
    for name in CAR_CONTROL_SECRET_FILES {
        assert!(!backup.join("data").join(name).exists());
    }
    let _ = files;
}

#[test]
fn backup_secret_exclusion_restore_requires_a_fresh_hub_login_and_a_new_virtual_key() {
    let hub = hub();
    plant_secrets(&hub);
    let report = hub.backup();
    let restored = hub.temporary.path().join("restored");
    restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("restore");
    // No pair, no sign-in and no credential-unit status: the state of a
    // fresh install, which needs a fresh Hub login.
    for table in ["credential_pairs", "credential_sign_ins", "collector_auth_state"] {
        assert_eq!(table_rows(&restored, table), 0, "{table} is empty after restore");
    }
    let restored_store = HubStore::open_immutable_read_only(&restored).expect("restored store");
    for slot in [
        crate::auth::credential_envelope::CredentialSlot::OwnerApi,
        crate::auth::credential_envelope::CredentialSlot::FleetApi,
    ] {
        assert!(
            restored_store
                .credential_pair_presence(slot)
                .expect("presence")
                .is_none()
        );
    }
    // No Fleet virtual-key private key or credential key: a new virtual key
    // is generated and enrolled after restore.
    assert!(!restored.join("fleet-command-key.pem").exists());
    assert!(!restored.join("secrets").exists());
}

// ---------------------------------------------------------------------------
// restore_revocation
// ---------------------------------------------------------------------------

#[test]
fn restore_revocation_a_pre_revocation_backup_does_not_resurrect_access() {
    let hub = hub();
    let (revoked_later, revoked_bearer) = pair_device(&hub.store, "Revoked later", 2_000);
    let (_, kept_bearer) = pair_device(&hub.store, "Still paired", 2_100);
    // A device removed before the backup by a writer without secure_delete.
    let (removed, removed_bearer) = pair_device(&hub.store, "Removed earlier", 2_200);
    let removed_digest = device_digest(&hub.store, removed);
    let revoked_digest = device_digest(&hub.store, revoked_later);
    hub.raw()
        .execute(
            "DELETE FROM paired_devices WHERE device_id = ?1",
            [removed.to_string()],
        )
        .expect("remove device");
    assert!(live_files_hold(&hub.data(), &removed_digest), "precondition");
    let report = hub.backup();
    hub.store.revoke_device(revoked_later).expect("revoke after the backup");

    let plaintext = archive_plaintext(&report.path, PASSWORD).expect("archive plaintext");
    assert!(!holds(&plaintext, &removed_digest), "no bearer digest in the archive");
    assert!(!holds(&plaintext, &revoked_digest));

    let restored = hub.temporary.path().join("restored");
    restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("restore");
    let image = fs::read(restored.join("hub.sqlite")).unwrap();
    assert!(!holds(&image, &removed_digest) && !holds(&image, &revoked_digest));
    let restored_store = HubStore::open_immutable_read_only(&restored).expect("restored store");
    for bearer in [&revoked_bearer, &kept_bearer, &removed_bearer] {
        assert!(
            restored_store
                .authenticate_device_at(bearer, 3_000)
                .expect("authenticate")
                .is_none(),
            "no pre-restore bearer authenticates"
        );
    }
    for table in [
        "paired_devices",
        "paired_device_token_grace",
        "device_vehicle_grants",
        "pairing_challenges",
        "pairing_challenge_vehicles",
        "supervised_collector_lease",
    ] {
        assert_eq!(table_rows(&restored, table), 0, "{table} is empty after restore");
    }
    // TLS identity and cursor key are host state the restored Hub generates.
    assert!(!restored.join("tls").exists());
    assert!(!crate::legacy_owner_credentials::cursor_key_path(&restored).exists());
}

#[test]
fn restore_revocation_revoked_devices_stay_revoked_and_bearers_are_regenerated() {
    let hub = hub();
    let (revoked, revoked_bearer) = pair_device(&hub.store, "Revoked first", 2_000);
    hub.store.revoke_device(revoked).expect("revoke");
    let report = hub.backup();
    let restored = hub.temporary.path().join("restored");
    restore_encrypted_backup(&report.path, PASSWORD, &restored).expect("restore");
    let restored_store = HubStore::initialize(&restored).expect("restored Hub");
    assert!(
        restored_store
            .authenticate_device_at(&revoked_bearer, 3_000)
            .expect("authenticate")
            .is_none()
    );
    // Re-pairing issues a new bearer; the old one stays refused.
    let (_, fresh_bearer) = pair_device(&restored_store, "Re-paired", 4_000);
    assert!(
        restored_store
            .authenticate_device_at(&fresh_bearer, 4_002)
            .expect("authenticate")
            .is_some()
    );
    assert!(
        restored_store
            .authenticate_device_at(&revoked_bearer, 4_002)
            .expect("authenticate")
            .is_none()
    );
}

#[test]
fn restore_revocation_directory_generations_carry_no_bearer_digest() {
    let hub = hub();
    let (device, bearer) = pair_device(&hub.store, "Directory device", 2_000);
    let digest = device_digest(&hub.store, device);
    let backup = hub.temporary.path().join("directory-generation");
    create_data_backup(&hub.store, &backup).expect("directory backup");
    let image = fs::read(backup.join("data/hub.sqlite")).unwrap();
    assert!(!holds(&image, &digest), "the sealed copy kept a bearer digest");
    let restored = hub.temporary.path().join("restored");
    restore_data_backup(&backup, &restored).expect("restore");
    assert!(!holds(&fs::read(restored.join("hub.sqlite")).unwrap(), &digest));
    let restored_store = HubStore::open_immutable_read_only(&restored).expect("restored store");
    assert!(
        restored_store
            .authenticate_device_at(&bearer, 3_000)
            .expect("authenticate")
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// backup_path_substitution
// ---------------------------------------------------------------------------

/// A canonical-looking generation name for a generation that does not exist.
fn lookalike_name(stamp: &str) -> String {
    format!("teslatlas-hub-backup-{stamp}-{}.tlbk", Uuid::new_v4())
}

fn stamp_of(report: &EncryptedBackupReport) -> String {
    let name = name_of(&report.path);
    name["teslatlas-hub-backup-".len()..]
        .split('-')
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn backup_path_substitution_rotation_removes_only_own_authenticated_generations() {
    let hub = hub();
    let first = hub.backup();
    tick();
    let second = hub.backup();
    tick();

    // Another Hub's generation, made with the same password.
    let other = self::hub();
    let foreign = other.backup();
    let foreign_copy = hub.destination.join(name_of(&foreign.path));
    fs::copy(&foreign.path, &foreign_copy).unwrap();
    // Garbage under an old and under a far-future generation name.
    let old_garbage = hub.destination.join(lookalike_name("20000101T000000000Z"));
    let future_garbage = hub.destination.join(lookalike_name("99991231T235959999Z"));
    for path in [&old_garbage, &future_garbage] {
        fs::write(path, b"not a generation").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    // One of this Hub's generations under another generation's name.
    let renamed = hub.destination.join(lookalike_name(&stamp_of(&first)));
    fs::copy(&first.path, &renamed).unwrap();
    // A link named like a generation, to a real generation kept elsewhere.
    let elsewhere = hub.temporary.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let kept_elsewhere = create_encrypted_backup(&hub.store, &hub.request_to(&elsewhere))
        .expect("backup elsewhere");
    let link = hub.destination.join(lookalike_name("20000101T000000001Z"));
    std::os::unix::fs::symlink(&kept_elsewhere.path, &link).unwrap();
    tick();

    let latest = hub.backup_keeping(1);
    assert!(latest.rotation_error.is_none());
    assert_eq!(latest.rotated_generations, 2, "only this Hub's two older generations go");
    assert!(!first.path.exists() && !second.path.exists());
    for kept in [&foreign_copy, &old_garbage, &future_garbage, &renamed, &kept_elsewhere.path] {
        assert!(kept.exists(), "{} is never deleted", kept.display());
    }
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    verify_encrypted_backup(&latest.path, PASSWORD).expect("newest generation");
    verify_encrypted_backup(&foreign_copy, PASSWORD).expect("foreign generation untouched");
}

#[test]
fn backup_path_substitution_verify_and_restore_refuse_substituted_files() {
    let hub = hub();
    let good = hub.backup();
    let renamed = hub.destination.join(lookalike_name(&stamp_of(&good)));
    fs::copy(&good.path, &renamed).unwrap();
    let garbage = hub.destination.join(lookalike_name("20000101T000000000Z"));
    fs::write(&garbage, vec![0x5A_u8; 4096]).unwrap();
    let link = hub.destination.join(lookalike_name("20000101T000000001Z"));
    std::os::unix::fs::symlink(&good.path, &link).unwrap();

    for substituted in [&renamed, &garbage, &link] {
        assert!(
            verify_encrypted_backup(substituted, PASSWORD).is_err(),
            "{} is not trusted",
            substituted.display()
        );
        let target = hub.temporary.path().join(format!("restore-{}", Uuid::new_v4()));
        assert!(
            restore_encrypted_backup(substituted, PASSWORD, &target).is_err(),
            "{} is not restored",
            substituted.display()
        );
        assert!(!target.exists(), "nothing is published");
    }
    // The genuine generation under its own name still verifies and restores.
    verify_encrypted_backup(&good.path, PASSWORD).expect("genuine generation");
    restore_encrypted_backup(&good.path, PASSWORD, &hub.temporary.path().join("restored"))
        .expect("genuine restore");
}

#[test]
fn backup_path_substitution_stale_partials_are_removed_only_when_they_are_own() {
    let hub = hub();
    let first = hub.backup();
    tick();
    // This Hub's own run, killed mid-write.
    let writes = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let counter = writes.clone();
    set_hook(move |stage, _| {
        if stage == "write" {
            counter.set(counter.get() + 1);
            if counter.get() == 6 {
                return HookAction::Crash;
            }
        }
        HookAction::Continue
    });
    assert!(create_encrypted_backup(&hub.store, &hub.request_to(&hub.destination)).is_err());
    clear_hook();
    let own_partial: Vec<String> = hub
        .entries()
        .into_iter()
        .filter(|name| name.ends_with(".partial"))
        .collect();
    assert_eq!(own_partial.len(), 1);

    let other = self::hub();
    let foreign = other.backup();
    let partial_name = |generation: Uuid| format!(".teslatlas-hub-backup-{generation}.partial");
    let foreign_partial = hub.destination.join(partial_name(foreign.generation));
    fs::copy(&foreign.path, &foreign_partial).unwrap();
    let garbage_partial = hub.destination.join(partial_name(Uuid::new_v4()));
    fs::write(&garbage_partial, b"not a partial").unwrap();
    let mismatched_partial = hub.destination.join(partial_name(Uuid::new_v4()));
    fs::copy(&first.path, &mismatched_partial).unwrap();
    tick();

    let second = hub.backup();
    assert!(!hub.destination.join(&own_partial[0]).exists(), "own partial cleared");
    for kept in [&foreign_partial, &garbage_partial, &mismatched_partial] {
        assert!(kept.exists(), "{} is never deleted", kept.display());
    }
    verify_encrypted_backup(&first.path, PASSWORD).expect("first");
    verify_encrypted_backup(&second.path, PASSWORD).expect("second");
}

#[test]
fn backup_path_substitution_posix_admission_applies_on_local_volumes_only() {
    let hub = hub();
    let good = hub.backup();
    let shared = hub.temporary.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o770)).unwrap();
    fs::set_permissions(&good.path, fs::Permissions::from_mode(0o660)).unwrap();

    // A local volume that enforces ownership: other accounts must not be able
    // to plant or swap a generation.
    test_seam::force(Some(VolumeTrust::Posix));
    let refused = create_encrypted_backup(&hub.store, &hub.request_to(&shared));
    assert!(matches!(refused, Err(DataRecoveryError::InvalidBackup(_))));
    assert_eq!(fs::read_dir(&shared).unwrap().count(), 0, "nothing written");
    assert!(verify_encrypted_backup(&good.path, PASSWORD).is_err());
    let target = hub.temporary.path().join("posix-restore");
    assert!(restore_encrypted_backup(&good.path, PASSWORD, &target).is_err());
    assert!(!target.exists());
    // A private folder and private archive are admitted.
    fs::set_permissions(&good.path, fs::Permissions::from_mode(0o600)).unwrap();
    verify_encrypted_backup(&good.path, PASSWORD).expect("private archive on a local volume");
    fs::set_permissions(&good.path, fs::Permissions::from_mode(0o660)).unwrap();

    // Network and ownership-ignoring volumes rely on the authenticated
    // encryption, and nothing is ever deleted there.
    test_seam::force(Some(VolumeTrust::EncryptionOnly));
    let shared_report = create_encrypted_backup(&hub.store, &hub.request_to(&shared))
        .expect("backup to a network or removable volume");
    verify_encrypted_backup(&good.path, PASSWORD).expect("modes are not relied on");
    let garbage = shared.join(lookalike_name("20000101T000000000Z"));
    fs::write(&garbage, vec![0_u8; 4096]).unwrap();
    assert!(verify_encrypted_backup(&garbage, PASSWORD).is_err(), "authentication still applies");
    tick();
    let mut request = hub.request_to(&shared);
    request.keep = std::num::NonZeroUsize::new(1);
    let latest = create_encrypted_backup(&hub.store, &request).expect("rotating backup");
    assert_eq!(latest.rotated_generations, 0, "no removal can be protected there");
    assert!(latest.rotation_error.is_some(), "the kept generations are reported");
    assert!(shared_report.path.exists() && garbage.exists());
    test_seam::force(None);
}

// ---------------------------------------------------------------------------
// backup_path_substitution, round 2: the six review findings
// ---------------------------------------------------------------------------

const PROTECTED: &str = ".teslatlas-hub-removals";

fn partial_name_of(generation: Uuid) -> String {
    format!(".teslatlas-hub-backup-{generation}.partial")
}

fn rotating_request<'a>(hub: &Hub, destination: &'a Path, keep: usize) -> EncryptedBackupRequest<'a> {
    let mut request = hub.request_to(destination);
    request.keep = std::num::NonZeroUsize::new(keep);
    request
}

/// Finding 1: the new generation counts for retention only once the complete
/// published file authenticates and is the object the run wrote.
#[test]
fn backup_path_substitution_newest_counts_only_as_the_authenticated_written_object() {
    let hub = hub();
    let first = hub.backup();
    tick();

    // A destination writer replaces the closed partial before publication.
    let stand_in = hub.temporary.path().join("stand-in");
    set_hook({
        let stand_in = stand_in.clone();
        move |stage, path| {
            if stage == "before_publish" {
                fs::write(&stand_in, vec![0x42_u8; 8192]).unwrap();
                fs::set_permissions(&stand_in, fs::Permissions::from_mode(0o600)).unwrap();
                fs::rename(&stand_in, path).unwrap();
            }
            HookAction::Continue
        }
    });
    let swapped = create_encrypted_backup(&hub.store, &rotating_request(&hub, &hub.destination, 1))
        .expect("the run publishes what it finds");
    clear_hook();
    assert_eq!(swapped.rotated_generations, 0);
    assert!(swapped.rotation_error.is_some(), "the doubt is reported");
    verify_encrypted_backup(&first.path, PASSWORD).expect("the last good generation stays");
    assert!(verify_encrypted_backup(&swapped.path, PASSWORD).is_err());
    tick();

    // The published archive is truncated once it has authenticated.
    set_hook(|stage, path| {
        if stage == "rotate_confirmed" {
            let file = fs::OpenOptions::new().write(true).open(path).unwrap();
            let length = file.metadata().unwrap().len();
            file.set_len(length / 2).unwrap();
        }
        HookAction::Continue
    });
    let truncated = create_encrypted_backup(&hub.store, &rotating_request(&hub, &hub.destination, 1))
        .expect("backup despite the truncation");
    clear_hook();
    assert_eq!(truncated.rotated_generations, 0);
    assert!(truncated.rotation_error.is_some());
    verify_encrypted_backup(&first.path, PASSWORD).expect("the last good generation stays");
    assert!(!hub.destination.join(PROTECTED).exists(), "nothing is left claimed");
}

/// Finding 2: a generation damaged beyond its manifest neither counts nor goes.
#[test]
fn backup_path_substitution_damaged_generations_neither_count_nor_go() {
    let hub = hub();
    let first = hub.backup();
    tick();
    let second = hub.backup();
    tick();
    let mut bytes = fs::read(&second.path).unwrap();
    let near_end = bytes.len() - 5;
    bytes[near_end] ^= 0x01;
    fs::write(&second.path, &bytes).unwrap();
    assert!(verify_encrypted_backup(&second.path, PASSWORD).is_err(), "precondition");

    let third = hub.backup_keeping(2);
    assert_eq!(third.rotated_generations, 0, "the damaged one does not count");
    assert!(third.rotation_error.is_some(), "the damaged generation is reported");
    verify_encrypted_backup(&first.path, PASSWORD).expect("the genuine older generation stays");
    assert!(second.path.exists(), "a damaged generation is never removed");
    verify_encrypted_backup(&third.path, PASSWORD).expect("newest");
}

/// Finding 3: a complete generation under a partial name is kept; only a
/// partial the live catalogue records as an abandoned run goes.
#[test]
fn backup_path_substitution_a_generation_renamed_as_a_partial_is_kept() {
    let hub = hub();
    let first = hub.backup();
    let disguised = hub.destination.join(partial_name_of(first.generation));
    fs::rename(&first.path, &disguised).unwrap();
    tick();
    hub.backup();
    assert!(disguised.exists(), "a renamed genuine generation is never cleaned up");
    fs::rename(&disguised, &first.path).unwrap();
    verify_encrypted_backup(&first.path, PASSWORD).expect("intact");
    tick();

    // This Hub's run killed after the archive was complete: the next run
    // publishes it under its own name instead of removing it.
    set_hook(|stage, _| {
        if stage == "before_publish" {
            HookAction::Crash
        } else {
            HookAction::Continue
        }
    });
    assert!(create_encrypted_backup(&hub.store, &hub.request_to(&hub.destination)).is_err());
    clear_hook();
    let abandoned: Vec<String> = hub
        .entries()
        .into_iter()
        .filter(|name| name.ends_with(".partial"))
        .collect();
    assert_eq!(abandoned.len(), 1);
    let generation = abandoned[0]
        .trim_start_matches(".teslatlas-hub-backup-")
        .trim_end_matches(".partial")
        .to_owned();
    tick();
    hub.backup();
    let published: Vec<String> = hub
        .entries()
        .into_iter()
        .filter(|name| name.ends_with(&format!("-{generation}.tlbk")))
        .collect();
    assert_eq!(published.len(), 1, "the complete abandoned run is kept and published");
    verify_encrypted_backup(&hub.destination.join(&published[0]), PASSWORD).expect("published");
    assert!(!hub.destination.join(&abandoned[0]).exists());
    tick();

    // A recorded partial replaced by another generation of this Hub: the
    // name and the generation say "own", the identity does not.
    set_hook(|stage, _| {
        if stage == "record" {
            HookAction::Crash
        } else {
            HookAction::Continue
        }
    });
    assert!(create_encrypted_backup(&hub.store, &hub.request_to(&hub.destination)).is_err());
    clear_hook();
    let recorded = hub
        .entries()
        .into_iter()
        .find(|name| name.ends_with(".partial"))
        .expect("the killed run's partial");
    let elsewhere = hub.temporary.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let substitute = create_encrypted_backup(&hub.store, &hub.request_to(&elsewhere)).unwrap();
    fs::rename(&substitute.path, hub.destination.join(&recorded)).unwrap();
    tick();
    hub.backup();
    assert!(hub.destination.join(&recorded).exists(), "not the recorded object: kept");
}

/// Finding 4: a removal claims its candidate into a protected directory and
/// authenticates what it claimed; where no directory can be protected,
/// nothing is deleted.
#[test]
fn backup_path_substitution_removal_claims_and_authenticates_what_it_removes() {
    let hub = hub();
    let first = hub.backup();
    tick();
    let second = hub.backup();
    tick();
    let elsewhere = hub.temporary.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let substitute = create_encrypted_backup(&hub.store, &hub.request_to(&elsewhere)).unwrap();
    let substitute_bytes = fs::read(&substitute.path).unwrap();
    tick();

    // The candidate's name is swapped for another genuine generation in the
    // moment before its removal.
    let swapped = std::rc::Rc::new(std::cell::RefCell::new(None::<PathBuf>));
    set_hook({
        let swapped = swapped.clone();
        let substitute = substitute.path.clone();
        move |stage, path| {
            if stage == "rotate_remove" && swapped.borrow().is_none() {
                fs::rename(&substitute, path).unwrap();
                *swapped.borrow_mut() = Some(path.to_path_buf());
            }
            HookAction::Continue
        }
    });
    let third = hub.backup_keeping(1);
    clear_hook();
    let swapped = swapped.borrow().clone().expect("a removal was attempted");
    assert_eq!(fs::read(&swapped).unwrap(), substitute_bytes, "the substitute is kept");
    assert!(third.rotation_error.is_some(), "the refused removal is reported");
    assert_eq!(third.rotated_generations, 1, "the other candidate went");
    assert!(!hub.destination.join(PROTECTED).exists());
    let _ = (&first, &second);
    tick();

    // A protected directory that others can enter protects nothing.
    let fourth = hub.backup();
    tick();
    fs::create_dir(hub.destination.join(PROTECTED)).unwrap();
    fs::set_permissions(hub.destination.join(PROTECTED), fs::Permissions::from_mode(0o777))
        .unwrap();
    let fifth = hub.backup_keeping(1);
    assert_eq!(fifth.rotated_generations, 0);
    assert!(fifth.rotation_error.is_some());
    verify_encrypted_backup(&fourth.path, PASSWORD).expect("kept");
    fs::remove_dir(hub.destination.join(PROTECTED)).unwrap();
    tick();

    // No protected directory on a volume that ignores ownership: no removal,
    // and a failed run's partial stays too.
    test_seam::force(Some(VolumeTrust::EncryptionOnly));
    let sixth = hub.backup_keeping(1);
    assert_eq!(sixth.rotated_generations, 0);
    assert!(sixth.rotation_error.is_some());
    for kept in [&third.path, &fourth.path, &fifth.path] {
        verify_encrypted_backup(kept, PASSWORD).expect("nothing is deleted");
    }
    set_hook(|stage, _| {
        if stage == "write" {
            HookAction::Fail(std::io::ErrorKind::BrokenPipe)
        } else {
            HookAction::Continue
        }
    });
    assert!(create_encrypted_backup(&hub.store, &hub.request_to(&hub.destination)).is_err());
    clear_hook();
    assert_eq!(
        hub.entries().iter().filter(|name| name.ends_with(".partial")).count(),
        1,
        "the failed run's partial is not deleted where no removal is protected"
    );
    test_seam::force(Some(VolumeTrust::Posix));
}

/// Finding 5: removable media that enforce ownership keep POSIX admission.
#[cfg(target_os = "macos")]
#[test]
fn backup_path_substitution_removable_volumes_with_ownership_keep_posix_admission() {
    use crate::data_recovery::backup_admission::trust_for_mount;
    const MNT_REMOVABLE: u32 = 0x0000_0200;
    const MNT_LOCAL: u32 = 0x0000_1000;
    const MNT_IGNORE_OWNERSHIP: u32 = 0x0020_0000;
    for kind in ["apfs", "hfs"] {
        assert_eq!(trust_for_mount(MNT_LOCAL | MNT_REMOVABLE, kind), VolumeTrust::Posix);
        assert_eq!(trust_for_mount(MNT_LOCAL, kind), VolumeTrust::Posix);
        assert_eq!(
            trust_for_mount(MNT_LOCAL | MNT_REMOVABLE | MNT_IGNORE_OWNERSHIP, kind),
            VolumeTrust::EncryptionOnly
        );
        assert_eq!(trust_for_mount(0, kind), VolumeTrust::EncryptionOnly, "network");
    }
    for kind in ["exfat", "msdos", "ntfs"] {
        assert_eq!(
            trust_for_mount(MNT_LOCAL | MNT_REMOVABLE, kind),
            VolumeTrust::EncryptionOnly
        );
    }
}

#[cfg(target_os = "macos")]
fn add_acl(path: &Path, entry: &str) {
    let status = std::process::Command::new("/bin/chmod")
        .arg("+a")
        .arg(entry)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "cannot set `{entry}` on {}", path.display());
}

/// Finding 6: an ACL that lets another account write, add or delete refuses
/// a destination or an archive whose mode bits look private.
#[cfg(target_os = "macos")]
#[test]
fn backup_path_substitution_acl_write_grants_are_refused() {
    use crate::data_recovery::backup_admission::acl_listing_grants_write;
    let hub = hub();
    let good = hub.backup();

    let open_folder = hub.temporary.path().join("acl-folder");
    fs::create_dir(&open_folder).unwrap();
    fs::set_permissions(&open_folder, fs::Permissions::from_mode(0o700)).unwrap();
    add_acl(&open_folder, "group:everyone allow list,search,add_file,delete_child");
    let refused = create_encrypted_backup(&hub.store, &hub.request_to(&open_folder));
    assert!(matches!(refused, Err(DataRecoveryError::InvalidBackup(_))));
    assert_eq!(fs::read_dir(&open_folder).unwrap().count(), 0, "nothing written");

    add_acl(&good.path, "group:everyone allow write,append");
    assert!(verify_encrypted_backup(&good.path, PASSWORD).is_err());
    let target = hub.temporary.path().join("acl-restore");
    assert!(restore_encrypted_backup(&good.path, PASSWORD, &target).is_err());
    assert!(!target.exists());

    // A deny entry grants nothing.
    let denied = hub.temporary.path().join("deny-folder");
    fs::create_dir(&denied).unwrap();
    fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();
    add_acl(&denied, "group:everyone deny delete");
    create_encrypted_backup(&hub.store, &hub.request_to(&denied)).expect("deny entries admit");

    let listing = "drwx------  2 hub  staff  64 Nov 11 17:21 d\n \
                   0: group:staff deny delete\n \
                   1: group:everyone inherited allow list,search,delete_child\n";
    assert!(acl_listing_grants_write(listing));
    assert!(!acl_listing_grants_write(
        "-rw-------  1 hub  staff  0 Nov 11 17:21 f\n 0: group:everyone allow read\n"
    ));
}
