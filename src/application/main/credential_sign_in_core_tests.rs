// SPDX-License-Identifier: AGPL-3.0-only

use std::{cell::RefCell, rc::Rc, sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}}};
use teslatlas_hub::{CredentialVaultEntry, auth::credential_sign_in::{self as sign_in, CredentialCommandError, SignInAttempt, SignInSettings}, platform::r78_lifecycle::{Completion, ReadinessBounds, ServiceBudgets, ServiceEffects, ServiceFailure}};
use zeroize::Zeroizing;

static SIGN_IN_LOG: std::sync::OnceLock<Arc<Mutex<Zeroizing<Vec<u8>>>>> = std::sync::OnceLock::new();
struct SignInLogWriter(Arc<Mutex<Zeroizing<Vec<u8>>>>);
impl Write for SignInLogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}
fn start_sign_in_log_capture() {
    SIGN_IN_LOG.get_or_init(|| {
        let bytes = Arc::new(Mutex::new(Zeroizing::new(Vec::new())));
        let sink = Arc::clone(&bytes);
        tracing_subscriber::fmt().without_time().with_ansi(false).with_max_level(tracing::Level::TRACE)
            .with_writer(move || SignInLogWriter(Arc::clone(&sink))).try_init().expect("test log capture");
        bytes
    });
}
fn assert_sign_in_logs(output: &str, request: &[u8]) {
    let logs = SIGN_IN_LOG.get().expect("capture started").lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let logs = std::str::from_utf8(&logs).expect("captured UTF-8");
    for value in ["access-0001", "access-0002", "refresh-0001", "refresh-0002", "code-0001", "secret-0001", "id-0001", "trailing-secret-0001", "client-0001", "description-0001"] {
        assert!(!logs.contains(value), "secret value in sign-in logs");
    }
    for line in output.lines() {
        if let Some(address) = line.strip_prefix("{\"authorizationUrl\":\"").and_then(|value| value.strip_suffix("\"}")) {
            assert!(!logs.contains(address), "authorization address in logs");
            for (name, value) in url::form_urlencoded::parse(address.split_once('?').expect("authorization query").1.as_bytes()) {
                if matches!(name.as_ref(), "state" | "code_challenge") { assert!(!logs.contains(value.as_ref()), "sign-in state or challenge in logs"); }
            }
        }
    }
    for (name, value) in url::form_urlencoded::parse(request) {
        if matches!(name.as_ref(), "code_verifier" | "client_secret") { assert!(!logs.contains(value.as_ref()), "verifier or client secret in logs"); }
    }
}

struct RecordingService {
    root: PathBuf,
    loaded: bool,
    events: Rc<RefCell<Vec<&'static str>>>,
}
impl ServiceEffects for RecordingService {
    type Admission = Arc<AdmittedUserHub>;
    type Error = std::io::Error;
    fn now(&self) -> Instant { Instant::now() }
    fn loaded(&mut self, _: Instant) -> Result<bool, Self::Error> { self.events.borrow_mut().push("loaded"); Ok(self.loaded) }
    fn bootout(&mut self, _: Instant) -> Result<(), Self::Error> { self.events.borrow_mut().push("bootout"); self.loaded = false; Ok(()) }
    fn wait_unloaded(&mut self, _: Instant) -> Result<(), Self::Error> { self.events.borrow_mut().push("wait_unloaded"); if self.loaded { Err(std::io::Error::other("loaded")) } else { Ok(()) } }
    fn admit(&mut self, _: Instant) -> Result<Self::Admission, Self::Error> { self.events.borrow_mut().push("admit"); AdmittedUserHub::admit(&self.root).map_err(std::io::Error::other) }
    fn bootstrap(&mut self, _: Instant) -> Result<(), Self::Error> { self.events.borrow_mut().push("bootstrap"); self.loaded = true; Ok(()) }
    fn kickstart(&mut self, _: Instant) -> Result<(), Self::Error> { self.events.borrow_mut().push("kickstart"); Ok(()) }
    fn wait_ready(&mut self, _: Instant, _: ReadinessBounds) -> Result<(), Self::Error> { self.events.borrow_mut().push("wait_ready"); Ok(()) }
    fn record_unwind_restart_failure(&mut self, _: ServiceFailure<Self::Error>) { self.events.borrow_mut().push("restart_failed"); }
}
fn root() -> tempfile::TempDir {
    start_sign_in_log_capture();
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new().prefix("v2-cli-").permissions(fs::Permissions::from_mode(0o700)).tempdir().expect("synthetic root")
}
fn with_entry<T>(root: &Path, excluded: bool, work: impl FnOnce(&CredentialVaultEntry) -> Result<T, CredentialCommandError>) -> T {
    let events = Rc::new(RefCell::new(Vec::new()));
    let mut service = RecordingService { root: root.to_owned(), loaded: true, events: Rc::clone(&events) };
    let completion = sign_in::with_sign_in_scope_for_test(&mut service, ServiceBudgets::SCALED_TEST, root, excluded, work);
    assert_eq!(&*events.borrow(), &["loaded", "bootout", "wait_unloaded", "admit", "loaded", "bootstrap", "kickstart", "wait_ready"]);
    match completion {
        Completion::Finished(Ok(value)) => value,
        Completion::Finished(Err(error)) => panic!("sign-in scope: {error:?}"),
        Completion::RestartFailed { .. } => panic!("sign-in restart failed"),
    }
}
fn projection(root: &Path) -> sign_in::CredentialStatus {
    sign_in::credential_status(&HubStore::open_read_only(root).expect("status lane"), root, current_epoch_ms().expect("clock")).expect("status")
}
fn sql(root: &Path) -> rusqlite::Connection { rusqlite::Connection::open(root.join("hub.sqlite")).expect("synthetic SQL") }

