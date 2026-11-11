// SPDX-License-Identifier: AGPL-3.0-only

fn dispatch_with_test_bridge(cli: Cli) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error>> {
    struct Reset(Option<CommandEntryOpener>, Option<serde_json::Value>);
    impl Drop for Reset {
        fn drop(&mut self) {
            COMMAND_ENTRY_OPENER.with(|slot| slot.set(self.0));
            COMMAND_RESTORE_RESULT.with(|slot| *slot.borrow_mut() = self.1.take());
        }
    }
    let reset = Reset(COMMAND_ENTRY_OPENER.with(|slot| slot.replace(Some(test_bridge))), COMMAND_RESTORE_RESULT.with(|slot| slot.borrow_mut().take()));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("command runtime");
    let result = runtime.block_on(run(cli));
    let report = COMMAND_RESTORE_RESULT.with(|slot| slot.borrow_mut().take());
    drop(reset);
    result.map(|_| report)
}

fn write_command_config(root: &Path) -> PathBuf {
    let path = root.join("config.toml");
    fs::write(&path, format!("data_dir = {root:?}\nbind = '127.0.0.1:18443'\n[collector]\ninterval_seconds = 0\n")).expect("synthetic configuration");
    path
}

#[test]
fn ct088_barriers_entry_command() {
    let root = root();
    let scope = sign_in::SignInScope::open_for_test(root.path(), true).expect("admitted process");
    let fleet = sign_in::prepare_fleet_entry(Zeroizing::new("access-0002".into()), Zeroizing::new("refresh-0002".into()), "client-0001".into(), FleetRegion::EuropeMiddleEastAndAfrica, 28800).expect("Fleet fixture");
    sign_in::persist_fleet_entry(scope.entry(), fleet, false).expect("existing other provider");
    let events = Rc::new(RefCell::new(Vec::new())); let observed = Rc::clone(&events);
    let fault = CredentialVaultEntry::test_faults(false, Some(1), None, Box::new(move |point| observed.borrow_mut().push(point.to_owned())));
    let error = sign_in::persist_owner_entry(scope.entry(), entered("access-0001", "refresh-0001"), true).unwrap_err();
    assert_eq!(error.code(), "durability_unavailable");
    assert!(!error.pair_stored(), "a failed barrier never authorises the dependent clear");
    assert_eq!(count(root.path(), "SELECT count(*) FROM credential_pairs WHERE slot='fleet_api'"), 1, "the dependent provider clear did not run");
    assert!(!events.borrow().iter().any(|event| event == "credential_barrier_passed"));
    drop(fault); scope.release().expect("release after failed barrier");
    with_entry(root.path(), true, |entry| {
        let prepared = sign_in::prepare_fleet_entry(Zeroizing::new("access-0002".into()), Zeroizing::new("refresh-0002".into()), "client-0001".into(), FleetRegion::EuropeMiddleEastAndAfrica, 28800)?;
        sign_in::persist_fleet_entry(entry, prepared, false)
    });
    let fault = CredentialVaultEntry::test_faults(false, Some(1), None, Box::new(|_| {}));
    let result = run_credential_control_with(&command_config(root.path()), &ControlCommand::SignOut, || Ok(()), test_bridge);
    assert!(result.is_err(), "a failed clear barrier cannot report signed out");
    assert_eq!(count(root.path(), "SELECT count(*) FROM credential_pairs WHERE slot='fleet_api'"), 0, "both independent clears are attempted");
    drop(fault);
    command_sign_out(root.path());
}

