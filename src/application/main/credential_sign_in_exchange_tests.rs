// SPDX-License-Identifier: AGPL-3.0-only

// M02-T12 (DEC-088): the service's `setup` backend exchanges the owner's code
// itself. The library is not built for tests here, so every exchange asks the
// process-wide egress gate, which no test opens: these tests prove that gate
// and the local refusals against a loopback stub issuer that must see no
// connection. Storing the Hub's own pair is proven in the library
// (`auth::credential_sign_in::exchange::tests`).

const EXCHANGE_VERIFIER: &str = "synthetic-verifier-0101-abcdefghijklmnopqrstuvwxyz-0123456789";

fn exchange_entry(root: &Path) -> CredentialVaultEntry {
    let store = HubStore::initialize(root).expect("store");
    let admission = AdmittedUserHub::admit(root).expect("admission");
    match CredentialVaultEntry::open_for_test(store, admission, root, true) {
        Ok(entry) => entry,
        Err(error) => panic!("credential entry: {}", error.code()),
    }
}

fn stub_setup(entry: &CredentialVaultEntry, issuer: &StubIssuer, code: &str, redirect: &str) -> Result<serde_json::Value, OperatorRefusal> {
    let token = format!("{}/token", issuer.endpoint);
    run_owner_exchange(|| sign_in::exchange_owner_code_with_stub_for_test(entry, code, EXCHANGE_VERIFIER, redirect, &token))
        .map_err(setup_refusal)?;
    Ok(owner_setup_report())
}

fn assert_no_pair(root: &Path) {
    assert_eq!(sql(root).query_row("SELECT count(*) FROM credential_pairs", [], |row| row.get::<_, i64>(0)).expect("pair count"), 0);
    assert_eq!(sql(root).query_row("SELECT count(*) FROM credential_sign_ins", [], |row| row.get::<_, i64>(0)).expect("binding count"), 0);
    assert!(!projection(root).legacy_credentials.present, "no own pair");
}

/// With no Open window the service's exchange is refused `window_closed` by
/// the production gate before any connection, and nothing is stored.
#[test]
fn sign_in_exchange_setup_refuses_while_the_gate_is_closed() {
    assert!(!teslatlas_hub::collection::window::EgressGate::process_wide().is_open(), "no test opens the process gate");
    let root = root();
    let entry = exchange_entry(root.path());
    let issuer = StubIssuer::valid();
    let refusal = stub_setup(&entry, &issuer, "code-bin-closed-0001", sign_in::OWNER_REDIRECT_URI).unwrap_err();
    assert_eq!(refusal.code, "window_closed");
    assert!(!refusal.message.contains("code-bin-closed-0001") && !refusal.message.contains(EXCHANGE_VERIFIER), "no material in the refusal");
    assert_eq!(issuer.calls(), 0, "no connection opened");
    drop(entry);
    assert_no_pair(root.path());
}

/// A wrong redirect is refused locally, and a code is used at most once: the
/// second `setup` with the same code is refused locally, whatever the first
/// one's outcome.
#[test]
fn sign_in_exchange_setup_refuses_a_wrong_redirect_and_a_reused_code_locally() {
    let root = root();
    let entry = exchange_entry(root.path());
    let issuer = StubIssuer::valid();
    for redirect in ["https://auth.tesla.com/void/callback", "http://127.0.0.1/callback", "tesla://auth/callback?x=1"] {
        let refusal = stub_setup(&entry, &issuer, "code-bin-redirect-0001", redirect).unwrap_err();
        assert_eq!(refusal.code, sign_in::SIGN_IN_MATERIAL_INVALID);
    }
    assert_eq!(stub_setup(&entry, &issuer, "code-bin-reused-0001", sign_in::OWNER_REDIRECT_URI).unwrap_err().code, "window_closed");
    let refusal = stub_setup(&entry, &issuer, "code-bin-reused-0001", sign_in::OWNER_REDIRECT_URI).unwrap_err();
    assert_eq!(refusal.code, sign_in::SIGN_IN_CODE_REUSED);
    assert!(!refusal.message.contains("code-bin-reused-0001"), "no code in the refusal");
    assert_eq!(issuer.calls(), 0, "nothing sent");
    drop(entry);
    assert_no_pair(root.path());
}

/// What `setup` reports names no token, code or account detail, and the stub
/// path never accepts the real token endpoint.
#[test]
fn sign_in_exchange_setup_report_names_no_secret() {
    assert_eq!(owner_setup_report(), serde_json::json!({"status": "signed_in", "slot": "owner_api", "origin": "hub_login", "vehicleDiscovery": "not_run"}));
    let root = root();
    let entry = exchange_entry(root.path());
    let error = run_owner_exchange(|| sign_in::exchange_owner_code_with_stub_for_test(&entry, "code-bin-real-0001", EXCHANGE_VERIFIER, sign_in::OWNER_REDIRECT_URI, sign_in::OWNER_TOKEN_ENDPOINT)).unwrap_err();
    assert_eq!(error.code(), "sign_in_settings_invalid");
    drop(entry);
    assert_no_pair(root.path());
}

fn setup_input(code: &str, redirect: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"code": code, "codeVerifier": EXCHANGE_VERIFIER, "redirectUri": redirect})).expect("setup input")
}

/// Round 2, finding 1: the forwarded `setup` command's own input parser
/// admits exactly the owner redirect the Mac app registers, and refuses every
/// other one before anything is forwarded. What it admits reaches the
/// exchange, which then refuses only at the (closed) egress gate.
#[test]
fn sign_in_exchange_forwarded_setup_parser_admits_the_owner_redirect() {
    let material = parse_setup_material(&setup_input("code-bin-parse-0001", sign_in::OWNER_REDIRECT_URI))
        .unwrap_or_else(|refusal| panic!("the owner redirect is refused: {}", refusal.code));
    assert_eq!(material.redirect_uri, sign_in::OWNER_REDIRECT_URI);
    for other in ["https://auth.tesla.com/void/callback", "http://127.0.0.1/callback", "http://localhost/callback",
        "tesla://auth/callback/", "tesla://auth/callback?x=1", "TESLA://auth/callback", ""] {
        let refusal = parse_setup_material(&setup_input("code-bin-parse-0002", other)).unwrap_err();
        assert_eq!(refusal.code, operator::admission::SETUP_INPUT_INVALID, "redirect {other:?}");
        assert!(!refusal.message.contains("code-bin-parse-0002"), "no code in the refusal");
    }
    let root = root();
    let entry = exchange_entry(root.path());
    let issuer = StubIssuer::valid();
    let token = format!("{}/token", issuer.endpoint);
    let error = run_owner_exchange(|| sign_in::exchange_owner_code_with_stub_for_test(
        &entry, &material.code, &material.code_verifier, &material.redirect_uri, &token)).unwrap_err();
    assert_eq!(error.code(), "window_closed", "the parsed material passes every local check");
    assert_eq!(issuer.calls(), 0, "nothing sent");
    drop(entry);
    assert_no_pair(root.path());
}
