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

async fn exchange(entry: &CredentialVaultEntry, code: &str, verifier: &str, redirect: &str, stub: &Stub) -> Result<OwnerSignIn, CredentialCommandError> {
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
    assert!(head.contains("\r\ncontent-type: application/json") && head.contains("\r\naccept: application/json") && head.contains("\r\nuser-agent: teslatlas-hub/")); // rename:keep
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
    let swift = include_str!("../../../macos/TesatlasHubApp/TesatlasHubApp/TeslaAuthWindowController.swift");
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
        pause.hold();
    }

    impl Pause {
        /// Blocks the calling thread here until released.
        pub(crate) fn hold(&self) {
            let mut state = self.state.lock().expect("pause");
            state.0 = true;
            self.changed.notify_all();
            let (_state, waited) = self.changed.wait_timeout_while(state, Duration::from_secs(30), |state| !state.1).expect("pause");
            assert!(!waited.timed_out(), "the paused write was released");
        }
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

/// Where a stalled exchange is held: after its final permit check (before
/// any write), at the binding (written, before its barrier), before a key's
/// publication, before the pair's commit, and at the durability barrier
/// after that commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stall { AfterPermitCheck, Binding, KeyPublication, Commit, DurabilityBarrier }

/// Arms `stall` on this thread: the exchange blocks there until released.
fn arm_stall(stall: Stall, pause: Arc<persistence_pause::Pause>) -> Box<dyn std::any::Any> {
    if stall == Stall::AfterPermitCheck {
        persistence_pause::arm(pause);
        return Box::new(CredentialVaultEntry::test_faults(false, None, None, Box::new(|_| {})));
    }
    let mut after_commit = false;
    let mut pause = Some(pause);
    Box::new(CredentialVaultEntry::test_faults(false, None, None, Box::new(move |point| {
        if point == "sign_in_pair_committed" { after_commit = true; }
        let reached = match stall {
            Stall::Binding => point == "sign_in_binding_committed",
            Stall::KeyPublication => point == "sign_in_key_publication",
            Stall::Commit => point == "sign_in_pair_commit",
            Stall::DurabilityBarrier => after_commit && point == "credential_barrier",
            Stall::AfterPermitCheck => false,
        };
        if reached && let Some(pause) = pause.take() { pause.hold(); }
    })))
}

/// What a write would change: the store's commit counter as seen from a
/// connection of its own (SQLite's `data_version` moves when any other
/// connection commits), the sign-in bindings and the secrets directory.
struct StoreWatch { connection: rusqlite::Connection, root: std::path::PathBuf }

impl StoreWatch {
    fn new(root: &std::path::Path) -> Self {
        Self { connection: rusqlite::Connection::open(root.join("hub.sqlite")).expect("synthetic SQL"), root: root.to_path_buf() }
    }
    fn state(&self) -> (i64, i64, Vec<(String, u64)>) {
        let version = self.connection.query_row("PRAGMA data_version", [], |row| row.get(0)).expect("data version");
        let bindings = self.connection.query_row("SELECT count(*) FROM credential_sign_ins", [], |row| row.get(0)).expect("bindings");
        let mut secrets = std::fs::read_dir(self.root.join("secrets")).map(|entries| entries.filter_map(Result::ok)
            .map(|entry| (entry.file_name().to_string_lossy().into_owned(), entry.metadata().map(|meta| meta.len()).unwrap_or(0)))
            .collect::<Vec<_>>()).unwrap_or_default();
        secrets.sort();
        (version, bindings, secrets)
    }
}

/// Steps the supervisor until it hands the window back, without moving the
/// clock (a write of another test may hold the process's write fence for a
/// moment). Returns the step that closed it.
fn step_until_closed<S: crate::collection::window::CollectorSlot>(supervisor: &mut crate::collection::window::Supervisor<S>) -> crate::collection::window::StepReport {
    for _ in 0..500 {
        let report = supervisor.step();
        if report.closed.is_some() { return report; }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the window was not handed back");
}

/// Round 2, finding 1 (REV-M02-T13; REV-M02-T12 finding 4): the operator
/// closes the window while an exchange, its answer in hand, is stalled in its
/// persistence: after its final permit check, at the binding, before a key's
/// publication, before the pair's commit, or at the durability barrier after
/// that commit. The window stays Closing while the exchange may still finish
/// normally, and is handed back within the grace however long the stall
/// lasts. Released after the handback, the exchange writes nothing more: a
/// stall before the commit refuses `window_closed`, leaving at most what it
/// wrote before the close; a stall after the commit is a stored sign-in whose
/// aftercare is left pending. Before this round the close waited for the
/// stalled write without bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_exchange_window_closes_within_the_grace_whatever_the_write_does() {
    use crate::collection::window::{CloseReason, EXCHANGE_GRACE_SECONDS, Phase, Supervisor, WindowClock};
    let stub = Stub::spawn(Reply::Pkce).await;
    let stalls = [Stall::AfterPermitCheck, Stall::Binding, Stall::KeyPublication, Stall::Commit, Stall::DurabilityBarrier];
    for stall in stalls {
        let code = format!("code-stall-{stall:?}-0001");
        let (root, _store, entry) = fixture();
        let window = SupervisedWindow::open(600);
        let _route = crate::fleet_api::egress::test_support::route_on(&stub.url, window.controller.gate().clone(), window.clock.clone());
        let mut supervisor = Supervisor::new(Arc::clone(&window.controller), IdleSlot).with_custody(crate::collection::window::CredentialCustody::new());
        let pause = Arc::new(persistence_pause::Pause::default());
        let watch = StoreWatch::new(root.path());
        let (result, at_handback) = std::thread::scope(|scope| {
            let exchange = {
                let (entry, pause, url, code) = (&entry, Arc::clone(&pause), stub.url.clone(), code.clone());
                scope.spawn(move || {
                    let _stall = arm_stall(stall, pause);
                    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
                        .block_on(exchange_at_stub_for_test(entry, &code, VERIFIER, OWNER_REDIRECT_URI, &url))
                })
            };
            pause.wait_reached();
            let closing_at = window.clock.wall();
            window.controller.close(CloseReason::OperatorClose);
            assert_eq!(supervisor.step().phase, Phase::Closing, "{stall:?}: Closing while the exchange holds its permit");
            window.clock.advance(30);
            assert_eq!(supervisor.step().phase, Phase::Closing, "{stall:?}: the exchange keeps its grace");
            window.clock.advance(15);
            let report = step_until_closed(&mut supervisor);
            assert_eq!(report.closed, Some(CloseReason::OperatorClose), "{stall:?}: handed back while the write is stalled");
            assert!(window.clock.wall() < closing_at + EXCHANGE_GRACE_SECONDS, "{stall:?}: within the grace");
            assert!(window.controller.status().last_handback.is_some(), "{stall:?}: handback recorded");
            let at_handback = watch.state();
            pause.release();
            (exchange.join().expect("exchange thread"), at_handback)
        });
        assert_eq!(watch.state(), at_handback, "{stall:?}: no write after the handback");
        match stall {
            Stall::DurabilityBarrier => {
                assert_eq!(result.expect("committed before the close"), OwnerSignIn::AftercarePending, "{stall:?}");
                assert!(entry.vault().load(CredentialSlot::OwnerApi).expect("read").is_some_and(|pair| pair.origin == Some(CredentialOrigin::HubLogin)));
            }
            _ => {
                let error = result.unwrap_err();
                assert_eq!(error.code(), WINDOW_CLOSED, "{stall:?}");
                assert_secret_free(&error, &code);
                assert!(entry.vault().credential_pair_presence(CredentialSlot::OwnerApi).expect("presence").is_none(), "{stall:?}: no pair");
                if stall == Stall::AfterPermitCheck { assert_untouched(root.path(), &entry); }
            }
        }
    }
}

fn binding_count(root: &std::path::Path) -> i64 {
    rusqlite::Connection::open(root.join("hub.sqlite")).expect("synthetic SQL")
        .query_row("SELECT count(*) FROM credential_sign_ins", [], |row| row.get(0)).expect("binding count")
}

/// One exchange with the vault cut at `point` (the cut stands in for a
/// failed write or durability barrier there).
async fn exchange_cut_at(entry: &CredentialVaultEntry, point: &'static str, code: &str, stub: &Stub) -> Result<OwnerSignIn, CredentialCommandError> {
    let _faults = CredentialVaultEntry::test_faults(false, None, Some(point), Box::new(|_| {}));
    exchange(entry, code, VERIFIER, OWNER_REDIRECT_URI, stub).await
}

/// Round 2, finding 3: faults at each persistence boundary of the exchange.
/// Before the pair's commit (the binding's barrier, the publication of a
/// keyring this attempt created) the store is left as it was: no binding, no
/// new keyring and an existing pair untouched. A commit whose barrier fails
/// is `sign_in_stored_unconfirmed`, never a plain refusal, and keeps the key
/// its pair needs. An aftercare failure after the durable write is a stored
/// sign-in with its aftercare pending, not a refusal.
#[tokio::test]
async fn owner_exchange_persistence_faults_leave_no_partial_state() {
    let stub = Stub::spawn(Reply::Pkce).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);

    // A first sign-in: the binding and key-publication faults leave nothing.
    for point in ["sign_in_binding_committed", "sign_in_key_published"] {
        let (root, _store, entry) = fixture();
        let error = exchange_cut_at(&entry, point, &format!("code-fault-fresh-{point}"), &stub).await.unwrap_err();
        assert_eq!(error.code(), "durability_unavailable", "{point}");
        assert_secret_free(&error, &format!("code-fault-fresh-{point}"));
        assert_untouched(root.path(), &entry);
    }

    // A re-sign-in over an existing own pair: a binding fault leaves the pair
    // as it was, with its key, and no binding.
    let (root, _store, entry) = fixture();
    exchange(&entry, "code-fault-existing-0001", VERIFIER, OWNER_REDIRECT_URI, &stub).await.expect("first sign-in");
    let existing = entry.vault().stored_generation(CredentialSlot::OwnerApi).expect("read").expect("existing pair");
    let error = exchange_cut_at(&entry, "sign_in_binding_committed", "code-fault-existing-0002", &stub).await.unwrap_err();
    assert_eq!(error.code(), "durability_unavailable");
    assert_eq!(entry.vault().stored_generation(CredentialSlot::OwnerApi).expect("read"), Some(existing), "the existing pair is kept");
    assert_eq!(binding_count(root.path()), 0, "no binding left behind");
    assert!(root.path().join("secrets").join("credential-keys").exists(), "the existing key is kept");

    // The commit barrier: the pair was committed, so the refusal says so and
    // the key the pair needs stays; the binding went with the commit.
    let error = exchange_cut_at(&entry, "sign_in_pair_committed", "code-fault-existing-0003", &stub).await.unwrap_err();
    assert_eq!(error.code(), "sign_in_stored_unconfirmed");
    assert_secret_free(&error, "code-fault-existing-0003");
    assert_eq!(binding_count(root.path()), 0, "the binding was consumed by the commit");
    assert!(root.path().join("secrets").join("credential-keys").exists(), "the committed pair keeps its key");
    let replaced = entry.vault().stored_generation(CredentialSlot::OwnerApi).expect("read after a passing barrier").expect("pair");
    assert_ne!(replaced, existing, "the committed pair is the one read once its barrier passes");
    assert!(entry.vault().load(CredentialSlot::OwnerApi).expect("read").is_some_and(|pair| pair.origin == Some(CredentialOrigin::HubLogin)));

    // Aftercare after the durable write: a stored sign-in, aftercare pending.
    let (root, _store, entry) = fixture();
    let signed_in = exchange_cut_at(&entry, "sign_in_aftercare", "code-fault-aftercare-0001", &stub).await.expect("stored");
    assert_eq!(signed_in, OwnerSignIn::AftercarePending);
    assert!(entry.vault().load(CredentialSlot::OwnerApi).expect("read").is_some(), "the pair is stored");
    assert_eq!(binding_count(root.path()), 0);
    assert_eq!(exchange(&entry, "code-fault-aftercare-0002", VERIFIER, OWNER_REDIRECT_URI, &stub).await.expect("stored"), OwnerSignIn::Complete);
}