#[test]
fn ct092_power_cuts_entry() {
    use std::os::unix::fs::PermissionsExt;
    for point in ["entry_reserved", "entry_sealed", "entry_committed", "credential_barrier_passed"] {
        let root = root();
        let scope = sign_in::SignInScope::open_for_test(root.path(), true).expect("admitted process");
        let image = Rc::new(RefCell::new(None)); let durable = Rc::clone(&image); let data = root.path().to_owned();
        let fault = CredentialVaultEntry::test_faults(false, None, Some(point), Box::new(move |event| {
            if matches!(event, "entry_reserved" | "credential_barrier_passed") {
                let mut files = Vec::new();
                for relative in [PathBuf::from("hub.sqlite"), PathBuf::from("hub.sqlite-wal"), PathBuf::from("secrets").join("credential-keys")] {
                    match fs::read(data.join(&relative)) {
                        Ok(bytes) => files.push((relative, Zeroizing::new(bytes))),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                        Err(error) => panic!("durable image read: {}", error.kind()),
                    }
                }
                *durable.borrow_mut() = Some(files);
            }
        }));
        assert_eq!(sign_in::persist_owner_entry(scope.entry(), entered("access-0001", "refresh-0001"), false).unwrap_err().code(), "durability_unavailable");
        drop(fault); scope.release().expect("cut releases the process");
        // Discard everything after the last passed barrier, including any
        // checkpoint performed by connection destruction during test cleanup.
        for name in ["hub.sqlite-wal", "hub.sqlite-shm"] {
            let path = root.path().join(name);
            if path.exists() { fs::rename(&path, root.path().join(format!("pre-cut-{name}"))).expect("retained process image"); }
        }
        for (relative, bytes) in image.borrow_mut().take().expect("last passed barrier image") {
            let path = root.path().join(relative); fs::write(&path, &*bytes).expect("restore durable image");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("image custody");
        }
        let present = projection(root.path()).legacy_credentials.present;
        assert_eq!(present, point == "credential_barrier_passed");
        if present {
            assert!(sign_in::pair_matches_for_test(root.path(), false, "access-0001", "refresh-0001").expect("fresh-process authenticated pair"));
        } else {
            store_owner(root.path(), "access-0002", "refresh-0002");
        }
        assert_eq!(count(root.path(), "SELECT count(*) FROM credential_pairs"), 1);
        assert!(count(root.path(), "SELECT sum(reserved_invocations) FROM credential_key_usage") >= 2);
    }
}

fn recovery_fixture(root: &Path, key: &[u8], fleet: &[u8], cursor: &[u8]) -> PathBuf {
    use aes_gcm::{Aes256Gcm, Nonce, aead::{Aead, KeyInit, Payload}};
    use std::os::unix::fs::PermissionsExt;
    let magic = b"TESLATLAS-HUB-CREDENTIAL-RECOVERY-V1\0";
    let id = HubStore::open_read_only(root).expect("identity").installation_id().expect("installation");
    let mut plaintext = Zeroizing::new(id.as_bytes().to_vec());
    plaintext.push(7); plaintext.extend_from_slice(&(key.len() as u16).to_be_bytes()); plaintext.extend_from_slice(key);
    plaintext.extend_from_slice(cursor); plaintext.extend_from_slice(fleet);
    let cipher = Aes256Gcm::new_from_slice(&[7;32]).expect("synthetic recovery key");
    let nonce = [8;12];
    let ciphertext = cipher.encrypt(&Nonce::from(nonce), Payload { msg: &plaintext, aad: magic }).expect("synthetic export");
    let mut envelope = magic.to_vec(); envelope.extend_from_slice(&nonce); envelope.extend_from_slice(&ciphertext);
    let path = root.join("export.tthcr"); fs::write(&path, envelope).expect("export");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("export custody"); path
}

#[test]
fn ct061_command_restore() {
    use std::os::unix::fs::PermissionsExt;
    let root = root();
    let key = CredentialVaultEntry::seed_migration_for_test(root.path(), 0, true, false).expect("pre-upgrade process");
    let fleet_path = root.path().join("secrets").join("fleet-credentials.key");
    let fleet = Zeroizing::new(fs::read(&fleet_path).expect("fixture Fleet member"));
    fs::rename(&fleet_path, root.path().join("saved-fleet-member")).expect("data-only restore");
    let cursor = Zeroizing::new([9;32]);
    let export = recovery_fixture(root.path(), &key, &fleet, cursor.as_slice());
    let config = write_command_config(root.path());
    let recovery = root.path().join("recovery.key"); fs::write(&recovery, [7;32]).expect("recovery key");
    fs::set_permissions(&recovery, fs::Permissions::from_mode(0o600)).expect("recovery key custody");
    let cli = || Cli::try_parse_from(["hub", "--config", config.to_str().unwrap(), "restore-recovery-credentials", "--source", export.to_str().unwrap(), "--recovery-key-file", recovery.to_str().unwrap()]).expect("real restore grammar");
    let refused = teslatlas_hub::credential_recovery::with_restore_floor_for_test(79, || dispatch_with_test_bridge(cli())).unwrap_err();
    assert!(matches!(refused.downcast_ref::<teslatlas_hub::credential_recovery::CredentialRecoveryError>(), Some(teslatlas_hub::credential_recovery::CredentialRecoveryError::InvalidPayload(_))));
    for member in ["pre-upgrade-owner-api.key", "pre-upgrade-fleet-api.key", "hub-cursor.key"] {
        assert!(!root.path().join("secrets").join(member).exists(), "floor refusal publishes no member");
    }
    let restored = dispatch_with_test_bridge(cli()).expect("real restore dispatch").expect("actual command report");
    assert_eq!(restored["status"], "restored");
    for field in ["cursorKeyIncluded", "preUpgradeOwnerApiKeyIncluded", "preUpgradeFleetApiKeyIncluded"] { assert_eq!(restored[field], true); }
    for (member, expected) in [("pre-upgrade-owner-api.key",key.as_slice()),("pre-upgrade-fleet-api.key",fleet.as_slice()),("hub-cursor.key",cursor.as_slice())] {
        let path = root.path().join("secrets").join(member);
        assert_eq!(fs::read(&path).expect("restored member"), expected);
        assert_eq!(fs::metadata(path).expect("member metadata").permissions().mode() & 0o777, 0o600);
    }
    with_entry(root.path(), true, |_| Ok(()));
    let status = projection(root.path());
    assert_eq!((status.legacy_credentials.store_state,status.fleet_credentials.store_state,status.credential_store.format_state), ("ok","ok","complete"));
    for member in ["pre-upgrade-owner-api.key", "pre-upgrade-fleet-api.key"] { assert!(!root.path().join("secrets").join(member).exists()); }
    assert_eq!(fs::read(root.path().join("secrets").join("hub-cursor.key")).expect("retained cursor"), cursor.as_slice());
}

