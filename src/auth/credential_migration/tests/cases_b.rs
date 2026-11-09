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
        env.run()
            .expect("CT56.hand_built_floor the landed floor converts as in CT46");
        assert!(is_complete(&env.state()), "CT56.hand_built_floor complete");
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
    // With the floor at its landed value the same store converts as in CT46.
    seam::with(|s| s.floor = None);
    env.run().expect("CT56.landed_floor converts as in CT46");
    assert!(is_complete(&env.state()), "CT56.landed_floor complete");
    assert_eq!(
        env.plaintexts(CredentialSlot::OwnerApi).1,
        vector["legacy_current"]["refresh_plaintext"]
            .as_str()
            .unwrap()
            .as_bytes()
            .to_vec(),
        "CT56.landed_floor the Legacy plaintext opens"
    );
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

#[test]
fn ct058_no_earlier_format_bytes() {
    // Hand-built residue: an earlier-format value written and deleted before
    // secure_delete was ever on stays in the file until the purge and the scrub.
    seam::reset();
    {
        const EARLIER: &[u8] = b"EARLIER-FORMAT-CIPHERTEXT-VALUE-0123456789abcdef";
        let key = key32(1);
        let row = PriorRow::fake(&key, "access-0001", "refresh-0001", Some(uuid_n(0xaa)));
        let mut env = Env::hand_built(Some(&row), None, false);
        env.put(REL_K1, &key);
        env.db()
            .execute(
                "INSERT INTO fleet_tokens(singleton_id, access, refresh, client_id, region,
                 expires_at, next_refresh_at, credential_generation)
                 VALUES(1, ?1, ?1, 'client-0001', 'eu', 900, 600, NULL)",
                [EARLIER],
            )
            .unwrap();
        env.db().execute("DELETE FROM fleet_tokens", []).unwrap();
        let values = vec![
            ("hand_built.earlier".to_owned(), EARLIER.to_vec()),
            ("hand_built.key".to_owned(), key.clone()),
        ];
        seam::with(|s| s.free_bytes = Some((0, 0)));
        env.run()
            .expect("CT58.hand_built conversion with the purge deferred");
        assert!(
            !scan_for_earlier(&env, &values).is_empty(),
            "CT58.hand_built the scan before step 6 finds the earlier values"
        );
        seam::with(|s| s.free_bytes = None);
        env.run().expect("CT58.hand_built remaining steps");
        assert!(is_complete(&env.state()), "CT58.hand_built complete");
        let found = scan_for_earlier(&env, &values);
        assert!(
            found.is_empty(),
            "CT58.hand_built no earlier-format byte or 16-byte run remains: {found:?}"
        );
    }
    seam::reset();
    let vector = catalogue_vector("residue");
    let values = earlier_values(&vector);
    let mut env = Env::from_catalogue("catalogue-residue.sqlite");
    env.put(REL_K1, &vector_bytes(&vector["legacy_current"], "key_hex"));
    env.put(
        REL_K2,
        &vector_bytes(&vector["earlier_replaced_deleted"], "key_hex"),
    );
    env.put(FLEET_K4, &vector_bytes(&vector["fleet_current"], "key_hex"));
    // AckWriter: a second connection opened before the first admitted run
    // commits one non-credential row at each Phase B boundary.
    let ack = Connection::open(env.data().join("hub.sqlite")).unwrap();
    ack.execute_batch("CREATE TABLE IF NOT EXISTS ack_rows(n INTEGER PRIMARY KEY, boundary TEXT)")
        .unwrap();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = count.clone();
    seam::with(|s| {
        s.boundary = Some(Box::new(move |name| {
            ack.execute("INSERT INTO ack_rows(boundary) VALUES(?1)", [name])
                .unwrap();
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        }));
    });
    // The purge is held by the available-bytes seam: the scan after conversion
    // but before step 6 finds the earlier values, so the test can fail.
    seam::with(|s| s.free_bytes = Some((0, 0)));
    env.run().expect("CT58 conversion with the purge deferred");
    let state = env.state();
    assert!(
        state.converted_at_ms.is_some() && state.purge_required,
        "CT58 converted, purge deferred: {state:?}"
    );
    assert!(
        !scan_for_earlier(&env, &values).is_empty(),
        "CT58 the scan before step 6 finds the earlier values"
    );
    // Release the seam: the purge, scrub, markers and removal complete.
    seam::with(|s| s.free_bytes = None);
    env.run().expect("CT58 remaining steps");
    let state = env.state();
    assert!(is_complete(&state), "CT58 complete: {state:?}");
    assert!(
        count.load(std::sync::atomic::Ordering::SeqCst) >= 4,
        "CT58 the AckWriter committed at the boundaries"
    );
    let found = scan_for_earlier(&env, &values);
    assert!(
        found.is_empty(),
        "CT58 no earlier-format byte or 16-byte run remains: {found:?}"
    );
    assert_eq!(
        env.listing(),
        vec!["credential-keys".to_owned()],
        "CT58 keyring only in the secrets directory"
    );
}

