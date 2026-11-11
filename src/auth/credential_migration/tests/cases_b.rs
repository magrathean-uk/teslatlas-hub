// SPDX-License-Identifier: AGPL-3.0-only

// Included into `g24_m2_m` (src/auth/credential_migration/tests.rs): the second
// half of the component tests, kept apart so no file passes 3,000 lines.

// ---------------------------------------------------------------- CT56

#[test]
fn ct056_version_guard() {
    // Case version_guard: a store whose user_version is SCHEMA_VERSION + 1 is
    // refused through the same admit_store_version branch, with no writes.
    seam::reset();
    let mut env = Env::fresh();
    let newer = crate::db::SCHEMA_VERSION + 1;
    env.db().pragma_update(None, "user_version", newer).unwrap();
    env.conn = None;
    env.store = None;
    let bytes = |env: &Env| -> (Vec<u8>, Vec<u8>) {
        (
            fs::read(env.data().join("hub.sqlite")).unwrap(),
            fs::read(env.data().join("hub.sqlite-wal")).unwrap_or_default(),
        )
    };
    let before = bytes(&env);
    let refused = HubStore::initialize(env.data());
    assert!(
        matches!(refused, Err(StoreError::UnsupportedSchema(v)) if v == newer),
        "CT56.version_guard UnsupportedSchema(rung + 1): {:?}",
        refused.as_ref().err()
    );
    assert_eq!(bytes(&env), before, "CT56.version_guard no writes");

    // Case floor_variant on a hand-built pre-upgrade store (fake decrypt seam).
    seam::reset();
    {
        let key = key32(1);
        let row = PriorRow::fake(&key, "access-0001", "refresh-0001", Some(uuid_n(0xaa)));
        let mut env = Env::hand_built(Some(&row), None, false);
        env.put(REL_K1, &key);
        let stand_in = stand_in_name("legacy.key");
        env.put(&stand_in, &key32(2));
        let observer = Connection::open(env.data().join("hub.sqlite")).unwrap();
        let before: i64 = observer
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        seam::with(|s| s.floor = Some(RUNG));
        let refused = env.run();
        assert!(
            matches!(
                refused,
                Err(CredentialMigrationError::Store(
                    StoreError::UnsupportedSchema(78)
                ))
            ),
            "CT56.hand_built_floor refused as a below-floor store: {refused:?}"
        );
        let after: i64 = observer
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after, "CT56.hand_built_floor no writes");
        assert!(
            env.keys.is_none() && !env.listing().contains(&"credential-keys".to_owned()),
            "CT56.hand_built_floor no keyring created"
        );
        assert!(
            seam::with(|s| s.decrypt_log.is_empty()),
            "CT56.hand_built_floor the decrypt seam records no attempt"
        );
        assert!(
            seam::with(|s| s
                .events
                .iter()
                .all(|e| !e.starts_with("inspect:") && !e.contains(&stand_in)
                    || e.starts_with("list_returned:"))),
            "CT56.hand_built_floor no candidate read and no operation on an old name"
        );
        assert_eq!(
            fs::read(env.secrets_path().join(REL_K1)).unwrap(),
            key,
            "CT56.hand_built_floor REL-K1 byte for byte"
        );
        seam::with(|s| s.floor = None);
    }
    // Case floor_variant (R50, M2P-6): on fixture item 1's catalogue, with the
    // floor raised above the last pre-C10 store version through M's own floor
    // read, Phase B refuses before any mutation, decryption or candidate read
    // and never touches an old name.
    seam::reset();
    let vector = catalogue_vector("floor");
    let legacy_key = vector_bytes(&vector["legacy_current"], "key_hex");
    let fleet_key = vector_bytes(&vector["fleet_current"], "key_hex");
    let mut env = Env::from_catalogue("catalogue-floor.sqlite");
    env.put(REL_K1, &legacy_key);
    env.put(FLEET_K4, &fleet_key);
    let stand_in = stand_in_name("legacy.key");
    env.put(&stand_in, &key32(2));
    let observer = Connection::open(env.data().join("hub.sqlite")).unwrap();
    let before: i64 = observer
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    seam::with(|s| s.floor = Some(RUNG));
    let refused = env.run();
    assert!(
        matches!(
            refused,
            Err(CredentialMigrationError::Store(
                StoreError::UnsupportedSchema(78)
            ))
        ),
        "CT56.floor refused as a below-floor store: {refused:?}"
    );
    let after: i64 = observer
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after, "CT56.floor no writes");
    assert!(
        env.keys.is_none() && !env.listing().contains(&"credential-keys".to_owned()),
        "CT56.floor no keyring created"
    );
    assert!(
        seam::with(|s| s.decrypt_log.is_empty()),
        "CT56.floor the decrypt seam records no attempt"
    );
    assert_eq!(
        fs::read(env.secrets_path().join(REL_K1)).unwrap(),
        legacy_key,
        "CT56.floor REL-K1 byte for byte"
    );
    assert!(
        seam::with(|s| s
            .events
            .iter()
            .all(|e| !e.contains(&stand_in) || e.starts_with("list_returned:"))),
        "CT56.floor no operation on an old name"
    );
    assert!(
        seam::with(|s| s.events.iter().all(|e| !e.starts_with("inspect:"))),
        "CT56.floor no candidate read"
    );
    // M02-T02: at the landed floor the pre-upgrade pairs are refused unread,
    // so the store no longer converts.
    seam::with(|s| s.floor = None);
}

