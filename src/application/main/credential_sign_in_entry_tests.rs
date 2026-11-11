// SPDX-License-Identifier: AGPL-3.0-only

fn entered(access: &str, refresh: &str) -> sign_in::PreparedOwnerEntry {
    sign_in::prepare_owner_entry(sign_in::OwnerEntryKind::Direct, Zeroizing::new(access.to_owned()), Some(Zeroizing::new(refresh.to_owned())), false).expect("supplied entry")
}
fn store_owner(root: &Path, access: &str, refresh: &str) {
    with_entry(root, true, |entry| { drop(sign_in::persist_owner_entry(entry, entered(access, refresh), false)?); Ok(()) });
}
fn test_bridge(store: HubStore, admission: Arc<AdmittedUserHub>, root: &Path) -> Result<CredentialVaultEntry, teslatlas_hub::CredentialVaultEntryError> {
    CredentialVaultEntry::open_for_test(store, admission, root, true)
}
fn count(root: &Path, query: &str) -> i64 { sql(root).query_row(query, [], |row| row.get(0)).expect("synthetic count") }
fn ring_bytes(root: &Path) -> Zeroizing<Vec<u8>> { Zeroizing::new(fs::read(root.join("secrets").join("credential-keys")).expect("published keyring")) }

#[test]
fn ct013_first_entry_publication_order() {
    let refused = root();
    with_entry(refused.path(), false, |entry| {
        assert!(!refused.path().join("secrets").join("credential-keys").exists());
        let error = sign_in::persist_owner_entry(entry, entered("access-0001", "refresh-0001"), false).unwrap_err();
        assert_eq!(error.code(), "secrets_backup_exclusion_failed");
        assert!(!refused.path().join("secrets").join("credential-keys").exists(), "exclusion refusal precedes key publication");
        assert_eq!(count(refused.path(), "SELECT count(*) FROM credential_pairs"), 0);
        assert_eq!(count(refused.path(), "SELECT count(*) FROM credential_key_usage"), 0);
        Ok(())
    });
    let accepted = root();
    let trace = Rc::new(RefCell::new(Vec::new())); let observed = Rc::clone(&trace);
    let faults = CredentialVaultEntry::test_faults(false, None, None, Box::new(move |point| observed.borrow_mut().push(point.to_owned())));
    let observed = Rc::clone(&trace);
    let io = CredentialVaultEntry::test_io_observer(Box::new(move |operation, path| {
        if matches!(operation, "file_barrier" | "directory_barrier") { observed.borrow_mut().push(operation.to_owned()); }
        if operation == "rename" && path.file_name().is_some_and(|name| name == "credential-keys") { observed.borrow_mut().push("keyring_publication".into()); }
    }));
    with_entry(accepted.path(), true, |entry| {
        assert!(!accepted.path().join("secrets").join("credential-keys").exists(), "open is lazy");
        drop(sign_in::persist_owner_entry(entry, entered("access-0001", "refresh-0001"), false)?);
        assert!(!ring_bytes(accepted.path()).is_empty());
        assert_eq!(count(accepted.path(), "SELECT sum(reserved_invocations) FROM credential_key_usage"), 2);
        assert_eq!(count(accepted.path(), "SELECT count(*) FROM credential_pairs"), 1);
        Ok(())
    });
    drop(io); drop(faults);
    let trace = trace.borrow();
    let publication = trace.iter().position(|point| point == "keyring_publication").expect("keyring publication");
    assert!(trace[..publication].iter().any(|point| point == "exclusion_verified"));
    assert!(trace[..publication].iter().any(|point| point == "file_barrier"));
    let directory = trace[publication+1..].iter().position(|point| point == "directory_barrier").map(|index| index + publication + 1).expect("directory barrier after publication");
    let reservation = trace.iter().position(|point| point == "entry_reserved").expect("reservation proved");
    let commit = trace.iter().position(|point| point == "entry_committed").expect("pair committed");
    assert!(directory < reservation && reservation < commit);
    assert!(trace[commit+1..].iter().any(|point| point == "credential_barrier_passed"));
}

#[test]
fn ct015_empty_store_first_entry() {
    let root = root();
    store_owner(root.path(), " \taccess-0001\r\n", "\trefresh-0001\n");
    let status = projection(root.path());
    assert_eq!((status.legacy_credentials.origin, status.legacy_credentials.refresh_owner), (Some("direct_entry"), Some("hub")));
    assert_eq!((status.legacy_credentials.expires_at, status.legacy_credentials.next_refresh_at), (Some(0), Some(0)));
    assert_eq!(status.credential_store.keyring, "ok");
    assert_eq!(status.credential_store.format_state, "complete");
    assert_eq!(count(root.path(), "SELECT count(*) FROM credential_pairs"), 1);
}

