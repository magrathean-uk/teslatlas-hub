// SPDX-License-Identifier: AGPL-3.0-only
#![cfg(unix)]

//! The built binary's admission boundary (M02-T10; design section 9, ruling
//! R5). Removed inputs are rejected by the parser; `setup`, the window verbs
//! and credential recovery refuse `service_not_running` before they read a
//! secret path or standard input and with no network connection; Fleet verbs
//! are refused `provider_parked`; import waits for the window to close; and the
//! service's window authority is what opens the process-wide egress gate. A
//! listener of another program on the socket is not the service and receives
//! nothing.
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

const BIN: &str = env!("CARGO_BIN_EXE_tesatlas-hub");

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
        self.run_with(arguments, stdin, &[])
    }

    fn run_with(&self, arguments: &[&str], stdin: &[u8], envs: &[(&str, &Path)]) -> Output {
        let config = self.path("config.toml");
        let mut child = Command::new(BIN)
            .arg("--config")
            .arg(&config)
            .args(arguments)
            .env_clear()
            .envs(envs.iter().copied())
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

/// An impostor on the control channel: another program of the same user
/// bound to the socket while the service is stopped. It records every byte
/// it receives and answers as if it were the service.
struct Impostor {
    received: Arc<Mutex<Vec<Vec<u8>>>>,
    _thread: JoinHandle<()>,
}

impl Impostor {
    fn bind(data: &Path) -> Self {
        let dir = data.join("operator");
        fs::create_dir_all(&dir).expect("channel dir");
        fs::set_permissions(data, fs::Permissions::from_mode(0o700)).expect("data mode");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("channel mode");
        let listener = UnixListener::bind(dir.join("control.sock")).expect("bind");
        let received = Arc::new(Mutex::new(Vec::new()));
        let recorded = received.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                let mut bytes = Vec::new();
                let _ = stream.read_to_end(&mut bytes);
                recorded.lock().expect("received").push(bytes);
                let _ = stream.write_all(br#"{"ok":true,"result":{"phase":"open"}}"#);
            }
        });
        Self {
            received,
            _thread: thread,
        }
    }

    fn bytes_received(&self) -> usize {
        self.received.lock().expect("received").iter().map(Vec::len).sum()
    }
}

