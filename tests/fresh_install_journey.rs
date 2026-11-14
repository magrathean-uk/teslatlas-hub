// SPDX-License-Identifier: AGPL-3.0-only

//! M02-T21 round 3 (DEC-116): the fresh-install journey on the real service
//! binary and a fresh data folder.
//!
//! The binary starts idle with the configuration the Mac app writes, a window
//! is trusted and opened through its operator channel, the owner signs in
//! through `setup`, the setup itself sets the collection interval and reloads
//! the collector, one collection completes (the collector lease and a stored
//! vehicle-data observation, through a Tesla double), the window closes, and
//! the Hub stops and starts again still configured, with nothing sent to the
//! Tesla double outside the window. The test writes only the App's default
//! configuration: no import and no other configuration write.
//!
//! The doubles are compile-time only (`journey-doubles`, enabled by the
//! crate's own test build): a file-backed window anchor in place of the login
//! Keychain, a zero time offset in place of the time server, an accepted
//! issuer confirmation, and loopback Tesla doubles for the token exchange,
//! the Owner API and the stream. The window issuer runs here, with the
//! library's issuer and an in-memory anchor. Every proxy variable points at a
//! counting endpoint, so any other connection the binary tries is counted.
#![cfg(target_os = "macos")]

use std::{
    fs,
    io::Write,
    net::{SocketAddr, TcpListener},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    extract::State,
    http::{StatusCode, Uri},
    response::IntoResponse,
    routing::{any, get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use tempfile::TempDir;
use teslatlas_hub::collection::window::{IssueRequest, Issuer, MemoryAnchor, SystemClock};

const BIN: &str = env!("CARGO_BIN_EXE_teslatlas-hub");
const SERVE_START_LIMIT: Duration = Duration::from_secs(90);
const COLLECTION_LIMIT: Duration = Duration::from_secs(90);
const QUIET_PERIOD: Duration = Duration::from_secs(4);
const PRODUCT_ID: u64 = 1_234_500_000_000_001;
const STREAM_VEHICLE_ID: u64 = 1_234_500_001;
const VIN: &str = "5YJ3E1EA7KF530003";
const SUPPORTED_INTERVAL_SECONDS: &str = "300";

fn now_s() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs())
        .expect("seconds")
}

/// A temporary directory short enough for the operator socket path.
fn short_tempdir() -> TempDir {
    let base = std::env::temp_dir();
    if base.as_os_str().len() <= 50 {
        TempDir::new_in(base).expect("temp dir")
    } else {
        TempDir::new_in("/tmp").expect("temp dir")
    }
}

/// Counts every connection on loopback and never answers: every proxy
/// variable points here, so nothing the binary tries leaves the Mac.
fn counting_endpoint() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = format!("http://{}", listener.local_addr().expect("address"));
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            held.push(stream);
        }
    });
    (address, accepted)
}

#[derive(Default)]
struct TeslaState {
    requests: Mutex<Vec<String>>,
    unexpected: AtomicUsize,
    no_cars: AtomicBool,
}

/// The Tesla double: the owner token endpoint, the Owner API (products,
/// vehicle data, the vehicle probe) and a stream endpoint that accepts and
/// closes. It counts every request; anything else is unexpected.
struct TeslaDouble {
    state: Arc<TeslaState>,
    http: SocketAddr,
    stream: SocketAddr,
    stream_connections: Arc<AtomicUsize>,
    _runtime: tokio::runtime::Runtime,
}

fn record(state: &TeslaState, line: String) {
    state.requests.lock().expect("requests").push(line);
}

fn jwt_access_token() -> String {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        format!(
            r#"{{"iss":"https://auth.tesla.com/oauth2/v3","exp":{},"aud":"journey"}}"#,
            now_s() + 86_400
        )
        .as_bytes(),
    );
    format!("{header}.{payload}.c2lnbmF0dXJl")
}