#[test]
fn ct018_entry_status_rotation_command() {
    let root = root(); store_owner(root.path(), "access-0001", "refresh-0001");
    let keys = ring_bytes(root.path());
    let config = command_config(root.path());
    let before = build_status_report(&HubStore::open_read_only(root.path()).expect("status"), &config, current_epoch_ms().expect("clock")).expect("status JSON");
    assert_eq!(before["legacyCredentials"]["origin"], "direct_entry");
    let stopped = std::cell::Cell::new(false);
    let result = run_credential_control_with(&config, &ControlCommand::RotateCredentialKey, || { stopped.set(true); Ok(()) }, |store, admission, root| {
        assert!(stopped.get(), "persistent stop precedes vault open");
        test_bridge(store, admission, root)
    }).expect("rotation command");
    assert_eq!(result, serde_json::json!({"status":"rotated", "rows":1, "keyAgeDays":0}));
    assert!(keys.as_slice() != ring_bytes(root.path()).as_slice(), "rotation publishes a new keyring");
    assert_eq!(projection(root.path()).legacy_credentials.store_state, "ok");
    assert_eq!(count(root.path(), "SELECT count(*) FROM credential_pairs"), 1);
    let cli = Cli::try_parse_from(["hub", "control", "rotate-credential-key"]).expect("rotation grammar");
    assert!(matches!(cli.command, Command::Control { command: ControlCommand::RotateCredentialKey, .. }));
}

#[test]
fn ct022_forced_rotation_first_entry() {
    let root = root(); store_owner(root.path(), "access-0001", "refresh-0001");
    let keys = ring_bytes(root.path());
    sql(root.path()).execute("UPDATE credential_key_usage SET reserved_invocations=?1", [1_i64 << 31]).expect("threshold fixture");
    store_owner(root.path(), "access-0002", "refresh-0002");
    assert!(keys.as_slice() != ring_bytes(root.path()).as_slice(), "threshold forces rotation");
    assert_eq!(count(root.path(), "SELECT count(*) FROM credential_pairs"), 1);
    assert_eq!(projection(root.path()).legacy_credentials.store_state, "ok");
}

#[test]
fn ct050_entry_refusal() {
    let root = root(); store_owner(root.path(), "access-0001", "refresh-0001");
    with_entry(root.path(), true, |entry| { sign_in::spend_refresh_for_test(entry, false)?; Ok(()) });
    let keys = ring_bytes(root.path());
    let usage = count(root.path(), "SELECT sum(reserved_invocations) FROM credential_key_usage");
    with_entry(root.path(), true, |entry| {
        let error = sign_in::persist_owner_entry(entry, entered("access-0002", " \trefresh-0001\r\n"), false).unwrap_err();
        assert_eq!(error.code(), "refresh_token_spent");
        Ok(())
    });
    assert!(keys.as_slice() == ring_bytes(root.path()).as_slice());
    assert_eq!(count(root.path(), "SELECT sum(reserved_invocations) FROM credential_key_usage"), usage);
    assert_eq!(count(root.path(), "SELECT count(*) FROM credential_refresh_attempts"), 1);
}

