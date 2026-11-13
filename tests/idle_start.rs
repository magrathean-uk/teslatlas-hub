// SPDX-License-Identifier: AGPL-3.0-only

//! M02-T21 (DEC-115): the real service binary on a fresh data folder.
//!
//! With the configuration the Mac app writes on a fresh Mac, the binary's
//! start check and its production serve admission let it start idle. Idle
//! answers on the operator channel and the loopback listener, opens no
//! network connection, refuses a sign-in while no window is Open, and starts
//! idle again after a restart. Once signed in and configured through the
//! existing test doubles (the Hub's own pair, an imported car and a cadence)
//! the same binary passes the full admission; a configured but broken Hub is
//! still refused.
//!
//! The binary's window authority keeps its anchor in the login Keychain and
//! checks the clock against an SNTP server, so no test opens a window in the
//! binary. The binary's test
//! `idle_start_collection_begins_only_after_sign_in_inside_the_window` opens
//! one in process for the service's own collector slot.
#![cfg(target_os = "macos")]

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_teslatlas-hub");
const SERVE_START_LIMIT: Duration = Duration::from_secs(90);
const QUIET_PERIOD: Duration = Duration::from_secs(3);

/// A temporary directory short enough for the operator socket path.
fn short_tempdir() -> TempDir {
    let base = std::env::temp_dir();
    if base.as_os_str().len() <= 50 {
        TempDir::new_in(base).expect("temp dir")
    } else {
        TempDir::new_in("/tmp").expect("temp dir")
    }
}

/// Every proxy variable points here, so any connection the binary tries is
/// counted on loopback instead of leaving the Mac. It never answers.
struct CountingEndpoint {
    address: String,
    accepted: Arc<AtomicUsize>,
}

