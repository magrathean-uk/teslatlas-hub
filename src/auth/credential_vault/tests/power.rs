// SPDX-License-Identifier: AGPL-3.0-only

/// Durable file images are independent of SQLite's close/checkpoint behaviour.
/// Reopening a new directory from an image discards every later write, even
/// when the live connection would checkpoint it as it is dropped.
#[derive(Clone)]
struct CutImage {
    files: std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    sent: bool,
    seals: std::collections::BTreeMap<[u8;8],u64>,
}
impl CutImage {
    fn initial(root: &Path) -> Self {
        let mut image = Self { files: Default::default(), sent: false, seals: Default::default() };
        image.capture(root, false); image.capture(root, true); image
    }
    fn capture(&mut self, root: &Path, keys: bool) {
        self.files.retain(|path, _| path.starts_with("secrets") != keys);
        let directory = if keys { root.join("secrets") } else { root.to_path_buf() };
        if !directory.exists() { return; }
        for entry in fs::read_dir(&directory).expect("image listing") {
            let entry = entry.expect("image entry");
            if !entry.file_type().expect("image type").is_file() { continue; }
            let name = entry.file_name();
            if !keys && name != "hub.sqlite" && name != "hub.sqlite-wal" { continue; }
            self.files.insert(entry.path().strip_prefix(root).expect("relative member").to_owned(), fs::read(entry.path()).expect("image bytes"));
        }
    }
    fn reopen(&self) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let root = crate::private_tempdir().expect("cut root");
        for (name, bytes) in &self.files {
            let path = root.path().join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("image directory");
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).expect("image directory mode");
            }
            fs::write(&path, bytes).expect("durable image member");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("image member mode");
        }
        root
    }
}