#[test]
fn ct063_source_key_flags() {
    for spelling in ["--source-token-key-file", "--encryption-key-file"] {
        let cli = Cli::try_parse_from(["hub", "migrate", "--source", "postgres://127.0.0.1/source", "--car-id", "1", "--postgres-password-file", "pass", "--accept-schema-evidence", spelling, "key"]).expect("key grammar");
        match cli.command {
            Command::Migrate { source_token_key_file, encryption_key_file, .. } => assert_eq!((source_token_key_file.is_some(), encryption_key_file.is_some()), (spelling == "--source-token-key-file", spelling == "--encryption-key-file")),
            _ => panic!("migration grammar"),
        }
    }
    for (primary, deprecated, expected_reads) in [(Some(Path::new("key-a")), None, 1), (None, Some(Path::new("key-b")), 1), (Some(Path::new("key-a")), Some(Path::new("key-b")), 2), (Some(Path::new("-")), Some(Path::new("-")), 1)] {
        let mut reads = 0; let mut notice = Vec::new();
        let value = prepare_source_key_files_with(primary, deprecated, &mut notice, |_, _| { reads += 1; Ok(Zeroizing::new(b"key-0001\r\n".to_vec())) }).expect("equal source key bytes").expect("key");
        assert_eq!(value.as_slice(), b"key-0001"); assert_eq!(reads, expected_reads);
        assert_eq!(notice.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()).count(), usize::from(deprecated.is_some()));
    }
    let mut notice = Vec::new();
    let error = prepare_source_key_files_with(Some(Path::new("key-a")), Some(Path::new("key-b")), &mut notice, |path, _| {
        Ok(Zeroizing::new(if path == Path::new("key-a") { b"key-0001\n".to_vec() } else { b"key-0001\r\n".to_vec() }))
    }).unwrap_err();
    assert_eq!(error.to_string(), "conflicting_source_key_files", "compare received bytes before newline removal");
    let root = root(); let data = root.path().join("data"); let config_dir = root.path().join("config"); let outside = root.path().join("input");
    for directory in [&data, &config_dir, &outside] { fs::create_dir(directory).expect("fixture directory"); }
    let config = config_dir.join("hub.toml");
    let session_root = root.path().join("sessions"); fs::create_dir(&session_root).expect("synthetic importer root");
    let session = session_root.join(format!("th-{}", uuid::Uuid::new_v4())); fs::create_dir(&session).expect("GUI session contract");
    let descendant = session.join("nested"); fs::create_dir(&descendant).expect("session descendant");
    let result = open_checked_key_directory_with_session_root(&descendant.join("must-not-open"), &data, &config, Some(&session_root));
    let error = match result { Ok(_) => panic!("import session is Hub-managed"), Err(error) => error };
    assert!(error.to_string().starts_with("source_key_file_location_refused:"));
    let refused = read_source_token_key_checked(&data.join("must-not-open"), &data, &config).unwrap_err();
    assert!(refused.to_string().starts_with("source_key_file_location_refused:"));
    let key = outside.join("key"); fs::write(&key, b"key-0001\n").expect("external key");
    assert_eq!(read_source_token_key_checked(&key, &data, &config).expect("external key").as_slice(), b"key-0001");
    let link = outside.join("linked-key"); std::os::unix::fs::symlink(&key, &link).expect("final link");
    assert!(read_source_token_key_checked(&link, &data, &config).is_err());
    let alias = root.path().join("input-alias"); std::os::unix::fs::symlink(&outside, &alias).expect("ancestor link");
    assert!(read_source_token_key_checked(&alias.join("key"), &data, &config).is_ok());
    let shifted = outside.join("key-moved");
    let error = read_source_token_key_raw_with(&key, &data, &config, || { fs::rename(&key, &shifted).expect("swap first entry"); fs::write(&key, b"key-0002").expect("replacement entry"); }).unwrap_err();
    assert!(error.to_string().starts_with("source_key_file_location_refused:"));
    assert!(!error.to_string().contains(key.to_str().expect("synthetic path")));
}

