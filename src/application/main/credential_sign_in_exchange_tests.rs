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
    let signed_in = run_owner_exchange(|| sign_in::exchange_owner_code_with_stub_for_test(entry, code, EXCHANGE_VERIFIER, redirect, &token))
        .map_err(setup_refusal)?;
    Ok(owner_setup_report(signed_in))
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
    assert_eq!(owner_setup_report(sign_in::OwnerSignIn::Complete), serde_json::json!({"status": "signed_in", "slot": "owner_api", "origin": "hub_login", "aftercare": "complete", "vehicleDiscovery": "not_run"}));
    assert_eq!(owner_setup_report(sign_in::OwnerSignIn::AftercarePending)["aftercare"], "pending");
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

/// A collector run as the service's slot keeps it: the vault opened on the
/// service's one admission when it starts, held until it stops.
#[derive(Clone)]
struct HoldingSlot {
    store: HubStore,
    admission: Arc<AdmittedUserHub>,
    root: std::path::PathBuf,
    held: Arc<Mutex<Option<CredentialVaultEntry>>>,
    starts: Arc<AtomicUsize>,
}

impl teslatlas_hub::collection::window::CollectorSlot for HoldingSlot {
    fn start(&mut self, _deadline_wall: i64) -> Result<(), String> {
        let entry = CredentialVaultEntry::open_for_test(self.store.clone(), Arc::clone(&self.admission), &self.root, true)
            .map_err(|error| error.code().to_owned())?;
        *self.held.lock().expect("held") = Some(entry);
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn is_running(&mut self) -> bool { self.held.lock().expect("held").is_some() }
    fn stop(&mut self) { self.held.lock().expect("held").take(); }
    fn release_lease(&mut self) -> Result<(), String> {
        if self.is_running() { Err("collector_stopping".to_owned()) } else { Ok(()) }
    }
}

/// Round 2, finding 2: a sign-in while the collector holds the vault inside
/// an Open window. The real `setup` path takes custody through the window
/// supervisor, which stops the collector; the sign-in then opens the one
/// vault (the collector's would refuse it) and reaches the exchange, here
/// refused only by the closed process gate. Afterwards the supervisor starts
/// the collector again on a fresh vault.
#[test]
fn sign_in_exchange_setup_takes_the_vault_from_an_active_collector() {
    use teslatlas_hub::collection::window::{CollectorSlot as _, CredentialCustody, Supervisor};
    let root = root();
    let scratch = tempfile::tempdir().expect("scratch");
    let store = HubStore::initialize(root.path()).expect("store");
    let admission = AdmittedUserHub::admit(root.path()).expect("one service admission");
    let window = operator_window::Window::new(scratch.path(), root.path());
    window.controller.open(&window.grant(600)).expect("window opens");
    let slot = HoldingSlot {
        store: store.clone(), admission: Arc::clone(&admission), root: root.path().to_owned(),
        held: Arc::default(), starts: Arc::default(),
    };
    let custody = CredentialCustody::new();
    let supervisor = Arc::new(Mutex::new(
        Supervisor::new(Arc::clone(&window.controller), slot.clone()).with_custody(custody.clone()),
    ));
    assert!(supervisor.lock().expect("supervisor").step().collector_running, "the collector runs inside the window");
    // The reviewer's scenario: with the collector running, a second vault is refused.
    assert!(CredentialVaultEntry::open_for_test(store.clone(), Arc::clone(&admission), root.path(), true).is_err());

    let issuer = StubIssuer::valid();
    let token = format!("{}/token", issuer.endpoint);
    let kick = {
        let supervisor = Arc::clone(&supervisor);
        move || { supervisor.lock().expect("supervisor").step(); }
    };
    let mut probe = slot.clone();
    let collector_ran_during_sign_in = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&collector_ran_during_sign_in);
    let refusal = owner_setup(
        &custody,
        &kick,
        || {
            *seen.lock().expect("seen") = Some(probe.is_running());
            CredentialVaultEntry::open_for_test(store.clone(), Arc::clone(&admission), root.path(), true)
        },
        |entry| run_owner_exchange(|| sign_in::exchange_owner_code_with_stub_for_test(
            entry, "code-bin-custody-0001", EXCHANGE_VERIFIER, sign_in::OWNER_REDIRECT_URI, &token)),
    )
    .unwrap_err();
    assert_eq!(refusal.code, "window_closed", "the sign-in opened the vault and reached the exchange");
    assert_eq!(*collector_ran_during_sign_in.lock().expect("seen"), Some(false), "the collector was stopped first");
    assert_eq!(issuer.calls(), 0, "nothing sent");
    let mut after = slot.clone();
    assert!(after.is_running(), "the collector started again once custody ended");
    assert_eq!(slot.starts.load(Ordering::SeqCst), 2, "a fresh collector run reloads the stored state");
    assert!(!custody.requested(), "custody released");
    after.stop();
    assert_no_pair(root.path());
}