impl CountingEndpoint {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = format!("http://{}", listener.local_addr().expect("address"));
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                if let Ok(stream) = stream {
                    counter.fetch_add(1, Ordering::SeqCst);
                    held.push(stream);
                }
            }
        });
        Self { address, accepted }
    }

    fn connections(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

struct Hub {
    tmp: TempDir,
    endpoint: CountingEndpoint,
    port: u16,
}

impl Hub {
    fn new() -> Self {
        let tmp = short_tempdir();
        for dir in ["home", "data"] {
            fs::create_dir(tmp.path().join(dir)).expect("dir");
            fs::set_permissions(tmp.path().join(dir), fs::Permissions::from_mode(0o700))
                .expect("private dir");
        }
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("free port");
            listener.local_addr().expect("address").port()
        };
        let hub = Self {
            tmp,
            endpoint: CountingEndpoint::spawn(),
            port,
        };
        hub.write_config("");
        hub
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn data(&self) -> PathBuf {
        self.path("data")
    }

    /// The configuration `ensureConfig()` writes on a fresh Mac, with an
    /// optional collector table and a free loopback port.
    fn write_config(&self, collector: &str) {
        let content = format!(
            "data_dir = {:?}\nbind = \"127.0.0.1:{}\"\n{collector}\n[geocoder]\nenabled = false\n\n[terrain]\nenabled = false\n",
            self.data(),
            self.port
        );
        let path = self.path("config.toml");
        fs::write(&path, content).expect("config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("config mode");
    }

    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .arg("--config")
            .arg(self.path("config.toml"))
            .args(arguments)
            .env_clear()
            .env("HOME", self.path("home"))
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("TMPDIR", self.tmp.path())
            .env("RUST_LOG", "info");
        for variable in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
            command.env(variable, &self.endpoint.address);
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

    /// `serve` exactly as the LaunchAgent runs it: production admission, no
    /// development opt-in.
    fn spawn_serve(&self, label: &str) -> Child {
        let log = |name: &str| fs::File::create(self.path(&format!("{label}.{name}"))).expect("log");
        self.command(&["serve"])
            .stdin(Stdio::null())
            .stdout(log("out"))
            .stderr(log("err"))
            .spawn()
            .expect("serve starts")
    }

    fn serve_log(&self, label: &str) -> String {
        fs::read_to_string(self.path(&format!("{label}.err"))).unwrap_or_default()
    }

    /// Waits until the running service answers `window status` on its
    /// operator channel; a service that exits first fails the test.
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

    /// Stops `serve` with SIGTERM, as launchd does, and waits for it.
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

    /// One plain HTTP request to the loopback listener.
    fn http_status_line(&self, path: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("loopback listener");
        stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
        // The loopback listener admits only its own exact host and port.
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            self.port
        )
        .expect("request");
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response.lines().next().unwrap_or_default().to_owned()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The start check the Mac app runs before its Start.
fn start_check(hub: &Hub) -> Output {
    hub.run(&["preflight"], b"")
}

/// A sign-in the Mac app forwards after the Tesla page; with no Open window
/// the service refuses it before any connection.
fn sign_in_material() -> Vec<u8> {
    format!(
        r#"{{"code":"idle-start-code-0001","codeVerifier":"{}","redirectUri":"tesla://auth/callback"}}"#,
        "v".repeat(64)
    )
    .into_bytes()
}

/// The production serve admission refuses: `serve` exits unsuccessfully on
/// its own, having opened no connection.
fn expect_serve_refused(hub: &Hub, label: &str) {
    let mut serve = hub.spawn_serve(label);
    let started = Instant::now();
    let exited = loop {
        if let Some(status) = serve.try_wait().expect("serve state") {
            break status;
        }
        if started.elapsed() > SERVE_START_LIMIT {
            hub.stop(serve);
            panic!("{label}: a Hub that must be refused kept serving");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(!exited.success(), "{label}: refused: {}", hub.serve_log(label));
    assert_eq!(hub.endpoint.connections(), 0, "{label}: no connection");
}

/// The sign-in's stored result and the configuration that follows it,
/// through the existing doubles: the Hub's own pair, one imported car and a
/// collector cadence. The service is stopped, as it is for a restart.
fn sign_in_and_configure(hub: &Hub) {
    use teslatlas_hub::{
        db::HubStore,
        hub_pack::ProjectionCarSettings,
        legacy_owner_credentials::load_or_create_cursor_key,
        source_db_import::{SourceDbImportRequest, SourceDbImportScope, publish_history},
        source_db_projection::{SourceDbCar, SourceDbHistory},
    };
    let data = hub.data();
    teslatlas_hub::auth::credential_sign_in::seed_owner_pair_for_test(
        &data,
        "idle-start-access-0001",
        "idle-start-refresh-0001",
        None,
    )
    .expect("the Hub's own pair");
    assert!(
        teslatlas_hub::auth::credential_sign_in::record_provider_admission_for_test(&data)
            .expect("provider admission record")
    );
    let store = HubStore::initialize(&data).expect("store");
    let cursor_key = load_or_create_cursor_key(&data).expect("cursor key");
    let history = SourceDbHistory {
        cars: vec![SourceDbCar {
            id: 1,
            eid: 5_300_001,
            vid: Some(5_400_001),
            vin: Some("5YJ3E1EA7KF530001".to_owned()),
            name: Some("Idle start fixture".to_owned()),
            model: Some("3".to_owned()),
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            efficiency_wh_per_km: None,
            settings: ProjectionCarSettings {
                enabled: true,
                use_streaming_api: false,
                ..Default::default()
            },
        }],
        drives: Vec::new(),
        positions: Vec::new(),
        charging_processes: Vec::new(),
        charges: Vec::new(),
        addresses: Vec::new(),
        geofences: Vec::new(),
        states: Vec::new(),
        updates: Vec::new(),
    };
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("milliseconds");
    publish_history(
        &store,
        &cursor_key,
        &SourceDbImportRequest {
            new_import_source: false,
            source_key: "idle-start-fixture".to_owned(),
            scope: SourceDbImportScope::Selected(1),
            imported_at_ms: now_ms - 60_000,
        },
        &history,
    )
    .expect("configured car");
    drop(store);
    hub.write_config("\n[collector]\ninterval_seconds = 30\n");
}

#[test]
fn idle_start_real_binary_starts_idle_and_is_configured_only_by_sign_in() {
    let hub = Hub::new();

    // Start check: never configured is ready idle and creates no store.
    let output = start_check(&hub);
    assert!(output.status.success(), "start check: {}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("start check JSON");
    assert_eq!(report["status"], "ready");
    assert_eq!(report["state"], "idle");
    assert_eq!(report["collectorEnabled"], false);
    assert!(!hub.data().join("hub.sqlite").exists(), "the start check creates nothing");

    // Serve admission: the real binary starts idle and answers on its
    // operator channel and its loopback listener.
    let mut serve = hub.spawn_serve("idle");
    let status = hub.await_window_status(&mut serve, "idle");
    assert_eq!(status["phase"], "closed", "{status}");
    let health = hub.http_status_line("/healthz");
    assert!(health.contains(" 200 "), "loopback health: {health}");

    // A sign-in with no Open window is refused before any connection.
    let refused = hub.run(&["setup", "--all-vehicles"], &sign_in_material());
    assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
    assert!(stderr(&refused).contains("window_closed"), "{}", stderr(&refused));
    assert!(stdout(&refused).is_empty());

    std::thread::sleep(QUIET_PERIOD);
    assert!(serve.try_wait().expect("serve state").is_none(), "idle keeps running: {}", hub.serve_log("idle"));
    assert_eq!(hub.endpoint.connections(), 0, "idle sends nothing");
    hub.stop(serve);

    // The store serve created is still never configured: a restart (login,
    // reboot, KeepAlive) starts idle again.
    assert!(hub.data().join("hub.sqlite").exists(), "serve created its store");
    let output = start_check(&hub);
    assert!(output.status.success(), "restart start check: {}", stderr(&output));
    assert!(stdout(&output).contains("\"idle\""), "{}", stdout(&output));
    let mut serve = hub.spawn_serve("restart");
    assert_eq!(hub.await_window_status(&mut serve, "restart")["phase"], "closed");
    hub.stop(serve);
    assert_eq!(hub.endpoint.connections(), 0, "the restart sends nothing");

    // A cadence with nothing configured is configured but broken: refused
    // by the start check and by the serve admission, as before.
    hub.write_config("\n[collector]\ninterval_seconds = 30\n");
    let output = start_check(&hub);
    assert!(!output.status.success(), "broken start check passes: {}", stdout(&output));
    expect_serve_refused(&hub, "broken");

    // Signed in and configured through the doubles: the full admission
    // passes and the service starts configured. With no Open window its
    // collector still sends nothing.
    sign_in_and_configure(&hub);
    let output = start_check(&hub);
    assert!(output.status.success(), "configured start check: {}", stderr(&output));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("start check JSON");
    assert_eq!(report["status"], "ready");
    assert!(report.get("state").is_none(), "configured is not idle: {report}");
    let mut serve = hub.spawn_serve("configured");
    assert_eq!(hub.await_window_status(&mut serve, "configured")["phase"], "closed");
    std::thread::sleep(QUIET_PERIOD);
    assert!(serve.try_wait().expect("serve state").is_none(), "configured keeps running: {}", hub.serve_log("configured"));
    assert_eq!(hub.endpoint.connections(), 0, "no window, no collection");
    hub.stop(serve);
}

/// A configured but broken store is refused even with the collector off: a
/// stored car without a credential is not idle.
#[test]
fn idle_start_real_binary_refuses_a_car_with_the_collector_off() {
    use teslatlas_hub::{
        db::HubStore,
        hub_pack::ProjectionCarSettings,
        source_db_import::{SourceDbImportRequest, SourceDbImportScope, publish_history},
        source_db_projection::{SourceDbCar, SourceDbHistory},
    };
    let hub = Hub::new();
    let store = HubStore::initialize(&hub.data()).expect("store");
    publish_history(
        &store,
        &teslatlas_hub::protocol::CursorKey::from_bytes([0x5A; 32]),
        &SourceDbImportRequest {
            new_import_source: false,
            source_key: "idle-start-broken".to_owned(),
            scope: SourceDbImportScope::Selected(1),
            imported_at_ms: 1_000,
        },
        &SourceDbHistory {
            cars: vec![SourceDbCar {
                id: 1,
                eid: 5_300_002,
                vid: Some(5_400_002),
                vin: Some("5YJ3E1EA7KF530002".to_owned()),
                name: Some("Idle start broken".to_owned()),
                model: Some("3".to_owned()),
                trim_badging: None,
                marketing_name: None,
                exterior_color: None,
                wheel_type: None,
                spoiler_type: None,
                efficiency_wh_per_km: None,
                settings: ProjectionCarSettings::default(),
            }],
            drives: Vec::new(),
            positions: Vec::new(),
            charging_processes: Vec::new(),
            charges: Vec::new(),
            addresses: Vec::new(),
            geofences: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
        },
    )
    .expect("car");
    drop(store);
    let output = start_check(&hub);
    assert!(!output.status.success(), "start check passes: {}", stdout(&output));
    expect_serve_refused(&hub, "car-only");
}