#[test]
fn ct064_hub_sign_in_to_c5_manager() {
    let issuer = StubIssuer::valid(); let root = root();
    let (result, _, _) = command_sign_in(&issuer, root.path(), false); result.expect("real Hub grant");
    assert_eq!(issuer.calls(), 1);
    let envelope: Vec<u8> = sql(root.path()).query_row("SELECT access_envelope FROM credential_pairs WHERE slot='owner_api'", [], |row| row.get(0)).expect("prior pair");
    let observed = Arc::new(AtomicUsize::new(0));
    {
        let data = root.path().to_owned(); let observed = Arc::clone(&observed);
        let mut exchange = issuer.exchange.lock().expect("exchange");
        exchange.response = Zeroizing::new(br#"{"access_token":"access-0002","refresh_token":"refresh-0002","expires_in":28800,"token_type":"Bearer"}"#.to_vec());
        zeroize::Zeroize::zeroize(&mut *exchange.request);
        exchange.observe_request = Some(Arc::new(move || {
            assert_eq!(count(&data, "SELECT count(*) FROM credential_refresh_attempts"), 1, "attempt committed before network");
            assert_eq!(count(&data, "SELECT count(*) FROM credential_refresh_fences"), 1, "spent fence before network");
            let current: Vec<u8> = sql(&data).query_row("SELECT access_envelope FROM credential_pairs WHERE slot='owner_api'", [], |row| row.get(0)).expect("pair at request");
            assert!(current == envelope, "the successor is not stored before its answer");
            observed.fetch_add(1, Ordering::SeqCst);
        }));
    }
    let result = issuer.runtime.block_on(sign_in::refresh_owner_manager_for_test(root.path(), &format!("{}/token", issuer.endpoint), "access-0001", "access-0002")).expect("C5 real manager");
    assert_eq!(result, (true, true), "C5 loads the grant, then adopts the durable successor");
    assert_eq!(observed.load(Ordering::SeqCst), 1);
    assert_eq!(issuer.calls(), 2, "one grant and one refresh");
    let exchange = issuer.exchange.lock().expect("exchange");
    let request = std::str::from_utf8(&exchange.request).expect("C5 JSON request");
    assert!(request.starts_with('{'));
    assert!(request.contains("\"grant_type\":\"refresh_token\""));
    assert!(request.contains("\"refresh_token\":\"refresh-0001\""));
    assert!(!request.contains("\"code\":") && !request.contains("\"code_verifier\":"));
    assert_eq!(projection(root.path()).legacy_credentials.origin, Some("hub_login"));
}

#[test]
fn ct067_cross_slot_entries() {
    let root = root(); store_owner(root.path(), "access-0001", "refresh-0001");
    with_entry(root.path(), true, |entry| { sign_in::spend_refresh_for_test(entry, false)?; Ok(()) });
    with_entry(root.path(), true, |entry| {
        let supplied = sign_in::prepare_fleet_entry(Zeroizing::new("access-0002".into()), Zeroizing::new(" \trefresh-0001\r\n".into()), "client-0001".into(), FleetRegion::EuropeMiddleEastAndAfrica, 28800)?;
        assert_eq!(sign_in::persist_fleet_entry(entry, supplied, false).unwrap_err().code(), "refresh_token_spent");
        let supplied = sign_in::prepare_fleet_entry(Zeroizing::new("access-0003".into()), Zeroizing::new("refresh-0003".into()), "client-0001".into(), FleetRegion::EuropeMiddleEastAndAfrica, 28800)?;
        sign_in::persist_fleet_entry(entry, supplied, false)?;
        sign_in::spend_refresh_for_test(entry, true)?;
        let error = sign_in::persist_owner_entry(entry, entered("access-0004", "refresh-0003"), false).unwrap_err();
        assert_eq!(error.code(), "refresh_token_spent");
        Ok(())
    });
    assert_eq!(count(root.path(), "SELECT count(*) FROM credential_refresh_attempts"), 2);
    assert!(projection(root.path()).legacy_credentials.present && projection(root.path()).fleet_credentials.present);
}

#[test]
fn ct072_stdin_origin_refusals() {
    for origin in ["hub_login", "source_token", "direct_entry", "provider_entry", "fleet_oauth"] {
        let bytes = Zeroizing::new(format!(r#"{{"accessToken":"access-0001","refreshToken":"refresh-0001","origin":"{origin}"}}"#));
        let error = sign_in::decode_owner_entry(bytes.as_bytes(), sign_in::OwnerEntryKind::Direct, false).unwrap_err();
        assert!(matches!(error, sign_in::OwnerInputError::Credential(ref error) if error.code() == "invalid_origin"));
    }
    for bytes in [br#"{"accessToken":"access-0001","refreshToken":"refresh-0001","unknownField":"private-value-0001"}"#.as_slice(), br#"{"accessToken":"access-0001","accessToken":"private-value-0001","refreshToken":"refresh-0001"}"#] {
        let error = sign_in::decode_owner_entry(bytes, sign_in::OwnerEntryKind::Direct, false).unwrap_err();
        assert!(!error.to_string().contains("private-value-0001"));
        assert!(matches!(error, sign_in::OwnerInputError::UnknownKey(_) | sign_in::OwnerInputError::DuplicateKey(_)));
    }
    let valid = sign_in::decode_owner_entry(br#"{"accessToken":"access-0001","refreshToken":"refresh-0001"}"#, sign_in::OwnerEntryKind::Direct, false).expect("ordinary supplied form");
    assert!(!format!("{valid:?}").contains("access-0001"));
}

fn command_sign_out(root: &Path) {
    let keys = ring_bytes(root);
    let retained = count(root, "SELECT (SELECT count(*) FROM credential_refresh_attempts)+(SELECT count(*) FROM credential_refresh_fences)+(SELECT count(*) FROM credential_legacy_fences)");
    let used = count(root, "SELECT sum(reserved_invocations) FROM credential_key_usage");
    let result = run_credential_control_with(&command_config(root), &ControlCommand::SignOut, || Ok(()), test_bridge).expect("sign-out command");
    assert_eq!(result, serde_json::json!({"status":"signed_out", "service":"stopped"}));
    assert_eq!(count(root, "SELECT count(*) FROM credential_pairs"), 0);
    assert_eq!(count(root, "SELECT (SELECT count(*) FROM credential_refresh_attempts)+(SELECT count(*) FROM credential_refresh_fences)+(SELECT count(*) FROM credential_legacy_fences)"), retained);
    assert_eq!(count(root, "SELECT sum(reserved_invocations) FROM credential_key_usage"), used);
    assert!(keys.as_slice() == ring_bytes(root).as_slice());
}

#[test]
fn ct077_sign_out() {
    let root = root(); store_owner(root.path(), "access-0001", "refresh-0001");
    with_entry(root.path(), true, |entry| {
        sign_in::spend_refresh_for_test(entry, false)?;
        let supplied = sign_in::prepare_fleet_entry(Zeroizing::new("access-0002".into()), Zeroizing::new("refresh-0002".into()), "client-0001".into(), FleetRegion::EuropeMiddleEastAndAfrica, 28800)?;
        sign_in::persist_fleet_entry(entry, supplied, false)
    });
    let opened = std::cell::Cell::new(false);
    let error = run_credential_control_with(&command_config(root.path()), &ControlCommand::SignOut, || Err(std::io::Error::other("stop-refused").into()), |store, admission, root| { opened.set(true); test_bridge(store, admission, root) }).unwrap_err();
    assert_eq!(error.to_string(), "stop-refused"); assert!(!opened.get());
    assert_eq!(count(root.path(), "SELECT count(*) FROM credential_pairs"), 2);
    command_sign_out(root.path());
}

#[test]
fn ct087_sign_out() {
    let root = root(); let issuer = StubIssuer::valid();
    let (result, _, _) = command_sign_in(&issuer, root.path(), false); result.expect("Hub grant and aftercare");
    assert_eq!(projection(root.path()).credential_store.format_state, "complete");
    command_sign_out(root.path());
    assert_eq!(projection(root.path()).credential_store.format_state, "complete");
}

#[test]
fn ct098_no_override_inputs() {
    let error = sign_in::decode_owner_entry(br#"{"accessToken":"access-0001","refreshToken":"refresh-0001","confirmNewToken":true}"#, sign_in::OwnerEntryKind::Direct, false).unwrap_err();
    assert!(matches!(error, sign_in::OwnerInputError::UnknownKey(ref key) if key == "confirmNewToken"));
    for args in [vec!["hub", "setup", "--confirm-new-token"], vec!["hub", "setup", "--sign-in", "--tokens-stdin"], vec!["hub", "setup", "--sign-in", "--vehicle-id", "1"], vec!["hub", "setup-fleet", "--sign-in", "--all-vehicles"], vec!["hub", "setup", "--sign-in", "--access-token-file", "token", "--refresh-token-file", "token"]] {
        assert!(Cli::try_parse_from(args).is_err(), "unknown or conflicting authority is refused by parsing");
    }
    assert!(Cli::try_parse_from(["hub", "setup", "--sign-in"]).is_ok());
    assert!(Cli::try_parse_from(["hub", "setup-fleet", "--sign-in"]).is_ok());
    let supplied = Cli::try_parse_from(["hub", "setup", "--tokens-stdin"]).expect("supplied grammar");
    assert!(matches!(supplied.command, Command::Setup { sign_in: false, tokens_stdin: true, .. }));
    for flag in ["--authorization-url", "--token-url", "--redirect-uri", "--client-id", "--scope", "--state", "--client-secret"] {
        assert!(Cli::try_parse_from(["hub", "setup", "--sign-in", flag, "ungranted-0001"]).is_err());
    }
    let base = vec!["hub", "migrate", "--source", "postgres://127.0.0.1/source", "--car-id", "1", "--postgres-password-file", "pass", "--accept-schema-evidence", "--online-snapshot", "--access-token-file", "access", "--refresh-token-file", "refresh"];
    assert!(matches!(Cli::try_parse_from(base.clone()).expect("preserving default").command, Command::Migrate { replace_credentials: false, .. }));
    let mut replacement = base; replacement.push("--replace-credentials");
    assert!(matches!(Cli::try_parse_from(replacement.clone()).expect("explicit replacement").command, Command::Migrate { replace_credentials: true, .. }));
    replacement.push("--preserve-existing-credentials");
    assert_eq!(Cli::try_parse_from(replacement).unwrap_err().kind(), clap::error::ErrorKind::ArgumentConflict);
}