#[test]
fn ct059_pre_upgrade_vectors() {
    let all = vectors();
    for case in all["ciphertext_cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        seam::reset();
        let key = vector_bytes(case, "key_hex");
        let fleet = id.starts_with("fleet");
        let row = PriorRow {
            access: vector_bytes(case, "access_ciphertext_hex"),
            refresh: vector_bytes(case, "refresh_ciphertext_hex"),
            expires: SCHEDULE.0,
            next: SCHEDULE.1,
            generation: Some(case["generation"].as_str().unwrap().parse().unwrap()),
        };
        let mut env = if fleet {
            Env::hand_built(None, Some(&row), false)
        } else {
            Env::hand_built(Some(&row), None, false)
        };
        env.fake = false; // the real kept OLD-D and OLD-G, never a fake
        match id {
            "fleet_old_k7" => {
                let cursor_bytes: [u8; 32] = unhex(
                    case["cursor_key_hex"]
                        .as_str()
                        .expect("the vector carries its cursor key"),
                )
                .try_into()
                .unwrap();
                let cursor = crate::protocol::CursorKey::from_bytes(cursor_bytes);
                assert_eq!(
                    cursor.fleet_credential_encryption_key().to_vec(),
                    key,
                    "CT59.{id} the vector key is the cursor-derived key"
                );
                env.cursor = Some(cursor);
            }
            "fleet_file" => env.put(FLEET_K4, &key),
            _ => env.put(REL_K1, &key),
        }
        env.run()
            .unwrap_or_else(|e| panic!("CT59.{id} Phase B failed: {e:?}"));
        let slot = if fleet {
            CredentialSlot::FleetApi
        } else {
            CredentialSlot::OwnerApi
        };
        let (access, refresh) = env.plaintexts(slot);
        assert_eq!(
            access,
            case["access_plaintext"].as_str().unwrap().as_bytes(),
            "CT59.{id} access plaintext"
        );
        assert_eq!(
            refresh,
            case["refresh_plaintext"].as_str().unwrap().as_bytes(),
            "CT59.{id} refresh plaintext"
        );
        assert_eq!(
            env.stored(slot).unwrap().generation.to_string(),
            case["generation"].as_str().unwrap(),
            "CT59.{id} generation"
        );
        assert!(is_complete(&env.state()), "CT59.{id} complete");
    }
}

// ---------------------------------------------------------------- CT93, CT101

#[test]
fn ct093_candidate_skips_unsafe() {
    use crate::platform::private_directory::{ENTRY_SEAM, EntrySeam};
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
    let mut env = Env::hand_built(Some(&owner), Some(&fleet), false);
    env.put(REL_K1, &owner_key);
    env.put(FLEET_K4, &fleet_key);
    // A symlinked pre-upgrade-owner-api.key (to a valid key file), a real bound
    // Unix-domain socket named pre-upgrade-fleet-api.key (bound at a short name
    // in the test's own root and renamed into secrets/ on the same filesystem)
    // and a FIFO named after the Fleet pending key as an extra case.
    let target = env.data().join("elsewhere.key");
    write_private(&target, &owner_key);
    std::os::unix::fs::symlink(&target, env.secrets_path().join(RST_K1)).unwrap();
    let socket_path = env.data().join("s");
    let listener =
        std::os::unix::net::UnixListener::bind(&socket_path).expect("bind a short-named socket");
    fs::rename(&socket_path, env.secrets_path().join(RST_K2)).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(env.secrets_path().join(FLEET_K5))
            .status()
            .unwrap()
            .success(),
        "CT93 create the FIFO extra case"
    );
    ENTRY_SEAM.with(|slot| *slot.borrow_mut() = Some(EntrySeam::default()));
    let report = env
        .run()
        .expect("CT93 Phase B converts with unsafe candidates present");
    let opens = ENTRY_SEAM
        .with(|slot| slot.borrow_mut().take().map(|s| s.opens))
        .unwrap();
    assert_eq!(
        report
            .unsafe_candidates
            .iter()
            .copied()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([RST_K1, RST_K2, FLEET_K5]),
        "CT93 every unsafe candidate is named in the migration receipt"
    );
    let regular_inspections = seam::with(|s| {
        s.events
            .iter()
            .filter(|e| *e == &format!("inspect:{REL_K1}") || *e == &format!("inspect:{FLEET_K4}"))
            .count()
    });
    assert_eq!(
        opens, regular_inspections,
        "CT93 only inspections of the two regular candidates opened a file: opens {opens}, regular inspections {regular_inspections}"
    );
    assert!(
        seam::with(|s| s
            .events
            .iter()
            .filter(|e| e.starts_with("read:"))
            .all(|e| e == &format!("read:{REL_K1}") || e == &format!("read:{FLEET_K4}"))),
        "CT93 no unsafe candidate is ever read"
    );
    assert!(is_complete(&env.state()), "CT93 both slots converted");
    assert_eq!(
        fs::read(&target).unwrap(),
        owner_key,
        "CT93 the symlink target is untouched"
    );
    drop(listener);
}

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
    env.run().expect("CT101 (a) Phase B conversion and cleanup");
    env.reopen();
    env.run().expect("CT101 (a) a second open after completion");
    assert_eq!(
        fingerprint(&all),
        before,
        "CT101 every old-named file keeps bytes, inode and mtime"
    );
    let attempts: Vec<Vec<u8>> =
        seam::with(|s| s.decrypt_log.iter().map(|(_, k)| k.clone()).collect());
    assert_eq!(
        attempts,
        vec![owner_key, fleet_key],
        "CT101 conversion uses REL-K1 and OLD-K4 only"
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
    // The listings that met a stand-in passed over it.
    assert!(
        seam::with(|s| s
            .events
            .iter()
            .any(|e| e.starts_with("list_returned:") && e.contains(&secrets_names[0]))),
        "CT101 the directory listing met the stand-in and nothing followed"
    );
}

