// SPDX-License-Identifier: AGPL-3.0-only

//! Part M recovery tests (spec CT60, CT61, R52 with errata E58 and the
//! rulings M2P-58, M2P-60). Synthetic data only, in private temporary
//! directories.

use super::super::*;
use crate::auth::credential_envelope::OsNonces;
use crate::auth::credential_keys::{
    BackupExclusion, CredentialKeyError, ExclusionState, KeyringHandle,
};
use crate::auth::credential_migration::{CredentialPhaseB, run_phase_b, seam};
use crate::db::{CredentialConnection, CredentialSlotFormatState};
use crate::hub_user_process::AdmittedUserHub;
use crate::platform::private_directory::open_validated;
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use std::{collections::BTreeSet, os::unix::fs::PermissionsExt, sync::Arc};

const RST_K1: &str = "pre-upgrade-owner-api.key";
const RST_K2: &str = "pre-upgrade-fleet-api.key";
const CURSOR: &str = "hub-cursor.key";
const RUNG: i32 = 79;

struct VerifiedExclusion;

impl BackupExclusion for VerifiedExclusion {
    fn ensure_excluded(&self, _: &Path) -> Result<ExclusionState, CredentialKeyError> {
        Ok(ExclusionState::Verified)
    }
}

fn hex_to_bytes(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

fn vectors() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/credential-pre-upgrade-vectors.json");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

/// An export built by the test with the retained container format (magic,
/// nonce, AES-256-GCM, payload with flags), including pre-upgrade members.
fn build_export(
    recovery_key: &[u8],
    installation: Uuid,
    legacy: Option<&[u8]>,
    cursor: Option<&[u8]>,
    fleet: Option<&[u8]>,
) -> Vec<u8> {
    let mut flags = 0u8;
    if legacy.is_some() {
        flags |= 1;
    }
    if cursor.is_some() {
        flags |= 2;
    }
    if fleet.is_some() {
        flags |= 4;
    }
    let legacy_bytes = legacy.unwrap_or_default();
    let mut plain = installation.as_bytes().to_vec();
    plain.push(flags);
    plain.extend_from_slice(&(legacy_bytes.len() as u16).to_be_bytes());
    plain.extend_from_slice(legacy_bytes);
    plain.extend_from_slice(cursor.unwrap_or_default());
    plain.extend_from_slice(fleet.unwrap_or_default());
    let cipher = Aes256Gcm::new_from_slice(recovery_key).unwrap();
    let nonce = [7u8; 12];
    let sealed = cipher
        .encrypt(
            &Nonce::from(nonce),
            Payload {
                msg: &plain,
                aad: FILE_MAGIC,
            },
        )
        .unwrap();
    let mut out = FILE_MAGIC.to_vec();
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&sealed);
    out
}

struct Rec {
    root: tempfile::TempDir,
    store: HubStore,
    conn: CredentialConnection,
    admission: Arc<AdmittedUserHub>,
    keys: Option<KeyringHandle>,
}

impl Rec {
    fn open(root: tempfile::TempDir) -> Self {
        let store = HubStore::initialize(root.path()).unwrap();
        let admission = AdmittedUserHub::for_test(root.path()).unwrap();
        let mut conn = store.take_credential_connection().unwrap();
        conn.set_test_now_ms(1_800_000_000_000);
        Self {
            root,
            store,
            conn,
            admission,
            keys: None,
        }
    }

    fn fresh() -> Self {
        Self::open(crate::private_tempdir().unwrap())
    }