// ---------------------------------------------------------------- CT58, CT59

fn vectors() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/credential-pre-upgrade-vectors.json");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn catalogue_vector(id: &str) -> serde_json::Value {
    vectors()["catalogues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap()
        .clone()
}

fn vector_bytes(v: &serde_json::Value, field: &str) -> Vec<u8> {
    unhex(v[field].as_str().unwrap())
}

/// Every earlier-format value of a catalogue vector: ciphertexts (current and
/// earlier) and the keys that open them.
fn earlier_values(catalogue: &serde_json::Value) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for part in [
        "legacy_current",
        "fleet_current",
        "earlier_replaced_deleted",
    ] {
        let v = &catalogue[part];
        out.push((
            format!("{part}.access"),
            vector_bytes(v, "access_ciphertext_hex"),
        ));
        out.push((
            format!("{part}.refresh"),
            vector_bytes(v, "refresh_ciphertext_hex"),
        ));
        out.push((format!("{part}.key"), vector_bytes(v, "key_hex")));
    }
    out
}

fn scan_for_earlier(env: &Env, values: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut found = BTreeSet::new();
    for (path, bytes) in env.all_files() {
        for (label, value) in values {
            if contains(&bytes, value) || has_16_byte_run(&bytes, value) {
                found.insert(format!(
                    "{label} in {}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    found.into_iter().collect()
}

// ---------------------------------------------------------------- CT93, CT101

#[test]
fn ct101_old_names_never_touched_phase_b() {
    seam::reset();
    let owner_key = key32(1);
    let fleet_key = key32(7);
    let owner = PriorRow::fake(
        &owner_key,
        "access-0001",
        "refresh-0001",
        Some(uuid_n(0xaa)),
    );
    let fleet = PriorRow::fake(&fleet_key, "access-0002", "refresh-0002", None);
    let mut env = Env::hand_built(Some(&owner), Some(&fleet), true);
    env.put(REL_K1, &owner_key);
    env.put(FLEET_K4, &fleet_key);
    // Stand-ins for OLD-K1, OLD-K2 and OLD-K3's pattern in secrets/, OLD-S1 and
    // OLD-S2 in a session directory and OLD-M in the config directory.
    let secrets_names = [
        stand_in_name("k1.key"),
        stand_in_name("k2.key"),
        stand_in_name("k3.tmp"),
    ];
    for name in &secrets_names {
        env.put(name, &key32(90));
    }
    let session = env.data().join("session");
    let config = env.data().join("config");
    fs::create_dir_all(&session).unwrap();
    fs::create_dir_all(&config).unwrap();
    let outside = [
        session.join(stand_in_name("s1")),
        session.join(stand_in_name("s2")),
        config.join(stand_in_name("m")),
    ];
    for path in &outside {
        write_private(path, b"synthetic-stand-in");
    }
    let all: Vec<PathBuf> = secrets_names
        .iter()
        .map(|n| env.secrets_path().join(n))
        .chain(outside.iter().cloned())
        .collect();
    let fingerprint = |paths: &[PathBuf]| -> Vec<(Vec<u8>, u64, i64, i64)> {
        paths
            .iter()
            .map(|p| {
                let m = fs::metadata(p).unwrap();
                (fs::read(p).unwrap(), m.ino(), m.mtime(), m.mtime_nsec())
            })
            .collect()
    };
    let before = fingerprint(&all);
    env.run().expect("CT101 (a) Phase B refuses both pre-upgrade slots");
    env.reopen();
    env.run().expect("CT101 (a) a second open");
    assert_eq!(
        fingerprint(&all),
        before,
        "CT101 every old-named file keeps bytes, inode and mtime"
    );
    let attempts: Vec<Vec<u8>> =
        seam::with(|s| s.decrypt_log.iter().map(|(_, k)| k.clone()).collect());
    // M02-T02: the pre-upgrade pairs are refused unread, so nothing is decrypted.
    assert_eq!(
        attempts,
        Vec::<Vec<u8>>::new(),
        "CT101 Phase B decrypts nothing"
    );
    for (index, name) in secrets_names.iter().enumerate() {
        assert!(
            seam::with(|s| s.events.iter().all(|e| !(e.starts_with("inspect:")
                || e.starts_with("remove:")
                || e.starts_with("read:"))
                || !e.contains(name))),
            "CT101 no inspect, read or remove of old-name stand-in #{index} (alias set OLD-K1, OLD-K2, OLD-K3)"
        );
    }
    // M02-T02: a refused store never completes, so no cleanup listing runs.
}

// ---------------------------------------------------------------- CT87, CT90, CT91

// ---------------------------------------------------------------- CT62

use crate::data_recovery::{
    BackupWait, CAR_CONTROL_SECRET_FILES, CAR_CONTROL_SECRET_TABLES, DataRecoveryError,
    PRE_UPGRADE_SECRET_TABLES, create_data_backup, credential_backup_wait, restore_data_backup,
};

fn both_slots_complete() -> Env {
    // M02-T02: no pre-upgrade pair converts, so the complete store holds an
    // own-login Owner pair and a stored Fleet row written directly.
    use crate::auth::credential_vault::{CredentialEntry, fixtures::seed_entry};
    let mut env = Env::fresh();
    seed_entry(env.data(), CredentialEntry::own_login_fixture(
        zeroize::Zeroizing::new("access-0001".to_owned()),
        zeroize::Zeroizing::new("refresh-0001".to_owned()),
    ).expect("CT62 setup: own-login pair"));
    seed_entry(env.data(), CredentialEntry::fleet_supplied(
        zeroize::Zeroizing::new("access-0002".to_owned()),
        zeroize::Zeroizing::new("refresh-0002".to_owned()),
        "client-0001".into(),
        crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    ).expect("CT62 setup: Fleet row"));
    assert!(is_complete(&env.state()), "CT62 setup: complete store");
    env
}

fn listing_of(dir: &Path) -> Vec<(String, u64)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, u64)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let name = format!("{prefix}/{}", entry.file_name().to_string_lossy());
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                walk(&entry.path(), &name, out);
            } else {
                out.push((name, meta.len()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out);
    out.sort();
    out
}

/// An unpublished leftover of an earlier run (the backup's own staging name):
/// a refused run must neither write beside it nor remove it (M2P-59).
fn plant_leftover(parent: &Path) -> PathBuf {
    let staging = parent.join(format!(
        ".teslatlas-data-recovery-{}.staging",
        uuid_n(0x7001)
    ));
    fs::create_dir_all(staging.join("data")).unwrap();
    for dir in [&staging, &staging.join("data")] {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    write_private(
        &staging.join("data/hub.sqlite"),
        b"unpublished leftover bytes",
    );
    staging
}

fn payload_conn(generation: &Path) -> Connection {
    Connection::open_with_flags(
        generation.join("data/hub.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

fn refused(result: Result<crate::data_recovery::DataRecoveryReport, DataRecoveryError>) -> bool {
    matches!(
        result,
        Err(DataRecoveryError::Store(
            StoreError::BackupWaitsForCredentialCleanup
        ))
    )
}

#[test]
pub(in crate::auth) fn ct062_data_backup() {
    // Case constants (R53): the exposed classification.
    assert_eq!(
        CAR_CONTROL_SECRET_TABLES,
        &["credential_pairs"],
        "CT62.constants car-control tables"
    );
    assert_eq!(
        PRE_UPGRADE_SECRET_TABLES,
        &["legacy_owner_tokens", "fleet_tokens"],
        "CT62.constants pre-upgrade tables"
    );
    assert_eq!(
        CAR_CONTROL_SECRET_FILES,
        &[
            "credential-keys",
            "fleet-command-key.pem",
            "fleet-proxy-tls-key.pem"
        ],
        "CT62.constants the keyring and the two PEM names of the operator-provisioned Fleet keys"
    );
    assert!(
        matches!(
            credential_backup_wait(&crate::db::CredentialBackupAdmission::Pending),
            Some(BackupWait::Pending)
        ) && matches!(
            credential_backup_wait(&crate::db::CredentialBackupAdmission::CleanupPending {
                needed_bytes: Some(7)
            }),
            Some(BackupWait::PurgePending {
                needed_bytes: Some(7)
            })
        ) && matches!(
            credential_backup_wait(&crate::db::CredentialBackupAdmission::Blocked {
                owner_api: Some(CredentialMigrationBlockReason::OldKeyUnavailable),
                fleet_api: None
            }),
            Some(BackupWait::Blocked)
        ) && credential_backup_wait(&crate::db::CredentialBackupAdmission::Clean).is_none(),
        "CT62.constants BackupWait mapping of the cached admission"
    );

    // Case a_complete_store: both slots, the kept payload is the only catalogue
    // file the run creates.
    seam::reset();
    let env = both_slots_complete();
    let live_envelopes: Vec<Vec<u8>> = {
        let mut statement = env
            .db()
            .prepare("SELECT access_envelope, refresh_envelope FROM credential_pairs")
            .unwrap();
        statement
            .query_map([], |r| {
                Ok([r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?])
            })
            .unwrap()
            .flat_map(|r| r.unwrap())
            .collect()
    };
    assert_eq!(
        live_envelopes.len(),
        4,
        "CT62.a four envelopes in the live store"
    );
    let live_fences = env.legacy_fences();
    let live_attempts = env.count("SELECT COUNT(*) FROM credential_refresh_attempts");
    let live_dir_before = listing_of(env.data());
    let dest = crate::private_tempdir().unwrap();
    let generation = dest.path().join("backup");
    create_data_backup(env.store.as_ref().unwrap(), &generation)
        .expect("CT62.a the backup succeeds");
    let created: Vec<_> = listing_of(dest.path())
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        created
            .iter()
            .filter(|n| n.ends_with(".sqlite") || n.contains("hub.sqlite"))
            .count(),
        1,
        "CT62.a the only catalogue file the run created is the kept payload: {created:?}"
    );
    let live_dir_after = listing_of(env.data());
    let names = |l: &[(String, u64)]| l.iter().map(|(n, _)| n.clone()).collect::<BTreeSet<_>>();
    assert_eq!(
        names(&live_dir_before),
        names(&live_dir_after),
        "CT62.a no file created in the data directory"
    );
    let payload = payload_conn(&generation);
    let payload_count = |sql: &str| -> i64 { payload.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        payload_count("SELECT COUNT(*) FROM credential_pairs"),
        0,
        "CT62.a no credential_pairs row"
    );
    assert_eq!(
        payload_count("SELECT COUNT(*) FROM credential_sign_ins"),
        0,
        "CT62.a no credential_sign_ins row"
    );
    assert_eq!(
        payload_count(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('legacy_owner_tokens','fleet_tokens')"
        ),
        0,
        "CT62.a no OLD-T1 or OLD-T2 table"
    );
    assert_eq!(
        payload_count("SELECT COUNT(*) FROM credential_legacy_fences"),
        live_fences.len() as i64,
        "CT62.a legacy fences kept"
    );
    assert_eq!(
        payload_count("SELECT COUNT(*) FROM credential_refresh_attempts"),
        live_attempts,
        "CT62.a attempts kept"
    );
    let identities: Vec<(String, String, i64)> = payload
        .prepare("SELECT lower(legacy_generation), identity, fenced_at_ms FROM credential_legacy_fences ORDER BY 1")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(identities, live_fences, "CT62.a fence identities kept");
    drop(payload);
    for side in ["-wal", "-shm", "-journal"] {
        assert!(
            !generation.join(format!("data/hub.sqlite{side}")).exists(),
            "CT62.a no {side} file beside the payload"
        );
    }
    let payload_bytes = fs::read(generation.join("data/hub.sqlite")).unwrap();
    for (i, envelope) in live_envelopes.iter().enumerate() {
        assert!(
            !contains(&payload_bytes, envelope),
            "CT62.a envelope {i} of the source store is absent from the payload"
        );
    }
    let gen_a = generation.clone();
    let _keep_a = dest;

    // Cases b, c, d: refused with backup_waits_for_credential_cleanup; nothing
    // is written at or removed from the destination.
    let wait_of = |env: &Env| {
        let snapshot = env.store.as_ref().unwrap().begin_backup_snapshot().unwrap();
        credential_backup_wait(&snapshot.credential_backup_admission())
    };
    // (b) pending: fixture catalogue, keyring missing.
    seam::reset();
    let pending_row = PriorRow::fake(&key32(1), "access-0001", "refresh-0001", Some(uuid_n(0xaa)));
    let env_b = Env::hand_built(Some(&pending_row), None, false);
    let dest_b = crate::private_tempdir().unwrap();
    let leftover_b = plant_leftover(dest_b.path());
    let before = listing_of(dest_b.path());
    assert!(
        refused(create_data_backup(
            env_b.store.as_ref().unwrap(),
            &dest_b.path().join("backup")
        )),
        "CT62.b pending is refused"
    );
    assert_eq!(
        listing_of(dest_b.path()),
        before,
        "CT62.b the destination's file list is identical before and after"
    );
    assert!(
        leftover_b.join("data/hub.sqlite").exists(),
        "CT62.b a refused run removes no earlier leftover"
    );
    assert!(
        matches!(wait_of(&env_b), Some(BackupWait::Pending)),
        "CT62.b names pending"
    );
    // (c) blocked: the pre-upgrade Legacy pair is refused unread (M02-T02).
    seam::reset();
    let legacy = PriorRow::fake(&key32(1), "access-0001", "refresh-0001", Some(uuid_n(0xaa)));
    let mut env_c = Env::hand_built(Some(&legacy), None, false);
    env_c.put(REL_K1, &key32(99));
    env_c.run().unwrap();
    let dest_c = crate::private_tempdir().unwrap();
    let before = listing_of(dest_c.path());
    assert!(
        refused(create_data_backup(
            env_c.store.as_ref().unwrap(),
            &dest_c.path().join("backup")
        )),
        "CT62.c blocked is refused"
    );
    assert_eq!(
        listing_of(dest_c.path()),
        before,
        "CT62.c destination unchanged"
    );
    assert!(
        matches!(wait_of(&env_c), Some(BackupWait::Blocked)),
        "CT62.c names blocked"
    );
    let snapshot = env_c
        .store
        .as_ref()
        .unwrap()
        .begin_backup_snapshot()
        .unwrap();
    assert_eq!(
        snapshot.credential_backup_admission(),
        crate::db::CredentialBackupAdmission::Blocked {
            owner_api: Some(CredentialMigrationBlockReason::OldPairInvalid),
            fleet_api: None
        },
        "CT62.c the refused pair is blocked (M02-T02)"
    );
    drop(snapshot);
    // (d) cleanup_pending is gone (M02-T02): no store converts, so no purge
    // is ever deferred.

    // Case e_interruption: a run stopped after the copy and before the seal
    // publishes no generation; the next run removes the leftover first.
    seam::reset();
    let env_e = both_slots_complete();
    let dest_e = crate::private_tempdir().unwrap();
    let target = dest_e.path().join("backup");
    seam::with(|s| {
        s.payload_hook = Some(Box::new(|stage, _| {
            if stage == "after_copy" {
                seam::PayloadAction::Crash
            } else {
                seam::PayloadAction::Continue
            }
        }));
    });
    assert!(
        create_data_backup(env_e.store.as_ref().unwrap(), &target).is_err(),
        "CT62.e the stopped run errors"
    );
    assert!(!target.exists(), "CT62.e no generation is published");
    let leftovers: Vec<_> = fs::read_dir(dest_e.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(
        leftovers.len(),
        1,
        "CT62.e the unpublished payload is left as a crash would leave it"
    );
    let leftover_bytes =
        fs::read(leftovers[0].join("data/hub.sqlite")).expect("CT62.e leftover payload");
    assert!(
        !contains(&leftover_bytes, b"access-0001") && !contains(&leftover_bytes, b"refresh-0001"),
        "CT62.e the leftover holds no plaintext token"
    );
    seam::with(|s| s.payload_hook = None);
    create_data_backup(env_e.store.as_ref().unwrap(), &target)
        .expect("CT62.e the next run succeeds");
    assert!(
        !leftovers[0].exists(),
        "CT62.e the leftover was removed; remaining: {:?}",
        listing_of(&leftovers[0])
    );
    assert!(
        target.exists(),
        "CT62.e the next run published its generation"
    );
    // A failed seal transaction and a copy whose state row is marked
    // purge_required = 1: the error returns and the payload is gone.
    for case in ["seal_fails", "state_row_marked"] {
        seam::reset();
        let dest = crate::private_tempdir().unwrap();
        let hook_case = case;
        seam::with(|s| {
            s.payload_hook = Some(Box::new(move |stage, payload| {
                if hook_case == "seal_fails" && stage == "seal_transaction" {
                    return seam::PayloadAction::Fail;
                }
                if hook_case == "state_row_marked" && stage == "after_copy" {
                    let raw = Connection::open(payload.join("hub.sqlite")).unwrap();
                    raw.execute("UPDATE credential_format_state SET purge_required=1, wal_scrubbed=0, completed_at_ms=NULL", []).unwrap();
                }
                seam::PayloadAction::Continue
            }));
        });
        let result = create_data_backup(env_e.store.as_ref().unwrap(), &dest.path().join("backup"));
        assert!(result.is_err(), "CT62.e.{case} the backup returns an error");
        assert!(
            listing_of(dest.path()).is_empty() && !dest.path().join("backup").exists(),
            "CT62.e.{case} the payload and its side files are gone: {:?}",
            listing_of(dest.path())
        );
    }

    // Case leftover_symlink (R53 step 4): a leftover staging entry whose data
    // child is a symbolic link pointing outside the destination is skipped, and
    // nothing outside the destination is touched.
    {
        seam::reset();
        let dest = crate::private_tempdir().unwrap();
        let outside = crate::private_tempdir().unwrap();
        let names = [
            "hub.sqlite",
            "hub.sqlite-wal",
            "hub.sqlite-shm",
            "hub.sqlite-journal",
        ];
        for name in names {
            write_private(
                &outside.path().join(name),
                format!("outside {name}").as_bytes(),
            );
        }
        let staging = dest.path().join(format!(
            ".teslatlas-data-recovery-{}.staging",
            uuid_n(0x7002)
        ));
        fs::create_dir(&staging).unwrap();
        fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(outside.path(), staging.join("data")).unwrap();
        create_data_backup(env_e.store.as_ref().unwrap(), &dest.path().join("backup"))
            .expect("CT62.leftover_symlink the backup succeeds beside the symlinked leftover");
        for name in names {
            assert!(
                fs::read(outside.path().join(name)).ok().as_deref()
                    == Some(format!("outside {name}").as_bytes()),
                "CT62.leftover_symlink a file outside the destination is untouched"
            );
        }
        assert!(
            fs::symlink_metadata(staging.join("data"))
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false),
            "CT62.leftover_symlink the symlinked data child is skipped, not followed or removed"
        );
    }
    // Case leftover_member_symlink (R53 step 4): a leftover whose data folder is
    // a real directory but whose catalogue member is a symbolic link to a file
    // outside the destination: the link is neither followed nor removed.
    {
        seam::reset();
        let dest = crate::private_tempdir().unwrap();
        let outside = crate::private_tempdir().unwrap();
        let target = outside.path().join("hub.sqlite");
        write_private(&target, b"outside catalogue");
        let staging = dest.path().join(format!(
            ".teslatlas-data-recovery-{}.staging",
            uuid_n(0x7003)
        ));
        fs::create_dir_all(staging.join("data")).unwrap();
        for dir in [&staging, &staging.join("data")] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::os::unix::fs::symlink(&target, staging.join("data/hub.sqlite")).unwrap();
        create_data_backup(env_e.store.as_ref().unwrap(), &dest.path().join("backup"))
            .expect("CT62.leftover_member_symlink the backup succeeds beside the leftover");
        assert!(
            fs::read(&target).ok().as_deref() == Some(b"outside catalogue".as_slice()),
            "CT62.leftover_member_symlink the file outside the destination is untouched"
        );
        assert!(
            fs::symlink_metadata(staging.join("data/hub.sqlite"))
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false),
            "CT62.leftover_member_symlink the catalogue link is skipped, not removed"
        );
    }
    // Case f_restore: (a)'s generation restores into a fresh directory.
    seam::reset();
    let fresh = crate::private_tempdir().unwrap();
    let restored = fresh.path().join("restored");
    restore_data_backup(&gen_a, &restored).expect("CT62.f restore of (a)'s generation");
    {
        let raw = Connection::open(restored.join("hub.sqlite")).unwrap();
        let count = |sql: &str| -> i64 { raw.query_row(sql, [], |r| r.get(0)).unwrap() };
        assert_eq!(
            count("SELECT COUNT(*) FROM credential_pairs"),
            0,
            "CT62.f no credential row"
        );
        assert_eq!(
            count("SELECT COUNT(*) FROM credential_legacy_fences"),
            live_fences.len() as i64,
            "CT62.f legacy fences present"
        );
    }
    let store = HubStore::initialize(&restored).unwrap();
    let mut restored_env = Env::assemble_at(fresh, restored.clone(), store);
    let observer = Connection::open(restored.join("hub.sqlite")).unwrap();
    let before: i64 = observer
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    restored_env
        .run()
        .expect("CT62.f the first admitted open's Phase B");
    let after: i64 = observer
        .query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        before, after,
        "CT62.f the first admitted open writes nothing in Phase B"
    );
    assert!(is_complete(&restored_env.state()), "CT62.f status complete");
    // Fixture item 1's pre-upgrade data-backup generation.
    seam::reset();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/data-backup");
    let copy_root = crate::private_tempdir().unwrap();
    let source = copy_root.path().join("source");
    // The retained validator wants the (empty) pack directories, which the
    // installed fixture cannot carry; only they are created, no fixture byte changes.
    for dir in ["data", "data/packs", "data/packs/sha256"] {
        fs::create_dir_all(source.join(dir)).unwrap();
        fs::set_permissions(source.join(dir), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["BACKUP_COMPLETE", "backup-v4.json", "data/hub.sqlite"] {
        write_private(&source.join(name), &fs::read(fixture.join(name)).unwrap());
    }
    let restored = copy_root.path().join("restored");
    restore_data_backup(&source, &restored).expect("CT62.f the pre-upgrade generation restores");
    {
        let raw = Connection::open(restored.join("hub.sqlite")).unwrap();
        for table in ["legacy_owner_tokens", "fleet_tokens"] {
            let exists: i64 = raw
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            if exists == 1 {
                let rows: i64 = raw
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                    .unwrap();
                assert_eq!(
                    rows, 0,
                    "CT62.f no {table} row before the restored store's first open"
                );
            }
        }
        let purge: i64 = raw
            .query_row(
                "SELECT purge_required FROM credential_format_state",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(purge, 1, "CT62.f the rung set purge_required = 1");
    }
    let store = HubStore::initialize(&restored).unwrap();
    let mut env = Env::assemble_at(copy_root, restored, store);
    env.run()
        .expect("CT62.f the first admitted open maps the fences, purges and completes");
    assert!(is_complete(&env.state()), "CT62.f complete");
    assert_eq!(env.predecessors_present(), 0, "CT62.f old tables dropped");
    let floor_values = earlier_values(&catalogue_vector("floor"));
    assert!(
        scan_for_earlier(&env, &floor_values).is_empty(),
        "CT62.f CT58's scan of the new data directory passes"
    );
}