// ---------------------------------------------------------------- CT87, CT90, CT91

fn prior_generations(env: &Env, table: &str) -> BTreeSet<String> {
    let mut statement = env
        .db()
        .prepare(&format!(
            "SELECT lower(input_credential_generation) FROM {table}"
        ))
        .unwrap();
    statement
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn ct087_consolidation() {
    // Hand-built consolidation (spec CT87 on synthetic rows): both current rows
    // carry one refresh token, the same generation is fenced in both stores in
    // a different letter case, one attempt id sits in both binding tables.
    seam::reset();
    {
        let shared = Uuid::new_v5(&uuid_n(3), b"refresh-shared");
        let (out_x, out_y, neither) = (uuid_n(0xb1), uuid_n(0xb2), uuid_n(0xc1));
        let attempt = uuid_n(0x5001);
        let key = key32(1);
        let fleet_key = key32(7);
        let owner_row = PriorRow::fake(&key, "access-0001", "refresh-shared", Some(shared));
        let fleet_row = PriorRow::fake(&fleet_key, "access-0002", "refresh-shared", None);
        let mut env = Env::hand_built(Some(&owner_row), Some(&fleet_row), true);
        env.put(REL_K1, &key);
        env.put(FLEET_K4, &fleet_key);
        env.binding(
            CredentialSlot::OwnerApi,
            1,
            attempt,
            &shared.to_string(),
            Some(&out_x.to_string()),
            (5_000, "success"),
        );
        env.binding(
            CredentialSlot::FleetApi,
            2,
            attempt,
            &shared.to_string().to_uppercase(),
            Some(&out_y.to_string()),
            (3_000, "success"),
        );
        env.fence(CredentialSlot::OwnerApi, &shared.to_string());
        env.fence(CredentialSlot::OwnerApi, &neither.to_string());
        env.fence(CredentialSlot::FleetApi, &shared.to_string().to_uppercase());
        env.fence(CredentialSlot::FleetApi, &out_x.to_string().to_uppercase());
        env.fence(CredentialSlot::FleetApi, &out_y.to_string());
        let report = env
            .run()
            .expect("CT87.hand_built Phase B commits under section 3.4's DDL");
        let state = env.state();
        assert!(is_complete(&state), "CT87.hand_built complete: {state:?}");
        let fences = env.legacy_fences();
        assert_eq!(
            fences.len(),
            4,
            "CT87.hand_built one row per distinct case-insensitive generation: {fences:?}"
        );
        let row = |g: Uuid| {
            fences
                .iter()
                .find(|f| f.0 == g.to_string())
                .cloned()
                .unwrap_or_else(|| panic!("CT87.hand_built fence {g} missing"))
        };
        assert_eq!(
            row(shared).1,
            "anchored",
            "CT87.hand_built the shared current generation is anchored"
        );
        assert_eq!(
            row(shared).2,
            3_000,
            "CT87.hand_built the shared generation has the earlier start time"
        );
        assert_eq!(
            row(out_x).1,
            "opaque",
            "CT87.hand_built a binding output of one store fenced in the other is one opaque row"
        );
        assert_eq!(
            row(out_y).1,
            "opaque",
            "CT87.hand_built a fenced binding output is opaque"
        );
        assert_eq!(
            row(neither).1,
            "opaque",
            "CT87.hand_built a fence that is neither is opaque"
        );
        assert_eq!(
            row(neither).2,
            T0_MS,
            "CT87.hand_built a fence with no binding takes the migration time"
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM credential_refresh_fences WHERE kind='migrated'"),
            1,
            "CT87.hand_built one migrated fingerprint fence for the shared token"
        );
        assert_eq!(
            (
                state.attempts_migrated,
                state.fences_fingerprinted,
                state.legacy_fences
            ),
            (2, 1, 4),
            "CT87.hand_built counts"
        );
        assert_eq!(
            report.opaque_binding_outputs, 2,
            "CT87.hand_built the receipt counts both opaque binding outputs"
        );
        let shared_on: Vec<i64> = {
            let mut st = env
                .db()
                .prepare("SELECT receipt_id FROM credential_refresh_attempts WHERE attempt_id=?1")
                .unwrap();
            st.query_map([attempt.to_string()], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            shared_on,
            vec![1],
            "CT87.hand_built the shared attempt id stays on the Legacy attempt only"
        );
        assert_eq!(
            env.count("SELECT COUNT(*) FROM credential_refresh_attempts"),
            2,
            "CT87.hand_built both attempts kept"
        );
    }
    seam::reset();
    let vector = catalogue_vector("residue");
    let mut env = Env::from_catalogue("catalogue-residue.sqlite");
    env.put(REL_K1, &vector_bytes(&vector["legacy_current"], "key_hex"));
    env.put(FLEET_K4, &vector_bytes(&vector["fleet_current"], "key_hex"));
    // Oracle values come from the fixture's own predecessor tables, read
    // before Phase B.
    let fences_legacy = prior_generations(&env, "legacy_refresh_input_fences");
    let fences_fleet = prior_generations(&env, "fleet_refresh_input_fences");
    let union: BTreeSet<String> = fences_legacy.union(&fences_fleet).cloned().collect();
    let binding_rows = env.count("SELECT COUNT(*) FROM legacy_refresh_receipt_bindings")
        + env.count("SELECT COUNT(*) FROM fleet_refresh_receipt_bindings");
    let outputs: BTreeSet<String> = {
        let mut statement = env
            .db()
            .prepare(
                "SELECT lower(output_credential_generation) FROM legacy_refresh_receipt_bindings
                 WHERE output_credential_generation IS NOT NULL
                 UNION SELECT lower(output_credential_generation) FROM fleet_refresh_receipt_bindings
                 WHERE output_credential_generation IS NOT NULL",
            )
            .unwrap();
        statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let shared = vector["shared_attempt_id"].as_str().unwrap().to_owned();
    let earliest = |generation: &str| -> Option<i64> {
        env.db()
            .query_row(
                &format!(
                    "SELECT min(started_at_ms) FROM outbound_request_receipts r WHERE r.id IN (
               SELECT receipt_id FROM legacy_refresh_receipt_bindings
               WHERE lower(input_credential_generation)='{generation}'
               UNION SELECT receipt_id FROM fleet_refresh_receipt_bindings
               WHERE lower(input_credential_generation)='{generation}')"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let cross_case = vector["cross_case_generation"]
        .as_str()
        .unwrap()
        .to_lowercase();
    let cross_case_time = earliest(&cross_case);
    let currents: BTreeSet<String> = [
        vector["legacy_current"]["generation"]
            .as_str()
            .unwrap()
            .to_lowercase(),
        vector["fleet_generation"].as_str().unwrap().to_lowercase(),
    ]
    .into();

    let report = env
        .run()
        .expect("CT87 Phase B commits under section 3.4's DDL");
    let state = env.state();
    assert!(is_complete(&state), "CT87 complete: {state:?}");
    let fences = env.legacy_fences();
    assert_eq!(
        fences.iter().map(|f| f.0.clone()).collect::<BTreeSet<_>>(),
        union,
        "CT87 one legacy fence per distinct case-insensitive generation"
    );
    assert_eq!(
        fences.len(),
        union.len(),
        "CT87 no duplicate row for the shared generation"
    );
    let row = |g: &str| {
        fences
            .iter()
            .find(|f| f.0 == g.to_lowercase())
            .unwrap_or_else(|| panic!("CT87 fence {g} missing"))
            .clone()
    };
    assert_eq!(
        row(&cross_case).2,
        cross_case_time.unwrap_or(T0_MS),
        "CT87 the shared generation has the earlier start time (the migration time when no binding has that input)"
    );
    for g in &union {
        let expected = if currents.contains(g) {
            "anchored"
        } else {
            "opaque"
        };
        assert_eq!(row(g).1, expected, "CT87 identity of {g}");
    }
    assert_eq!(
        row(vector["cross_store_binding_output"].as_str().unwrap()).1,
        "opaque",
        "CT87 cross-store output fence is opaque"
    );
    assert_eq!(
        row(vector["fenced_binding_output"].as_str().unwrap()).1,
        "opaque",
        "CT87 binding-output fence is opaque"
    );
    assert_eq!(
        row(vector["opaque_fence"].as_str().unwrap()).1,
        "opaque",
        "CT87 neither-fence is opaque"
    );
    assert_eq!(
        state.attempts_migrated, binding_rows,
        "CT87 the attempt count equals the two binding tables' rows together"
    );
    assert_eq!(
        env.count("SELECT COUNT(*) FROM credential_refresh_attempts"),
        binding_rows,
        "CT87 attempt rows"
    );
    assert_eq!(
        env.count(&format!(
            "SELECT COUNT(*) FROM credential_refresh_attempts WHERE attempt_id='{shared}'"
        )),
        1,
        "CT87 the shared attempt id stays on exactly one attempt"
    );
    assert_eq!(
        env.count("SELECT COUNT(*) FROM credential_refresh_fences WHERE kind='migrated'"),
        1,
        "CT87 one migrated fingerprint fence for the shared token"
    );
    let opaque_outputs = fences
        .iter()
        .filter(|f| f.1 == "opaque" && outputs.contains(&f.0))
        .count() as u64;
    assert_eq!(
        report.opaque_binding_outputs, opaque_outputs,
        "CT87 the receipt counts every opaque binding output"
    );
    assert!(
        opaque_outputs >= 2,
        "CT87 both binding-output fences are counted"
    );

    // Permanent: clear, cancel_unsent and close(release) leave both fence
    // tables, identities included, unchanged.
    let fences_before = (
        env.legacy_fences(),
        env.count("SELECT COUNT(*) FROM credential_refresh_fences"),
    );
    let attempts: Vec<(i64, String, String)> = {
        let mut statement = env
            .db()
            .prepare("SELECT receipt_id, slot, input_generation FROM credential_refresh_attempts")
            .unwrap();
        statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        env.conn
            .as_mut()
            .unwrap()
            .clear_credential_pair(slot)
            .expect("CT87 clear");
    }
    for (receipt, _, input) in &attempts {
        let input: Uuid = input.parse().unwrap();
        let conn = env.conn.as_mut().unwrap();
        let receipt = crate::db::OutboundRequestReceiptId(*receipt);
        // Closing a migrated attempt may be refused once closed; neither
        // result may release a fence.
        let _ = conn.cancel_unsent_credential_refresh(receipt, input);
        let _ = conn.close_credential_refresh_attempt(
            receipt,
            input,
            crate::db::OutboundRequestOutcome::Success,
            None,
            None,
            true,
        );
    }
    assert_eq!(
        (
            env.legacy_fences(),
            env.count("SELECT COUNT(*) FROM credential_refresh_fences")
        ),
        fences_before,
        "CT87 both fence tables are unchanged by clear, cancel_unsent and close(release)"
    );

    // Case reader_refusal_propagates (M2P-57, M2P-68, E-A1): a refused
    // predecessor read surfaces as a Store error with no purge, removal or
    // cleanup, and the replay refuses the same way.
    for case in ["noncanonical_table", "capacity_over_100000"] {
        seam::reset();
        let mut env = Env::from_catalogue("catalogue-residue.sqlite");
        env.put(REL_K1, &vector_bytes(&vector["legacy_current"], "key_hex"));
        env.put(FLEET_K4, &vector_bytes(&vector["fleet_current"], "key_hex"));
        match case {
            "noncanonical_table" => recase_fence_table(
                &env,
                "fleet_refresh_input_fences",
                "Fleet_Refresh_Input_Fences",
            ),
            _ => env
                .db()
                .execute_batch(
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<100001)
                     INSERT OR IGNORE INTO legacy_refresh_input_fences
                     SELECT printf('ffffffff-ffff-4fff-8fff-%012x', i) FROM n;",
                )
                .unwrap(),
        }
        let first = env.run();
        assert!(
            matches!(first, Err(CredentialMigrationError::Store(_))),
            "CT87.{case} the refusal surfaces as CredentialMigrationError::Store: {first:?}"
        );
        assert!(
            env.listing().contains(&REL_K1.to_owned()),
            "CT87.{case} no key removed"
        );
        assert!(
            events_matching("vacuum").is_empty() && events_matching("remove:").is_empty(),
            "CT87.{case} no purge or removal"
        );
        let second = env.run();
        assert!(
            matches!(second, Err(CredentialMigrationError::Store(_))),
            "CT87.{case} replay refuses again: {second:?}"
        );
        assert_eq!(
            env.count(
                "SELECT COUNT(*) FROM credential_format_state WHERE converted_at_ms IS NOT NULL"
            ),
            0,
            "CT87.{case} nothing converted"
        );
    }
}

fn purge_cases(label: &str, setup: &dyn Fn() -> Env, values: &[(String, Vec<u8>)]) {
    let scan_clean = |env: &Env, case: &str| {
        if !values.is_empty() {
            let found = scan_for_earlier(env, values);
            assert!(
                found.is_empty(),
                "CT90.{label}.{case} CT58's scan passes: {found:?}"
            );
        }
    };
    // Case one_vacuum: the admitted open runs one VACUUM and CT58's scan passes.
    seam::reset();
    let mut env = setup();
    env.run()
        .unwrap_or_else(|e| panic!("CT90.{label}.one_vacuum: {e:?}"));
    assert_eq!(
        events_matching("vacuum").len(),
        1,
        "CT90.{label}.one_vacuum exactly one VACUUM"
    );
    let state = env.state();
    assert!(
        !state.purge_required && is_complete(&state),
        "CT90.{label}.one_vacuum purge_required = 0: {state:?}"
    );
    scan_clean(&env, "one_vacuum");

    // Case deferred: below twice the catalogue size nothing is rebuilt.
    seam::reset();
    let mut env = setup();
    let size = fs::metadata(env.data().join("hub.sqlite")).unwrap().len();
    seam::with(|s| s.free_bytes = Some((size, size)));
    env.run()
        .unwrap_or_else(|e| panic!("CT90.{label}.deferred the open itself: {e:?}"));
    assert!(
        events_matching("vacuum").is_empty(),
        "CT90.{label}.deferred no VACUUM"
    );
    let state = env.state();
    assert!(
        state.converted_at_ms.is_some() && state.purge_required,
        "CT90.{label}.deferred cleanup_pending: {state:?}"
    );
    let size_now = fs::metadata(env.data().join("hub.sqlite")).unwrap().len();
    assert_eq!(
        state.purge_needed_bytes,
        Some(i64::try_from(2 * size_now - size).unwrap()),
        "CT90.{label}.deferred cleanupNeededBytes equals the shortfall"
    );
    assert!(
        state.completed_at_ms.is_none(),
        "CT90.{label}.deferred not complete"
    );
    assert_ne!(
        state.owner_api_state,
        CredentialSlotFormatState::Pending,
        "CT90.{label}.deferred the slot is usable"
    );
    seam::with(|s| s.free_bytes = None);
    env.reopen();
    env.run()
        .unwrap_or_else(|e| panic!("CT90.{label}.deferred after raising the seam: {e:?}"));
    assert!(is_complete(&env.state()), "CT90.{label}.deferred completes");
    assert_eq!(
        events_matching("vacuum").len(),
        1,
        "CT90.{label}.deferred exactly one VACUUM after raising"
    );
    scan_clean(&env, "deferred");

    // Case interrupted_vacuum: the catalogue is unchanged and opens,
    // purge_required = 1, retried at the next open.
    seam::reset();
    let mut env = setup();
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handler_flag = flag.clone();
    let _ = env.db().progress_handler(
        1,
        Some(move || handler_flag.load(std::sync::atomic::Ordering::SeqCst)),
    );
    let arm = flag.clone();
    seam::with(|s| {
        s.boundary = Some(Box::new(move |name| {
            if name == "before_vacuum" {
                arm.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            true
        }));
    });
    assert!(
        env.run().is_err(),
        "CT90.{label}.interrupted the interrupted VACUUM is an error"
    );
    flag.store(false, std::sync::atomic::Ordering::SeqCst);
    let _ = env.db().progress_handler(1, None::<fn() -> bool>);
    let state = env.state();
    assert!(
        state.purge_required && state.converted_at_ms.is_some(),
        "CT90.{label}.interrupted purge_required stays 1"
    );
    seam::with(|s| s.boundary = None);
    env.reopen();
    env.run()
        .unwrap_or_else(|e| panic!("CT90.{label}.interrupted retried at the next open: {e:?}"));
    assert!(
        is_complete(&env.state()),
        "CT90.{label}.interrupted completes"
    );
    scan_clean(&env, "interrupted");
}

#[test]
fn ct090_residue_purge() {
    // Hand-built store: real S state machine, fake pre-upgrade decrypt seam.
    let hand = || {
        let key = key32(1);
        let row = PriorRow::fake(&key, "access-0001", "refresh-0001", Some(uuid_n(0xaa)));
        let env = Env::hand_built(Some(&row), None, false);
        env.put(REL_K1, &key);
        env
    };
    purge_cases("hand_built", &hand, &[]);
    // Fixture item 1's second catalogue (real OLD-D ciphertext and residue).
    let vector = catalogue_vector("residue");
    let values = earlier_values(&vector);
    let fixture = || {
        let env = Env::from_catalogue("catalogue-residue.sqlite");
        env.put(REL_K1, &vector_bytes(&vector["legacy_current"], "key_hex"));
        env.put(
            REL_K2,
            &vector_bytes(&vector["earlier_replaced_deleted"], "key_hex"),
        );
        env.put(FLEET_K4, &vector_bytes(&vector["fleet_current"], "key_hex"));
        env
    };
    purge_cases("fixture", &fixture, &values);
}

#[test]
fn ct091_completion_order() {
    const V: &[u8] = b"V-earlier-format-value-0123456789abcdef";
    // Order case: while the purge is deferred, step 7 does not run and the DDL
    // refuses wal_scrubbed = 1; once the purge commits wal_scrubbed is 0 and
    // the scrub runs before completion.
    seam::reset();
    let key = key32(1);
    let row = PriorRow::fake(&key, "access-0001", "refresh-0001", Some(uuid_n(0xaa)));
    let mut env = Env::hand_built(Some(&row), None, false);
    env.put(REL_K1, &key);
    // V is written and deleted before the first open, with checkpoint-on-close
    // disabled, so that it stays in WAL frames.
    {
        let raw = Connection::open(env.data().join("hub.sqlite")).unwrap();
        raw.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
        raw.execute(
            "INSERT INTO fleet_tokens(singleton_id, access, refresh, client_id, region, expires_at,
             next_refresh_at, credential_generation) VALUES(1, ?1, ?1, 'client-0001', 'eu', 900, 600, NULL)",
            [V],
        )
        .unwrap();
        raw.execute("DELETE FROM fleet_tokens", []).unwrap();
    }
    seam::with(|s| s.free_bytes = Some((0, 0)));
    env.run()
        .expect("CT91.order conversion with the purge deferred");
    let state = env.state();
    assert!(
        state.purge_required && !state.wal_scrubbed && state.completed_at_ms.is_none(),
        "CT91.order deferred: {state:?}"
    );
    assert!(
        events_matching("scrub_marker").is_empty(),
        "CT91.order step 7 does not run while the purge is deferred"
    );
    let refused = env
        .db()
        .execute("UPDATE credential_format_state SET wal_scrubbed=1", []);
    assert!(
        refused.is_err(),
        "CT91.order the DDL refuses wal_scrubbed = 1 while purge_required = 1"
    );
    seam::with(|s| s.free_bytes = None);
    seam::with(|s| s.events.clear());
    env.reopen();
    env.run()
        .expect("CT91.order the next open purges, scrubs and completes");
    let events = seam::with(|s| s.events.clone());
    let (purge, scrub, complete) = (
        event_index(&events, "purge_commit"),
        event_index(&events, "scrub_marker"),
        event_index(&events, "complete_marker"),
    );
    assert!(
        matches!((purge, scrub, complete), (Some(p), Some(s), Some(c)) if p < s && s < c),
        "CT91.order purge commit, then scrub, then completion: {:?}",
        alias_view(&events)
    );

    // Snapshot case: a reader's snapshot makes every checkpoint busy.
    seam::reset();
    let mut env = Env::hand_built(Some(&row), None, false);
    env.put(REL_K1, &key);
    {
        let raw = Connection::open(env.data().join("hub.sqlite")).unwrap();
        raw.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
        raw.execute(
            "INSERT INTO fleet_tokens(singleton_id, access, refresh, client_id, region, expires_at,
             next_refresh_at, credential_generation) VALUES(1, ?1, ?1, 'client-0001', 'eu', 900, 600, NULL)",
            [V],
        )
        .unwrap();
        raw.execute("DELETE FROM fleet_tokens", []).unwrap();
    }
    let reader = Connection::open(env.data().join("hub.sqlite")).unwrap();
    reader
        .execute_batch("BEGIN; SELECT count(*) FROM sqlite_schema;")
        .unwrap();
    let _ = env.run(); // the scrub cannot truncate the WAL; the run may report it
    let state = env.state();
    assert!(state.converted_at_ms.is_some(), "CT91.snapshot converted");
    assert!(
        !state.wal_scrubbed && state.completed_at_ms.is_none(),
        "CT91.snapshot status stays cleanup_pending (completed_at_ms NULL, wal_scrubbed 0): {state:?}"
    );
    reader.execute_batch("COMMIT").unwrap();
    drop(reader);

    // AckWriter, open throughout, commits one row at each remaining boundary;
    // the scrub leaves the WAL at length 0 by stat at its boundary.
    let ack = Connection::open(env.data().join("hub.sqlite")).unwrap();
    ack.execute_batch("CREATE TABLE IF NOT EXISTS ack_rows(n INTEGER PRIMARY KEY, boundary TEXT)")
        .unwrap();
    let wal = env.data().join("hub.sqlite-wal");
    let wal_at_scrub = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    // (0 = the WAL at the scrub boundary holds no earlier value)
    let observed = wal_at_scrub.clone();
    seam::with(|s| {
        s.boundary = Some(Box::new(move |name| {
            if name == "after_scrub_marker" {
                // The marker commit appends frames after the truncation, so the
                // WAL is checked for content: none of the earlier value remains.
                let bytes = fs::read(&wal).unwrap_or_default();
                observed.store(
                    contains(&bytes, V) as u64,
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
            ack.execute("INSERT INTO ack_rows(boundary) VALUES(?1)", [name])
                .unwrap();
            true
        }));
    });
    env.reopen();
    env.run()
        .expect("CT91.snapshot the next open runs the remaining steps");
    assert_eq!(
        wal_at_scrub.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "CT91.snapshot the WAL holds no earlier value at the scrub boundary (0 = none, MAX = boundary not reached)"
    );
    let state = env.state();
    assert!(
        is_complete(&state),
        "CT91.snapshot complete after the reader is gone: {state:?}"
    );
    assert!(
        !env.listing().contains(&REL_K1.to_owned()),
        "CT91.snapshot the files are removed"
    );
    let found: Vec<_> = env
        .all_files()
        .into_iter()
        .filter(|(_, bytes)| contains(bytes, V))
        .map(|(p, _)| p)
        .collect();
    assert!(
        found.is_empty(),
        "CT91.snapshot no earlier-format value remains in catalogue, WAL or shm: {found:?}"
    );
}

// ---------------------------------------------------------------- CT62

use crate::data_recovery::{
    BackupWait, CAR_CONTROL_SECRET_FILES, CAR_CONTROL_SECRET_TABLES, DataRecoveryError,
    PRE_UPGRADE_SECRET_TABLES, create_data_backup, credential_backup_wait, restore_data_backup,
};

fn both_slots_complete() -> Env {
    let mut env = crash_fixture(true);
    env.run().expect("CT62 setup: convert both slots");
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
fn ct062_data_backup() {
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
    // (c) blocked: the Legacy key is wrong.
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
            owner_api: Some(CredentialMigrationBlockReason::OldKeyUnavailable),
            fleet_api: None
        },
        "CT62.c old_key_unavailable"
    );
    drop(snapshot);
    // (d) cleanup_pending: converted store whose purge is deferred.
    seam::reset();
    let mut env_d = crash_fixture(true);
    seam::with(|s| s.free_bytes = Some((0, 0)));
    env_d.run().unwrap();
    let dest_d = crate::private_tempdir().unwrap();
    let leftover_d = plant_leftover(dest_d.path());
    let before = listing_of(dest_d.path());
    assert!(
        refused(create_data_backup(
            env_d.store.as_ref().unwrap(),
            &dest_d.path().join("backup")
        )),
        "CT62.d cleanup_pending is refused"
    );
    assert_eq!(
        listing_of(dest_d.path()),
        before,
        "CT62.d destination unchanged"
    );
    assert!(
        leftover_d.join("data/hub.sqlite").exists(),
        "CT62.d a refused run removes no earlier leftover"
    );
    assert!(
        matches!(wait_of(&env_d), Some(BackupWait::PurgePending { needed_bytes: Some(n) }) if n > 0),
        "CT62.d names cleanup_pending with the needed bytes"
    );
    // (d) continued: after raising the seam the open purges; a backup taken
    // while the scrub is still held succeeds as in (a).
    seam::with(|s| s.free_bytes = None);
    stop_at("after_purge_commit");
    assert!(
        env_d.run().is_err(),
        "CT62.d the run is held after the purge commit"
    );
    seam::with(|s| s.boundary = None);
    let state = env_d.state();
    assert!(
        !state.purge_required && !state.wal_scrubbed,
        "CT62.d purge committed, scrub still due: {state:?}"
    );
    let generation_d = dest_d.path().join("backup");
    create_data_backup(env_d.store.as_ref().unwrap(), &generation_d)
        .expect("CT62.d a backup while the scrub is held succeeds");
    assert_eq!(
        payload_conn(&generation_d)
            .query_row("SELECT COUNT(*) FROM credential_pairs", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "CT62.d payload carries no credential row"
    );

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
