// SPDX-License-Identifier: AGPL-3.0-only

//! The owner code exchange against a loopback stub token endpoint; nothing
//! here reaches Tesla. Each test uses its own code, because the used-code
//! record is process-wide.

use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};

use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener, task::JoinHandle};
use zeroize::Zeroizing;

use super::*;
use super::super::owner_client::{OWNER_AUTHORIZATION_ENDPOINT, OWNER_SCOPE};
use crate::{
    auth::{credential_envelope::{CredentialOrigin, RefreshOwner}, credential_vault::fixtures::open_fixture_vault},
    db::HubStore,
    fleet_api::egress::test_support::TestWindow,
};

const VERIFIER: &str = "synthetic-verifier-0001-abcdefghijklmnopqrstuvwxyz-0123456789";
const OTHER_VERIFIER: &str = "synthetic-verifier-0002-abcdefghijklmnopqrstuvwxyz-0123456789";
const ACCESS: &str = "exchange-access-0001";
const REFRESH: &str = "exchange-refresh-0001";
const ID_TOKEN: &str = "exchange-id-0001";

fn valid_answer() -> String {
    format!(r#"{{"access_token":"{ACCESS}","refresh_token":"{REFRESH}","id_token":"{ID_TOKEN}","expires_in":28800,"token_type":"Bearer"}}"#)
}

/// How the stub answers one request.
#[derive(Clone)]
enum Reply {
    /// A fixed status and JSON body.
    Json(u16, String),
    /// The issuer's own checks: the verifier must be `VERIFIER` and the
    /// redirect the owner client's, or it refuses with `invalid_grant`.
    Pkce,
    /// A declared length over the cap.
    DeclaredOversize,
    /// No declared length and a body over the cap, ended by the close.
    StreamedOversize,
}

/// A loopback token endpoint that counts connections and keeps each request.
struct Stub {
    url: String,
    accepted: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<Zeroizing<Vec<u8>>>>>,
    task: JoinHandle<()>,
}

impl Drop for Stub {
    fn drop(&mut self) { self.task.abort(); }
}

fn header_end(bytes: &[u8]) -> Option<usize> { bytes.windows(4).position(|window| window == b"\r\n\r\n").map(|at| at + 4) }

fn content_length(head: &[u8]) -> usize {
    std::str::from_utf8(head).unwrap_or_default().lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok()).unwrap_or(0)
}

impl Stub {
    async fn spawn(reply: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("loopback stub");
        let url = format!("http://{}/oauth2/v3/token", listener.local_addr().expect("stub address"));
        let accepted = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let task = {
            let (accepted, requests) = (Arc::clone(&accepted), Arc::clone(&requests));
            tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let mut request = Zeroizing::new(Vec::new());
                    let mut buffer = [0u8; 4096];
                    loop {
                        if let Some(end) = header_end(&request) && request.len() >= end + content_length(&request[..end]) { break; }
                        match socket.read(&mut buffer).await { Ok(0) | Err(_) => break, Ok(read) => request.extend_from_slice(&buffer[..read]) }
                    }
                    let body = header_end(&request).map(|end| request[end..].to_vec()).unwrap_or_default();
                    requests.lock().expect("requests").push(request);
                    let response = match &reply {
                        Reply::Json(status, json) => json_response(*status, json),
                        Reply::Pkce => {
                            let sent: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                            if sent["code_verifier"] == VERIFIER && sent["redirect_uri"] == OWNER_REDIRECT_URI {
                                json_response(200, &valid_answer())
                            } else {
                                json_response(400, r#"{"error":"invalid_grant","error_description":"synthetic-description-0001"}"#)
                            }
                        }
                        Reply::DeclaredOversize => {
                            let mut response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", ANSWER_CAP + 1).into_bytes();
                            response.resize(response.len() + ANSWER_CAP + 1, b'a');
                            response
                        }
                        Reply::StreamedOversize => {
                            let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n".to_vec();
                            response.resize(response.len() + ANSWER_CAP + 1, b' ');
                            response
                        }
                    };
                    let _ = socket.write_all(&response).await;
                    let _ = socket.shutdown().await;
                }
            })
        };
        Self { url, accepted, requests, task }
    }

    fn accepted(&self) -> usize { self.accepted.load(Ordering::SeqCst) }
}