fn assert_retry_restart_probe() {
    let (root, input) = seeded(); let vault = open(root.path());
    let receipt = vault.begin_refresh(OWNER, input).expect("CT96 durable begin before cut");
    let image = CutImage::initial(root.path()); let output = Uuid::new_v4();
    vault.lock().expect("state").connection.set_test_barrier_failure_at(Some(2));
    assert!(matches!(vault.persist_refreshed(OWNER, receipt, input, output, "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000), Err(CredentialStoreError::DurabilityUnavailable)), "CT96 cut before completion proof");
    let committed: String = HubStore::initialize(root.path()).expect("observer store").open().expect("observer").query_row(
        "SELECT generation FROM credential_pairs WHERE slot='owner_api'", [], |row| row.get(0)).expect("CT96 WAL output");
    assert!(committed == output.to_string(), "CT96 output frames reached the WAL");
    drop(vault);
    let recovered = image.reopen(); let events = Arc::new(Mutex::new(Vec::new())); let seen = Arc::clone(&events);
    fixtures::probe_observer(Some(Box::new(move |point| seen.lock().expect("probe trace").push(point.to_owned()))));
    let restart_store = HubStore::initialize(recovered.path()).expect("restart store");
    let vault = CredentialVault::open(restart_store.clone(), AdmittedUserHub::for_test(recovered.path()).expect("restart admission"), recovered.path(),
        VaultOptions { exclusion: Some(Arc::new(FixtureExclusion)), now_ms: Some(1_800_000_000_000), probe_failure: true }).expect("CT96 failed-probe open");
    fixtures::probe_observer(None);
    assert!(events.lock().expect("probe trace").as_slice() == ["after_probe_checkpoint"] && vault.lock().expect("state").read_only, "CT96 failed probe preceded the manager's first load");
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("restart runtime").block_on(async {
        let posts = Arc::new(AtomicUsize::new(0)); let seen = Arc::clone(&posts);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("restart issuer"); let address = listener.local_addr().expect("issuer address");
        let server = tokio::spawn(async move { axum::serve(listener, axum::Router::new().route("/token", axum::routing::post(move || { let seen = Arc::clone(&seen); async move { seen.fetch_add(1, Ordering::SeqCst); "{}" } }))).await.expect("restart issuer serve"); });
        let endpoint = crate::legacy_auth::AuthEndpointConfig { mode: crate::legacy_auth::AuthMode::Tesla, region: Some(crate::tesla_stream::StreamRegion::Global), token_url: None, allow_loopback_http: true };
        let mut manager = crate::credentials::LegacyAuthManager::from_vault_for_test(restart_store.clone(), recovered.path(), &endpoint,
            Some(url::Url::parse(&format!("http://{address}/token")).expect("issuer URL")), false, Arc::new(crate::legacy_auth::attempt::SystemEnv), Arc::clone(&vault)).expect("CT96 restart manager");
        assert!(manager.generation() == input && manager.access_token() == "access-0001", "CT96 restart cannot adopt discarded frames");
        let _ = manager.refresh_now(&crate::legacy_auth::build_auth_client(true).expect("auth client"), SystemTime::now()).await;
        assert!(posts.load(Ordering::SeqCst) == 0 && vault.test_commit_counts().0 == 0, "CT96 read-only restart sends no refresh and writes no credential row");
        server.abort(); let _ = server.await;
    });
}

#[test]
fn ct092_power_cuts_refresh_rotation_migration() {
    crate::crypto::install_default_provider();
    use std::cell::RefCell;
    use std::rc::Rc;
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { fixtures::power_observer(None); fixtures::power_seal_observer(None); credential_migration::seam::reset(); } }
    let _reset = Reset;
    for operation in ["entry", "refresh", "rotation", "migration"] {
        credential_migration::seam::reset();
        let (root, input) = match operation {
            "entry" => (crate::private_tempdir().expect("entry root"), None),
            "migration" => { fake_on(); (admitted_migration_fixture(false, None, false, 0), None) },
            _ => { let (root, generation) = seeded(); (root, Some(generation)) },
        };
        // A checkpoint makes the initial database and its image self-contained.
        // The entry root has no catalogue yet: opening without create leaves its
        // creation to the store, so the process umask never sets the file mode.
        if root.path().join("hub.sqlite").exists() {
            let db = rusqlite::Connection::open_with_flags(root.path().join("hub.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).expect("initial image observer");
            db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").expect("initial checkpoint"); drop(db);
        }
        let durable = Rc::new(RefCell::new(CutImage::initial(root.path())));
        if input.is_some() {
            let directory=private_directory::open_validated(&root.path().join("secrets"),false).expect("initial keys");
            let keys=credential_keys::load_keyring_read_only(&directory).expect("initial ring").expect("present");
            durable.borrow_mut().seals.insert(keys.keyring().current.id.0,2);
        }
        let cuts = Rc::new(RefCell::new(vec![durable.borrow().clone()]));
        let seals=Rc::clone(&durable);
        fixtures::power_seal_observer(Some(Box::new(move |id| { *seals.borrow_mut().seals.entry(id.0).or_default()+=1; })));
        let image = Rc::clone(&durable); let saved = Rc::clone(&cuts); let data = root.path().to_owned();
        fixtures::power_observer(Some(Box::new(move |kind, path| {
            let mut image = image.borrow_mut();
            match kind {
                "database_barrier" => image.capture(&data, false),
                "directory_barrier" if path == Some(data.join("secrets").as_path()) => image.capture(&data, true),
                "sent" => image.sent = true,
                "seal" => (),
                _ => return,
            }
            saved.borrow_mut().push(image.clone());
        })));
        let vault = open(root.path());
        match operation {
            "entry" => { vault.store_supplied_entry(entry("access-0001", "refresh-0001"), ReplacePolicy::ReplaceAny).expect("entry"); }
            "refresh" => {
                let input = input.expect("input"); let receipt = vault.begin_refresh(OWNER, input).expect("durable begin");
                // The issuer acknowledges a real loopback request only after
                // begin returned. No completion proof precedes this send.
                tokio::runtime::Builder::new_current_thread().enable_all().build().expect("issuer runtime").block_on(async {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("issuer listener"); let addr = listener.local_addr().expect("issuer address");
                    let server = tokio::spawn(async move { axum::serve(listener, axum::Router::new().route("/token", axum::routing::post(|| async { "successor" }))).await.expect("issuer serve"); });
                    reqwest::Client::new().post(format!("http://{addr}/token")).body("refresh-0001").send().await.expect("issuer acknowledgement");
                    server.abort(); let _ = server.await;
                });
                fixtures::power_event("sent", None);
                vault.persist_refreshed(OWNER, receipt, input, Uuid::new_v4(), "access-0002", "refresh-0002", 1_800_028_800, 1_800_027_000).expect("complete");
            }
            "rotation" => vault.rotate().expect("rotation"),
            "migration" => complete(root.path()),
            _ => unreachable!(),
        }
        fixtures::power_observer(None); fixtures::power_seal_observer(None); drop(vault);
        assert!(cuts.borrow().len() >= 3, "CT92.multiple barrier cut points");
        for image in cuts.borrow().iter() {
            let recovered = image.reopen();
            if operation == "migration" { fake_on(); }
            let vault = open(recovered.path());
            for slot in [OWNER, CredentialSlot::FleetApi] {
                match vault.load(slot) {
                    Ok(Some(row)) => assert!(!row.access.is_empty(), "CT92.every occupied pair authenticates"),
                    Ok(None) => (),
                    Err(CredentialStoreError::MigrationPending(_)) => assert!(recovered.path().join("secrets").join(REL).exists(), "CT92.pending candidate retained"),
                    // M02-T02: a non-own or pre-upgrade pair is refused at load, unread.
                    Err(CredentialStoreError::SourceCredentialDisabled(_)) => (),
                    Err(_) => panic!("CT92.row lacks its key"),
                }
            }
            if image.sent { assert!(vault.begin_refresh(OWNER, input.expect("sent input")).is_err(), "CT92.received input never reusable"); }
            let mut state = vault.lock().expect("recovered state");
            let reserved: i64 = state.connection.test_connection().query_row("SELECT coalesce(sum(reserved_invocations),0) FROM credential_key_usage", [], |r| r.get(0)).expect("durable reservations");
            let occupied: i64 = state.connection.test_connection().query_row("SELECT count(*) FROM credential_pairs", [], |r| r.get(0)).expect("occupied count");
            assert!(reserved >= occupied * 2, "CT92.reservation covers every sealed pair");
            for (id,count) in &image.seals {
                assert!(state.connection.credential_reserved_invocations(super::super::credential_envelope::KeyId(*id)).expect("key reservation")>=*count,"CT92.durable per-key reservations cover every seal before the cut");
            }
            // A fresh proved reservation must also be usable after replay.
            state.guard_read().expect("recovered proof");
        }
    }
}