async fn token(State(state): State<Arc<TeslaState>>, body: String) -> impl IntoResponse {
    record(&state, "POST /oauth2/v3/token".to_owned());
    let request: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    if request["grant_type"] != "authorization_code" || request["code"] != "journey-code-0001" {
        state.unexpected.fetch_add(1, Ordering::SeqCst);
        return (StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#.to_owned());
    }
    let answer = serde_json::json!({
        "access_token": jwt_access_token(),
        "refresh_token": "journey-refresh-0001",
        "expires_in": 28_800,
        "token_type": "Bearer",
    });
    (StatusCode::OK, answer.to_string())
}

async fn products(State(state): State<Arc<TeslaState>>) -> impl IntoResponse {
    record(&state, "GET /api/1/products".to_owned());
    if state.no_cars.load(Ordering::SeqCst) {
        return (StatusCode::OK, r#"{"response":[],"count":0}"#.to_owned());
    }
    let body = serde_json::json!({
        "response": [{
            "id": PRODUCT_ID,
            "vehicle_id": STREAM_VEHICLE_ID,
            "vin": VIN,
            "state": "online",
            "display_name": "Journey car",
        }],
        "count": 1,
    });
    (StatusCode::OK, body.to_string())
}

async fn vehicle(State(state): State<Arc<TeslaState>>, uri: Uri) -> impl IntoResponse {
    let path = uri.path().to_owned();
    record(&state, format!("GET {path}"));
    let prefix = format!("/api/1/vehicles/{PRODUCT_ID}");
    if path == format!("{prefix}/vehicle_data") {
        let now_ms = now_s() * 1000;
        let body = serde_json::json!({
            "response": {
                "state": "online",
                "drive_state": {
                    "timestamp": now_ms,
                    "latitude": 47.4979,
                    "longitude": 19.0402,
                    "shift_state": null,
                    "speed": null,
                },
            },
        });
        return (StatusCode::OK, body.to_string());
    }
    if path == prefix {
        let body = serde_json::json!({
            "response": {"id": PRODUCT_ID, "state": "online", "in_service": false},
        });
        return (StatusCode::OK, body.to_string());
    }
    state.unexpected.fetch_add(1, Ordering::SeqCst);
    (StatusCode::NOT_FOUND, "{}".to_owned())
}

async fn unexpected(State(state): State<Arc<TeslaState>>, uri: Uri) -> impl IntoResponse {
    record(&state, format!("UNEXPECTED {}", uri.path()));
    state.unexpected.fetch_add(1, Ordering::SeqCst);
    StatusCode::NOT_FOUND
}

impl TeslaDouble {
    fn spawn() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let state = Arc::new(TeslaState::default());
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let http = listener.local_addr().expect("address");
        let router = Router::new()
            .route("/oauth2/v3/token", post(token))
            .route("/api/1/products", get(products))
            .route("/api/1/vehicles/{*rest}", get(vehicle))
            .fallback(any(unexpected))
            .with_state(state.clone());
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            let _ = axum::serve(listener, router).await;
        });
        // The stream double accepts and closes: the collector's stream fails
        // and reconnects, and its connections are counted.
        let stream_listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let stream = stream_listener.local_addr().expect("address");
        let stream_connections = Arc::new(AtomicUsize::new(0));
        let counter = stream_connections.clone();
        std::thread::spawn(move || {
            for connection in stream_listener.incoming().flatten() {
                counter.fetch_add(1, Ordering::SeqCst);
                drop(connection);
            }
        });
        Self {
            state,
            http,
            stream,
            stream_connections,
            _runtime: runtime,
        }
    }

    fn requests(&self) -> Vec<String> {
        self.state.requests.lock().expect("requests").clone()
    }

    fn count(&self, line: &str) -> usize {
        self.requests().iter().filter(|request| request.as_str() == line).count()
    }

    /// Every request and stream connection so far.
    fn traffic(&self) -> (usize, usize) {
        (self.requests().len(), self.stream_connections.load(Ordering::SeqCst))
    }
}

struct Hub {
    tmp: TempDir,
    tesla: TeslaDouble,
    proxy: String,
    proxied: Arc<AtomicUsize>,
    port: u16,
}