#[test]
fn ct053_blocked_slot_entry_and_grant() {
    for (candidate, reason) in [(0, "old_key_unavailable"), (2, "old_pair_invalid")] {
        let root = root();
        CredentialVaultEntry::seed_migration_for_test(root.path(), candidate, false, false).expect("pending fixture");
        let before = projection(root.path());
        assert_eq!((before.credential_store.keyring, before.credential_store.key_age_days), ("ok", None));
        with_entry(root.path(), true, |entry| {
            assert_eq!(sign_in::persist_owner_entry(entry, entered("access-0002", "refresh-0002"), false).unwrap_err().code(), "fresh_sign_in_required");
            Ok(())
        });
        let blocked = projection(root.path());
        assert_eq!(blocked.legacy_credentials.store_state, "migration_blocked");
        assert_eq!(blocked.credential_store.blocked_reason.owner_api, Some(reason));
        assert_eq!((blocked.credential_store.keyring, blocked.credential_store.key_age_days), ("ok", Some(0)));
        let issuer = StubIssuer::valid();
        let (result, output, _) = command_sign_in(&issuer, root.path(), false);
        result.expect("genuine grant replaces the blocked slot");
        assert!(std::str::from_utf8(&output).expect("outcome JSON").contains("signed_in")); assert_eq!(issuer.calls(), 1);
        assert_eq!(projection(root.path()).legacy_credentials.origin, Some("hub_login"));
        assert_eq!(projection(root.path()).credential_store.format_state, "complete");
    }
}

#[test]
fn m2p056_keyring_missing_sign_in_retries_phase_b() {
    let root = root();
    CredentialVaultEntry::seed_migration_for_test(root.path(), 1, false, true).expect("pending store and absent keyring");
    with_entry(root.path(), true, |entry| {
        assert_eq!(sign_in::persist_owner_entry(entry, entered("access-0002", "refresh-0002"), false).unwrap_err().code(), "migration_pending");
        Ok(())
    });
    assert_eq!(projection(root.path()).credential_store.keyring, "keyring_missing");
    let issuer = StubIssuer::valid();
    let inspected = Arc::new(AtomicUsize::new(0)); let seen = Arc::clone(&inspected); let data = root.path().to_owned();
    issuer.exchange.lock().expect("exchange").observe_request = Some(Arc::new(move || {
        assert!(data.join("secrets").join("pre-upgrade-owner-api.relocated.key").exists());
        assert!(!data.join("secrets").join("credential-keys").exists(), "binding validation precedes keyring publication");
        seen.fetch_add(1, Ordering::SeqCst);
    }));
    let (result, _, _) = command_sign_in(&issuer, root.path(), false);
    assert!(result.is_ok(), "replacement keyring and Phase B before grant: result={:?} status={:?}", result.err(), projection(root.path()));
    assert_eq!(inspected.load(Ordering::SeqCst), 1); assert_eq!(issuer.calls(), 1);
    let state = projection(root.path());
    assert_eq!(state.credential_store.format_state, "complete");
    assert_eq!(state.legacy_credentials.origin, Some("hub_login"));
    assert!(sign_in::pair_matches_for_test(root.path(), false, "access-0001", "refresh-0001").expect("authenticated grant"));
    assert!(!root.path().join("secrets").join("pre-upgrade-owner-api.relocated.key").exists());
}

