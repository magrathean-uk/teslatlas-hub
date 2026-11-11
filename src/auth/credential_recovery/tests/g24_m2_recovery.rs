// SPDX-License-Identifier: AGPL-3.0-only

//! Part M recovery tests (spec CT60, CT61, R52 with errata E58 and the
//! rulings M2P-58, M2P-60). Synthetic data only, in private temporary
//! directories.

use super::super::*;
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
            exclusion: &VerifiedExclusion,
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

/// Finding 5 (M02-T02 round 3): recovery refuses a bundle carrying a
/// pre-upgrade Owner API or Fleet key member before any pending pair is
/// decrypted and before any member is staged or published, even when the
/// member would open the stored pre-upgrade row. (The pre-upgrade restore and
/// conversion round trips CT61, M2P-58 and the C10 Fleet-only round trip
/// tested that removed behaviour.)
#[test]
fn source_credential_disabled_recovery_refuses_pre_upgrade_members_unread() {
    seam::reset();
    let (legacy_key, fleet_key) = real_keys();
    let cursor = [0x5Au8; 32];
    let export_key = [0x33u8; 32];
    let mut rec = Rec::real_pair(true, true);
    rec.phase_b().expect("the restoring process's Phase B");
    let listing_before = rec.secrets_listing();
    seam::reset();
    for (legacy, fleet) in [
        (Some(legacy_key.as_slice()), None),
        (None, Some(fleet_key.as_slice())),
        (Some(legacy_key.as_slice()), Some(fleet_key.as_slice())),
    ] {
        let (_hold, export) = write_export(&rec, &export_key, legacy, Some(&cursor), fleet);
        let refused = rec.restore(&export, &export_key);
        assert!(
            matches!(&refused, Err(CredentialRecoveryError::InvalidPayload(reason)) if reason.starts_with("source_credential_disabled")),
            "legacy member refused: {refused:?}"
        );
        assert_eq!(rec.secrets_listing(), listing_before, "nothing was staged or published");
    }
    let events = seam::with(|state| state.events.clone());
    assert!(!events.iter().any(|event| event.starts_with("restore:")), "no member staged: {events:?}");
    let state = rec.state();
    assert!(
        state.owner_api_state == CredentialSlotFormatState::Pending
            && state.fleet_api_state == CredentialSlotFormatState::Pending,
        "the refused pre-upgrade rows stay unconverted"
    );
}