struct Exchange {
    response: Zeroizing<Vec<u8>>,
    request: Zeroizing<Vec<u8>>,
    delay: Duration,
    observe_request: Option<Arc<dyn Fn() + Send + Sync>>,
}
struct StubIssuer {
    runtime: tokio::runtime::Runtime,
    task: tokio::task::JoinHandle<()>,
    endpoint: String,
    calls: Arc<AtomicUsize>,
    exchange: Arc<Mutex<Exchange>>,
}
impl StubIssuer {
    fn new(response: &[u8]) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().expect("issuer runtime");
        let exchange = Arc::new(Mutex::new(Exchange { response: Zeroizing::new(response.to_vec()), request: Zeroizing::new(Vec::new()), delay: Duration::ZERO, observe_request: None }));
        let calls = Arc::new(AtomicUsize::new(0));
        let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).expect("loopback issuer");
        let endpoint = format!("http://{}", listener.local_addr().expect("issuer address"));
        let app = axum::Router::new().route("/token", axum::routing::post({
            let exchange = Arc::clone(&exchange);
            let calls = Arc::clone(&calls);
            move |body: axum::body::Bytes| {
                let exchange = Arc::clone(&exchange);
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let (response, delay, observe) = {
                        let mut exchange = exchange.lock().expect("issuer exchange");
                        exchange.request.extend_from_slice(&body);
                        (exchange.response.to_vec(), exchange.delay, exchange.observe_request.clone())
                    };
                    if let Some(observe) = observe { observe(); }
                    tokio::time::sleep(delay).await;
                    ([(axum::http::header::CONTENT_TYPE, "application/json")], response)
                }
            }
        }));
        let task = runtime.spawn(async move { axum::serve(listener, app).await.expect("issuer serve"); });
        Self { runtime, task, endpoint, calls, exchange }
    }
    fn valid() -> Self { Self::new(br#"{"access_token":"access-0001","refresh_token":"refresh-0001","expires_in":28800,"token_type":"Bearer"}"#) }
    fn settings(&self) -> SignInSettings { SignInSettings::stub(&format!("{}/authorize", self.endpoint), &format!("{}/token", self.endpoint), "http://127.0.0.1/callback").expect("synthetic issuer settings") }
    fn calls(&self) -> usize { self.calls.load(Ordering::SeqCst) }
}
impl Drop for StubIssuer { fn drop(&mut self) { self.task.abort(); } }
fn state(address: &str) -> &str { address.split("state=").nth(1).expect("state parameter").split('&').next().expect("state value") }
fn answer(address: &str, fields: &str) -> Zeroizing<String> { Zeroizing::new(format!(r#"{{"redirectUrl":"http://127.0.0.1/callback?state={}&{fields}"}}"#, state(address))) }
fn redeem(issuer: &StubIssuer, root: &Path, settings: SignInSettings, line: impl FnOnce(&str) -> Zeroizing<String>) -> Result<Vec<&'static str>, CredentialCommandError> {
    with_entry(root, true, |entry| {
        let attempt = SignInAttempt::begin(entry, settings)?;
        let address = attempt.authorization_url();
        let line = line(&address);
        let mut output = Vec::new();
        let result = issuer.runtime.block_on(attempt.redeem(line.as_bytes(), |slot| {
            let projection = projection(root);
            assert!(if slot == "owner_api" { projection.legacy_credentials.present } else { projection.fleet_credentials.present }, "pair barrier precedes success output");
            output.push(if slot == "owner_api" { "owner_api" } else { "fleet_api" });
            Ok(())
        }));
        Ok(result.map(|()| output))
    })
}

#[test]
fn ct100_g_fleet_sign_in() {
    let issuer = StubIssuer::valid(); let root = root();
    let settings = SignInSettings::fleet(br#"{"clientId":"client-0001","region":"eu","redirectUri":"http://127.0.0.1/callback","clientSecret":" secret-0001 "}"#).expect("Fleet settings")
        .with_stub_endpoints(&format!("{}/authorize", issuer.endpoint), &format!("{}/token", issuer.endpoint)).expect("loopback endpoints");
    assert_eq!(redeem(&issuer, root.path(), settings, |address| {
        assert!(!address.contains("code_challenge") && !address.contains("secret-0001"));
        assert!(address.contains("scope=openid+offline_access+vehicle_device_data+vehicle_location"));
        answer(address, "code=code-0001")
    }).expect("Fleet grant"), ["fleet_api"]);
    assert_eq!(issuer.calls(), 1);
    let exchange = issuer.exchange.lock().expect("exchange");
    let form: std::collections::BTreeMap<_, _> = url::form_urlencoded::parse(&exchange.request).collect();
    assert_eq!(form.get("audience").map(|value| value.as_ref()), Some("https://fleet-api.prd.eu.vn.cloud.tesla.com"));
    assert_eq!(form.get("client_secret").map(|value| value.as_ref()), Some(" secret-0001 "));
    assert!(!form.contains_key("code_verifier"));
    let projection = projection(root.path());
    assert_eq!((projection.fleet_credentials.origin, projection.fleet_credentials.refresh_owner), (Some("fleet_oauth"), Some("hub")));
}

#[test]
fn ct100_h_preconditions() {
    let issuer = StubIssuer::valid(); let root = root();
    with_entry(root.path(), false, |entry| {
        let error = match SignInAttempt::begin(entry, issuer.settings()) { Ok(_) => panic!("exclusion must refuse before output"), Err(error) => error };
        assert_eq!(error.code(), "secrets_backup_exclusion_failed");
        assert_eq!(issuer.calls(), 0);
        Ok(())
    });
    assert!(!projection(root.path()).legacy_credentials.present);
    let refusal = sign_in::owner_sign_in_precondition(teslatlas_hub::config::CollectorMode::Provider).unwrap_err();
    assert_eq!((refusal.class, refusal.key), ("configuration_precondition", "provider_mode_sign_in_unavailable"));
    // The command that paused the service around this attempt is gone
    // (M02-T10): sign-in is forwarded to the running service, which holds the
    // window authority.
}

#[test]
fn ct100_i_zeroisation() {
    let drops = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&drops);
    sign_in::observe_sign_in_secret_drops(move |event| record.lock().expect("drop records").push(event), || {
        for fleet in [false, true] {
            let issuer = StubIssuer::valid(); let root = root();
            let settings = if fleet {
                SignInSettings::fleet(br#"{"clientId":"client-0001","region":"eu","redirectUri":"http://127.0.0.1/callback","clientSecret":" secret-0001 "}"#).expect("Fleet settings")
                    .with_stub_endpoints(&format!("{}/authorize", issuer.endpoint), &format!("{}/token", issuer.endpoint)).expect("loopback endpoints")
            } else { issuer.settings() };
            redeem(&issuer, root.path(), settings, |address| answer(address, "code=code-0001")).expect("sign-in");
            assert_eq!(issuer.calls(), 1);
        }
        for response in [br#"{"access_token":"access-0001","refresh_token":"refresh-0001","expires_in":28800,"token_type":"Basic"}"#.as_slice(), br#"{"access_token":"access-0001","refresh_token":" refresh-0001","expires_in":28800,"token_type":"Bearer","id_token":"id-0001"}"#] {
            let issuer = StubIssuer::new(response); let root = root();
            let error = redeem(&issuer, root.path(), issuer.settings(), |address| answer(address, "code=code-0001")).unwrap_err();
            assert_eq!(error.code(), "sign_in_answer_invalid");
        }
        for line in [br#"{"clientId":"client-0001","region":"eu","redirectUri":"http://127.0.0.1/callback","clientSecret":"secret-0001","extra":true}"#.as_slice(), br#"{"clientSecret":"secret-0001","clientSecret":"secret-0002"}"#] {
            assert!(SignInSettings::fleet(line).is_err());
        }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().expect("input runtime");
        runtime.block_on(async {
            let (_sender, mut cancellation) = tokio::sync::watch::channel(false);
            let mut input = sign_in::SignInInput::new(b"line-0001\ntrailing-secret-0001".as_slice());
            input.line(tokio::time::Instant::now() + Duration::from_secs(1), &mut cancellation).await.expect("one line");
            drop(input);
            for bytes in [vec![b'x'; 16_385], b"secret-0001".to_vec()] {
                let cancelled = bytes.len() < 16_384;
                let (_sender, mut cancellation) = tokio::sync::watch::channel(cancelled);
                let mut input = sign_in::SignInInput::new(bytes.as_slice());
                let error = input.line(tokio::time::Instant::now() + Duration::from_secs(1), &mut cancellation).await.unwrap_err();
                assert_eq!(error, if cancelled { sign_in::SignInInputError::Cancelled } else { sign_in::SignInInputError::TooLong });
            }
        });
        assert_sign_in_logs("", b"");
    });
    let drops = drops.lock().expect("drop records");
    assert!(drops.iter().all(|event| event.zeroised), "every observed allocation wiped");
    for kind in ["grant_secret", "state", "verifier", "request_body", "answer_buffer", "wire_secret", "input_line", "input_tail", "input_scratch", "grant_access", "grant_refresh"] {
        assert!(drops.iter().any(|event| event.kind == kind && event.before > 0), "drop proof missing for {kind}");
    }
    let proof = format!("{drops:?}");
    for secret in ["access-0001", "refresh-0001", "code-0001", "secret-0001", "id-0001"] { assert!(!proof.contains(secret)); }
}
