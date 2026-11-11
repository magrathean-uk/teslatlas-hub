// SPDX-License-Identifier: AGPL-3.0-only
#![cfg(unix)]

//! The built binary's admission boundary (M02-T10; design section 9, ruling
//! R5). Removed inputs are rejected by the parser; `setup`, the window verbs
//! and credential recovery refuse `service_not_running` before they read a
//! secret path or standard input and with no network connection; Fleet verbs
//! are refused `provider_parked`; import waits for the window to close; and the
//! service's window authority is what opens the process-wide egress gate.
//!
//! Secret paths sit inside a mode-000 directory, so a command that tried to
//! read one would fail with an I/O error instead of its policy code. A
//! counting loopback listener is named as every proxy the child could use; it
//! must see no connection.

use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_teslatlas-hub");

/// A temporary directory short enough for a Unix socket path.
fn short_tempdir() -> TempDir {
    let base = std::env::temp_dir();
    if base.as_os_str().len() <= 50 {
        TempDir::new_in(base).expect("temp dir")
    } else {
        TempDir::new_in("/tmp").expect("temp dir")
    }
}

/// Counts TCP connections; it never answers.
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
            for stream in listener.incoming() {
                if stream.is_ok() {
                    counter.fetch_add(1, Ordering::SeqCst);
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
}

impl Hub {
    fn new() -> Self {
        let tmp = short_tempdir();
        for dir in ["home", "sealed"] {
            fs::create_dir(tmp.path().join(dir)).expect("dir");
        }
        fs::set_permissions(tmp.path().join("home"), fs::Permissions::from_mode(0o700))
            .expect("home mode");
        let config = format!(
            "data_dir = {:?}\nbind = '127.0.0.1:18443'\n[collector]\ninterval_seconds = 0\n",
            tmp.path().join("data")
        );
        fs::write(tmp.path().join("config.toml"), config).expect("config");
        fs::set_permissions(tmp.path().join("config.toml"), fs::Permissions::from_mode(0o600))
            .expect("config mode");
        Self {
            tmp,
            endpoint: CountingEndpoint::spawn(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn data(&self) -> PathBuf {
        self.path("data")
    }

    /// A path inside the sealed directory: unreadable while sealed.
    fn secret(&self, name: &str) -> String {
        self.path("sealed").join(name).to_str().expect("path").to_owned()
    }

    fn seal(&self, sealed: bool) {
        let mode = if sealed { 0o000 } else { 0o700 };
        fs::set_permissions(self.path("sealed"), fs::Permissions::from_mode(mode)).expect("seal");
    }

    fn run(&self, arguments: &[&str], stdin: &[u8]) -> Output {
        let config = self.path("config.toml");
        let mut child = Command::new(BIN)
            .arg("--config")
            .arg(&config)
            .args(arguments)
            .env_clear()
            .env("HOME", self.path("home"))
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("TMPDIR", self.tmp.path())
            .env("RUST_LOG", "off")
            .env("HTTP_PROXY", &self.endpoint.address)
            .env("HTTPS_PROXY", &self.endpoint.address)
            .env("ALL_PROXY", &self.endpoint.address)
            .env("http_proxy", &self.endpoint.address)
            .env("https_proxy", &self.endpoint.address)
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
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.seal(false);
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn assert_refused(output: &Output, code: &str, arguments: &[&str]) {
    let text = stderr(output);
    assert_eq!(output.status.code(), Some(1), "{arguments:?}: {text}");
    assert!(text.contains(&format!("{code}: ")), "{arguments:?}: {text}");
    for io in ["Permission denied", "No such file", "os error"] {
        assert!(!text.contains(io), "{arguments:?} read a path: {text}");
    }
}

const TOKENS: &[u8] = br#"{"accessToken":"access-0001","refreshToken":"refresh-0001"}"#;

#[test]
fn source_credential_disabled_removed_inputs_are_rejected_by_the_parser() {
    let hub = Hub::new();
    hub.seal(true);
    let key = hub.secret("key");
    let migrate = [
        "migrate", "--source", "postgresql://reader@127.0.0.1:9/sourcedb", "--car-id", "1",
        "--postgres-password-file", &key, "--accept-schema-evidence",
    ];
    let mut cases: Vec<Vec<&str>> = vec![
        vec!["setup", "--tokens-stdin"],
        vec!["setup", "--sign-in"],
        vec!["setup", "--access-token-file", &key, "--refresh-token-file", &key],
        vec!["setup-fleet", "--sign-in"],
        vec!["setup-fleet", "--all-vehicles"],
        vec![
            "source-check", "--source", "postgresql://reader@127.0.0.1:9/sourcedb", "--car-id",
            "1", "--postgres-password-file", &key, "--history-only",
        ],
    ];
    for removed in [
        "--source-token-key-file",
        "--encryption-key-file",
        "--access-token-file",
        "--refresh-token-file",
    ] {
        let mut case = migrate.to_vec();
        case.extend([removed, key.as_str()]);
        cases.push(case);
    }
    for flag in ["--replace-credentials", "--preserve-existing-credentials"] {
        let mut case = migrate.to_vec();
        case.extend(["--online-snapshot", flag]);
        cases.push(case);
    }
    for case in &cases {
        let output = hub.run(case, TOKENS);
        let text = stderr(&output);
        assert_eq!(output.status.code(), Some(2), "{case:?}: {text}");
        assert!(text.contains("unexpected argument"), "{case:?}: {text}");
    }
    assert!(!hub.data().exists(), "no Hub state was created");
    assert_eq!(hub.endpoint.connections(), 0);
}

#[test]
fn source_credential_disabled_service_verbs_refuse_before_any_secret_read() {
    let hub = Hub::new();
    hub.seal(true);
    let (grant, trust, key, out) = (
        hub.secret("grant.json"),
        hub.secret("trust.json"),
        hub.secret("recovery.key"),
        hub.secret("export.tthcr"),
    );
    let cases: Vec<Vec<&str>> = vec![
        vec!["setup", "--all-vehicles"],
        vec!["setup", "--vehicle-id", "7"],
        vec!["window", "status"],
        vec!["window", "close"],
        vec!["window", "trust", &trust],
        vec!["window", "open", "--grant", &grant],
        vec!["export-recovery-credentials", "--destination", &out, "--recovery-key-file", &key],
        vec!["restore-recovery-credentials", "--source", &out, "--recovery-key-file", &key],
    ];
    // No data directory, then a data directory with no service behind it.
    for prepare in [false, true] {
        if prepare {
            fs::create_dir(hub.data()).expect("data dir");
            fs::set_permissions(hub.data(), fs::Permissions::from_mode(0o700)).expect("mode");
        }
        for case in &cases {
            // Token-shaped standard input is never read: the refusal is
            // `service_not_running`, not `source_credential_disabled`.
            let output = hub.run(case, TOKENS);
            assert_refused(&output, "service_not_running", case);
        }
    }
    let created: Vec<_> = fs::read_dir(hub.data()).expect("data").collect();
    assert!(created.is_empty(), "nothing was created in the data directory");
    assert_eq!(hub.endpoint.connections(), 0, "no network connection");
}

#[test]
fn source_credential_disabled_fleet_verbs_are_parked_before_the_configuration_is_read() {
    let hub = Hub::new();
    fs::remove_file(hub.path("config.toml")).expect("no configuration");
    for case in [["setup-fleet"], ["configure-fleet-telemetry"]] {
        let output = hub.run(&case, TOKENS);
        assert_refused(&output, "provider_parked", &case);
    }
    assert_eq!(hub.endpoint.connections(), 0);
}

/// A stand-in service on the control channel: it records each request and
/// answers it.
struct FakeService {
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    _thread: JoinHandle<()>,
}

impl FakeService {
    fn bind(data: &Path) -> Self {
        let dir = data.join("operator");
        fs::create_dir_all(&dir).expect("channel dir");
        fs::set_permissions(data, fs::Permissions::from_mode(0o700)).expect("data mode");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("channel mode");
        let listener = UnixListener::bind(dir.join("control.sock")).expect("bind");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                let mut bytes = Vec::new();
                let _ = stream.read_to_end(&mut bytes);
                // The admission probe connects and closes without a request.
                let Ok(request) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                    continue;
                };
                recorded.lock().expect("requests").push(request);
                let _ = stream.write_all(br#"{"ok":true,"result":{"phase":"closed","forwarded":true}}"#);
            }
        });
        Self {
            requests,
            _thread: thread,
        }
    }

    fn requests(&self) -> Vec<serde_json::Value> {
        self.requests.lock().expect("requests").clone()
    }
}

#[test]
fn collection_window_forwarded_verbs_reach_the_running_service() {
    let hub = Hub::new();
    let service = FakeService::bind(&hub.data());
    let output = hub.run(&["window", "status"], b"");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let printed: serde_json::Value = serde_json::from_str(stdout(&output).trim()).expect("JSON");
    assert_eq!(printed, serde_json::json!({"phase": "closed", "forwarded": true}));

    let grant = hub.path("grant.json");
    fs::write(&grant, b"{\"body\":{}}").expect("grant");
    let output = hub.run(&["window", "open", "--grant", grant.to_str().expect("path")], b"");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    let material = format!(
        r#"{{"code":"code-0001","codeVerifier":"{}","redirectUri":"https://auth.tesla.com/void/callback"}}"#,
        "v".repeat(43)
    );
    let output = hub.run(&["setup", "--vehicle-id", "7"], material.as_bytes());
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    // Token-shaped input is refused as a source credential and never sent.
    let output = hub.run(&["setup", "--all-vehicles"], TOKENS);
    assert_refused(&output, "source_credential_disabled", &["setup"]);
    let output = hub.run(&["setup"], br#"{"code":"c"}"#);
    assert_refused(&output, "setup_input_invalid", &["setup"]);

    // The recovery key is never read by the command: only its path travels.
    hub.seal(true);
    let key = hub.secret("recovery.key");
    let out = hub.secret("export.tthcr");
    let output = hub.run(
        &["export-recovery-credentials", "--destination", &out, "--recovery-key-file", &key],
        b"",
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    hub.seal(false);

    let requests = service.requests();
    let ops: Vec<_> = requests.iter().map(|request| request["op"].clone()).collect();
    assert_eq!(
        ops,
        ["window_status", "window_open", "setup", "recovery_export"].map(serde_json::Value::from)
    );
    assert_eq!(requests[1]["grant"], "eyJib2R5Ijp7fX0=");
    assert_eq!(requests[2]["vehicle_id"], 7);
    assert_eq!(requests[2]["material"]["code"], "code-0001");
    let setup = requests[2].to_string().to_ascii_lowercase();
    assert!(!setup.contains("token"), "{setup}");
    assert_eq!(requests[3]["recovery_key_file"], key.as_str());
    assert_eq!(hub.endpoint.connections(), 0);
}

#[test]
fn collection_window_issuer_verbs_run_without_a_service() {
    let hub = Hub::new();
    let output = hub.run(&["window", "host-id"], b"");
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let report: serde_json::Value = serde_json::from_str(stdout(&output).trim()).expect("JSON");
    let host = report["hostId"].as_str().expect("host id");
    assert!(host.starts_with("host-") && host.len() == 37, "{host}");
    // Issuance with an unreadable key and no ledger is refused with a stable
    // issuer code, not an I/O error.
    hub.seal(true);
    let output = hub.run(
        &[
            "window", "issue", "--key-file", &hub.secret("issuer.key"), "--host-id", host,
            "--starts-at", "2030-01-01T00:00:00Z", "--expires-at", "2030-01-01T01:00:00Z",
            "--approver", "G", "--reason", "test", "--approval-ref", "approval-1",
            "--out", &hub.secret("grant.json"),
        ],
        b"",
    );
    let text = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("issuer_"), "{text}");
    hub.seal(false);
    let output = hub.run(
        &[
            "window", "issue", "--key-file", "k", "--host-id", host, "--starts-at", "soon",
            "--expires-at", "later", "--approver", "G", "--reason", "r", "--approval-ref", "a",
            "--out", "g",
        ],
        b"",
    );
    assert_refused(&output, "time_invalid", &["window", "issue"]);
    assert_eq!(hub.endpoint.connections(), 0);
}

mod authority {
    //! In this process of its own, the service's window authority drives the
    //! process-wide egress gate that every Tesla send of the library asks.

    use std::sync::Arc;

    use teslatlas_hub::{
        collection::window::{
            CloseReason, EgressGate, IssueRequest, Issuer, ManualProbe, MemoryAnchor, NoHighWater,
            Phase, PermitKind, Provider, SystemClock, WindowClock, WindowConfig, WindowController,
        },
        runtime::operator::authority::service_window_deps,
    };

    use super::*;

    const HOST: &str = "host-cli-admission";

    /// A trusted controller on `data` with the service's dependencies, and a
    /// grant for now plus `length` seconds.
    fn open_window(tmp: &Path, data: &Path, length: i64, gate_wide: bool) -> (WindowController, Phase) {
        let clock = Arc::new(SystemClock::new());
        let anchor = Arc::new(MemoryAnchor::new());
        let issuer = Issuer::new(&tmp.join("issuer"), "host-cli-issuer", anchor.clone(), clock.clone());
        let key = tmp.join("issuer.key");
        let trust = issuer.init(&key).expect("issuer");
        let mut deps = service_window_deps(anchor, Arc::new(ManualProbe::new(0.1)), Arc::new(NoHighWater));
        if !gate_wide {
            deps.gate = EgressGate::new(deps.clock.clone());
        }
        let (controller, _) = WindowController::start(
            WindowConfig {
                dir: data.join("window"),
                claim_path: tmp.join("claim/host-claim.lock"),
                host_id: HOST.to_owned(),
            },
            deps,
        );
        controller.trust(&trust).expect("trusted");
        let now = clock.wall();
        let out = tmp.join("grant.json");
        issuer
            .issue(
                &key,
                &IssueRequest {
                    host_id: HOST.to_owned(),
                    starts_at: now,
                    expires_at: now + length,
                    approver: "G".to_owned(),
                    reason: "cli admission test".to_owned(),
                    approval_ref: "approval-cli-admission".to_owned(),
                },
                &out,
            )
            .expect("grant");
        let phase = controller.open(&fs::read(out).expect("grant bytes")).expect("open").phase;
        (controller, phase)
    }

    #[test]
    fn collection_window_service_authority_opens_and_closes_the_process_wide_gate() {
        let tmp = short_tempdir();
        let gate = EgressGate::process_wide();
        assert!(!gate.is_open(), "closed until a window opens");
        assert_eq!(gate.permit(Provider::Legacy, PermitKind::Request).unwrap_err().code(), "window_closed");
        let (controller, phase) = open_window(tmp.path(), &tmp.path().join("data"), 600, true);
        assert_eq!(phase, Phase::Open);
        assert!(gate.is_open(), "the service's controller opened the process-wide gate");
        let permit = gate.permit(Provider::Legacy, PermitKind::Request).expect("permitted inside the window");
        assert!(permit.time_left().as_secs() <= 600);
        assert_eq!(gate.permit(Provider::Fleet, PermitKind::Request).unwrap_err().code(), "provider_parked");
        controller.close(CloseReason::OperatorClose);
        assert!(permit.is_cancelled(), "the close cancels permits already granted");
        assert_eq!(gate.permit(Provider::Legacy, PermitKind::Request).unwrap_err().code(), "window_closed");
        assert_eq!(controller.status().phase, Phase::Closing);
        controller.finish_close().expect("handback");
        assert!(!gate.is_open());
    }

    #[test]
    fn collection_window_import_and_restore_wait_for_the_window_to_close() {
        let hub = Hub::new();
        fs::create_dir(hub.data()).expect("data dir");
        fs::set_permissions(hub.data(), fs::Permissions::from_mode(0o700)).expect("data mode");
        let (controller, phase) = open_window(hub.path("home").as_path(), &hub.data(), 600, false);
        assert_eq!(phase, Phase::Open);
        hub.seal(true);
        let password = hub.secret("password");
        let migrate = [
            "migrate", "--source", "postgresql://reader@127.0.0.1:9/sourcedb", "--car-id", "1",
            "--postgres-password-file", &password, "--accept-schema-evidence", "--history-only",
            "--online-snapshot",
        ];
        let output = hub.run(&migrate, b"");
        assert_refused(&output, "window_active", &migrate);
        let restore = ["restore-data", "--source", &hub.secret("backup"), "--destination", &hub.secret("restored")];
        let output = hub.run(&restore, b"");
        assert_refused(&output, "window_active", &restore);
        controller.close(CloseReason::OperatorClose);
        let output = hub.run(&migrate, b"");
        assert_refused(&output, "window_active", &migrate);
        controller.finish_close().expect("handback");
        hub.seal(false);
        assert_eq!(hub.endpoint.connections(), 0);
    }
}