impl Hub {
    fn new() -> Self {
        let tmp = short_tempdir();
        for dir in ["home", "data", "anchor"] {
            fs::create_dir(tmp.path().join(dir)).expect("dir");
            fs::set_permissions(tmp.path().join(dir), fs::Permissions::from_mode(0o700))
                .expect("private dir");
        }
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("free port");
            listener.local_addr().expect("address").port()
        };
        let (proxy, proxied) = counting_endpoint();
        let hub = Self {
            tmp,
            tesla: TeslaDouble::spawn(),
            proxy,
            proxied,
            port,
        };
        let doubles = serde_json::json!({
            "anchorDir": hub.path("anchor"),
            "tokenEndpoint": format!("http://{}/oauth2/v3/token", hub.tesla.http),
            "ownerApiBase": format!("http://{}/", hub.tesla.http),
            "streamEndpoint": format!("wss://{}/streaming/", hub.tesla.stream),
        });
        fs::write(hub.path("doubles.json"), doubles.to_string()).expect("doubles");
        // The configuration `ensureConfig()` writes on a fresh Mac, with a
        // free loopback port: the only configuration this test writes.
        let content = format!(
            "data_dir = {:?}\nbind = \"127.0.0.1:{}\"\n\n[geocoder]\nenabled = false\n\n[terrain]\nenabled = false\n",
            hub.data(),
            hub.port
        );
        fs::write(hub.config_path(), content).expect("config");
        fs::set_permissions(hub.config_path(), fs::Permissions::from_mode(0o600)).expect("config mode");
        hub
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn data(&self) -> PathBuf {
        self.path("data")
    }

    fn config_path(&self) -> PathBuf {
        self.path("config.toml")
    }