fn json_response(status: u16, json: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status} Synthetic\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len()).into_bytes()
}

fn fixture() -> (tempfile::TempDir, HubStore, CredentialVaultEntry) {
    let root = crate::private_tempdir().expect("synthetic root");
    let store = HubStore::initialize(root.path()).expect("store");
    let entry = CredentialVaultEntry::for_test(open_fixture_vault(store.clone(), root.path()));
    (root, store, entry)
}

/// Nothing was written: no pair in either slot, no sign-in binding and no
/// keyring, so no partial state of the exchange survives.
fn assert_untouched(root: &std::path::Path, entry: &CredentialVaultEntry) {
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        assert!(entry.vault().credential_pair_presence(slot).expect("presence").is_none(), "no pair stored");
    }
    let bindings: i64 = rusqlite::Connection::open(root.join("hub.sqlite")).expect("synthetic SQL")
        .query_row("SELECT count(*) FROM credential_sign_ins", [], |row| row.get(0)).expect("binding count");
    assert_eq!(bindings, 0, "no sign-in binding left behind");
    assert!(!root.join("secrets").join("credential-keys").exists(), "no keyring created");
}

/// No secret of the attempt appears in an error's code, text or debug form.
fn assert_secret_free(error: &CredentialCommandError, code: &str) {
    let shown = format!("{error} {error:?}");
    for secret in [code, VERIFIER, OTHER_VERIFIER, ACCESS, REFRESH, ID_TOKEN, "synthetic-description-0001"] {
        assert!(!shown.contains(secret), "a secret appears in the refusal");
    }
}

async fn exchange(entry: &CredentialVaultEntry, code: &str, verifier: &str, redirect: &str, stub: &Stub) -> Result<(), CredentialCommandError> {
    exchange_at_stub_for_test(entry, code, verifier, redirect, &stub.url).await
}

/// Acceptance 1: inside an Open window one exchange sends the standard owner
/// values once and stores exactly one pair, the Hub's own, and nothing else.
#[tokio::test]
async fn owner_exchange_stores_one_own_pair_and_nothing_else() {
    let (_root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Pkce).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    exchange(&entry, "code-stores-one-pair", VERIFIER, OWNER_REDIRECT_URI, &stub).await.expect("exchange");
    assert_eq!(stub.accepted(), 1, "one connection");
    let requests = stub.requests.lock().expect("requests");
    let request = std::str::from_utf8(&requests[0]).expect("request text");
    let (head, body) = request.split_once("\r\n\r\n").expect("request parts");
    assert!(head.starts_with("POST /oauth2/v3/token HTTP/1.1\r\n"));
    let head = head.to_ascii_lowercase();
    assert!(head.contains("\r\ncontent-type: application/json") && head.contains("\r\naccept: application/json") && head.contains("\r\nuser-agent: teslatlas-hub/"));
    let sent: serde_json::Value = serde_json::from_str(body).expect("JSON body");
    assert_eq!(sent, serde_json::json!({
        "grant_type": "authorization_code", "client_id": OWNER_CLIENT_ID, "code": "code-stores-one-pair",
        "code_verifier": VERIFIER, "redirect_uri": OWNER_REDIRECT_URI,
    }));
    let pair = entry.vault().load(CredentialSlot::OwnerApi).expect("read").expect("own pair");
    assert_eq!((pair.origin, pair.refresh_owner, pair.access.as_str(), pair.refresh.as_str()),
        (Some(CredentialOrigin::HubLogin), RefreshOwner::Hub, ACCESS, REFRESH));
    assert!(pair.client_id.is_none() && pair.region.is_none(), "an own owner pair names no Fleet client");
    assert!(entry.vault().credential_pair_presence(CredentialSlot::FleetApi).expect("presence").is_none(), "nothing else stored");
}