// The Fleet sign-in (M09-T09): `setup-fleet begin` and the redeem, against a
// loopback stub token endpoint; nothing here reaches Tesla.

const FLEET_CLIENT: &str = "fleet-client-0001";
const FLEET_SECRET: &str = "fleet-secret-value-0001";
const FLEET_REDIRECT: &str = "https://owner.example/fleet/callback";

fn fleet_begin_input(region: &str) -> String {
    serde_json::json!({"clientId": FLEET_CLIENT, "region": region, "redirectUri": FLEET_REDIRECT}).to_string()
}

fn fleet_redeem_input(code: &str) -> String {
    serde_json::json!({"code": code, "redirectUri": FLEET_REDIRECT, "clientId": FLEET_CLIENT, "clientSecret": FLEET_SECRET, "region": "eu"}).to_string()
}

/// The redeem input with the binding its begin answered (M09-T19 ruling 5).
fn fleet_redeem_input_for(code: &str, binding: &FleetSignInBinding) -> String {
    serde_json::json!({"code": code, "redirectUri": FLEET_REDIRECT, "clientId": FLEET_CLIENT, "clientSecret": FLEET_SECRET, "region": "eu", "binding": binding.id().to_string()}).to_string()
}

/// A synthetic Fleet access token: a JWT shape whose claims carry `scopes`.
fn fleet_access(scopes: &[&str]) -> String {
    use base64::Engine as _;
    let encode = |text: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text);
    format!("{}.{}.c3ludGhldGlj", encode(r#"{"alg":"RS256"}"#), encode(&serde_json::json!({"scp": scopes, "ou_code": "EU"}).to_string()))
}

/// A Fleet token answer whose access token carries `scopes`.
fn fleet_answer(scopes: &[&str]) -> String {
    serde_json::json!({"access_token": fleet_access(scopes), "refresh_token": REFRESH, "id_token": ID_TOKEN, "expires_in": 28800, "token_type": "Bearer"}).to_string()
}

/// A Fleet answer with every scope the collector needs.
fn fleet_valid_answer() -> String { fleet_answer(&["openid", "offline_access", "vehicle_device_data", "vehicle_location"]) }

fn fleet_begin(entry: &CredentialVaultEntry, stub: &Stub) -> Result<FleetSignInBinding, CredentialCommandError> {
    fleet_begin_at(entry, fleet_begin_input("eu").as_bytes(), Some(&Url::parse(&stub.url).expect("stub URL")))
}

/// Redeems `input` against the held `binding`. An input object without a
/// `binding` key is given the held one's, so each test checks only its own
/// defect.
async fn fleet_redeem(entry: &CredentialVaultEntry, binding: FleetSignInBinding, input: &str, stub: &Stub) -> Result<OwnerSignIn, CredentialCommandError> {
    // Inserted as text, so a duplicate or malformed member stays as written.
    let input = match serde_json::from_str::<serde_json::Value>(input) {
        Ok(serde_json::Value::Object(object)) if !object.contains_key("binding") && !object.keys().any(|key| key.to_ascii_lowercase().contains("token")) =>
            format!("{{\"binding\":\"{}\"{}{}", binding.id(), if object.is_empty() { "" } else { "," }, &input.trim_start()[1..]),
        _ if input.trim_start().starts_with('{') && !input.contains("\"binding\"") && !input.to_ascii_lowercase().contains("token") =>
            format!("{{\"binding\":\"{}\",{}", binding.id(), &input.trim_start()[1..]),
        _ => input.to_owned(),
    };
    let mut held = Some(binding);
    fleet_exchange_at(entry, &mut held, input.as_bytes(), Some(Url::parse(&stub.url).expect("stub URL"))).await
}

/// No secret of a Fleet attempt appears in a refusal.
fn assert_fleet_secret_free(error: &CredentialCommandError, code: &str) {
    assert_secret_free(error, code);
    assert!(!format!("{error} {error:?}").contains(FLEET_SECRET), "the client secret appears in the refusal");
}

/// Begin records one binding and answers the regional authorise address
/// with the Fleet scope and the binding's state; nothing is sent.
#[tokio::test]
async fn fleet_sign_in_begin_returns_the_regional_authorise_url_and_binding() {
    let (root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Json(200, valid_answer())).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    let before = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs() as i64;
    for (region, authorise) in [("eu", "https://auth.tesla.com/oauth2/v3/authorize?"), ("na", "https://auth.tesla.com/oauth2/v3/authorize?"), ("cn", "https://auth.tesla.cn/oauth2/v3/authorize?")] {
        let url = Url::parse(&stub.url).expect("stub URL");
        let binding = fleet_begin_at(&entry, fleet_begin_input(region).as_bytes(), Some(&url)).expect("begin");
        let address = binding.authorization_url();
        assert!(address.starts_with(authorise), "{region}: regional authorise endpoint");
        let query: std::collections::BTreeMap<String, String> = url::form_urlencoded::parse(address.split_once('?').expect("query").1.as_bytes()).into_owned().collect();
        assert_eq!(query.keys().map(String::as_str).collect::<Vec<_>>(), ["client_id", "redirect_uri", "response_type", "scope", "state"], "no PKCE challenge for Fleet");
        assert_eq!((query["client_id"].as_str(), query["redirect_uri"].as_str(), query["response_type"].as_str(), query["scope"].as_str()),
            (FLEET_CLIENT, FLEET_REDIRECT, "code", super::super::FLEET_SIGN_IN_SCOPE));
        assert_eq!(query["state"].len(), 43, "a 32-byte state");
        assert!((before + 595..=before + 605).contains(&binding.expires_at_s()), "the vault's 10-minute binding");
        let recorded: (String, String) = rusqlite::Connection::open(root.path().join("hub.sqlite")).expect("synthetic SQL")
            .query_row("SELECT sign_in_id, slot FROM credential_sign_ins WHERE sign_in_id=?1", [binding.id().to_string()], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("the binding is recorded");
        assert_eq!(recorded.0, binding.id().to_string());
        abandon_fleet_sign_in(&entry, binding);
    }
    assert_eq!(binding_count(root.path()), 0, "an abandoned binding is removed");
    assert_eq!(stub.accepted(), 0, "begin sends nothing");
    assert!(entry.vault().credential_pair_presence(CredentialSlot::FleetApi).expect("presence").is_none());
}

/// Begin writes its binding only inside an Open window.
#[tokio::test]
async fn fleet_sign_in_begin_needs_an_open_window() {
    for window in [
        TestWindow::closed(),
        { let window = TestWindow::open(600); window.close(); window },
        { let window = TestWindow::open(600); window.advance(601); window },
    ] {
        let (root, _store, entry) = fixture();
        let stub = Stub::spawn(Reply::Json(200, valid_answer())).await;
        let _route = window.route(&stub.url);
        assert_eq!(fleet_begin(&entry, &stub).unwrap_err().code(), WINDOW_CLOSED);
        assert_untouched(root.path(), &entry);
    }
}

/// The redeem posts exactly the six Fleet form members once to the token
/// endpoint and stores the answer through the vault's grant path: origin
/// `fleet_oauth`, the Hub as refresher, the client id and region, and the
/// begin's binding is consumed.
#[tokio::test]
async fn fleet_sign_in_redeem_exchanges_and_stores_through_the_hub_grant() {
    let (root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Json(200, fleet_valid_answer())).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    let binding = fleet_begin(&entry, &stub).expect("begin");
    let signed_in = fleet_redeem(&entry, binding, &fleet_redeem_input("code-fleet-stores-0001"), &stub).await.expect("redeem");
    assert_eq!(signed_in, OwnerSignIn::Complete);
    assert_eq!(stub.accepted(), 1, "one connection");
    let requests = stub.requests.lock().expect("requests");
    let request = std::str::from_utf8(&requests[0]).expect("request text");
    let (head, body) = request.split_once("\r\n\r\n").expect("request parts");
    assert!(head.starts_with("POST /oauth2/v3/token HTTP/1.1\r\n"));
    assert!(head.to_ascii_lowercase().contains("\r\ncontent-type: application/x-www-form-urlencoded"));
    let form: Vec<(String, String)> = url::form_urlencoded::parse(body.as_bytes()).into_owned().collect();
    assert_eq!(form, [
        ("grant_type", "authorization_code"), ("client_id", FLEET_CLIENT), ("client_secret", FLEET_SECRET),
        ("code", "code-fleet-stores-0001"), ("audience", "https://fleet-api.prd.eu.vn.cloud.tesla.com"), ("redirect_uri", FLEET_REDIRECT),
    ].map(|(key, value)| (key.to_owned(), value.to_owned())));
    let pair = entry.vault().load(CredentialSlot::FleetApi).expect("read").expect("own Fleet pair");
    assert_eq!((pair.origin, pair.refresh_owner, pair.access.as_str(), pair.refresh.as_str()),
        (Some(CredentialOrigin::FleetOauth), RefreshOwner::Hub, fleet_access(&["openid", "offline_access", "vehicle_device_data", "vehicle_location"]).as_str(), REFRESH));
    assert_eq!((pair.client_id.as_deref(), pair.region.as_deref()), (Some(FLEET_CLIENT), Some("eu")));
    assert!(entry.vault().credential_pair_presence(CredentialSlot::OwnerApi).expect("presence").is_none(), "nothing else stored");
    assert_eq!(binding_count(root.path()), 0, "the binding was consumed");
}

/// The redeem reads exactly code, redirectUri, clientId, clientSecret,
/// region and binding; any other shape is refused locally with no
/// connection and nothing stored.
#[tokio::test]
async fn fleet_sign_in_redeem_reads_exactly_its_six_keys() {
    let (root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Json(200, valid_answer())).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    let base = |code: &str| serde_json::json!({"code": code, "redirectUri": FLEET_REDIRECT, "clientId": FLEET_CLIENT, "clientSecret": FLEET_SECRET, "region": "eu"});
    let mut invalid: Vec<String> = vec![String::new(), "[]".to_owned(), "{}".to_owned(), format!("{} trailing", base("code-fleet-shape-0001"))];
    for missing in ["code", "redirectUri", "clientId", "clientSecret", "region"] {
        let mut object = base("code-fleet-shape-0002");
        object.as_object_mut().expect("object").remove(missing);
        invalid.push(object.to_string());
    }
    for (key, value) in [("codeVerifier", serde_json::json!(VERIFIER)), ("domain", serde_json::json!("owner.example"))] {
        let mut object = base("code-fleet-shape-0003");
        object.as_object_mut().expect("object").insert(key.to_owned(), value);
        invalid.push(object.to_string());
    }
    for (key, value) in [("clientSecret", serde_json::json!("")), ("region", serde_json::json!("us")), ("clientId", serde_json::json!("")),
        ("redirectUri", serde_json::json!("https://owner.example/callback?query=1")), ("code", serde_json::json!(7))] {
        let mut object = base("code-fleet-shape-0004");
        object[key] = value;
        invalid.push(object.to_string());
    }
    invalid.push(format!(r#"{{"code":"code-fleet-shape-0005","code":"code-fleet-shape-0006","redirectUri":"{FLEET_REDIRECT}","clientId":"{FLEET_CLIENT}","clientSecret":"{FLEET_SECRET}","region":"eu"}}"#));
    for input in &invalid {
        assert_eq!(check_fleet_sign_in_input_for_test(super::super::FleetSignInStep::Redeem, input), Some("sign_in_settings_invalid"), "{input}");
        let binding = fleet_begin(&entry, &stub).expect("begin");
        let error = fleet_redeem(&entry, binding, input, &stub).await.unwrap_err();
        assert_eq!(error.code(), "sign_in_settings_invalid", "{input}");
        assert_fleet_secret_free(&error, "code-fleet-shape-0001");
    }
    for input in [fleet_begin_input("eu"), serde_json::json!({"clientId": FLEET_CLIENT, "region": "eu"}).to_string(),
        serde_json::json!({"clientId": FLEET_CLIENT, "region": "eu", "redirectUri": FLEET_REDIRECT, "clientSecret": FLEET_SECRET}).to_string()] {
        let begin = check_fleet_sign_in_input_for_test(super::super::FleetSignInStep::Begin, &input);
        assert_eq!(begin, (input != fleet_begin_input("eu")).then_some("sign_in_settings_invalid"), "begin reads exactly three keys: {input}");
    }
    assert_eq!(stub.accepted(), 0, "nothing sent");
    assert!(entry.vault().credential_pair_presence(CredentialSlot::FleetApi).expect("presence").is_none());
    drop(root);
}

fn check_fleet_sign_in_input_for_test(step: super::super::FleetSignInStep, input: &str) -> Option<&'static str> {
    super::super::check_fleet_sign_in_input(step, input.as_bytes()).err().map(|error| error.code())
}

/// A key naming a token is a source credential: both steps refuse it
/// `source_credential_disabled` from the keys alone, before any value (the
/// code or the client secret) is read into memory, whatever follows it.
#[tokio::test]
async fn fleet_sign_in_token_keys_are_refused_before_any_secret_is_read() {
    let inputs = [
        format!(r#"{{"clientSecret":"{FLEET_SECRET}","accessToken":"access-0001","code":"c","redirectUri":"{FLEET_REDIRECT}","clientId":"{FLEET_CLIENT}","region":"eu"}}"#),
        r#"{"refresh_token":"refresh-0001"}"#.to_owned(),
        r#"{"IdTOKEN":"id-0001","clientId":"#.to_owned(),
        format!(r#"{{"code":"c","clientSecret":"{FLEET_SECRET}","sourceToken": not json"#),
        r#"{"tokens":{"access":"a"}}"#.to_owned(),
    ];
    let drops = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&drops);
    super::super::observe_sign_in_secret_drops(move |event| record.lock().expect("drops").push(event.kind), || {
        for input in &inputs {
            for step in [super::super::FleetSignInStep::Begin, super::super::FleetSignInStep::Redeem] {
                assert_eq!(check_fleet_sign_in_input_for_test(step, input), Some("source_credential_disabled"), "{step:?} {input}");
            }
        }
    });
    assert!(!drops.lock().expect("drops").contains(&"wire_secret"), "no value was read into a secret");
    let (root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Json(200, valid_answer())).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    let url = Url::parse(&stub.url).expect("stub URL");
    assert_eq!(fleet_begin_at(&entry, inputs[1].as_bytes(), Some(&url)).unwrap_err().code(), "source_credential_disabled");
    assert_eq!(binding_count(root.path()), 0, "no binding for a token input");
    let binding = fleet_begin(&entry, &stub).expect("begin");
    assert_eq!(fleet_redeem(&entry, binding, &inputs[0], &stub).await.unwrap_err().code(), "source_credential_disabled");
    assert_eq!(stub.accepted(), 0, "nothing sent");
    assert!(entry.vault().credential_pair_presence(CredentialSlot::FleetApi).expect("presence").is_none());
}

/// The exchange asks the window's gate immediately before its send: once
/// the window has closed or passed its deadline after the begin, the redeem
/// is refused `window_closed`, nothing is sent and nothing stored.
#[tokio::test]
async fn fleet_sign_in_exchange_is_window_gated() {
    let ends: [fn(&TestWindow); 2] = [|window| window.close(), |window| window.advance(601)];
    for (index, end) in ends.into_iter().enumerate() {
        let (_root, _store, entry) = fixture();
        let stub = Stub::spawn(Reply::Json(200, valid_answer())).await;
        let window = TestWindow::open(600);
        let _route = window.route(&stub.url);
        let binding = fleet_begin(&entry, &stub).expect("begin inside the window");
        end(&window);
        let code = format!("code-fleet-window-{index}");
        let error = fleet_redeem(&entry, binding, &fleet_redeem_input(&code), &stub).await.unwrap_err();
        assert_eq!(error.code(), WINDOW_CLOSED);
        assert_fleet_secret_free(&error, &code);
        assert_eq!(stub.accepted(), 0, "no connection opened");
        assert!(entry.vault().credential_pair_presence(CredentialSlot::FleetApi).expect("presence").is_none(), "nothing stored");
    }
}

/// The client secret is used for the exchange only: every copy is wiped
/// (the parsed value and the request body that carried it), and no byte of
/// it reaches the store, its write-ahead log or the key folder.
#[test]
fn fleet_sign_in_client_secret_is_zeroised_and_never_stored() {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("runtime");
    let (root, _store, entry) = fixture();
    let stub = runtime.block_on(Stub::spawn(Reply::Json(200, fleet_valid_answer())));
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    let binding = fleet_begin(&entry, &stub).expect("begin");
    let drops = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&drops);
    let input = fleet_redeem_input("code-fleet-zeroised-0001");
    // The observer is this thread's; the redeem is polled on this thread.
    super::super::observe_sign_in_secret_drops(move |event| record.lock().expect("drops").push(event), || {
        runtime.block_on(fleet_redeem(&entry, binding, &input, &stub)).expect("redeem");
    });
    let wiped_body = || drops.lock().expect("drops").iter().any(|event| event.kind == "request_body" && event.before > FLEET_SECRET.len() && event.zeroised);
    let waited = std::time::Instant::now();
    while !wiped_body() && waited.elapsed() < std::time::Duration::from_secs(5) { std::thread::sleep(std::time::Duration::from_millis(20)); }
    assert!(wiped_body(), "the request body holding the secret was wiped once sent");
    let drops = drops.lock().expect("drops");
    assert!(drops.iter().any(|event| event.kind == "wire_secret" && event.before == FLEET_SECRET.len()), "the parsed client secret was dropped");
    assert!(drops.iter().filter(|event| event.kind == "wire_secret").all(|event| event.zeroised && event.after == 0), "every parsed value was wiped");
    let mut files = vec![root.path().to_owned()];
    while let Some(path) = files.pop() {
        if path.is_dir() {
            files.extend(std::fs::read_dir(&path).expect("listing").map(|entry| entry.expect("entry").path()));
        } else if let Ok(bytes) = std::fs::read(&path) {
            assert!(!bytes.windows(FLEET_SECRET.len()).any(|window| window == FLEET_SECRET.as_bytes()), "the client secret is stored in {path:?}");
        }
    }
}

/// A redeem finishes only the sign-in its begin started: another client,
/// region or redirect is refused locally, as is a redeem with no begin.
#[tokio::test]
async fn fleet_sign_in_redeem_must_finish_its_begin() {
    let (_root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Json(200, valid_answer())).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    for (key, value) in [("clientId", "fleet-client-0002"), ("region", "na"), ("redirectUri", "https://owner.example/other")] {
        let mut input: serde_json::Value = serde_json::from_str(&fleet_redeem_input("code-fleet-binding-0001")).expect("input");
        input[key] = value.into();
        let binding = fleet_begin(&entry, &stub).expect("begin");
        let error = fleet_redeem(&entry, binding, &input.to_string(), &stub).await.unwrap_err();
        assert_eq!(error.code(), SIGN_IN_BINDING_INVALID, "{key}");
        assert_fleet_secret_free(&error, "code-fleet-binding-0001");
    }
    assert_eq!(fleet_sign_in_not_begun().code(), SIGN_IN_BINDING_INVALID);
    assert_eq!(stub.accepted(), 0, "nothing sent");
    assert!(entry.vault().credential_pair_presence(CredentialSlot::FleetApi).expect("presence").is_none());
}

/// Tesla's refusal reaches the operator as its fixed code and stores
/// nothing; a code is exchanged at most once.
#[tokio::test]
async fn fleet_sign_in_refusal_stores_nothing_and_a_code_is_used_once() {
    let (_root, _store, entry) = fixture();
    let refusing = Stub::spawn(Reply::Json(400, r#"{"error":"invalid_grant","error_description":"synthetic-description-0001"}"#.to_owned())).await;
    let window = TestWindow::open(600);
    let _route = window.route(&refusing.url);
    let binding = fleet_begin(&entry, &refusing).expect("begin");
    let error = fleet_redeem(&entry, binding, &fleet_redeem_input("code-fleet-refused-0001"), &refusing).await.unwrap_err();
    assert_eq!(error.code(), "sign_in_refused");
    assert_fleet_secret_free(&error, "code-fleet-refused-0001");
    assert_eq!(refusing.accepted(), 1);
    assert!(entry.vault().credential_pair_presence(CredentialSlot::FleetApi).expect("presence").is_none(), "nothing stored");
    let binding = fleet_begin(&entry, &refusing).expect("begin again");
    let error = fleet_redeem(&entry, binding, &fleet_redeem_input("code-fleet-refused-0001"), &refusing).await.unwrap_err();
    assert_eq!(error.code(), SIGN_IN_CODE_REUSED, "the same code is not sent twice");
    assert_eq!(refusing.accepted(), 1);
}

/// Car discovery after a Fleet sign-in uses only the vault's own Fleet pair
/// on a Fleet-configured Hub, and asks the window's gate before its list:
/// with the window closed it is refused `window_closed` with nothing sent
/// and no car configured.
#[tokio::test]
async fn fleet_sign_in_discovery_refuses_before_any_send() {
    use crate::{auth::credential_vault::fixtures::fixture_admission, collection::collector::OwnerCarSelection, config::HubConfig};
    let (root, store, entry) = fixture();
    let admission = fixture_admission(&store, root.path());
    let valid_answer = fleet_valid_answer;
    let config = |provider: &str| HubConfig::from_exact_bytes(format!(
        "data_dir = {:?}\nbind = '127.0.0.1:18443'\n[collector]\ninterval_seconds = 0\nprovider = {provider:?}\n", root.path()).as_bytes())
        .expect("configuration").0;
    let discover = |config: HubConfig, selection| {
        let (entry, store, admission) = (&entry, &store, &admission);
        async move { crate::auth::fleet_credentials::discover_fleet_cars(entry, store, &config, admission, selection).await.map_err(|error| error.code()) }
    };
    assert_eq!(discover(config("legacy"), OwnerCarSelection::All).await, Err("discovery_settings_invalid"), "a Legacy Hub lists no Fleet cars");
    assert_eq!(discover(config("fleet"), OwnerCarSelection::Vehicle(0)).await, Err("discovery_selection_invalid"));
    assert_eq!(discover(config("fleet"), OwnerCarSelection::All).await, Err("discovery_pair_unavailable"), "no own Fleet pair yet");

    let stub = Stub::spawn(Reply::Json(200, valid_answer())).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    let binding = fleet_begin(&entry, &stub).expect("begin");
    fleet_redeem(&entry, binding, &fleet_redeem_input("code-fleet-discovery-0001"), &stub).await.expect("redeem");
    let closed = TestWindow::closed();
    let _fleet_api = closed.route(crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica.base_url());
    assert_eq!(discover(config("fleet"), OwnerCarSelection::All).await, Err(WINDOW_CLOSED));
    assert!(store.configured_tesla_vehicles().expect("cars").is_empty(), "no car configured");
    assert!(entry.vault().load(CredentialSlot::FleetApi).expect("read").is_some(), "the stored pair stays");
}

/// M09-T19 ruling 5: the redeem carries the binding its begin answered and
/// is refused, with the sign-in kept and nothing sent, when that is not the
/// newest one held; a failure proven to come before the send keeps the
/// sign-in and leaves the code unused, so the same code then succeeds.
#[tokio::test]
async fn fleet_sign_in_redeem_binding_and_unsent_failures_keep_the_attempt() {
    let (_root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Json(200, fleet_valid_answer())).await;
    let url = || Some(Url::parse(&stub.url).expect("stub URL"));
    let window = TestWindow::open(600);
    let route = window.route(&stub.url);
    let older = fleet_begin(&entry, &stub).expect("begin A");
    let older_input = fleet_redeem_input_for("code-fleet-binding-0101", &older);
    abandon_fleet_sign_in(&entry, older);
    let newest = fleet_begin(&entry, &stub).expect("begin B");
    let newest_id = newest.id();
    let mut held = Some(newest);
    // A's binding with B held: refused locally, B kept, nothing sent.
    let error = fleet_exchange_at(&entry, &mut held, older_input.as_bytes(), url()).await.unwrap_err();
    assert_eq!(error.code(), SIGN_IN_BINDING_INVALID);
    assert_eq!(held.as_ref().map(FleetSignInBinding::id), Some(newest_id), "the newest sign-in is kept");
    // No binding key at all: the input is refused as a shape.
    let error = fleet_exchange_at(&entry, &mut held, fleet_redeem_input("code-fleet-binding-0102").as_bytes(), url()).await.unwrap_err();
    assert_eq!(error.code(), "sign_in_settings_invalid");
    assert!(held.is_some());
    // The window closes before the send: refused, B kept, the code unused.
    let input = fleet_redeem_input_for("code-fleet-binding-0103", held.as_ref().expect("held"));
    window.close();
    let error = fleet_exchange_at(&entry, &mut held, input.as_bytes(), url()).await.unwrap_err();
    assert_eq!(error.code(), WINDOW_CLOSED);
    assert_eq!(held.as_ref().map(FleetSignInBinding::id), Some(newest_id), "a refusal before the send keeps the sign-in");
    assert_eq!(stub.accepted(), 0, "nothing sent");
    // Reopened: the same code and binding now redeem.
    drop(route);
    let reopened = TestWindow::open(600);
    let _reopened_route = reopened.route(&stub.url);
    let signed_in = fleet_exchange_at(&entry, &mut held, input.as_bytes(), url()).await.expect("the unsent code is still usable");
    assert_eq!(signed_in, OwnerSignIn::Complete);
    assert!(held.is_none(), "a sent redeem uses the sign-in up");
    assert_eq!(stub.accepted(), 1);
}

// M09-T19 B5: the redeem input and Tesla's answers are read without escapes.

/// The test text with each `~` written as a backslash, so the escapes
/// below read as JSON writes them.
fn esc(text: &str) -> String { text.replace('~', "\\") }

const ESCAPED_REDIRECT: &str = r#"https:~/~/owner.example~/fleet~/callback"#;
/// `fleet-secret-value/0001`, written with a `~u` and a `~/` escape.
const ESCAPED_SECRET: &str = r#"fleet-secret-~u0076alue~/0001"#;

fn escaped_redeem_input(code: &str, binding: &FleetSignInBinding, secret: &str) -> String {
    esc(&format!(r#"{{"code":"{code}","redirectUri":"{ESCAPED_REDIRECT}","clientId":"{FLEET_CLIENT}","clientSecret":"{secret}","region":"eu","binding":"{}"}}"#, binding.id()))
}

/// A client secret written with `\u` and `\/` escapes, and a redirect
/// address written with `\/` as Apple's encoders write it, begin and redeem;
/// the secret and address sent are the decoded ones. A token answer with
/// escaped tokens is read correctly. No secret is taken from the parser's
/// own storage, and every document without escapes is wiped.
#[test]
fn fleet_sign_in_escaped_input_and_answer_redeem_without_a_parser_copy() {
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("runtime");
    let (_root, _store, entry) = fixture();
    let access = fleet_access(&["openid", "offline_access", "vehicle_device_data", "vehicle_location"]);
    let answer = serde_json::json!({"access_token": access, "refresh_token": REFRESH, "expires_in": 28800, "token_type": "Bearer"}).to_string()
        .replacen('.', &esc("~u002e"), 1).replace("exchange-refresh", &esc("exchange-r~u0065fresh"));
    assert!(answer.contains(&esc("~u002e")) && answer.contains(&esc("~u0065")), "escaped tokens");
    let stub = runtime.block_on(Stub::spawn(Reply::Json(200, answer)));
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    let url = Url::parse(&stub.url).expect("stub URL");
    let begin = esc(&format!(r#"{{"clientId":"{FLEET_CLIENT}","region":"eu","redirectUri":"{ESCAPED_REDIRECT}"}}"#));
    let binding = fleet_begin_at(&entry, begin.as_bytes(), Some(&url)).expect("an escaped redirect begins");
    let input = escaped_redeem_input("code-fleet-escaped-0001", &binding, ESCAPED_SECRET);
    assert_eq!(check_fleet_sign_in_input_for_test(super::super::FleetSignInStep::Redeem, &input), None, "the CLI check accepts it");
    let copied = crate::fleet_api::parser_copied_secrets();
    let drops = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&drops);
    let signed_in = super::super::observe_sign_in_secret_drops(move |event| record.lock().expect("drops").push(event), || {
        let mut held = Some(binding);
        runtime.block_on(fleet_exchange_at(&entry, &mut held, input.as_bytes(), Some(url)))
    });
    assert_eq!(signed_in.expect("redeem"), OwnerSignIn::Complete);
    assert_eq!(crate::fleet_api::parser_copied_secrets(), copied, "no secret decoded outside wiping storage");
    let drops = drops.lock().expect("drops");
    let resolved: Vec<_> = drops.iter().filter(|event| event.kind == "escape_free_json").collect();
    assert!(resolved.len() >= 2, "the input and the answer were resolved: {}", resolved.len());
    assert!(resolved.iter().all(|event| event.zeroised && event.after == 0), "every resolved document was wiped");
    let requests = stub.requests.lock().expect("requests");
    let request = std::str::from_utf8(&requests[0]).expect("request text");
    let form: std::collections::BTreeMap<String, String> = url::form_urlencoded::parse(request.split_once("\r\n\r\n").expect("body").1.as_bytes()).into_owned().collect();
    assert_eq!((form["client_secret"].as_str(), form["redirect_uri"].as_str()), ("fleet-secret-value/0001", FLEET_REDIRECT));
    let pair = entry.vault().load(CredentialSlot::FleetApi).expect("read").expect("own Fleet pair");
    assert_eq!((pair.access.as_str(), pair.refresh.as_str()), (access.as_str(), REFRESH), "the escaped tokens were read correctly");
}

/// `\"`, `\\`, `\n`, a lone surrogate, a truncated escape and a backslash
/// outside a string are each refused `sign_in_settings_invalid` before any
/// parse: a token-named key beside them is not even read. The CLI check and
/// the service refuse alike, the attempt is kept and nothing is sent.
#[tokio::test]
async fn fleet_sign_in_unresolvable_escapes_are_refused_before_any_parse() {
    let (_root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Json(200, fleet_valid_answer())).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    let binding = fleet_begin(&entry, &stub).expect("begin");
    let binding_id = binding.id();
    let mut held = Some(binding);
    let held_binding = held.as_ref().expect("held");
    let mut inputs: Vec<String> = [r#"a~"b"#, "a~~b", "a~nb", "a~uD800b", "a~u00"]
        .iter().map(|secret| escaped_redeem_input("code-fleet-escape-0001", held_binding, secret)).collect();
    inputs.push(esc(&format!("~{}", fleet_redeem_input_for("code-fleet-escape-0001", held_binding))));
    for input in inputs {
        let with_token_key = input.replacen('{', r#"{"accessToken":"x","#, 1);
        for line in [&input, &with_token_key] {
            assert_eq!(check_fleet_sign_in_input_for_test(super::super::FleetSignInStep::Redeem, line), Some("sign_in_settings_invalid"), "{line}");
            let error = fleet_exchange_at(&entry, &mut held, line.as_bytes(), Some(Url::parse(&stub.url).expect("stub URL"))).await.unwrap_err();
            assert_eq!(error.code(), "sign_in_settings_invalid", "{line}");
            assert_fleet_secret_free(&error, "code-fleet-escape-0001");
            assert_eq!(held.as_ref().map(FleetSignInBinding::id), Some(binding_id), "the attempt is kept");
        }
    }
    assert_eq!(stub.accepted(), 0, "nothing sent");
}

/// Tesla's `invalid_grant` with `\/` in its description is still the
/// refusal it was; a refused escape in an error answer is an answer not
/// understood, as one that does not parse.
#[tokio::test]
async fn fleet_sign_in_escaped_error_answer_keeps_its_class() {
    for (body, code, number) in [
        (r#"{"error":"invalid_grant","error_description":"see https:\/\/auth.example\/help"}"#, "sign_in_refused", "0001"),
        (r#"{"error":"invalid_grant","error_description":"a \"quoted\" word"}"#, "sign_in_answer_invalid", "0002"),
    ] {
        let (_root, _store, entry) = fixture();
        let refusing = Stub::spawn(Reply::Json(400, body.to_owned())).await;
        let window = TestWindow::open(600);
        let _route = window.route(&refusing.url);
        let binding = fleet_begin(&entry, &refusing).expect("begin");
        let code_text = format!("code-fleet-escaped-error-{number}");
        let error = fleet_redeem(&entry, binding, &fleet_redeem_input(&code_text), &refusing).await.unwrap_err();
        assert_eq!(error.code(), code, "{body}");
        if number == "0001" {
            assert_eq!(error.code(), refused(body.replace(r"\/", "/").as_bytes()).code(), "classified as the same answer without escapes");
        }
        assert_eq!(refusing.accepted(), 1);
    }
}

/// M09-T19 R1, the reviewer's interleaving through the service's slot: a
/// redeem of A refused before any send is paused before its restore; begin
/// B; redeem B succeeds; A's restore resumes. A is gone, and a later redeem
/// with A's binding is refused without a send. Without anything in between,
/// the same refusal puts A back; a begin alone in between keeps B.
#[tokio::test]
async fn fleet_sign_in_slot_never_revives_a_superseded_attempt() {
    let (_root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Json(200, fleet_valid_answer())).await;
    let url = || Some(Url::parse(&stub.url).expect("stub URL"));
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    let slot = FleetSignInSlot::default();
    let mismatched = |code: &str| serde_json::json!({"code": code, "redirectUri": FLEET_REDIRECT, "clientId": FLEET_CLIENT,
        "clientSecret": FLEET_SECRET, "region": "eu", "binding": uuid::Uuid::new_v4().to_string()}).to_string();
    let held_id = |slot: &FleetSignInSlot| {
        let mut taken = slot.take().expect("slot");
        let id = taken.as_mut().and_then(|taken| taken.attempt().as_ref().map(FleetSignInBinding::id));
        if let Some(taken) = taken { slot.settle(taken); }
        id
    };

    let a = fleet_begin(&entry, &stub).expect("begin A");
    let (a_id, a_input) = (a.id(), fleet_redeem_input_for("code-fleet-revive-0001", &a));
    slot.publish(&entry, a).expect("publish A");
    // Nothing in between: a refusal before the send puts A back.
    let mut taken_a = slot.take().expect("slot").expect("A held");
    let error = fleet_exchange_at(&entry, taken_a.attempt(), mismatched("code-fleet-revive-0002").as_bytes(), url()).await.unwrap_err();
    assert_eq!(error.code(), SIGN_IN_BINDING_INVALID);
    slot.settle(taken_a);
    assert_eq!(held_id(&slot), Some(a_id), "an unsent refusal keeps the attempt");

    // The interleaving: A's refused redeem is paused before its restore.
    let mut taken_a = slot.take().expect("slot").expect("A held");
    let error = fleet_exchange_at(&entry, taken_a.attempt(), mismatched("code-fleet-revive-0003").as_bytes(), url()).await.unwrap_err();
    assert_eq!(error.code(), SIGN_IN_BINDING_INVALID);
    assert_eq!(taken_a.attempt().as_ref().map(FleetSignInBinding::id), Some(a_id), "A is kept by its paused redeem");
    // Begin B finds the slot empty; redeem B succeeds.
    let b = fleet_begin(&entry, &stub).expect("begin B");
    let b_input = fleet_redeem_input_for("code-fleet-revive-0004", &b);
    slot.publish(&entry, b).expect("publish B");
    let mut taken_b = slot.take().expect("slot").expect("B held");
    assert_eq!(fleet_exchange_at(&entry, taken_b.attempt(), b_input.as_bytes(), url()).await.expect("redeem B"), OwnerSignIn::Complete);
    assert!(taken_b.attempt().is_none(), "B is used up");
    slot.settle(taken_b);
    assert_eq!(stub.accepted(), 1, "B's one exchange");
    let stored = entry.vault().load(CredentialSlot::FleetApi).expect("read").expect("B's login").generation;
    // A's restore resumes: A is gone.
    slot.settle(taken_a);
    assert!(slot.take().expect("slot").is_none(), "the superseded attempt is not revived");
    // A later redeem with A's binding finds nothing held: refused, nothing sent.
    let error = fleet_exchange_at(&entry, &mut None, a_input.as_bytes(), url()).await.unwrap_err();
    assert_eq!(error.code(), SIGN_IN_BINDING_INVALID);
    assert_eq!(stub.accepted(), 1, "nothing sent for A");
    assert_eq!(entry.vault().load(CredentialSlot::FleetApi).expect("read").expect("login").generation, stored, "B's login stays");

    // A begin alone in between: A's paused restore leaves B held.
    let c = fleet_begin(&entry, &stub).expect("begin C");
    let c_id = c.id();
    slot.publish(&entry, c).expect("publish C");
    let mut taken_c = slot.take().expect("slot").expect("C held");
    let error = fleet_exchange_at(&entry, taken_c.attempt(), mismatched("code-fleet-revive-0005").as_bytes(), url()).await.unwrap_err();
    assert_eq!(error.code(), SIGN_IN_BINDING_INVALID);
    let d = fleet_begin(&entry, &stub).expect("begin D");
    let d_id = d.id();
    slot.publish(&entry, d).expect("publish D");
    slot.settle(taken_c);
    assert_eq!(held_id(&slot), Some(d_id), "the newer begin stays held");
    assert_ne!(c_id, d_id);
    assert_eq!(stub.accepted(), 1);
}

/// M09-T19 ruling 5: a refused connection is proven to have sent nothing:
/// the sign-in stays and the code is not recorded as used. The used-code
/// record is per issuer, application and slot: the same text redeemed for
/// another application is not "reused".
#[tokio::test]
async fn fleet_sign_in_unsent_connection_and_code_namespaces() {
    let (_root, _store, entry) = fixture();
    let stub = Stub::spawn(Reply::Json(200, fleet_valid_answer())).await;
    let window = TestWindow::open(600);
    let _route = window.route(&stub.url);
    // A loopback port with nothing listening: the connection is refused.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("port");
    let dead = format!("http://{}/oauth2/v3/token", closed.local_addr().expect("address"));
    drop(closed);
    let _dead_route = window.route(&dead);
    let binding = fleet_begin(&entry, &stub).expect("begin");
    let id = binding.id();
    let input = fleet_redeem_input_for("code-fleet-namespace-0201", &binding);
    let mut held = Some(binding);
    let error = fleet_exchange_at(&entry, &mut held, input.as_bytes(), Some(Url::parse(&dead).expect("URL"))).await.unwrap_err();
    assert_eq!(error.code(), "sign_in_unavailable");
    assert_eq!(held.as_ref().map(FleetSignInBinding::id), Some(id), "a refused connection keeps the sign-in");
    fleet_exchange_at(&entry, &mut held, input.as_bytes(), Some(Url::parse(&stub.url).expect("URL"))).await.expect("the code was not spent");
    // Another application's code with the same text is its own code.
    let other_client = "fleet-client-0099";
    let begin = serde_json::json!({"clientId": other_client, "region": "eu", "redirectUri": FLEET_REDIRECT}).to_string();
    let other = fleet_begin_at(&entry, begin.as_bytes(), Some(&Url::parse(&stub.url).expect("URL"))).expect("begin");
    let input = serde_json::json!({"code": "code-fleet-namespace-0201", "redirectUri": FLEET_REDIRECT, "clientId": other_client,
        "clientSecret": FLEET_SECRET, "region": "eu", "binding": other.id().to_string()}).to_string();
    let mut held = Some(other);
    fleet_exchange_at(&entry, &mut held, input.as_bytes(), Some(Url::parse(&stub.url).expect("URL"))).await
        .expect("the same text for another application is not a reused code");
}

/// M09-T19 ruling 6: a grant without a scope the collector needs, or with
/// claims it cannot read, is refused with a fixed code naming what is
/// missing, and the stored login stays exactly as it was.
#[tokio::test]
async fn fleet_sign_in_grant_without_collection_scopes_keeps_the_stored_login() {
    let (_root, _store, entry) = fixture();
    let window = TestWindow::open(600);
    let good = Stub::spawn(Reply::Json(200, fleet_valid_answer())).await;
    let _good_route = window.route(&good.url);
    let binding = fleet_begin(&entry, &good).expect("begin");
    fleet_redeem(&entry, binding, &fleet_redeem_input("code-fleet-scope-0300"), &good).await.expect("the first login");
    let stored = entry.vault().stored_generation(CredentialSlot::FleetApi).expect("read").expect("stored");
    for (index, (answer, code)) in [
        (fleet_answer(&["openid", "offline_access", "vehicle_device_data"]), FLEET_SCOPE_MISSING_VEHICLE_LOCATION),
        (fleet_answer(&["openid", "offline_access", "vehicle_location"]), FLEET_SCOPE_MISSING_VEHICLE_DEVICE_DATA),
        (fleet_answer(&["openid", "offline_access", "vehicle_cmds"]), FLEET_SCOPES_MISSING),
        (valid_answer(), FLEET_TOKEN_CLAIMS_INVALID),
    ].into_iter().enumerate() {
        let stub = Stub::spawn(Reply::Json(200, answer)).await;
        let _route = window.route(&stub.url);
        let binding = fleet_begin(&entry, &stub).expect("begin");
        let error = fleet_redeem(&entry, binding, &fleet_redeem_input(&format!("code-fleet-scope-03{index:02}x")), &stub).await.unwrap_err();
        assert_eq!(error.code(), code);
        assert!(error.to_string().contains("stored login was kept"), "{error}");
        assert_eq!(entry.vault().stored_generation(CredentialSlot::FleetApi).expect("read"), Some(stored), "the stored login stays");
    }
}

/// M09-T19 ruling 8: the Fleet form is allocated at its full bounded size
/// before the client secret is copied in, so a short secret followed by the
/// longest code never reallocates it (no freed block keeps the secret).
#[test]
fn fleet_request_form_never_grows_past_its_first_allocation() {
    let code = "c".repeat(MAX_OWNER_CODE_BYTES);
    for secret in ["s", "a-much-longer-synthetic-secret-0001"] {
        let input = serde_json::json!({"code": code, "redirectUri": FLEET_REDIRECT, "clientId": FLEET_CLIENT, "clientSecret": secret, "region": "eu",
            "binding": uuid::Uuid::new_v4().to_string()}).to_string();
        let (settings, code, _binding) = SignInSettings::fleet_redeem(input.as_bytes()).expect("input");
        let (form, bounded) = fleet_request_form(&settings, code.as_str()).expect("form");
        assert_eq!(form.capacity(), bounded, "{secret}: the form never grew");
        assert!(form.len() <= bounded);
    }
    // The growth that remains possible elsewhere wipes what it replaces: the
    // bytes move, nothing is lost and later pushes fit the reserved room.
    let mut buffer = SecretBuffer::new("growth", 4);
    super::super::protocol::parameter(&mut buffer, "client_secret", FLEET_SECRET);
    let after_reserve = buffer.capacity();
    super::super::protocol::parameter(&mut buffer, "code", "c");
    assert!(buffer.starts_with(format!("client_secret={FLEET_SECRET}").as_bytes()));
    assert!(after_reserve >= format!("client_secret={FLEET_SECRET}").len());
}