fn encoded_pre_upgrade_name() -> String {
    // Accepted M fixture oracle: source-name prefix plus a synthetic key name.
    String::from_utf8(vec![116,101,115,108,97,109,97,116,101,45,108,101,103,97,99,121,46,107,101,121]).expect("encoded fixture name")
}

#[test]
fn ct101_old_names_never_touched_commands() {
    for action in 0..3 {
        let root = root();
        CredentialVaultEntry::seed_migration_for_test(root.path(), 1, true, false).expect("unopened pre-upgrade process");
        let name = encoded_pre_upgrade_name(); let path = root.path().join("secrets").join(&name);
        fs::write(&path, b"synthetic-key-0001").expect("opaque pre-upgrade artefact");
        let events = Rc::new(RefCell::new(Vec::new())); let seen = Rc::clone(&events);
        let observer = CredentialVaultEntry::test_io_observer(Box::new(move |op, path| seen.borrow_mut().push((op.to_owned(), path.to_owned()))));
        if action == 0 {
            let config = write_command_config(root.path());
            let cli = Cli::try_parse_from(["hub","--config",config.to_str().unwrap(),"bootstrap"]).expect("Bootstrap grammar");
            dispatch_with_test_bridge(cli).expect("real first-open Bootstrap dispatch");
            let state = projection(root.path());
            assert!(state.legacy_credentials.present && state.fleet_credentials.present);
        } else {
            let command = if action == 1 { ControlCommand::SignOut } else { ControlCommand::RotateCredentialKey };
            run_credential_control_with(&command_config(root.path()), &command, || Ok(()), test_bridge).expect("real control");
        }
        drop(observer);
        assert_eq!(projection(root.path()).credential_store.format_state, "complete");
        assert!(!events.borrow().iter().any(|(_, touched)| touched == &path));
        assert_eq!(fs::read(&path).expect("untouched artefact"), b"synthetic-key-0001");
        assert!(!events.borrow().is_empty(), "observer saw the actual vault operations");
        for neutral in ["pre-upgrade-owner-api.relocated.key", "fleet-credentials.key"] { assert!(!root.path().join("secrets").join(neutral).exists()); }
    }
}

#[test]
fn ct102_hub_completion_leaves_old_names_untouched() {
    for grant in [false, true] {
        let root = root(); let key = CredentialVaultEntry::seed_migration_for_test(root.path(), 0, true, false).expect("pending fixture");
        let path = root.path().join("secrets").join(encoded_pre_upgrade_name());
        fs::write(&path, &*key).expect("opaque pre-upgrade key");
        let events = Rc::new(RefCell::new(Vec::new())); let seen = Rc::clone(&events);
        let observer = CredentialVaultEntry::test_io_observer(Box::new(move |op, path| seen.borrow_mut().push((op.to_owned(), path.to_owned()))));
        with_entry(root.path(), true, |_| Ok(()));
        assert_eq!(projection(root.path()).legacy_credentials.store_state, "migration_blocked");
        assert_eq!(projection(root.path()).fleet_credentials.store_state, "ok");
        if grant {
            let issuer = StubIssuer::valid(); command_sign_in(&issuer, root.path(), false).0.expect("grant completion");
        } else {
            run_credential_control_with(&command_config(root.path()), &ControlCommand::SignOut, || Ok(()), test_bridge).expect("sign-out completion");
        }
        assert_eq!(projection(root.path()).credential_store.format_state, "complete");
        assert!(!events.borrow().iter().any(|(_, touched)| touched == &path));
        assert_eq!(fs::read(&path).expect("retained encoded key"), key.as_slice());
        let neutral = root.path().join("secrets").join("pre-upgrade-owner-api.relocated.key");
        fs::write(&neutral, &*key).expect("relocated leftover");
        let fault = CredentialVaultEntry::test_faults(true, None, None, Box::new(|_| {}));
        with_entry(root.path(), true, |_| Ok(()));
        assert!(neutral.exists(), "a failed probe never removes a leftover");
        assert_eq!(projection(root.path()).credential_store.durability, Some("unavailable"));
        drop(fault);
        with_entry(root.path(), true, |_| Ok(()));
        assert!(!neutral.exists());
        assert_eq!(projection(root.path()).credential_store.durability, Some("ok"));
        let events = events.borrow();
        let removal = events.iter().rposition(|(op, touched)| op == "remove" && touched == &neutral).expect("neutral removal");
        assert!(events[removal+1..].iter().any(|(op, _)| op == "directory_barrier"));
        drop(observer);
    }
}