/// Finding 7: with the service stopped, a process of the same user bound to
/// the socket is not the admitted service. Every forwarded verb is refused
/// `service_unauthenticated` before it reads its input, and the impostor
/// receives no byte: no code material, grant, trust document or path.
#[test]
fn collection_window_an_impostor_on_the_socket_receives_nothing() {
    let hub = Hub::new();
    let impostor = Impostor::bind(&hub.data());
    let grant = hub.path("grant.json");
    fs::write(&grant, b"{\"body\":{}}").expect("grant");
    let trust = hub.path("trust.json");
    fs::write(&trust, b"{}").expect("trust");
    let material = format!(
        r#"{{"code":"code-0001","codeVerifier":"{}","redirectUri":"https://auth.tesla.com/void/callback"}}"#,
        "v".repeat(43)
    );
    let (grant, trust) = (grant.to_str().expect("path"), trust.to_str().expect("path"));
    let key = hub.path("recovery.key");
    let key = key.to_str().expect("path");
    let cases: Vec<(Vec<&str>, &[u8])> = vec![
        (vec!["setup", "--vehicle-id", "7"], material.as_bytes()),
        // Token-shaped input would be refused as a source credential if it
        // were read; it is not.
        (vec!["setup", "--all-vehicles"], TOKENS),
        (vec!["window", "status"], b""),
        (vec!["window", "open", "--grant", grant], b""),
        (vec!["window", "trust", trust], b""),
        (vec!["window", "close"], b""),
        (vec!["export-recovery-credentials", "--destination", "/tmp/x", "--recovery-key-file", key], b""),
        (vec!["restore-recovery-credentials", "--source", "/tmp/x", "--recovery-key-file", key], b""),
    ];
    for (case, input) in &cases {
        let output = hub.run(case, input);
        assert_refused(&output, "service_unauthenticated", case);
        assert!(stdout(&output).is_empty(), "{case:?}: nothing printed from the impostor");
    }
    assert_eq!(impostor.bytes_received(), 0, "the impostor received nothing");
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

    use tesatlas_hub::{
        collection::window::{
            CloseReason, EgressGate, HostClaim, IssueRequest, Issuer, ManualProbe, MemoryAnchor, NoHighWater,
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
                // The claim of the Hub user's home, as the service holds it.
                claim_path: HostClaim::default_path(tmp),
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
        let fleet = gate.permit(Provider::Fleet, PermitKind::Request).expect("Fleet rides the same window (M09-T08)");
        controller.close(CloseReason::OperatorClose);
        assert!(permit.is_cancelled(), "the close cancels permits already granted");
        assert!(fleet.is_cancelled(), "the close cancels a Fleet permit as well");
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
        // The source-token take-over is an import too (M09-T04): refused
        // before its standard input is read or the source is contacted.
        let takeover = [
            "take-over-source-token", "--source", "postgresql://reader@127.0.0.1:9/sourcedb",
            "--accept-source-stopped",
        ];
        let output = hub.run(&takeover, TOKENS);
        assert_refused(&output, "window_active", &takeover);
        let restore = ["restore-data", "--source", &hub.secret("backup"), "--destination", &hub.secret("restored")];
        let output = hub.run(&restore, b"");
        assert_refused(&output, "window_active", &restore);
        // Finding 8: with no configuration the restore is still bound to the
        // host's collection authority.
        fs::rename(hub.path("config.toml"), hub.path("moved.toml")).expect("configuration away");
        let output = hub.run(&restore, b"");
        assert_refused(&output, "window_active", &restore);
        fs::rename(hub.path("moved.toml"), hub.path("config.toml")).expect("configuration back");
        controller.close(CloseReason::OperatorClose);
        let output = hub.run(&migrate, b"");
        assert_refused(&output, "window_active", &migrate);
        controller.finish_close().expect("handback");
        hub.seal(false);
        assert_eq!(hub.endpoint.connections(), 0);
    }
}

/// M02-T11 acceptance 3: `purge-refused-credentials`, listing or confirmed,
/// goes only through the running service. With no service it is refused
/// `service_not_running` having created nothing; an impostor on the socket
/// receives nothing; no network connection is made.
#[test]
fn refused_credential_purge_goes_through_the_service_and_makes_no_connection() {
    let cases: [&[&str]; 2] = [
        &["purge-refused-credentials"],
        &["purge-refused-credentials", "--confirm"],
    ];
    let hub = Hub::new();
    for prepare in [false, true] {
        if prepare {
            fs::create_dir(hub.data()).expect("data dir");
            fs::set_permissions(hub.data(), fs::Permissions::from_mode(0o700)).expect("mode");
        }
        for case in &cases {
            let output = hub.run(case, b"");
            assert_refused(&output, "service_not_running", case);
            assert!(stdout(&output).is_empty(), "{case:?}: nothing listed or removed");
        }
    }
    let created: Vec<_> = fs::read_dir(hub.data()).expect("data").collect();
    assert!(created.is_empty(), "nothing was created in the data directory");
    assert_eq!(hub.endpoint.connections(), 0, "no network connection");

    let hub = Hub::new();
    let impostor = Impostor::bind(&hub.data());
    for case in &cases {
        let output = hub.run(case, b"");
        assert_refused(&output, "service_unauthenticated", case);
        assert!(stdout(&output).is_empty(), "{case:?}: nothing printed from the impostor");
    }
    assert_eq!(impostor.bytes_received(), 0, "the impostor received nothing");
    assert_eq!(hub.endpoint.connections(), 0, "no network connection");
}

/// M09-T04 (DEC-172): `take-over-source-token`.
mod source_takeover {
    use super::*;

    const DOUBLE_ENV: &str = "TESATLAS_HUB_SOURCE_TAKEOVER_DOUBLE";
    const PASSWORD: &str = "synthetic-source-password-0001";
    const KEY: &str = "synthetic-source-encryption-key-0001";
    const REFRESH: &str = "synthetic-source-refresh-0001";
    const REPORT: &str = r#"{"status":"taken_over","origin":"source_token","firstRefresh":"next_window","refreshProgress":"none_sent","sourceSessionsSeen":0,"sourceNeverMutated":true}"#;
    /// M09-T16 (B3): a run that finds the source row's login already in the slot.
    const ALREADY: &str = r#"{"status":"already_taken_over","origin":"source_token","firstRefresh":"next_window","refreshProgress":"none_sent","sourceSessionsSeen":0,"sourceNeverMutated":true}"#;

    fn input() -> Vec<u8> {
        format!(r#"{{"postgresPassword":"{PASSWORD}","encryptionKey":"{KEY}"}}"#).into_bytes()
    }

    /// A synthetic access token with Tesla's issuer, as the reader returns one.
    fn access() -> String {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let claims = serde_json::json!({"iss": "https://auth.tesla.com/oauth2/v3", "exp": 4_000_000_000_i64});
        format!(
            "{}.{}.c3ludGhldGlj",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        )
    }

    /// A Hub whose data directory holds an initialised store, and a counting
    /// listener standing in for the source database's host.
    fn hub_with_store() -> (Hub, CountingEndpoint) {
        let hub = Hub::new();
        fs::create_dir(hub.data()).expect("data dir");
        fs::set_permissions(hub.data(), fs::Permissions::from_mode(0o700)).expect("data mode");
        drop(tesatlas_hub::db::HubStore::initialize(hub.data()).expect("store"));
        (hub, CountingEndpoint::spawn())
    }

    fn source_url(source: &CountingEndpoint) -> String {
        let address = source.address.trim_start_matches("http://");
        format!("postgresql://reader@{address}/sourcedb")
    }

    /// Writes the reader double: a synthetic pair or a reader refusal.
    fn double(hub: &Hub, body: serde_json::Value) -> PathBuf {
        let path = hub.path("source-double.json");
        fs::write(&path, body.to_string()).expect("double");
        path
    }

    fn take_over(hub: &Hub, source: &str, stdin: &[u8], double: Option<&Path>) -> Output {
        let arguments = ["take-over-source-token", "--source", source, "--accept-source-stopped"];
        match double {
            Some(path) => hub.run_with(&arguments, stdin, &[(DOUBLE_ENV, path)]),
            None => hub.run(&arguments, stdin),
        }
    }

    fn assert_secret_free(output: &Output) {
        let text = format!("{}{}", stdout(output), stderr(output));
        for secret in [PASSWORD, KEY, REFRESH, access().as_str()] {
            assert!(!text.contains(secret), "secret material printed: {text}");
        }
    }

    #[test]
    fn source_takeover_parser_requires_the_acceptance_and_takes_no_token_flag() {
        let hub = Hub::new();
        hub.seal(true);
        let key = hub.secret("key");
        let source = "postgresql://reader@127.0.0.1:9/sourcedb";
        let required: Vec<Vec<&str>> = vec![
            vec!["take-over-source-token", "--source", source],
            vec!["take-over-source-token", "--accept-source-stopped"],
        ];
        for case in &required {
            let output = hub.run(case, &input());
            let text = stderr(&output);
            assert_eq!(output.status.code(), Some(2), "{case:?}: {text}");
            assert!(text.contains("required"), "{case:?}: {text}");
        }
        for removed in [
            "--encryption-key-file", "--access-token-file", "--refresh-token-file",
            "--postgres-password-file", "--source-token-key-file",
        ] {
            let case = ["take-over-source-token", "--source", source, "--accept-source-stopped", removed, &key];
            let output = hub.run(&case, &input());
            let text = stderr(&output);
            assert_eq!(output.status.code(), Some(2), "{case:?}: {text}");
            assert!(text.contains("unexpected argument"), "{case:?}: {text}");
        }
        assert!(!hub.data().exists(), "no Hub state was created");
        assert_eq!(hub.endpoint.connections(), 0);
    }

    /// Standard input carries exactly postgresPassword and encryptionKey: a
    /// key naming a token is refused `source_credential_disabled`, any other
    /// shape `setup_input_invalid`, before the source is contacted, the store
    /// is opened or a credential key exists.
    #[test]
    fn source_takeover_input_is_refused_before_any_connection() {
        let (hub, source) = hub_with_store();
        let url = source_url(&source);
        let pair = double(&hub, serde_json::json!({"access": access(), "refresh": REFRESH}));
        let tokens: [&[u8]; 4] = [
            TOKENS,
            br#"{"postgresPassword":"p","encryptionKey":"k","refreshToken":"r"}"#,
            br#"{"postgresPassword":"p","encryptionKey":"k","source_token":"t"}"#,
            br#"{"postgresPassword":"p","encryptionKey":"k","AccessTOKEN":"a"}"#,
        ];
        let invalid: [&[u8]; 9] = [
            b"",
            b"[]",
            br#"{"postgresPassword":"p"}"#,
            br#"{"postgresPassword":"p","encryptionKey":"k","extra":"x"}"#,
            br#"{"postgresPassword":"p","encryptionKey":"k","postgresPassword":"q"}"#,
            br#"{"postgresPassword":"p","encryptionKey":7}"#,
            br#"{"postgresPassword":"","encryptionKey":"k"}"#,
            br#"{"postgresPassword":"p","encryptionKey":""}"#,
            br#"{"postgresPassword":"p","encryptionKey":"k"} trailing"#,
        ];
        let oversized = format!(r#"{{"postgresPassword":"p","encryptionKey":"{}"}}"#, "k".repeat(9000));
        let cases = tokens.iter().map(|input| (*input, "source_credential_disabled"))
            .chain(invalid.iter().map(|input| (*input, "setup_input_invalid")))
            .chain([(oversized.as_bytes(), "setup_input_invalid")]);
        for (stdin, code) in cases {
            // Even with a reader double named, nothing reaches the reader.
            for double in [None, Some(pair.as_path())] {
                let output = take_over(&hub, &url, stdin, double);
                assert_refused(&output, code, &[&String::from_utf8_lossy(stdin)]);
                assert!(stdout(&output).is_empty());
            }
        }
        assert_eq!(source.connections(), 0, "the source was never contacted");
        assert!(!hub.data().join("secrets").exists(), "no credential key was created");
        assert_eq!(hub.endpoint.connections(), 0);
    }

    /// While the Hub service holds the data directory's lifetime lock the
    /// take-over is refused `service_running` before standard input is read.
    #[test]
    fn source_takeover_is_refused_while_the_service_runs() {
        let (hub, source) = hub_with_store();
        let lock = fs::OpenOptions::new()
            .read(true).write(true).create(true).truncate(false)
            .open(hub.data().join(".hub-instance.lock"))
            .expect("lock file");
        fs::set_permissions(hub.data().join(".hub-instance.lock"), fs::Permissions::from_mode(0o600)).expect("lock mode");
        lock.try_lock().expect("held as the service holds it");
        for stdin in [input(), TOKENS.to_vec()] {
            let output = take_over(&hub, &source_url(&source), &stdin, None);
            assert_refused(&output, "service_running", &["take-over-source-token"]);
        }
        drop(lock);
        assert_eq!(source.connections(), 0);
        assert_eq!(hub.endpoint.connections(), 0);
    }

    /// A completed take-over prints one JSON report and no secret; a second
    /// one is `slot_occupied`. The reader's and the vault's refusal codes
    /// reach the operator unchanged and store nothing.
    #[test]
    fn source_takeover_reports_once_and_passes_refusal_codes_through() {
        let (hub, source) = hub_with_store();
        let url = source_url(&source);
        for code in ["source_still_running", "source_token_key_mismatch", "source_token_invalid", "source_token_row_missing"] {
            let refusal = double(&hub, serde_json::json!({"refusal": code}));
            let output = take_over(&hub, &url, &input(), Some(&refusal));
            assert_refused(&output, code, &[code]);
            assert!(stdout(&output).is_empty(), "{code}: nothing reported");
            assert_secret_free(&output);
        }
        let pair = double(&hub, serde_json::json!({"access": access(), "refresh": REFRESH}));
        let output = take_over(&hub, &url, &input(), Some(&pair));
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert_eq!(stdout(&output), format!("{REPORT}\n"), "exactly one JSON report");
        assert_secret_free(&output);

        // The same login again (a lost report, a retry): exit 0, nothing written.
        let output = take_over(&hub, &url, &input(), Some(&pair));
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert_eq!(stdout(&output), format!("{ALREADY}\n"), "exactly one JSON report");
        assert_secret_free(&output);
        // A different login in the source row: the slot is occupied.
        let other = double(&hub, serde_json::json!({"access": access(), "refresh": "synthetic-source-refresh-0002"}));
        let output = take_over(&hub, &url, &input(), Some(&other));
        assert_refused(&output, "slot_occupied", &["second take-over of another login"]);
        assert!(stdout(&output).is_empty());
        assert_secret_free(&output);
        assert_eq!(source.connections(), 0, "the double stood in for the source");

        // An upgraded store whose legacy fence is opaque takes no supplied
        // login: `fresh_sign_in_required`, nothing stored.
        let (fenced, source) = hub_with_store();
        let connection = rusqlite::Connection::open(fenced.data().join("hub.sqlite")).expect("store");
        connection
            .execute("INSERT INTO credential_legacy_fences VALUES('00000000-0000-4000-8000-000000002000','opaque',0)", [])
            .expect("opaque fence");
        drop(connection);
        let pair = double(&fenced, serde_json::json!({"access": access(), "refresh": REFRESH}));
        let output = take_over(&fenced, &source_url(&source), &input(), Some(&pair));
        assert_refused(&output, "fresh_sign_in_required", &["fenced take-over"]);
        assert_secret_free(&output);
        let output = take_over(&fenced, &source_url(&source), &input(), Some(&pair));
        assert_refused(&output, "fresh_sign_in_required", &["nothing was stored"]);
        assert_eq!(source.connections(), 0, "the double stood in for the source");

        // A refresh token the Hub already spent is refused
        // `refresh_token_spent`, nothing stored.
        let (spent, source) = hub_with_store();
        let fingerprint = {
            use sha2::Digest as _;
            let mut hash = sha2::Sha256::new();
            hash.update(b"teslatlas-hub/refresh-fingerprint/v1"); // rename:keep
            hash.update([0]);
            hash.update(REFRESH.as_bytes());
            hash.finalize().to_vec()
        };
        let connection = rusqlite::Connection::open(spent.data().join("hub.sqlite")).expect("store");
        connection
            .execute("INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms) VALUES(?1,'attempt',0)", [fingerprint])
            .expect("spent fence");
        drop(connection);
        let pair = double(&spent, serde_json::json!({"access": access(), "refresh": REFRESH}));
        for _ in 0..2 {
            let output = take_over(&spent, &source_url(&source), &input(), Some(&pair));
            assert_refused(&output, "refresh_token_spent", &["spent take-over"]);
            assert!(stdout(&output).is_empty());
            assert_secret_free(&output);
        }

        assert_eq!(source.connections(), 0, "the double stood in for the source");
        assert_eq!(spent.endpoint.connections(), 0);
        assert_eq!(hub.endpoint.connections(), 0, "nothing was sent to Tesla");
        assert_eq!(fenced.endpoint.connections(), 0);
    }

    /// M09-T16 (B3): the run ends after the vault commit and before its report, as a
    /// crash would; the next run with the same source row reconciles with exit 0,
    /// `already_taken_over`, writes nothing, and `status` shows the taken-over login.
    #[test]
    fn source_takeover_after_a_crash_past_the_commit_reconciles() {
        let (hub, source) = hub_with_store();
        let url = source_url(&source);
        let crashing = double(&hub, serde_json::json!({"access": access(), "refresh": REFRESH, "exitAfterCommit": true}));
        let output = take_over(&hub, &url, &input(), Some(&crashing));
        assert_eq!(output.status.code(), Some(86), "{}", stderr(&output));
        assert!(stdout(&output).is_empty(), "no report");
        assert_secret_free(&output);

        let pair = double(&hub, serde_json::json!({"access": access(), "refresh": REFRESH}));
        for _ in 0..2 {
            let output = take_over(&hub, &url, &input(), Some(&pair));
            assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
            assert_eq!(stdout(&output), format!("{ALREADY}\n"));
            assert_secret_free(&output);
        }
        let output = hub.run(&["status"], b"");
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        let status: serde_json::Value = serde_json::from_str(stdout(&output).trim()).expect("status JSON");
        let legacy = &status["legacyCredentials"];
        assert_eq!((legacy["present"].as_bool(), legacy["origin"].as_str(), legacy["refreshProgress"].as_str()),
            (Some(true), Some("source_token"), Some("none_sent")), "{status}");
        assert_eq!(source.connections(), 0, "the double stood in for the source");
        assert_eq!(hub.endpoint.connections(), 0, "nothing was sent to Tesla");
    }
}