    fn config(&self) -> String {
        fs::read_to_string(self.config_path()).expect("config")
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .arg("--config")
            .arg(self.config_path())
            .args(arguments)
            .env_clear()
            .env("HOME", self.path("home"))
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("TMPDIR", self.tmp.path())
            .env("RUST_LOG", "info")
            .env("TESLATLAS_HUB_JOURNEY_DOUBLES", self.path("doubles.json"))
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost");
        for variable in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
            command.env(variable, &self.proxy);
        }
        command
    }

    fn run(&self, arguments: &[&str], stdin: &[u8]) -> Output {
        let mut child = self
            .command(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("binary starts");
        {
            let mut input = child.stdin.take().expect("stdin");
            let _ = input.write_all(stdin);
        }
        child.wait_with_output().expect("binary exits")
    }

    fn json(&self, arguments: &[&str], stdin: &[u8]) -> serde_json::Value {
        let output = self.run(arguments, stdin);
        assert!(
            output.status.success(),
            "{arguments:?}: {} {}",
            stdout(&output),
            stderr(&output)
        );
        serde_json::from_slice(&output.stdout).expect("JSON answer")
    }

    /// `serve` exactly as the LaunchAgent runs it: production admission.
    fn spawn_serve(&self, label: &str) -> Child {
        let log = |name: &str| fs::File::create(self.path(&format!("{label}.{name}"))).expect("log");
        self.command(&["serve"])
            .stdin(Stdio::null())
            .stdout(log("out"))
            .stderr(log("err"))
            .spawn()
            .expect("serve starts")
    }

    /// The service's log (it logs to standard output) and its standard error.
    fn serve_log(&self, label: &str) -> String {
        let read = |name: &str| fs::read_to_string(self.path(&format!("{label}.{name}"))).unwrap_or_default();
        let out = read("out");
        let tail: Vec<&str> = out.lines().rev().take(60).collect();
        format!(
            "{}\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
            read("err")
        )
    }

    /// What the collection probe sees, for a failure message.
    fn collection_probe(&self) -> String {
        let status = self.run(&["status"], b"");
        let watermark = self.run(&["observation-watermark", "--car-id", &PRODUCT_ID.to_string()], b"");
        let observations = rusqlite::Connection::open_with_flags(
            self.data().join("hub.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .and_then(|connection| {
            let mut statement = connection.prepare(
                "SELECT 'raw', COUNT(*) FROM raw_observations UNION ALL SELECT 'current', COUNT(*) FROM current_observations",
            )?;
            let rows = statement
                .query_map([], |row| Ok(format!("{}={}", row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows.join(","))
        });
        format!(
            "status: {} {}\nwatermark: {} {}\nobservations: {observations:?}",
            stdout(&status),
            stderr(&status),
            stdout(&watermark),
            stderr(&watermark)
        )
    }

    fn await_window_status(&self, serve: &mut Child, label: &str) -> serde_json::Value {
        let started = Instant::now();
        loop {
            if let Some(status) = serve.try_wait().expect("serve state") {
                panic!("{label}: serve exited {status} before answering: {}", self.serve_log(label));
            }
            let output = self.run(&["window", "status"], b"");
            if output.status.success() {
                return serde_json::from_slice(&output.stdout).expect("window status JSON");
            }
            assert!(started.elapsed() < SERVE_START_LIMIT, "{label}: no answer: {}", stderr(&output));
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn stop(&self, mut serve: Child) {
        let _ = Command::new("/bin/kill")
            .args(["-TERM", &serve.id().to_string()])
            .status();
        let started = Instant::now();
        while serve.try_wait().expect("serve state").is_none() {
            if started.elapsed() > SERVE_START_LIMIT {
                let _ = serve.kill();
                let _ = serve.wait();
                panic!("serve did not stop on SIGTERM");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Serve is refused by its own admission: it exits unsuccessfully.
    fn expect_serve_refused(&self, label: &str) -> String {
        let mut serve = self.spawn_serve(label);
        let started = Instant::now();
        let exited = loop {
            if let Some(status) = serve.try_wait().expect("serve state") {
                break status;
            }
            if started.elapsed() > SERVE_START_LIMIT {
                self.stop(serve);
                panic!("{label}: a Hub that must be refused kept serving");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(!exited.success(), "{label}: refused: {}", self.serve_log(label));
        self.serve_log(label)
    }

    /// Trusts the issuer and opens a one-hour window for this host, through
    /// the running service's operator channel.
    fn open_window(&self) {
        let host_id = teslatlas_hub::runtime::operator::host::this_host_id().expect("host id");
        let issuer = Issuer::new(
            &self.path("issuer"),
            &host_id,
            Arc::new(MemoryAnchor::new()),
            Arc::new(SystemClock::new()),
        );
        let key = self.path("issuer.key");
        if !key.exists() {
            let document = issuer.init(&key).expect("issuer");
            fs::write(self.path("trust.json"), document.to_bytes().expect("trust bytes")).expect("trust file");
            let trusted = self.json(&["window", "trust", self.path("trust.json").to_str().expect("path")], b"");
            assert_eq!(trusted["trusted"], true, "{trusted}");
        }
        let now = now_s();
        let grant = self.path("grant.json");
        let _ = fs::remove_file(&grant);
        issuer
            .issue(
                &key,
                &IssueRequest {
                    host_id,
                    starts_at: now - 1,
                    expires_at: now + 3_600,
                    approver: "G".to_owned(),
                    reason: "fresh install journey".to_owned(),
                    approval_ref: "M02-T21".to_owned(),
                },
                &grant,
            )
            .expect("grant");
        let opened = self.json(&["window", "open", "--grant", grant.to_str().expect("path")], b"");
        assert_eq!(opened["phase"], "open", "{opened}");
    }

    fn close_window(&self) {
        let closed = self.json(&["window", "close"], b"");
        assert_ne!(closed["phase"], "open", "{closed}");
    }

    /// The latest durable observation id of the discovered car, through the
    /// binary's read-only `observation-watermark` (0 when none is stored).
    fn observation_watermark(&self) -> i64 {
        let output = self.run(&["observation-watermark", "--car-id", &PRODUCT_ID.to_string()], b"");
        if !output.status.success() {
            return 0;
        }
        serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .ok()
            .and_then(|answer| answer["watermark"].as_i64())
            .unwrap_or(0)
    }

    /// The collector lease the read-only `status` reports.
    fn collector_lease(&self) -> Option<serde_json::Value> {
        let output = self.run(&["status"], b"");
        if !output.status.success() {
            return None;
        }
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
        status.get("collector").filter(|lease| !lease.is_null()).cloned()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// What the Mac app forwards after the Tesla page.
fn sign_in_material() -> Vec<u8> {
    format!(
        r#"{{"code":"journey-code-0001","codeVerifier":"{}","redirectUri":"tesla://auth/callback"}}"#,
        "v".repeat(64)
    )
    .into_bytes()
}

fn interval_line(config: &str) -> Option<String> {
    config
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("interval_seconds"))
        .map(str::to_owned)
}

#[test]
fn fresh_install_journey_sign_in_sets_the_interval_and_collects_only_inside_the_window() {
    let hub = Hub::new();
    let default_config = hub.config();

    // Start idle: the start check and the production serve admission.
    let check = hub.json(&["preflight"], b"");
    assert_eq!(check["state"], "idle", "{check}");
    let mut serve = hub.spawn_serve("idle");
    assert_eq!(hub.await_window_status(&mut serve, "idle")["phase"], "closed");
    std::thread::sleep(QUIET_PERIOD);
    assert_eq!(hub.tesla.traffic(), (0, 0), "idle sends nothing");

    // Open a window, then sign in: the setup stores the Hub's own login,
    // configures the car and sets the collection interval itself.
    hub.open_window();
    assert_eq!(hub.tesla.traffic(), (0, 0), "opening a window sends nothing");
    let report = hub.json(&["setup", "--all-vehicles"], &sign_in_material());
    assert_eq!(report["status"], "signed_in", "{report}");
    assert_eq!(report["vehicleDiscovery"], "configured", "{report}");
    assert_eq!(report["vehiclesConfigured"], 1, "{report}");
    assert_eq!(hub.tesla.count("POST /oauth2/v3/token"), 1, "{:?}", hub.tesla.requests());
    assert_eq!(
        interval_line(&hub.config()).as_deref(),
        Some(format!("interval_seconds = {SUPPORTED_INTERVAL_SECONDS}").as_str()),
        "the setup itself sets the collection interval (finding 1): {report}\n{}",
        hub.config()
    );
    assert_eq!(report["collectionIntervalSeconds"], 300, "{report}");

    // One collection inside the window: the lease and a stored observation
    // from the Tesla double, with no restart.
    let started = Instant::now();
    let lease = loop {
        let lease = hub.collector_lease();
        if let Some(lease) = lease.filter(|_| hub.observation_watermark() > 0) {
            break lease;
        }
        assert!(
            started.elapsed() < COLLECTION_LIMIT,
            "no collection inside the window: {:?}\n{}\n{}",
            hub.tesla.requests(),
            hub.collection_probe(),
            hub.serve_log("idle")
        );
        assert!(serve.try_wait().expect("serve state").is_none(), "{}", hub.serve_log("idle"));
        std::thread::sleep(Duration::from_millis(500));
    };
    assert!(lease["leaseUntilMs"].as_i64().is_some(), "{lease}");
    assert!(hub.tesla.count(&format!("GET /api/1/vehicles/{PRODUCT_ID}/vehicle_data")) >= 1);
    assert_eq!(hub.tesla.state.unexpected.load(Ordering::SeqCst), 0, "{:?}", hub.tesla.requests());

    // Closing the window stops collection: the lease is released and nothing
    // more reaches the Tesla double.
    hub.close_window();
    let started = Instant::now();
    while hub.collector_lease().is_some() {
        assert!(started.elapsed() < SERVE_START_LIMIT, "the collector kept its lease after the close");
        std::thread::sleep(Duration::from_millis(250));
    }
    let after_close = hub.tesla.traffic();
    std::thread::sleep(QUIET_PERIOD);
    assert_eq!(hub.tesla.traffic(), after_close, "nothing is collected outside a window");
    hub.stop(serve);

    // Stopped and started again (a reboot or KeepAlive): still configured,
    // through the full admission, and with no window nothing is sent.
    let check = hub.json(&["preflight"], b"");
    assert_eq!(check["status"], "ready", "{check}");
    assert!(check.get("state").is_none(), "configured is not idle: {check}");
    let mut serve = hub.spawn_serve("restart");
    assert_eq!(hub.await_window_status(&mut serve, "restart")["phase"], "closed");
    std::thread::sleep(QUIET_PERIOD);
    assert!(serve.try_wait().expect("serve state").is_none(), "{}", hub.serve_log("restart"));
    assert_eq!(hub.tesla.traffic(), after_close, "a restart with no window sends nothing");
    assert!(hub.collector_lease().is_none());
    hub.stop(serve);

    // Only the interval was added: no other configuration write.
    let mut expected: toml::Table = default_config.parse().expect("default configuration");
    expected.insert(
        "collector".to_owned(),
        toml::Value::Table(toml::Table::from_iter([(
            "interval_seconds".to_owned(),
            toml::Value::Integer(SUPPORTED_INTERVAL_SECONDS.parse().expect("interval")),
        )])),
    );
    let written = hub.config();
    assert!(written.starts_with(&default_config), "{written}");
    assert_eq!(written.parse::<toml::Table>().expect("written configuration"), expected, "{written}");
    assert_eq!(hub.proxied.load(Ordering::SeqCst), 0, "nothing left the Mac");
}

/// Finding 2: a Hub that was set up (the Hub's own login stored, collection
/// still off because the account listed no car) and then loses its catalogue
/// refuses to start; it never comes up as new or idle and creates nothing.
#[test]
fn fresh_install_journey_lost_catalogue_is_refused() {
    let hub = Hub::new();
    hub.tesla.state.no_cars.store(true, Ordering::SeqCst);
    let mut serve = hub.spawn_serve("idle");
    assert_eq!(hub.await_window_status(&mut serve, "idle")["phase"], "closed");
    hub.open_window();
    let report = hub.json(&["setup", "--all-vehicles"], &sign_in_material());
    assert_eq!(report["status"], "signed_in", "{report}");
    assert_eq!(report["vehicleDiscovery"], "failed", "{report}");
    assert!(interval_line(&hub.config()).is_none(), "collection stays off: {}", hub.config());
    hub.close_window();
    hub.stop(serve);
    let marker = fs::read(hub.data().join("hub-setup.json")).expect("the setup marker");

    let catalogue = hub.data().join("hub.sqlite");
    let remove_journal = || {
        for name in ["hub.sqlite-wal", "hub.sqlite-shm"] {
            let _ = fs::remove_file(hub.data().join(name));
        }
    };

    // Truncated to an empty database.
    remove_journal();
    fs::OpenOptions::new().write(true).open(&catalogue).expect("catalogue").set_len(0).expect("truncate");
    let check = hub.run(&["preflight"], b"");
    assert!(!check.status.success(), "start check passes: {}", stdout(&check));
    assert!(stderr(&check).contains("set up before"), "{}", stderr(&check));
    let log = hub.expect_serve_refused("truncated");
    assert!(log.contains("set up before"), "{log}");
    assert_eq!(fs::metadata(&catalogue).expect("catalogue").len(), 0, "nothing initialised");

    // Missing.
    fs::remove_file(&catalogue).expect("remove");
    remove_journal();
    let check = hub.run(&["preflight"], b"");
    assert!(!check.status.success(), "start check passes: {}", stdout(&check));
    assert!(stderr(&check).contains("set up before"), "{}", stderr(&check));
    hub.expect_serve_refused("missing");
    assert!(!catalogue.exists(), "nothing created");
    assert_eq!(fs::read(hub.data().join("hub-setup.json")).expect("marker"), marker, "identity kept");
    assert_eq!(hub.proxied.load(Ordering::SeqCst), 0, "nothing left the Mac");
}

/// DEC-120 (a): a Hub signed in before the marker existed (the Hub's own
/// login in its catalogue, its credential keyring beside it, collection still
/// off, no marker). Its catalogue removed, truncated to an empty database or
/// replaced by an empty catalogue is refused by the start check and the
/// serve admission, which create nothing and leave the catalogue identity as
/// it is. The same once the marker written from its intact catalogue is lost
/// with the catalogue: the surviving keyring still tells it was set up.
#[test]
fn fresh_install_journey_pre_marker_catalogue_loss_is_refused() {
    use teslatlas_hub::db::HubStore;
    let identity = |data: &Path| {
        HubStore::open_read_only(data)
            .and_then(|store| store.installation_id())
            .expect("catalogue identity")
    };
    for marker_lost in [false, true] {
        for loss in ["removed", "truncated", "replaced"] {
            let label = format!("{loss}{}", if marker_lost { "-marker-lost" } else { "" });
            let hub = Hub::new();
            let data = hub.data();
            teslatlas_hub::auth::credential_sign_in::seed_owner_pair_for_test(
                &data,
                "pre-marker-access-0001",
                "pre-marker-refresh-0001",
                None,
            )
            .expect("the Hub's own login");
            let catalogue = data.join("hub.sqlite");
            let marker = data.join("hub-setup.json");
            assert!(data.join("secrets").join("credential-keys").exists(), "{label}: the keyring");
            assert!(!marker.exists(), "{label}: a Hub from before the marker");
            if marker_lost {
                // The intact catalogue gets its marker written from it.
                let installation = identity(&data);
                let check = hub.run(&["preflight"], b"");
                let recorded = fs::read(&marker).unwrap_or_else(|_| {
                    panic!("{label}: no marker from the intact catalogue: {} {}", stdout(&check), stderr(&check))
                });
                let recorded: serde_json::Value = serde_json::from_slice(&recorded).expect("marker JSON");
                assert_eq!(recorded["installationId"], installation.to_string(), "{label}");
                fs::remove_file(&marker).expect("the marker is lost");
            }
            for name in ["hub.sqlite-wal", "hub.sqlite-shm"] {
                let _ = fs::remove_file(data.join(name));
            }
            let replacement = match loss {
                "removed" => {
                    fs::remove_file(&catalogue).expect("remove");
                    None
                }
                "truncated" => {
                    fs::OpenOptions::new().write(true).open(&catalogue).expect("catalogue").set_len(0).expect("truncate");
                    None
                }
                _ => {
                    fs::remove_file(&catalogue).expect("remove");
                    drop(HubStore::initialize(&data).expect("an empty catalogue"));
                    Some(identity(&data))
                }
            };

            let check = hub.run(&["preflight"], b"");
            assert!(!check.status.success(), "{label}: start check passes: {}", stdout(&check));
            assert!(stderr(&check).contains("set up before"), "{label}: {}", stderr(&check));
            let log = hub.expect_serve_refused(&label);
            assert!(log.contains("set up before"), "{label}: {log}");
            match replacement {
                Some(replacement) => assert_eq!(identity(&data), replacement, "{label}: identity unchanged"),
                None if loss == "removed" => assert!(!catalogue.exists(), "{label}: nothing created"),
                None => assert_eq!(fs::metadata(&catalogue).expect("catalogue").len(), 0, "{label}: nothing initialised"),
            }
            assert!(!marker.exists(), "{label}: no marker for a lost catalogue");
            assert_eq!(hub.proxied.load(Ordering::SeqCst), 0, "{label}: nothing left the Mac");
        }
    }
}

/// DEC-116 condition 4: the build check. A plain `cargo build`, debug or
/// release (features do not vary by profile), resolves the Hub without the
/// test-only `journey-doubles` feature, which only the dev-dependency edge
/// (the crate's own test build) enables; a build without debug assertions
/// refuses the feature; and the configured Owner API base still accepts only
/// HTTPS, so the loopback double exists only as that feature's override.
#[test]
fn fresh_install_journey_release_configuration_has_no_doubles() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let features = |edges: &str| {
        let output = Command::new(&cargo)
            .current_dir(manifest)
            .args([
                "tree", "--offline", "--locked", "--package", "teslatlas-hub", "--edges", edges,
                "--target", "all", "--depth", "0", "--format", "{p} features=[{f}]",
            ])
            .output()
            .expect("cargo tree");
        assert!(output.status.success(), "cargo tree: {}", stderr(&output));
        stdout(&output)
    };
    let build = features("normal,build");
    assert!(build.contains("features=[default]"), "{build}");
    assert!(!build.contains("journey-doubles"), "a plain build has the doubles: {build}");
    let test_build = features("normal,build,dev");
    assert!(test_build.contains("journey-doubles"), "the test build lacks the doubles: {test_build}");

    let toml: toml::Table = fs::read_to_string(manifest.join("Cargo.toml"))
        .expect("manifest")
        .parse()
        .expect("manifest TOML");
    let default = toml["features"]["default"].as_array().expect("default features");
    assert!(default.is_empty(), "{default:?}");
    for table in ["dependencies", "build-dependencies"] {
        if let Some(dependencies) = toml.get(table).and_then(toml::Value::as_table) {
            assert!(!dependencies.contains_key("teslatlas-hub"), "{table} enables the doubles");
        }
    }
    let guard = fs::read_to_string(manifest.join("src/api/owner_api.rs")).expect("owner API source");
    assert!(guard.contains(
        "#[cfg(all(feature = \"journey-doubles\", not(debug_assertions)))]\ncompile_error!"
    ));
    let build_script = fs::read_to_string(manifest.join("build.rs")).expect("build script source");
    assert!(
        build_script.contains(
            "if std::env::var_os(\"CARGO_FEATURE_JOURNEY_DOUBLES\").is_some()\n        \
             && std::env::var(\"PROFILE\").as_deref() == Ok(\"release\")\n    {\n        panic!("
        ),
        "build.rs no longer refuses the doubles in a release-profile build"
    );

    use teslatlas_hub::owner_api::OwnerApiBase;
    for base in ["http://127.0.0.1:9/", "http://localhost:9/", "http://owner-api.example/"] {
        assert!(OwnerApiBase::parse(base).is_err(), "{base} is accepted as a configured base");
    }
    assert!(OwnerApiBase::parse("https://owner-api.teslamotors.com/").is_ok());
}