/// Acceptance 2: with no Open window (never opened, closed by the operator,
/// or past its deadline) the gate refuses before any connection and nothing
/// is written.
#[tokio::test]
async fn owner_exchange_closed_window_refuses_before_any_connection() {
    for (index, window) in [
        TestWindow::closed(),
        { let window = TestWindow::open(600); window.close(); window },
        { let window = TestWindow::open(600); window.advance(601); window },
    ].into_iter().enumerate() {
        let (root, _store, entry) = fixture();
        let stub = Stub::spawn(Reply::Json(200, valid_answer())).await;
        let _route = window.route(&stub.url);
        let code = format!("code-closed-window-{index}");
        let error = exchange(&entry, &code, VERIFIER, OWNER_REDIRECT_URI, &stub).await.unwrap_err();
        assert_eq!(error.code(), WINDOW_CLOSED);
        assert_eq!(stub.accepted(), 0, "no connection opened");
        assert_secret_free(&error, &code);
        assert_untouched(root.path(), &entry);
    }
}

/// Acceptance 3: an error answer, a malformed or oversized answer, a wrong
/// verifier at the issuer and a wrong redirect each leave no state, and no
/// refusal carries the code, the verifier or a token.
#[tokio::test]
async fn owner_exchange_failures_leave_no_partial_state() {
    let cases: Vec<(&str, Reply, &str, &str)> = vec![
        ("wrong-verifier", Reply::Pkce, OTHER_VERIFIER, "sign_in_refused"),
        ("issuer-error", Reply::Json(401, r#"{"error":"unauthorized_client"}"#.into()), VERIFIER, "sign_in_refused"),
        ("server-failure", Reply::Json(500, "not json".into()), VERIFIER, "sign_in_answer_invalid"),
        ("malformed", Reply::Json(200, r#"{"access_token":"#.into()), VERIFIER, "sign_in_answer_invalid"),
        ("not-an-object", Reply::Json(200, "[]".into()), VERIFIER, "sign_in_answer_invalid"),
        ("no-refresh", Reply::Json(200, format!(r#"{{"access_token":"{ACCESS}","expires_in":28800,"token_type":"Bearer"}}"#)), VERIFIER, "sign_in_answer_invalid"),
        ("wrong-type", Reply::Json(200, format!(r#"{{"access_token":"{ACCESS}","refresh_token":"{REFRESH}","expires_in":28800,"token_type":"mac"}}"#)), VERIFIER, "sign_in_answer_invalid"),
        ("bad-access", Reply::Json(200, format!(r#"{{"access_token":"a b","refresh_token":"{REFRESH}","expires_in":28800,"token_type":"Bearer"}}"#)), VERIFIER, "sign_in_answer_invalid"),
        ("bad-lifetime", Reply::Json(200, format!(r#"{{"access_token":"{ACCESS}","refresh_token":"{REFRESH}","expires_in":0,"token_type":"Bearer"}}"#)), VERIFIER, "sign_in_answer_invalid"),
        ("declared-oversize", Reply::DeclaredOversize, VERIFIER, "sign_in_answer_invalid"),
        ("streamed-oversize", Reply::StreamedOversize, VERIFIER, "sign_in_answer_invalid"),
    ];
    for (name, reply, verifier, expected) in cases {
        let (root, _store, entry) = fixture();
        let stub = Stub::spawn(reply).await;
        let window = TestWindow::open(600);
        let _route = window.route(&stub.url);
        let code = format!("code-failure-{name}");
        let error = exchange(&entry, &code, verifier, OWNER_REDIRECT_URI, &stub).await.unwrap_err();
        assert_eq!(error.code(), expected, "{name}");
        assert_eq!(stub.accepted(), 1, "{name}: one send");
        assert_secret_free(&error, &code);
        assert_untouched(root.path(), &entry);
    }
    // The issuer is never reachable when nothing listens: still no state.
    let (root, _store, entry) = fixture();
    let port = std::net::TcpListener::bind("127.0.0.1:0").expect("free port").local_addr().expect("address").port();
    let url = format!("http://127.0.0.1:{port}/oauth2/v3/token");
    let window = TestWindow::open(600);
    let _route = window.route(&url);
    let error = exchange_at_stub_for_test(&entry, "code-failure-unreachable", VERIFIER, OWNER_REDIRECT_URI, &url).await.unwrap_err();
    assert!(matches!(error.code(), "sign_in_unavailable" | "sign_in_answer_invalid"), "unreachable issuer");
    assert_untouched(root.path(), &entry);
}

/// Acceptance 3 (local half): a wrong redirect, a malformed verifier or a
/// malformed code is refused before the gate is asked or a connection opens.
#[tokio::test]
async fn owner_exchange_refuses_malformed_material_locally() {
    let long_code = "c".repeat(MAX_OWNER_CODE_BYTES + 1);
    let short_verifier = "v".repeat(42);
    let cases: [(&str, &str, &str); 8] = [
        ("code-local-redirect-https", VERIFIER, "https://auth.tesla.com/void/callback"),
        ("code-local-redirect-path", VERIFIER, "tesla://auth/callback/extra"),
        ("code-local-redirect-empty", VERIFIER, ""),
        ("code-local-verifier-short", &short_verifier, OWNER_REDIRECT_URI),
        ("code-local-verifier-char", "synthetic-verifier-0003-abcdefghijklmnopqrstuvwxyz+0123456789", OWNER_REDIRECT_URI),
        ("", VERIFIER, OWNER_REDIRECT_URI),
        ("code local space", VERIFIER, OWNER_REDIRECT_URI),
        (&long_code, VERIFIER, OWNER_REDIRECT_URI),
    ];
    for (code, verifier, redirect) in cases {
        let (root, _store, entry) = fixture();
        let stub = Stub::spawn(Reply::Pkce).await;
        let window = TestWindow::open(600);
        let _route = window.route(&stub.url);
        let error = exchange(&entry, code, verifier, redirect, &stub).await.unwrap_err();
        assert_eq!(error.code(), SIGN_IN_MATERIAL_INVALID);
        assert_eq!(stub.accepted(), 0, "refused locally");
        if !code.is_empty() { assert_secret_free(&error, code); }
        assert_untouched(root.path(), &entry);
    }
}

/// Acceptance 5: a code is exchanged at most once. A second exchange with the
/// same code, after a success or after a failed attempt, is refused locally
/// and the stored pair stays as it was.
#[tokio::test]
async fn owner_exchange_refuses_a_reused_code_locally() {
    let (_root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Pkce).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    exchange(&entry, "code-reused-0001", VERIFIER, OWNER_REDIRECT_URI, &stub).await.expect("first exchange");
    let generation = entry.vault().stored_generation(CredentialSlot::OwnerApi).expect("generation");
    let error = exchange(&entry, "code-reused-0001", VERIFIER, OWNER_REDIRECT_URI, &stub).await.unwrap_err();
    assert_eq!(error.code(), SIGN_IN_CODE_REUSED);
    assert_secret_free(&error, "code-reused-0001");
    assert_eq!(stub.accepted(), 1, "the second exchange sent nothing");
    assert_eq!(entry.vault().stored_generation(CredentialSlot::OwnerApi).expect("generation"), generation, "pair unchanged");

    let (root, _store, entry) = fixture();
    let refusing = Stub::spawn(Reply::Pkce).await;
    let _second = window.route(&refusing.url);
    assert_eq!(exchange(&entry, "code-reused-0002", OTHER_VERIFIER, OWNER_REDIRECT_URI, &refusing).await.unwrap_err().code(), "sign_in_refused");
    assert_eq!(exchange(&entry, "code-reused-0002", VERIFIER, OWNER_REDIRECT_URI, &refusing).await.unwrap_err().code(), SIGN_IN_CODE_REUSED);
    assert_eq!(refusing.accepted(), 1, "the retry sent nothing");
    assert_untouched(root.path(), &entry);
}

/// Acceptance 4: the log lines of a successful and a failed exchange carry
/// no code, verifier or token.
#[test]
fn owner_exchange_logs_carry_no_secret() {
    #[derive(Clone)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.lock().expect("log").extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink = Sink(Arc::clone(&logs));
    let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false).with_max_level(tracing::Level::TRACE)
        .with_writer(move || sink.clone()).finish();
    let _default = tracing::subscriber::set_default(subscriber);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    runtime.block_on(async {
        let (_root, _store, entry) = fixture();
        let stub = Stub::spawn(Reply::Pkce).await;
        let window = TestWindow::open(600);
        let _route = window.route(&stub.url);
        exchange(&entry, "code-logs-0001", VERIFIER, OWNER_REDIRECT_URI, &stub).await.expect("exchange");
        let (_root, _store, entry) = fixture();
        exchange(&entry, "code-logs-0002", OTHER_VERIFIER, OWNER_REDIRECT_URI, &stub).await.unwrap_err();
    });
    let logs = logs.lock().expect("log");
    let logs = std::str::from_utf8(&logs).expect("UTF-8 logs");
    assert!(logs.contains("credential write") && logs.contains("own_legacy_exchange"), "the write is logged by origin only");
    for secret in ["code-logs-0001", "code-logs-0002", VERIFIER, OTHER_VERIFIER, ACCESS, REFRESH, ID_TOKEN, "synthetic-description-0001"] {
        assert!(!logs.contains(secret), "a secret appears in the logs");
    }
}

/// The owner client is defined once and matches what the Mac app's sign-in
/// window sends: its client id, redirect, authorisation endpoint and scope.
/// The refresh path's values are the same constants.
#[test]
fn owner_exchange_client_matches_the_mac_sign_in_window() {
    let swift = include_str!("../../../macos/TeslatlasHubApp/TeslatlasHubApp/TeslaAuthWindowController.swift");
    for line in [
        format!("static let clientID = \"{OWNER_CLIENT_ID}\""),
        format!("static let redirectURI = \"{OWNER_REDIRECT_URI}\""),
        format!("static let authorizationEndpoint = URL(string: \"{OWNER_AUTHORIZATION_ENDPOINT}\")!"),
        format!("static let scopes = \"{OWNER_SCOPE}\""),
    ] {
        assert!(swift.contains(&line), "the Mac sign-in window must send {line}");
    }
    assert_eq!(OWNER_TOKEN_ENDPOINT.strip_suffix("/token"), OWNER_AUTHORIZATION_ENDPOINT.strip_suffix("/authorize"), "one issuer");
    assert!(stub_endpoint(OWNER_TOKEN_ENDPOINT).is_err(), "the stub path never accepts the real endpoint");
}

/// A pause at the start of the exchange's write, armed on one thread only so
/// that no other test's exchange can meet it.
pub(super) mod persistence_pause {
    use std::{cell::RefCell, sync::{Arc, Condvar, Mutex}, time::Duration};

    #[derive(Default)]
    pub(crate) struct Pause { state: Mutex<(bool, bool)>, changed: Condvar }

    thread_local! { static ARMED: RefCell<Option<Arc<Pause>>> = const { RefCell::new(None) }; }

    pub(crate) fn arm(pause: Arc<Pause>) { ARMED.with(|slot| *slot.borrow_mut() = Some(pause)); }

    /// Called by the exchange once its permit has been checked, before it
    /// writes anything; blocks while a pause is armed on this thread.
    pub(crate) fn reached() {
        let Some(pause) = ARMED.with(|slot| slot.borrow_mut().take()) else { return };
        let mut state = pause.state.lock().expect("pause");
        state.0 = true;
        pause.changed.notify_all();
        let (_state, waited) = pause.changed.wait_timeout_while(state, Duration::from_secs(30), |state| !state.1).expect("pause");
        assert!(!waited.timed_out(), "the paused write was released");
    }

    impl Pause {
        pub(crate) fn wait_reached(&self) {
            let state = self.state.lock().expect("pause");
            let (_state, waited) = self.changed.wait_timeout_while(state, Duration::from_secs(30), |state| !state.0).expect("pause");
            assert!(!waited.timed_out(), "the exchange reached its write");
        }
        pub(crate) fn release(&self) {
            self.state.lock().expect("pause").1 = true;
            self.changed.notify_all();
        }
    }
}

/// A trusted window controller on a manual clock, shared with a supervisor.
struct SupervisedWindow {
    clock: Arc<crate::collection::window::ManualClock>,
    controller: Arc<crate::collection::window::WindowController>,
    _dir: tempfile::TempDir,
}

impl SupervisedWindow {
    fn open(length: i64) -> Self {
        use crate::collection::window::{EgressGate, IssueRequest, Issuer, ManualClock, ManualProbe, MemoryAnchor, NoHighWater, Phase, WindowClock, WindowConfig, WindowController, WindowDeps};
        let dir = tempfile::TempDir::new().expect("window dir");
        let clock = Arc::new(ManualClock::new(1_800_000_000));
        let anchor = Arc::new(MemoryAnchor::new());
        let issuer = Issuer::new(&dir.path().join("issuer"), "host-exchange-issuer", anchor.clone(), clock.clone());
        let key = dir.path().join("issuer.key");
        let trust = issuer.init(&key).expect("issuer init");
        let (controller, _) = WindowController::start(
            WindowConfig { dir: dir.path().join("data/window"), claim_path: dir.path().join("claim/host-claim.lock"), host_id: "host-exchange-test".to_owned() },
            WindowDeps { clock: clock.clone(), anchor, probe: Arc::new(ManualProbe::new(0.2)), gate: EgressGate::new(clock.clone()), handback: Arc::new(NoHighWater) },
        );
        controller.trust(&trust).expect("trust");
        let now = clock.wall();
        let out = dir.path().join("grant.json");
        issuer.issue(&key, &IssueRequest {
            host_id: "host-exchange-test".to_owned(), starts_at: now, expires_at: now + length,
            approver: "G".to_owned(), reason: "exchange close test".to_owned(), approval_ref: "approval-exchange-close".to_owned(),
        }, &out).expect("grant issued");
        assert_eq!(controller.open(&std::fs::read(out).expect("grant")).expect("open").phase, Phase::Open);
        Self { clock, controller: Arc::new(controller), _dir: dir }
    }
}

/// No collector: the supervisor only drives the window.
#[derive(Clone, Default)]
struct IdleSlot;

impl crate::collection::window::CollectorSlot for IdleSlot {
    fn start(&mut self, _deadline_wall: i64) -> Result<(), String> { Err("no collector here".to_owned()) }
    fn is_running(&mut self) -> bool { false }
    fn stop(&mut self) {}
    fn release_lease(&mut self) -> Result<(), String> { Ok(()) }
}

/// Round 2, finding 4: the operator closes the window while an exchange,
/// its answer in hand, is paused before its write. The window stays Closing
/// with no handback while the exchange holds its permit: released within the
/// grace, the write completes and only then is the window handed back;
/// released after the grace has lapsed, the answer is dropped unstored, and
/// again the handback follows the refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_exchange_close_waits_for_a_paused_persistence() {
    use crate::collection::window::{CloseReason, Phase, Supervisor};
    let stub = Stub::spawn(Reply::Pkce).await;
    for (case, (code, past_grace)) in [("code-close-paused-0001", false), ("code-close-lapsed-0001", true)].into_iter().enumerate() {
        let (root, _store, entry) = fixture();
        let window = SupervisedWindow::open(600);
        let _route = crate::fleet_api::egress::test_support::route_on(&stub.url, window.controller.gate().clone(), window.clock.clone());
        let mut supervisor = Supervisor::new(Arc::clone(&window.controller), IdleSlot).with_custody(crate::collection::window::CredentialCustody::new());
        let pause = Arc::new(persistence_pause::Pause::default());
        let result = std::thread::scope(|scope| {
            let exchange = {
                let (entry, pause, url) = (&entry, Arc::clone(&pause), stub.url.clone());
                scope.spawn(move || {
                    persistence_pause::arm(pause);
                    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
                        .block_on(exchange_at_stub_for_test(entry, code, VERIFIER, OWNER_REDIRECT_URI, &url))
                })
            };
            pause.wait_reached();
            window.controller.close(CloseReason::OperatorClose);
            assert_eq!(supervisor.step().phase, Phase::Closing, "case {case}: Closing while the exchange holds its answer");
            window.clock.advance(if past_grace { 61 } else { 30 });
            let report = supervisor.step();
            assert_eq!((report.phase, report.closed), (Phase::Closing, None), "case {case}: no handback while the write is paused");
            assert!(window.controller.status().last_handback.is_none(), "case {case}: no handback recorded");
            assert!(window.controller.gate().exchange_in_flight(), "case {case}");
            pause.release();
            exchange.join().expect("exchange thread")
        });
        if past_grace {
            let error = result.unwrap_err();
            assert_eq!(error.code(), WINDOW_CLOSED, "case {case}");
            assert_untouched(root.path(), &entry);
        } else {
            result.expect("the paused write completes within the grace");
            assert!(entry.vault().load(CredentialSlot::OwnerApi).expect("read").is_some(), "the pair is durable before the handback");
        }
        assert_eq!(supervisor.step().closed, Some(CloseReason::OperatorClose), "case {case}: then the window is handed back");
    }
}

