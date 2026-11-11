// SPDX-License-Identifier: AGPL-3.0-only

// Supplied token entries are refused (M02-T02) and the CLI sign-in that paused
// the service is gone (M02-T10): these cover the control commands on a pair
// the Hub's own login produced.

fn store_owner(root: &Path, access: &str, refresh: &str) {
    sign_in::seed_owner_pair_for_test(root, access, refresh, None).expect("own-login pair");
}
fn test_bridge(store: HubStore, admission: Arc<AdmittedUserHub>, root: &Path) -> Result<CredentialVaultEntry, teslatlas_hub::CredentialVaultEntryError> {
    CredentialVaultEntry::open_for_test(store, admission, root, true)
}
fn count(root: &Path, query: &str) -> i64 { sql(root).query_row(query, [], |row| row.get(0)).expect("synthetic count") }
fn ring_bytes(root: &Path) -> Zeroizing<Vec<u8>> { Zeroizing::new(fs::read(root.join("secrets").join("credential-keys")).expect("published keyring")) }
fn command_config(root: &Path) -> HubConfig {
    HubConfig::from_exact_bytes(format!("data_dir = {root:?}\nbind = '127.0.0.1:18443'\n[collector]\ninterval_seconds = 0\n").as_bytes()).expect("command configuration").0
}

#[test]
fn ct015_empty_store_first_entry() {
    let root = root();
    store_owner(root.path(), "access-0001", "refresh-0001");
    let status = projection(root.path());
    assert_eq!((status.legacy_credentials.origin, status.legacy_credentials.refresh_owner), (Some("hub_login"), Some("hub")));
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
    assert_eq!(before["legacyCredentials"]["origin"], "hub_login");
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
    with_entry(root.path(), true, |entry| sign_in::spend_refresh_for_test(entry, false));
    let opened = std::cell::Cell::new(false);
    let error = run_credential_control_with(&command_config(root.path()), &ControlCommand::SignOut, || Err(std::io::Error::other("stop-refused").into()), |store, admission, root| { opened.set(true); test_bridge(store, admission, root) }).unwrap_err();
    assert_eq!(error.to_string(), "stop-refused"); assert!(!opened.get());
    assert_eq!(count(root.path(), "SELECT count(*) FROM credential_pairs"), 1);
    command_sign_out(root.path());
}

#[test]
fn ct087_sign_out() {
    let root = root(); store_owner(root.path(), "access-0001", "refresh-0001");
    assert_eq!(projection(root.path()).credential_store.format_state, "complete");
    command_sign_out(root.path());
    assert_eq!(projection(root.path()).credential_store.format_state, "complete");
}

#[test]
fn ct098_no_override_inputs() {
    // No token, override or endpoint option exists on any sign-in path.
    for args in [
        vec!["hub", "setup", "--confirm-new-token"],
        vec!["hub", "setup", "--sign-in"],
        vec!["hub", "setup", "--tokens-stdin"],
        vec!["hub", "setup", "--access-token-file", "token", "--refresh-token-file", "token"],
        vec!["hub", "setup-fleet", "--sign-in"],
        vec!["hub", "setup-fleet", "--all-vehicles"],
        vec!["hub", "setup", "--vehicle-id", "1", "--all-vehicles"],
    ] {
        assert!(Cli::try_parse_from(args.clone()).is_err(), "{args:?} is refused by parsing");
    }
    for flag in ["--authorization-url", "--token-url", "--redirect-uri", "--client-id", "--scope", "--state", "--client-secret"] {
        assert!(Cli::try_parse_from(["hub", "setup", flag, "ungranted-0001"]).is_err());
    }
    assert!(matches!(Cli::try_parse_from(["hub", "setup", "--all-vehicles"]).expect("setup grammar").command, Command::Setup { all_vehicles: true, vehicle_id: None }));
    assert!(matches!(Cli::try_parse_from(["hub", "setup-fleet"]).expect("parked verb").command, Command::SetupFleet));
    let base = ["hub", "migrate", "--source", "postgres://127.0.0.1/source", "--car-id", "1", "--postgres-password-file", "pass", "--accept-schema-evidence", "--online-snapshot"];
    assert!(Cli::try_parse_from(base).is_ok());
    for removed in [["--access-token-file", "access"], ["--replace-credentials", "x"], ["--preserve-existing-credentials", "x"], ["--source-token-key-file", "key"], ["--encryption-key-file", "key"]] {
        let mut args = base.to_vec(); args.extend(removed);
        assert_eq!(Cli::try_parse_from(args).unwrap_err().kind(), clap::error::ErrorKind::UnknownArgument);
    }
}