    /// A hand-built pre-upgrade store whose rows are the real OLD-D
    /// ciphertexts of fixture item 1's ciphertext cases `legacy_32` (Legacy)
    /// and `fleet_file` (Fleet); the keys that open them are `real_keys`.
    fn real_pair(owner: bool, fleet: bool) -> Self {
        let all = vectors();
        let row = |id: &str| {
            let case = all["ciphertext_cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == id)
                .unwrap();
            seam::PreRow {
                access: hex_to_bytes(case["access_ciphertext_hex"].as_str().unwrap()),
                refresh: hex_to_bytes(case["refresh_ciphertext_hex"].as_str().unwrap()),
                expires_at: 1_800_028_800,
                next_refresh_at: 1_800_021_600,
                generation: Some(case["generation"].as_str().unwrap().parse().unwrap()),
            }
        };
        let rec = Self::fresh();
        seam::seed_pre_upgrade_store(
            rec.conn.test_connection(),
            owner.then(|| row("legacy_32")).as_ref(),
            fleet.then(|| row("fleet_file")).as_ref(),
            false,
        );
        rec
    }

    fn data(&self) -> &Path {
        self.root.path()
    }

    fn secrets_path(&self) -> PathBuf {
        self.data().join("secrets")
    }

    fn phase_b(
        &mut self,
    ) -> Result<(), crate::auth::credential_migration::CredentialMigrationError> {
        seam::with(|s| {
            s.fake_pre_upgrade = false;
            s.fake_generation = false;
        });
        let secrets = open_validated(&self.secrets_path(), true).unwrap();
        let mut context = CredentialPhaseB {
            data_dir: self.root.path(),
            sqlite_temp_dir: self.root.path(),
            admission: &self.admission,
            secrets: &secrets,
            keys: &mut self.keys,
            cursor_key: None,
            exclusion: &VerifiedExclusion,
            nonces: &mut OsNonces,
            now_ms: 1_800_000_000_000,
        };
        run_phase_b(&mut self.conn, &mut context, &mut || {}).map(|_| ())
    }

    fn restore(
        &mut self,
        export: &Path,
        key: &[u8],
    ) -> Result<CredentialRecoveryReport, CredentialRecoveryError> {
        restore_credentials_with_connection(
            &self.store,
            &mut self.conn,
            self.root.path(),
            export,
            key,
        )
    }

    fn secrets_listing(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.secrets_path())
            .map(|d| {
                d.map(|e| e.unwrap().file_name().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn state(&mut self) -> crate::db::CredentialFormatState {
        self.conn.credential_format_state().unwrap()
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Decrypts an export written by `export_credentials` and returns its flags
/// and total plaintext length.
fn export_facts(path: &Path, recovery_key: &[u8]) -> (u8, usize) {
    let bytes = fs::read(path).unwrap();
    let cipher = recovery_cipher(recovery_key).unwrap();
    let decoded = decode_envelope(&cipher, &bytes).unwrap();
    let flags = (decoded.source_db_key.is_some() as u8)
        | ((decoded.cursor_key.is_some() as u8) << 1)
        | ((decoded.fleet_key.is_some() as u8) << 2);
    let length = decoded.source_db_key.map_or(0, |k| k.len())
        + decoded.cursor_key.map_or(0, |k| k.len())
        + decoded.fleet_key.map_or(0, |k| k.len());
    (flags, length)
}

const EIGHT_KEYS: [&str; 8] = [
    "status",
    "path",
    "installationId",
    "secretBearing",
    "encryption",
    "cursorKeyIncluded",
    "preUpgradeOwnerApiKeyIncluded",
    "preUpgradeFleetApiKeyIncluded",
];

#[test]
fn credential_recovery_report_uses_only_r52_fields() {
    // M2P-60: the replacement asserts exactly R52's eight keys; an encoded
    // former alias cannot supply a missing new field; no deny_unknown_fields
    // and no broader input policy.
    let report = report(
        "exported",
        Path::new("/tmp/synthetic-export"),
        Uuid::from_u128(1),
        true,
        true,
        true,
    );
    let value = serde_json::to_value(&report).unwrap();
    let keys: BTreeSet<String> = value.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        EIGHT_KEYS
            .iter()
            .map(|k| (*k).to_owned())
            .collect::<BTreeSet<_>>(),
        "M2P-60.eight_keys the serialised report holds exactly R52's eight keys"
    );
    assert_eq!(
        value["preUpgradeOwnerApiKeyIncluded"], true,
        "M2P-60.fields owner member flag"
    );
    assert_eq!(
        value["preUpgradeFleetApiKeyIncluded"], true,
        "M2P-60.fields fleet member flag"
    );
    // The former alias, built through the encoded name, cannot fill the new
    // field when it is missing.
    let former = crate::runtime::legacy_names::RECOVERY_PREVIOUS_KEY;
    let mut without = value.clone();
    without
        .as_object_mut()
        .unwrap()
        .remove("preUpgradeOwnerApiKeyIncluded");
    without
        .as_object_mut()
        .unwrap()
        .insert(former.to_owned(), serde_json::Value::Bool(true));
    let parsed: Result<CredentialRecoveryReport, _> = serde_json::from_value(without);
    assert!(
        parsed.is_err(),
        "M2P-60.alias an encoded former alias cannot supply a missing new field"
    );
    // No broader policy: an unrelated extra key beside all eight is accepted.
    let mut extra = value.clone();
    extra
        .as_object_mut()
        .unwrap()
        .insert("unrelatedExtraKey".to_owned(), serde_json::Value::Null);
    let parsed: CredentialRecoveryReport = serde_json::from_value(extra)
        .expect("M2P-60.no_policy an unknown key beside the eight is not rejected");
    assert_eq!(
        parsed, report,
        "M2P-60.round_trip the eight keys round-trip"
    );
    // The former key is not an output key either.
    assert!(
        !value.as_object().unwrap().contains_key(former),
        "M2P-60.output the former key is not emitted"
    );
}

#[test]
fn ct060_cursor_only_export() {
    seam::reset();
    let rec = Rec::fresh();
    let secrets = rec.secrets_path();
    fs::create_dir(&secrets).unwrap();
    fs::set_permissions(&secrets, fs::Permissions::from_mode(0o700)).unwrap();
    let cursor = [0x5au8; 32];
    write_private(&secrets.join(CURSOR), &cursor);
    // A keyring and pre-upgrade key files are present and never exported.
    write_private(&secrets.join("credential-keys"), &[1u8; 88]);
    write_private(&secrets.join(RST_K1), &[2u8; 32]);
    write_private(&secrets.join(RST_K2), &[3u8; 32]);
    let key = [9u8; 32];
    let out_dir = crate::private_tempdir().unwrap();
    let destination = out_dir.path().join("export.bin");
    let report = export_credentials(&rec.store, rec.data(), &destination, &key)
        .expect("CT60 a C10 Hub exports");
    let (flags, length) = export_facts(&destination, &key);
    assert_eq!(flags, 0x02, "CT60 payload flags 0x02 (cursor key only)");
    assert_eq!(
        length, 32,
        "CT60 the payload carries the cursor key and nothing else"
    );
    let value = serde_json::to_value(&report).unwrap();
    let keys: BTreeSet<String> = value.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        EIGHT_KEYS
            .iter()
            .map(|k| (*k).to_owned())
            .collect::<BTreeSet<_>>(),
        "CT60 report keys exactly R52's"
    );
    assert_eq!(
        value["preUpgradeOwnerApiKeyIncluded"], false,
        "CT60 owner pre-upgrade flag false"
    );
    assert_eq!(
        value["preUpgradeFleetApiKeyIncluded"], false,
        "CT60 fleet pre-upgrade flag false"
    );
    assert_eq!(value["cursorKeyIncluded"], true, "CT60 cursor flag true");
    let forbidden = crate::runtime::name_test_support::forbidden_name();
    for key in value.as_object().unwrap().keys() {
        assert!(
            !key.to_lowercase().contains(forbidden),
            "CT60 no key names the source project: {key}"
        );
    }
    let raw = fs::read(&destination).unwrap();
    assert!(
        !raw.windows(32)
            .any(|w| w == [1u8; 32] || w == [2u8; 32] || w == [3u8; 32]),
        "CT60 no keyring or pre-upgrade key bytes are visible"
    );
}

/// The keys that open `Rec::real_pair`'s rows (fixture item 1).
fn real_keys() -> (Vec<u8>, Vec<u8>) {
    let all = vectors();
    let key = |id: &str| {
        let case = all["ciphertext_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap();
        hex_to_bytes(case["key_hex"].as_str().unwrap())
    };
    (key("legacy_32"), key("fleet_file"))
}

/// The approved synthetic plaintexts of a ciphertext case of fixture item 1.
fn real_plaintexts(id: &str) -> (Vec<u8>, Vec<u8>) {
    let all = vectors();
    let case = all["ciphertext_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap();
    (
        case["access_plaintext"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec(),
        case["refresh_plaintext"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec(),
    )
}

fn write_export(
    rec: &Rec,
    key: &[u8],
    legacy: Option<&[u8]>,
    cursor: Option<&[u8]>,
    fleet: Option<&[u8]>,
) -> (tempfile::TempDir, PathBuf) {
    let dir = crate::private_tempdir().unwrap();
    let path = dir.path().join("export.bin");
    write_private(
        &path,
        &build_export(
            key,
            rec.store.installation_id().unwrap(),
            legacy,
            cursor,
            fleet,
        ),
    );
    (dir, path)
}

#[test]
fn ct061_old_export_restore() {
    seam::reset();
    let (legacy_key, fleet_key) = real_keys();
    let cursor = [0x5Au8; 32];
    let export_key = [0x33u8; 32];
    // Case fresh_directory: the catalogue is installed first and the restoring
    // process's ordinary Phase B runs (it creates the keyring and leaves both
    // slots blocked: no key file exists yet); then the export restores.
    let mut rec = Rec::real_pair(true, true);
    rec.phase_b().expect("CT61 the restoring process's Phase B");
    let state = rec.state();
    assert_eq!(
        state.owner_api_state,
        CredentialSlotFormatState::Pending,
        "CT61 owner slot waits for a key"
    );
    assert_eq!(
        state.fleet_api_state,
        CredentialSlotFormatState::Pending,
        "CT61 fleet slot waits for a key"
    );
    assert!(
        rec.secrets_listing()
            .contains(&"credential-keys".to_owned()),
        "CT61 Phase B created the keyring"
    );
    // Per-slot matching: a legacy member that does not authenticate the Legacy
    // row is refused before anything is published.
    let listing_before = rec.secrets_listing();
    let (_hold_bad, bad) = write_export(
        &rec,
        &export_key,
        Some(&[9u8; 32]),
        Some(&cursor),
        Some(&fleet_key),
    );
    let refused = rec.restore(&bad, &export_key);
    assert!(
        matches!(refused, Err(CredentialRecoveryError::CatalogueMismatch)),
        "CT61.matching a wrong Legacy member is refused: {refused:?}"
    );
    assert_eq!(
        rec.secrets_listing(),
        listing_before,
        "CT61.matching nothing was published"
    );
    let (_hold, export) = write_export(
        &rec,
        &export_key,
        Some(&legacy_key),
        Some(&cursor),
        Some(&fleet_key),
    );
    let report = rec
        .restore(&export, &export_key)
        .expect("CT61 the export restores");
    for name in [CURSOR, RST_K1, RST_K2] {
        assert_eq!(
            mode(&rec.secrets_path().join(name)),
            0o600,
            "CT61 {name} is 0600"
        );
    }
    assert!(
        report.pre_upgrade_owner_api_key_included,
        "CT61 owner pre-upgrade field true"
    );
    assert!(
        report.pre_upgrade_fleet_api_key_included,
        "CT61 fleet pre-upgrade field true"
    );
    assert_eq!(
        fs::read(rec.secrets_path().join(CURSOR)).unwrap(),
        cursor.to_vec(),
        "CT61 the cursor key is restored byte for byte"
    );
    assert_eq!(
        fs::read(rec.secrets_path().join(RST_K1)).unwrap(),
        legacy_key,
        "CT61 the Legacy member is restored byte for byte"
    );
    // After the restore the next admitted open converts both slots with those
    // files, which are then removed.
    rec.phase_b()
        .expect("CT61 the following admitted open converts");
    let state = rec.state();
    assert_eq!(
        state.owner_api_state,
        CredentialSlotFormatState::Migrated,
        "CT61 owner migrated"
    );
    assert_eq!(
        state.fleet_api_state,
        CredentialSlotFormatState::Migrated,
        "CT61 fleet migrated"
    );
    let listing = rec.secrets_listing();
    assert!(
        !listing.contains(&RST_K1.to_owned()) && !listing.contains(&RST_K2.to_owned()),
        "CT61 the restore-name files are removed: {listing:?}"
    );
    assert!(
        listing.contains(&CURSOR.to_owned()),
        "CT61 the cursor key stays"
    );
    // Late restore into a migrated Hub: the slots are migrated, so the members
    // are accepted under their restore names and removed at the next open.
    rec.restore(&export, &export_key)
        .expect("CT61.late a restore into a migrated Hub is accepted");
    assert!(
        rec.secrets_listing().contains(&RST_K1.to_owned()),
        "CT61.late the member is published"
    );
    rec.phase_b().expect("CT61.late the next open removes it");
    assert!(
        !rec.secrets_listing().contains(&RST_K1.to_owned()),
        "CT61.late the member is removed again"
    );

    // Case floor_variant (R50, R52, M2P-6, M-FLOOR): with the floor above the
    // last pre-C10 store version the same export is refused before anything is
    // written, with an old-name stand-in present in the secrets directory.
    seam::reset();
    let rec = Rec::real_pair(true, true);
    let secrets = rec.secrets_path();
    fs::create_dir(&secrets).unwrap();
    fs::set_permissions(&secrets, fs::Permissions::from_mode(0o700)).unwrap();
    let stand_in = format!(
        "{}-legacy.key",
        crate::runtime::name_test_support::forbidden_name()
    );
    write_private(&secrets.join(&stand_in), &[4u8; 32]);
    let snapshot = |dir: &Path| -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.file_name().into_string().unwrap(),
                    fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    };
    let before = snapshot(&secrets);
    let (_hold2, export2) = write_export(
        &rec,
        &export_key,
        Some(&legacy_key),
        Some(&cursor),
        Some(&fleet_key),
    );
    let mut rec = rec;
    seam::with(|s| s.floor = Some(RUNG));
    let refused = rec.restore(&export2, &export_key);
    assert!(
        refused.is_err(),
        "CT61.floor the old export is refused above the floor"
    );
    assert!(
        snapshot(&secrets) == before,
        "CT61.floor the secrets directory's listing and bytes are unchanged"
    );
    assert!(
        seam::with(|s| s
            .events
            .iter()
            .all(|e| !e.contains(&stand_in) || e.starts_with("list_returned:"))),
        "CT61.floor no operation on an old name"
    );
}

#[test]
fn m2p058_matching_precedes_publication() {
    // Case absent_directory_data_only_restore: with no secrets/ directory the
    // retained staging directory (mode 0700, only 0600 members) is published
    // without replacement; the keyring does not exist yet.
    seam::reset();
    let (legacy_key, fleet_key) = real_keys();
    let cursor = [0x5Au8; 32];
    let export_key = [0x44u8; 32];
    let mut rec = Rec::real_pair(true, true);
    let (_hold, export) = write_export(
        &rec,
        &export_key,
        Some(&legacy_key),
        Some(&cursor),
        Some(&fleet_key),
    );
    assert!(
        !rec.secrets_path().exists(),
        "m2p058.absent the data directory has no secrets/"
    );
    // A member that does not authenticate its slot's row is refused before the
    // staging directory is created.
    let (_hold_bad, bad) = write_export(
        &rec,
        &export_key,
        Some(&legacy_key),
        Some(&cursor),
        Some(&[7u8; 32]),
    );
    assert!(
        matches!(
            rec.restore(&bad, &export_key),
            Err(CredentialRecoveryError::CatalogueMismatch)
        ),
        "m2p058.absent a wrong Fleet member is refused"
    );
    assert!(
        !rec.secrets_path().exists(),
        "m2p058.absent matching precedes publication: nothing was created"
    );
    // The same holds for the Legacy member.
    let (_hold_bad_owner, bad_owner) = write_export(
        &rec,
        &export_key,
        Some(&[7u8; 32]),
        Some(&cursor),
        Some(&fleet_key),
    );
    assert!(
        matches!(
            rec.restore(&bad_owner, &export_key),
            Err(CredentialRecoveryError::CatalogueMismatch)
        ),
        "m2p058.absent a wrong Legacy member is refused"
    );
    assert!(
        !rec.secrets_path().exists(),
        "m2p058.absent matching precedes publication: nothing was created for the Legacy member"
    );
    // Readback: a pre-upgrade member altered right after the no-replace
    // publication is caught by the post-publication readback.
    {
        let mut tampered = Rec::real_pair(true, true);
        let (_hold_t, export_t) = write_export(
            &tampered,
            &export_key,
            Some(&legacy_key),
            Some(&cursor),
            Some(&fleet_key),
        );
        let member = tampered.secrets_path().join(RST_K1);
        seam::with(|s| {
            s.boundary = Some(Box::new(move |name| {
                if name == "restore:absent_published" {
                    write_private(&member, &[0xEEu8; 32]);
                }
                true
            }));
        });
        let refused = tampered.restore(&export_t, &export_key);
        seam::with(|s| s.boundary = None);
        assert!(
            matches!(refused, Err(CredentialRecoveryError::CatalogueMismatch)),
            "m2p058.absent_readback an altered pre-upgrade member is refused after publication: {refused:?}"
        );
    }
    // Readback of a member whose slot is not pending: only the byte comparison
    // with the payload can catch an alteration after publication.
    {
        let mut tampered = Rec::real_pair(false, true);
        let (_hold_t, export_t) = write_export(
            &tampered,
            &export_key,
            Some(&legacy_key),
            Some(&cursor),
            Some(&fleet_key),
        );
        let member = tampered.secrets_path().join(RST_K1);
        seam::with(|s| {
            s.boundary = Some(Box::new(move |name| {
                if name == "restore:absent_published" {
                    write_private(&member, &[0xEEu8; 32]);
                }
                true
            }));
        });
        let refused = tampered.restore(&export_t, &export_key);
        seam::with(|s| s.boundary = None);
        assert!(
            matches!(refused, Err(CredentialRecoveryError::CatalogueMismatch)),
            "m2p058.absent_readback an altered member of a slot with no row is refused: {refused:?}"
        );
    }
    let report = rec
        .restore(&export, &export_key)
        .expect("m2p058.absent the data-only restore publishes");
    assert_eq!(
        mode(&rec.secrets_path()),
        0o700,
        "m2p058.absent secrets/ is mode 0700"
    );
    assert_eq!(
        rec.secrets_listing(),
        vec![CURSOR.to_owned(), RST_K2.to_owned(), RST_K1.to_owned()],
        "m2p058.absent only the payload members (sorted)"
    );
    for name in [CURSOR, RST_K1, RST_K2] {
        assert_eq!(
            mode(&rec.secrets_path().join(name)),
            0o600,
            "m2p058.absent {name} is 0600"
        );
    }
    assert!(
        report.pre_upgrade_owner_api_key_included && report.pre_upgrade_fleet_api_key_included,
        "m2p058.absent report fields"
    );
    let staging_left: Vec<_> = fs::read_dir(rec.data())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".teslatlas-credential-restore-")
        })
        .collect();
    assert!(
        staging_left.is_empty(),
        "m2p058.absent no staging directory is left on success"
    );
    assert_eq!(
        fs::read(rec.secrets_path().join(CURSOR)).unwrap(),
        cursor.to_vec(),
        "m2p058.absent cursor key restored"
    );

    // Case existing_directory_stage_sync_before_rename_crash: a migrated Hub
    // whose secrets/ exists. Publication stages each missing member through a
    // synced 0600 .hub-restore-<UUID>.tmp file and renames it without
    // replacement; the run is stopped after one staged file is synced and
    // before its rename. The resumed ordinary open removes exactly that
    // staging file, with the directory barrier before any marker, and leaves
    // cursor, keyring and unrelated entries alone.
    seam::reset();
    let mut rec = Rec::fresh();
    rec.phase_b()
        .expect("m2p058.existing Phase B on a fresh (complete) store");
    let secrets = rec.secrets_path();
    write_private(&secrets.join("credential-keys"), &[8u8; 88]);
    write_private(&secrets.join("unrelated.txt"), b"unrelated entry");
    let installation = rec.store.installation_id().unwrap();
    let export_key = [0x33u8; 32];
    let legacy_key = [0xA1u8; 32];
    let fleet_key = [0xB2u8; 32];
    let cursor_key = [0xC3u8; 32];
    let dir = crate::private_tempdir().unwrap();
    let export = dir.path().join("export.bin");
    write_private(
        &export,
        &build_export(
            &export_key,
            installation,
            Some(&legacy_key),
            Some(&cursor_key),
            Some(&fleet_key),
        ),
    );
    let untouched = |rec: &Rec| -> Vec<(String, Vec<u8>)> {
        ["credential-keys", "unrelated.txt"]
            .iter()
            .map(|n| {
                (
                    (*n).to_owned(),
                    fs::read(rec.secrets_path().join(n)).unwrap(),
                )
            })
            .collect()
    };
    let before_untouched = untouched(&rec);
    // Conflict preflight: a different target refuses before any write.
    write_private(&secrets.join(CURSOR), &[0xEEu8; 32]);
    let listing_before = rec.secrets_listing();
    assert!(
        rec.restore(&export, &export_key).is_err(),
        "m2p058.existing a different target refuses"
    );
    assert_eq!(
        rec.secrets_listing(),
        listing_before,
        "m2p058.existing the refusal wrote nothing"
    );
    assert_eq!(
        fs::read(secrets.join(CURSOR)).unwrap(),
        vec![0xEEu8; 32],
        "m2p058.existing the different target is untouched"
    );
    // An identical target stays untouched and does not block the others.
    write_private(&secrets.join(CURSOR), &cursor_key);
    seam::with(|s| {
        s.boundary = Some(Box::new(|name| !name.starts_with("restore:staged_synced:")));
    });
    let crashed = rec.restore(&export, &export_key);
    assert!(
        crashed.is_err(),
        "m2p058.existing the run stops after a staged file is synced"
    );
    seam::with(|s| s.boundary = None);
    let staged: Vec<String> = rec
        .secrets_listing()
        .into_iter()
        .filter(|n| n.starts_with(".hub-restore-") && n.ends_with(".tmp"))
        .collect();
    assert_eq!(
        staged.len(),
        1,
        "m2p058.existing exactly one staged file is left: {:?}",
        rec.secrets_listing()
    );
    assert!(
        !rec.secrets_listing().contains(&RST_K1.to_owned())
            && !rec.secrets_listing().contains(&RST_K2.to_owned()),
        "m2p058.existing no RST target was published"
    );
    assert_eq!(
        mode(&secrets.join(&staged[0])),
        0o600,
        "m2p058.existing the staged file is 0600"
    );
    assert_eq!(
        fs::read(secrets.join(CURSOR)).unwrap(),
        cursor_key.to_vec(),
        "m2p058.existing the identical cursor target is untouched"
    );
    // A near-match must survive the cleanup.
    let near = ".hub-restore-not-a-uuid.tmp";
    write_private(&secrets.join(near), b"near match");
    seam::with(|s| s.events.clear());
    rec.phase_b()
        .expect("m2p058.existing the resumed ordinary open runs the R49 cleanup");
    let listing = rec.secrets_listing();
    assert!(
        !listing.contains(&staged[0]),
        "m2p058.existing the exact staging file is removed"
    );
    assert!(
        listing.contains(&near.to_owned()),
        "m2p058.existing a near match stays"
    );
    let events = seam::with(|s| s.events.clone());
    let removal = events
        .iter()
        .position(|e| *e == format!("remove:{}", staged[0]));
    let barrier = events.iter().rposition(|e| e == "directory_barrier");
    assert!(
        matches!((removal, barrier), (Some(r), Some(b)) if r < b),
        "m2p058.existing the secrets-directory barrier follows the removal: {events:?}"
    );
    let after_untouched = untouched(&rec);
    assert_eq!(
        before_untouched, after_untouched,
        "m2p058.existing keyring and unrelated entries are unchanged"
    );
    // CT58-style byte absence of the staged earlier key after the cleanup.
    for file in fs::read_dir(rec.data()).unwrap() {
        let path = file.unwrap().path();
        if path.is_file() {
            let bytes = fs::read(&path).unwrap();
            assert!(
                !bytes.windows(32).any(|w| w == legacy_key || w == fleet_key),
                "m2p058.existing no earlier key bytes in {path:?}"
            );
        }
    }
}

/// The C10 Fleet-only round trip (M-owned helper for the retained test
/// `fleet_only_credentials_round_trip_with_the_dedicated_key`): restoring a
/// flag-4 pre-upgrade export writes `pre-upgrade-fleet-api.key` at 0600 and
/// reports the Fleet pre-upgrade field; the next admitted open migrates the
/// Fleet slot and removes the file; a later export carries flags 0x02 with
/// both pre-upgrade fields false. Hand-built store, real OLD-D ciphertext.
pub(super) fn fleet_only_c10_round_trip() {
    seam::reset();
    let (_, fleet_key) = real_keys();
    let export_key = [0x55u8; 32];
    let mut rec = Rec::real_pair(false, true);
    rec.phase_b().expect("Phase B first creates the keyring");
    assert_eq!(
        rec.state().fleet_api_state,
        CredentialSlotFormatState::Pending,
        "the Fleet slot waits for a key"
    );
    let (_hold, export) = write_export(&rec, &export_key, None, None, Some(&fleet_key));
    let report = rec
        .restore(&export, &export_key)
        .expect("the flag-4 export restores");
    let restored = rec.secrets_path().join(RST_K2);
    assert_eq!(mode(&restored), 0o600, "pre-upgrade-fleet-api.key is 0600");
    assert_eq!(
        fs::read(&restored).unwrap(),
        fleet_key,
        "the Fleet member is restored byte for byte"
    );
    assert!(
        report.pre_upgrade_fleet_api_key_included,
        "preUpgradeFleetApiKeyIncluded is true"
    );
    assert!(
        !report.pre_upgrade_owner_api_key_included,
        "preUpgradeOwnerApiKeyIncluded is false"
    );
    rec.phase_b()
        .expect("the next admitted open migrates the Fleet slot");
    assert_eq!(
        rec.state().fleet_api_state,
        CredentialSlotFormatState::Migrated,
        "the Fleet slot is migrated"
    );
    assert!(!restored.exists(), "the restored Fleet key file is removed");
    // The migrated Fleet pair is reopened and both recovered token values are
    // compared with the approved synthetic input (fixture item 1's fleet_file
    // case), so a well-formed pair holding other values fails here.
    let pair = rec
        .conn
        .load_credential_pair(crate::auth::credential_envelope::CredentialSlot::FleetApi)
        .unwrap()
        .expect("the migrated Fleet row exists");
    let keys = crate::auth::credential_keys::load_keyring(
        &open_validated(&rec.secrets_path(), false).unwrap(),
    )
    .unwrap()
    .expect("the keyring exists");
    let (access, refresh) = pair
        .open(&keys)
        .expect("the migrated Fleet pair opens under the keyring");
    let (want_access, want_refresh) = real_plaintexts("fleet_file");
    assert!(
        access.as_slice() == want_access.as_slice()
            && refresh.as_slice() == want_refresh.as_slice(),
        "the recovered Fleet access and refresh tokens equal the approved synthetic input"
    );
    write_private(&rec.secrets_path().join(CURSOR), &[0x5Au8; 32]);
    let out = crate::private_tempdir().unwrap();
    let destination = out.path().join("export.bin");
    let report = export_credentials(&rec.store, rec.data(), &destination, &export_key)
        .expect("a subsequent export succeeds");
    let (flags, length) = export_facts(&destination, &export_key);
    assert_eq!(
        (flags, length),
        (0x02, 32),
        "the subsequent export carries flags 0x02 and only the cursor key"
    );
    assert!(
        !report.pre_upgrade_owner_api_key_included && !report.pre_upgrade_fleet_api_key_included,
        "both pre-upgrade report fields are false"
    );
}
