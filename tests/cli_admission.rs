// SPDX-License-Identifier: AGPL-3.0-only
#![cfg(unix)]

//! The built binary's admission boundary (M02-T10; design section 9, ruling
//! R5). Removed inputs are rejected by the parser; `setup`, the window verbs
//! and credential recovery refuse `service_not_running` before they read a
//! secret path or standard input and with no network connection, and so does
//! the Fleet sign-in (`setup-fleet`, M09-T09); Fleet telemetry push is refused
//! `provider_parked`; import waits for the window to close; and the
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
        vec!["setup-fleet", "begin", "--all-vehicles"],
        vec!["setup-fleet", "--client-secret", &key],
        vec!["setup-fleet", "--access-token-file", &key],
        vec!["setup-fleet", "register", "--all-vehicles"],
        vec!["setup-fleet", "register", "--client-secret", &key],
        vec!["setup-fleet", "public-key", "--output", &key, "--vehicle-id", "7"],
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
    // The public key export names its file (M09-T18).
    let output = hub.run(&["setup-fleet", "public-key"], TOKENS);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("--output"), "{}", stderr(&output));
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
    let pem = hub.path("fleet-public-key.pem").to_str().expect("path").to_owned();
    let cases: Vec<Vec<&str>> = vec![
        vec!["setup", "--all-vehicles"],
        vec!["setup", "--vehicle-id", "7"],
        vec!["setup-fleet", "begin"],
        vec!["setup-fleet"],
        vec!["setup-fleet", "--all-vehicles"],
        vec!["setup-fleet", "--vehicle-id", "7"],
        vec!["setup-fleet", "register"],
        vec!["setup-fleet", "public-key", "--output", &pem],
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
    assert!(!Path::new(&pem).exists(), "no public key file was written");
    assert_eq!(hub.endpoint.connections(), 0, "no network connection");
}

/// Fleet telemetry push is not offered (M09-T09): the verb is refused with
/// its fixed text before the configuration is read.
#[test]
fn fleet_sign_in_telemetry_push_is_refused_before_the_configuration_is_read() {
    let hub = Hub::new();
    fs::remove_file(hub.path("config.toml")).expect("no configuration");
    let output = hub.run(&["configure-fleet-telemetry"], TOKENS);
    assert_refused(&output, "provider_parked", &["configure-fleet-telemetry"]);
    assert!(stderr(&output).contains("provider_parked: Fleet telemetry push is not offered"), "{}", stderr(&output));
    assert_eq!(hub.endpoint.connections(), 0);
}

/// A Hub configured for Fleet refuses a vehicle command exactly as a Legacy
/// Hub does (design B.5), before anything is sent.
#[test]
fn fleet_sign_in_configured_hub_refuses_vehicle_commands_as_legacy_does() {
    let mut refusals = Vec::new();
    for provider in ["legacy", "fleet"] {
        let hub = Hub::new();
        let config = format!(
            "data_dir = {:?}\nbind = '127.0.0.1:18443'\n[collector]\ninterval_seconds = 0\nprovider = {provider:?}\n",
            hub.data()
        );
        fs::write(hub.path("config.toml"), config).expect("config");
        fs::create_dir(hub.data()).expect("data dir");
        fs::set_permissions(hub.data(), fs::Permissions::from_mode(0o700)).expect("data mode");
        drop(tesatlas_hub::db::HubStore::initialize(hub.data()).expect("store"));
        for case in [
            vec!["control", "climate-start", "--confirm"],
            vec!["control", "--vehicle-id", "00000000-0000-4000-8000-000000000007", "climate-start", "--confirm"],
        ] {
            let output = hub.run(&case, b"");
            assert_eq!(output.status.code(), Some(1), "{provider} {case:?}: {}", stderr(&output));
            assert!(stdout(&output).is_empty(), "{provider} {case:?}: nothing reported");
            // M09-T19 ruling 1: refused as not offered, before anything is read.
            assert!(
                stderr(&output).contains("vehicle_commands_not_offered: vehicle commands are not offered"),
                "{provider} {case:?}: {}",
                stderr(&output)
            );
            refusals.push((provider, case.clone(), stderr(&output)));
        }
        // Every vehicle action is refused the same way, with or without
        // --confirm: the product's refusal, exit 1, not a usage error.
        for action in ["wake", "climate-start", "climate-stop", "charge-start", "charge-stop", "lock", "unlock", "flash-lights", "honk-horn"] {
            for confirm in [true, false] {
                let arguments: &[&str] = if confirm { &["control", action, "--confirm"] } else { &["control", action] };
                let output = hub.run(arguments, b"");
                assert_eq!(output.status.code(), Some(1), "{provider} {arguments:?}: {}", stderr(&output));
                assert!(stdout(&output).is_empty(), "{provider} {arguments:?}");
                assert!(stderr(&output).contains("vehicle_commands_not_offered"), "{provider} {arguments:?}: {}", stderr(&output));
            }
        }
        for confirm in [true, false] {
            let arguments: &[&str] = if confirm { &["control", "set-charge-limit", "--percent", "80", "--confirm"] } else { &["control", "set-charge-limit", "--percent", "80"] };
            let output = hub.run(arguments, b"");
            assert_eq!(output.status.code(), Some(1), "{provider} {arguments:?}: {}", stderr(&output));
            assert!(stderr(&output).contains("vehicle_commands_not_offered"), "{provider} {arguments:?}: {}", stderr(&output));
        }
        assert_eq!(hub.endpoint.connections(), 0, "{provider}: nothing sent");
    }
    let (legacy, fleet) = refusals.split_at(2);
    for (legacy, fleet) in legacy.iter().zip(fleet) {
        assert_eq!(legacy.2, fleet.2, "{:?}: the Fleet Hub refuses as the Legacy Hub does", legacy.1);
    }
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

    /// M09-T16: a Hub whose history import set up a car, as the guided
    /// migration's import does before the take-over.
    fn hub_with_car() -> (Hub, CountingEndpoint) {
        use tesatlas_hub::{
            db::HubStore,
            hub_pack::ProjectionCarSettings,
            legacy_owner_credentials::load_or_create_cursor_key,
            source_db_import::{SourceDbImportRequest, SourceDbImportScope, publish_history},
            source_db_projection::{SourceDbCar, SourceDbHistory},
        };
        let (hub, source) = hub_with_store();
        let store = HubStore::initialize(hub.data()).expect("store");
        let cursor_key = load_or_create_cursor_key(&hub.data()).expect("cursor key");
        let history = SourceDbHistory {
            cars: vec![SourceDbCar {
                id: 1,
                eid: 5_300_001,
                vid: Some(5_400_001),
                vin: Some("5YJ3E1EA7KF530001".to_owned()),
                name: Some("Take-over fixture".to_owned()),
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
        };
        let request = SourceDbImportRequest {
            new_import_source: false,
            source_key: "takeover-fixture".to_owned(),
            scope: SourceDbImportScope::Selected(1),
            imported_at_ms: 1_000,
        };
        publish_history(&store, &cursor_key, &request, &history).expect("configured car");
        drop(store);
        (hub, source)
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
        let (hub, source) = hub_with_car();
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
        let (fenced, source) = hub_with_car();
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
        let (spent, source) = hub_with_car();
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

    /// M09-T16: on a Hub with no car set up the take-over is refused
    /// `history_import_required` (exit 1): the source is not read, the key is
    /// not used and nothing is written. Once the history import has set up a
    /// car, the same take-over succeeds unchanged.
    #[test]
    fn source_takeover_requires_the_history_import_first() {
        let (hub, source) = hub_with_store();
        let url = source_url(&source);
        let pair = double(&hub, serde_json::json!({"access": access(), "refresh": REFRESH}));
        for _ in 0..2 {
            let output = take_over(&hub, &url, &input(), Some(&pair));
            assert_refused(&output, "history_import_required", &["import the history first"]);
            assert!(stdout(&output).is_empty());
            assert_secret_free(&output);
        }
        assert!(!hub.data().join("secrets").exists(), "no credential key was created");
        let output = hub.run(&["status"], b"");
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        let status: serde_json::Value = serde_json::from_str(stdout(&output).trim()).expect("status JSON");
        assert_eq!(status["legacyCredentials"]["present"].as_bool(), Some(false), "nothing was stored: {status}");
        assert_eq!(source.connections(), 0, "the source was never contacted");
        assert_eq!(hub.endpoint.connections(), 0, "nothing was sent to Tesla");

        let (hub, source) = hub_with_car();
        let url = source_url(&source);
        let pair = double(&hub, serde_json::json!({"access": access(), "refresh": REFRESH}));
        let output = take_over(&hub, &url, &input(), Some(&pair));
        assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
        assert_eq!(stdout(&output), format!("{REPORT}\n"), "the unchanged report");
        assert_secret_free(&output);
        assert_eq!(source.connections(), 0, "the double stood in for the source");
        assert_eq!(hub.endpoint.connections(), 0, "nothing was sent to Tesla");
    }

    /// M09-T16 (B3): the run ends after the vault commit and before its report, as a
    /// crash would; the next run with the same source row reconciles with exit 0,
    /// `already_taken_over`, writes nothing, and `status` shows the taken-over login.
    #[test]
    fn source_takeover_after_a_crash_past_the_commit_reconciles() {
        let (hub, source) = hub_with_car();
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

/// M08-T18: a Legacy Hub's command line refuses every vehicle action with the
/// fixed refusal, before the configuration or catalogue is read and before it
/// connects to the running service; nothing is sent. Without `--confirm` the
/// parser refuses it as a usage error before that.
#[test]
fn legacy_hub_refuses_every_vehicle_action_at_the_command_line() {
    let hub = Hub::new();
    fs::create_dir(hub.data()).expect("data dir");
    fs::set_permissions(hub.data(), fs::Permissions::from_mode(0o700)).expect("data mode");
    drop(tesatlas_hub::db::HubStore::initialize(hub.data()).expect("store"));
    let actions: [&[&str]; 10] = [
        &["wake"],
        &["climate-start"],
        &["climate-stop"],
        &["charge-start"],
        &["charge-stop"],
        &["set-charge-limit", "--percent", "80"],
        &["lock"],
        &["unlock"],
        &["flash-lights"],
        &["honk-horn"],
    ];
    for action in actions {
        for target in [&[][..], &["--vehicle-id", "00000000-0000-4000-8000-000000000007"][..]] {
            let mut arguments = vec!["control"];
            arguments.extend_from_slice(target);
            arguments.extend_from_slice(action);
            let mut confirmed = arguments.clone();
            confirmed.push("--confirm");
            let output = hub.run(&confirmed, b"");
            assert_refused(&output, "vehicle_commands_not_offered", &confirmed);
            assert!(
                stderr(&output).contains(
                    "vehicle_commands_not_offered: vehicle commands are not offered: the Hub only reads vehicle data and never sends a command to a car"
                ),
                "{confirmed:?}: {}",
                stderr(&output)
            );
            assert!(stdout(&output).is_empty(), "{confirmed:?}: nothing reported");
            let output = hub.run(&arguments, b"");
            assert_ne!(output.status.code(), Some(0), "{arguments:?} without --confirm");
            assert!(stdout(&output).is_empty(), "{arguments:?} without --confirm");
        }
    }
    // Refused before the configuration is read: an unreadable one changes nothing.
    fs::set_permissions(hub.path("config.toml"), fs::Permissions::from_mode(0o000)).expect("seal config");
    let output = hub.run(&["control", "wake", "--confirm"], b"");
    fs::set_permissions(hub.path("config.toml"), fs::Permissions::from_mode(0o600)).expect("config mode");
    assert_refused(&output, "vehicle_commands_not_offered", &["control", "wake", "--confirm"]);
    assert_eq!(hub.endpoint.connections(), 0, "nothing sent");
}

/// `setup-fleet figures` (M09-T21) is answered from the configuration alone:
/// no service, no data folder, no input, no connection, nothing written.
#[test]
fn setup_fleet_figures_answer_without_the_service_or_a_data_folder() {
    let hub = Hub::new();
    hub.seal(true);
    let legacy = hub.run(&["setup-fleet", "figures"], TOKENS);
    assert_figures_refused(&legacy, FIGURES_NOT_FLEET);
    let config = format!(
        "data_dir = {:?}\nbind = '127.0.0.1:18443'\n[collector]\nprovider = \"fleet\"\n\
         interval_seconds = 300\n\n[collector.\"fleet\\u005fpolling\"]\nmonthly_cap_cents = 2000\n\
         driving_poll_seconds = 60\n",
        hub.data()
    );
    fs::write(hub.path("config.toml"), config).expect("config");
    let output = hub.run(&["setup-fleet", "figures"], TOKENS);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "{\"provider\":\"fleet\",\"collectionIntervalSeconds\":300,\"pollingSeconds\":{\"driving\":60,\
         \"charging\":120,\"updating\":300,\"online\":120,\"stateCheck\":600,\"inventory\":3600},\
         \"requestsPerDollar\":{\"data\":500,\"state\":500},\"monthlyCapCents\":2000}\n"
    );
    let extra = hub.run(&["setup-fleet", "figures", "--all-vehicles"], TOKENS);
    assert_eq!(extra.status.code(), Some(2), "{}", stderr(&extra));
    fs::remove_file(hub.path("config.toml")).expect("remove config");
    let missing = hub.run(&["setup-fleet", "figures"], TOKENS);
    assert_figures_refused(&missing, FIGURES_UNREADABLE);
    assert!(!hub.data().exists(), "no Hub state was created");
    assert_eq!(hub.endpoint.connections(), 0);
}

const FIGURES_UNREADABLE: &str = "config_unreadable: the configuration file cannot be read safely\n";
const FIGURES_INVALID: &str = "config_invalid: the configuration is not a valid Hub configuration\n";
const FIGURES_NOT_FLEET: &str =
    "fleet_provider_not_selected: the configuration does not select the Fleet provider\n";
const FIGURES_CADENCE: &str = "collector_cadence_invalid: a polling setting of the configuration is invalid; \
     the service does not collect with it\n";

/// A refused `setup-fleet figures`: exit 1, nothing on standard output, and
/// standard error exactly the one bare `<code>: <text>` line of interface
/// section 12 (M09-T21 R4: the line the Mac app's refusal reader parses).
fn assert_figures_refused(output: &Output, line: &str) {
    assert_eq!(output.status.code(), Some(1), "{}", stderr(output));
    assert!(stdout(output).is_empty(), "{}", stdout(output));
    assert_eq!(stderr(output), line);
}

/// The reviewer's configuration of the M09-T21 review with `state` as the
/// state check interval and `extra` appended.
fn figures_config(hub: &Hub, state: &str, extra: &str) -> String {
    format!(
        "data_dir = {:?}\nbind = '127.0.0.1:8080'\n[collector]\nprovider = 'fleet'\n\
         interval_seconds = 300\n[collector.fleet_polling]\nstate_poll_seconds = {state}\n{extra}",
        hub.data()
    )
}

/// M09-T21 R3 and R4: success leaves standard error empty, also with an
/// accepted deprecated table name, and each of the four refusals is exactly its
/// one line, also when the file uses that name.
#[test]
fn setup_fleet_figures_streams_are_exactly_the_contract_lines() {
    let hub = Hub::new();
    // The accepted earlier name of `[import_source]`, as the loader's own
    // warning names it.
    let warning = tesatlas_hub::config::deprecated_table_warning();
    let name = warning.split('[').nth(1).and_then(|rest| rest.split(']').next()).expect("table name");
    assert_ne!(name, "import_source");
    let deprecated = format!("[{name}]\n");
    for extra in ["", deprecated.as_str()] {
        fs::write(hub.path("config.toml"), figures_config(&hub, "1200", extra)).expect("config");
        let output = hub.run(&["setup-fleet", "figures"], b"");
        assert_eq!(output.status.code(), Some(0), "{extra:?}: {}", stderr(&output));
        assert_eq!(stderr(&output), "", "{extra:?}");
        assert!(stdout(&output).contains("\"stateCheck\":1200,"), "{}", stdout(&output));
        assert_eq!(stdout(&output).lines().count(), 1);

        // The reviewer's example: an interval beyond the scheduler's horizon.
        fs::write(hub.path("config.toml"), figures_config(&hub, "9223372036854775807", extra))
            .expect("config");
        assert_figures_refused(&hub.run(&["setup-fleet", "figures"], b""), FIGURES_CADENCE);
        let horizon = 100 * 365 * 24 * 60 * 60_u64;
        fs::write(hub.path("config.toml"), figures_config(&hub, &(horizon + 1).to_string(), extra))
            .expect("config");
        assert_figures_refused(&hub.run(&["setup-fleet", "figures"], b""), FIGURES_CADENCE);
        fs::write(hub.path("config.toml"), figures_config(&hub, &horizon.to_string(), extra))
            .expect("config");
        let largest = hub.run(&["setup-fleet", "figures"], b"");
        assert_eq!(stderr(&largest), "", "{extra:?}");
        assert!(stdout(&largest).contains(&format!("\"stateCheck\":{horizon},")), "{}", stdout(&largest));

        fs::write(hub.path("config.toml"), figures_config(&hub, "1200", &format!("{extra}unknown_key = 1\n")))
            .expect("config");
        assert_figures_refused(&hub.run(&["setup-fleet", "figures"], b""), FIGURES_INVALID);
        let legacy = figures_config(&hub, "1200", extra).replace("provider = 'fleet'", "provider = 'legacy'");
        fs::write(hub.path("config.toml"), legacy).expect("config");
        assert_figures_refused(&hub.run(&["setup-fleet", "figures"], b""), FIGURES_NOT_FLEET);
    }
    fs::set_permissions(hub.path("config.toml"), fs::Permissions::from_mode(0o666)).expect("mode");
    assert_figures_refused(&hub.run(&["setup-fleet", "figures"], b""), FIGURES_UNREADABLE);
    assert!(!hub.data().exists(), "no Hub state was created");
}

/// Every file name, mode and content below `folder`.
fn folder_snapshot(folder: &Path) -> Vec<(String, u32, Vec<u8>)> {
    let mut entries: Vec<_> = fs::read_dir(folder)
        .expect("folder")
        .map(|entry| {
            let entry = entry.expect("entry");
            let metadata = entry.metadata().expect("metadata");
            let bytes = if metadata.is_file() { fs::read(entry.path()).expect("file") } else { Vec::new() };
            (entry.file_name().to_string_lossy().into_owned(), metadata.permissions().mode(), bytes)
        })
        .collect();
    entries.sort();
    entries
}

/// M09-T21 R2: with the development variables set, the verb creates and
/// changes nothing in a usable journal folder, and an unusable one does not
/// turn its answer into a journal error.
#[test]
fn setup_fleet_figures_leave_the_development_journal_alone() {
    let hub = Hub::new();
    fs::write(hub.path("config.toml"), figures_config(&hub, "1200", "")).expect("config");
    // The journal accepts only a folder whose every ancestor is private, which
    // the short socket folder (possibly under `/tmp`) need not be.
    let private = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .expect("private folder");
    let journal = private.path().join("journal");
    fs::create_dir(&journal).expect("journal folder");
    fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).expect("journal mode");
    fs::write(journal.join("hub-events.0.jsonl"), b"{\"earlier\":true}\n").expect("segment");
    fs::set_permissions(journal.join("hub-events.0.jsonl"), fs::Permissions::from_mode(0o600))
        .expect("segment mode");
    let before = folder_snapshot(&journal);
    let development = |folder: &Path| -> Output {
        hub.run_with(
            &["setup-fleet", "figures"],
            b"",
            &[
                ("TESATLAS_HUB_DEVELOPMENT", Path::new("1")),
                ("TESATLAS_HUB_DEVELOPMENT_LOG_DIRECTORY", folder),
            ],
        )
    };
    for folder in [journal.as_path(), hub.path("config.toml").as_path(), hub.path("absent").as_path()] {
        let output = development(folder);
        assert_eq!(output.status.code(), Some(0), "{folder:?}: {}", stderr(&output));
        assert_eq!(stderr(&output), "", "{folder:?}");
        assert!(stdout(&output).contains("\"stateCheck\":1200,"), "{}", stdout(&output));
    }
    assert_eq!(folder_snapshot(&journal), before, "the journal folder is byte-identical");
    assert!(!hub.path("absent").exists());
    fs::remove_file(hub.path("config.toml")).expect("remove config");
    for folder in [journal.as_path(), hub.path("absent").as_path()] {
        assert_figures_refused(&development(folder), FIGURES_UNREADABLE);
    }
    assert_eq!(folder_snapshot(&journal), before, "a refusal writes nothing either");
    // The folder is usable: another command records its events there.
    let other = hub.run_with(
        &["control", "wake", "--confirm"],
        b"",
        &[
            ("TESATLAS_HUB_DEVELOPMENT", Path::new("1")),
            ("TESATLAS_HUB_DEVELOPMENT_LOG_DIRECTORY", journal.as_path()),
        ],
    );
    assert_eq!(other.status.code(), Some(1), "{}", stderr(&other));
    assert_ne!(folder_snapshot(&journal), before, "the journal folder was usable");
}

/// The refusal texts of the `setup-fleet` verbs, as the binary prints them.
#[cfg(target_os = "macos")]
mod fleet_refusal_lines {
    pub const NOT_RUNNING: &str = "service_not_running: the Hub service is not running; start Hub and try again";
    pub const UNAUTHENTICATED: &str =
        "service_unauthenticated: the process on the control channel is not the Hub service; nothing was sent";
    pub const INPUT_INVALID: &str = "setup_input_invalid: setup-fleet input is unreadable or larger than 4 KiB";
    pub const SOURCE_CREDENTIAL_TEXT: &str = "Source credentials are disabled: apart from the one-time takeover of \
        the migration source's login, the Hub does not import, store, use or refresh another application's sign-in, \
        its tokens or its key, and no option or setting changes that. Sign in with the Hub's own login (in the app, \
        or with setup --sign-in).";
    pub const SIGN_IN_SETTINGS_TEXT: &str = "The Fleet sign-in settings were missing, incomplete, too large or \
        written with an escape the Hub does not read. Nothing was changed. Check them and start again.";
    pub const PARTNER_SETTINGS_INVALID: &str = "fleet_partner_settings_invalid: setup-fleet register reads one JSON \
        object with exactly clientId, clientSecret, domain (a host name such as example.com) and region (na, eu or \
        cn), with no escape other than \\/ or \\u";
    pub const WINDOW_CLOSED: &str = "window_closed: sign-in and vehicle discovery need an open collection window";
    pub const NOT_FLEET: &str = "fleet_activation_unavailable: this Hub does not collect with the Fleet API";
    pub const SIGN_IN_REQUIRED: &str = "fleet_sign_in_required: sign in with your Fleet application (setup-fleet \
        begin, then setup-fleet) before collection is switched on";
    pub const PATH_PROTECTED: &str = "fleet_public_key_path_protected: the public key cannot be saved inside the \
        Hub's own data, secrets, configuration or log folders; choose another folder";
    pub const UNWRITABLE: &str = "fleet_public_key_unwritable: the public key file cannot be written at that path";
}

/// One `setup-fleet` refusal: the verb, its arguments, its input and the
/// whole standard error line the contract gives it, with what the binary did.
#[cfg(target_os = "macos")]
struct FleetRefusal {
    verb: &'static str,
    arguments: Vec<String>,
    line: String,
    output: Output,
}

/// Runs `verb` (with `extra` arguments and `input`) on `hub` and records it
/// with the line it must print.
#[cfg(target_os = "macos")]
fn fleet_refusal(hub: &Hub, verb: &'static str, extra: &[&str], input: &[u8], line: impl Into<String>) -> FleetRefusal {
    let mut arguments: Vec<String> = verb.split(' ').map(str::to_owned).collect();
    arguments.extend(extra.iter().map(|argument| (*argument).to_owned()));
    let output = hub.run(&arguments.iter().map(String::as_str).collect::<Vec<_>>(), input);
    FleetRefusal { verb, arguments, line: line.into(), output }
}

/// Every `setup-fleet` verb but `figures` (which answers without the
/// service), with the arguments it needs.
#[cfg(target_os = "macos")]
const FLEET_VERBS: [&str; 6] = [
    "setup-fleet begin",
    "setup-fleet",
    "setup-fleet discover",
    "setup-fleet activate",
    "setup-fleet register",
    "setup-fleet public-key",
];

/// A Hub that `serve` can run: a free loopback port, no geocoder or terrain
/// lookups, and the test build's window anchor in the test's own folder
/// (never the login Keychain), as `tests/idle_start.rs` runs it.
#[cfg(target_os = "macos")]
fn write_serving_config(hub: &Hub, collector: &str) {
    let port = TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("address").port();
    let config = format!(
        "data_dir = {:?}\nbind = \"127.0.0.1:{port}\"\n{collector}\n[geocoder]\nenabled = false\n\n[terrain]\nenabled = false\n",
        hub.data()
    );
    fs::write(hub.path("config.toml"), config).expect("config");
    fs::write(hub.path("doubles.json"), serde_json::json!({ "anchorDir": hub.path("anchor") }).to_string())
        .expect("doubles");
}

/// Runs `serve` on `hub` until its operator channel answers, then `work`,
/// then stops it with SIGTERM.
#[cfg(target_os = "macos")]
fn while_serving(hub: &Hub, label: &str, work: impl FnOnce()) {
    let doubles = hub.path("doubles.json");
    let log = |name: &str| fs::File::create(hub.path(&format!("{label}.{name}"))).expect("log");
    let mut serve = Command::new(BIN)
        .arg("--config")
        .arg(hub.path("config.toml"))
        .arg("serve")
        .env_clear()
        .env("HOME", hub.path("home"))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TMPDIR", hub.tmp.path())
        .env("RUST_LOG", "off")
        .env("TESATLAS_HUB_JOURNEY_DOUBLES", &doubles)
        .env("HTTP_PROXY", &hub.endpoint.address)
        .env("HTTPS_PROXY", &hub.endpoint.address)
        .env("ALL_PROXY", &hub.endpoint.address)
        .env("http_proxy", &hub.endpoint.address)
        .env("https_proxy", &hub.endpoint.address)
        .stdin(Stdio::null())
        .stdout(log("out"))
        .stderr(log("err"))
        .spawn()
        .expect("serve starts");
    let serve_log = || fs::read_to_string(hub.path(&format!("{label}.err"))).unwrap_or_default();
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = serve.try_wait().expect("serve state") {
            panic!("{label}: serve exited {status} before answering: {}", serve_log());
        }
        if hub.run(&["window", "status"], b"").status.success() {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(90), "{label}: serve did not answer: {}", serve_log());
        std::thread::sleep(Duration::from_millis(200));
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
    let _ = Command::new("/bin/kill").args(["-TERM", &serve.id().to_string()]).status();
    let stopping = std::time::Instant::now();
    while serve.try_wait().expect("serve state").is_none() {
        if stopping.elapsed() > Duration::from_secs(90) {
            let _ = serve.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = serve.wait();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// The fixture the Mac app's Fleet refusal test parses: every line below,
/// exactly as the built binary printed it.
#[cfg(target_os = "macos")]
const FLEET_REFUSAL_FIXTURE: &str = "macos/TesatlasHubApp/TesatlasHubAppTests/Fixtures/fleet-refusal-lines.json";

/// M09-T24: every `setup-fleet` verb refuses with the contract's bare
/// `<code>: <text>` line (interface sections 1 and 11), the form `setup-fleet
/// figures` writes and the Mac app's `HubFleetRefusal.parse` reads: exit 1,
/// nothing on standard output, and standard error exactly that one line. Each
/// code is reached as the real service and command give it: with no service,
/// with another program on the socket, and with the real `serve` idle (no
/// Open window) for a Legacy and a Fleet Hub. Codes that need Tesla or an
/// Open window are not reached here; they leave through the same report.
///
/// What the binary printed is written to the app's fixture with each case's
/// contract code; the app's Fleet refusal test parses every entry and must get
/// that code back. A changed line rewrites the
/// fixture and fails this test, so the change is seen and committed with the
/// app test that reads it.
#[cfg(target_os = "macos")]
#[test]
fn setup_fleet_refusals_are_the_contract_lines() {
    use fleet_refusal_lines::*;

    let hub = Hub::new();
    let mut printed = Vec::new();

    // No service: every verb is refused before it reads its input.
    let key = hub.path("fleet-public-key.pem");
    let key = key.to_str().expect("path");
    for verb in FLEET_VERBS {
        let extra: &[&str] = if verb.ends_with("public-key") { &["--output", key] } else { &[] };
        printed.push(fleet_refusal(&hub, verb, extra, TOKENS, NOT_RUNNING));
    }

    // Another program of the same user on the socket is not the service.
    {
        let impostor_hub = Hub::new();
        let impostor = Impostor::bind(&impostor_hub.data());
        for verb in FLEET_VERBS {
            let extra: &[&str] = if verb.ends_with("public-key") { &["--output", key] } else { &[] };
            printed.push(fleet_refusal(&impostor_hub, verb, extra, TOKENS, UNAUTHENTICATED));
        }
        assert_eq!(impostor.bytes_received(), 0, "the impostor received nothing");
    }

    // The real service, idle: a Legacy Hub.
    write_serving_config(&hub, "");
    let oversized = vec![b' '; 5000];
    let token: &[u8] = br#"{"accessToken":"access-0001"}"#;
    let begin: &[u8] = br#"{"clientId":"fleet-client-0001","region":"eu","redirectUri":"https://owner.example/fleet/callback"}"#;
    let redeem: &[u8] = br#"{"code":"code-0001","redirectUri":"https://owner.example/fleet/callback","clientId":"fleet-client-0001","clientSecret":"fleet-secret-value-0001","region":"eu","binding":"6f1c2a52-7d7e-4b8e-9a39-2d1f6b0c4e11"}"#;
    let register: &[u8] = br#"{"clientId":"synthetic-client","clientSecret":"fleet-secret-value-0001","domain":"synthetic.example.test","region":"eu"}"#;
    // The command checks the sign-in settings with the sign-in's own error,
    // whose text already starts with its code (present behaviour, M09-T24
    // notes).
    let source_credential_sign_in = format!("source_credential_disabled: source_credential_disabled: {SOURCE_CREDENTIAL_TEXT}");
    let settings = format!("sign_in_settings_invalid: sign_in_settings_invalid: {SIGN_IN_SETTINGS_TEXT}");
    let protected = hub.data().join("fleet-public-key.pem");
    let protected = protected.to_str().expect("path");
    let unwritable = hub.path("missing").join("fleet-public-key.pem");
    let unwritable = unwritable.to_str().expect("path");
    while_serving(&hub, "legacy", || {
        for verb in ["setup-fleet begin", "setup-fleet"] {
            printed.push(fleet_refusal(&hub, verb, &[], &oversized, INPUT_INVALID));
            printed.push(fleet_refusal(&hub, verb, &[], token, source_credential_sign_in.as_str()));
            printed.push(fleet_refusal(&hub, verb, &[], b"{}", settings.as_str()));
        }
        printed.push(fleet_refusal(&hub, "setup-fleet begin", &[], begin, WINDOW_CLOSED));
        printed.push(fleet_refusal(&hub, "setup-fleet", &[], redeem, WINDOW_CLOSED));
        printed.push(fleet_refusal(&hub, "setup-fleet discover", &[], b"", WINDOW_CLOSED));
        printed.push(fleet_refusal(&hub, "setup-fleet activate", &[], b"", NOT_FLEET));
        printed.push(fleet_refusal(&hub, "setup-fleet register", &[], &oversized, INPUT_INVALID));
        printed.push(fleet_refusal(
            &hub,
            "setup-fleet register",
            &[],
            token,
            format!("source_credential_disabled: {SOURCE_CREDENTIAL_TEXT}"),
        ));
        printed.push(fleet_refusal(&hub, "setup-fleet register", &[], b"{}", PARTNER_SETTINGS_INVALID));
        printed.push(fleet_refusal(&hub, "setup-fleet register", &[], register, WINDOW_CLOSED));
        printed.push(fleet_refusal(&hub, "setup-fleet public-key", &["--output", protected], b"", PATH_PROTECTED));
        printed.push(fleet_refusal(&hub, "setup-fleet public-key", &["--output", unwritable], b"", UNWRITABLE));
    });

    // The real service, idle: a Fleet Hub that is not signed in yet.
    write_serving_config(&hub, "[collector]\nprovider = \"fleet\"\ninterval_seconds = 0\n");
    while_serving(&hub, "fleet", || {
        printed.push(fleet_refusal(&hub, "setup-fleet activate", &[], b"", SIGN_IN_REQUIRED));
    });

    // The fixture holds what the binary printed, before any line is judged.
    let document = serde_json::to_string_pretty(
        &printed
            .iter()
            .map(|refusal| {
                serde_json::json!({
                    "verb": refusal.verb,
                    "code": refusal.line.split(": ").next().expect("code"),
                    "stderr": stderr(&refusal.output),
                })
            })
            .collect::<Vec<_>>(),
    )
    .expect("fixture") + "\n";
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FLEET_REFUSAL_FIXTURE);
    let unchanged = fs::read_to_string(&fixture).is_ok_and(|held| held == document);
    if !unchanged {
        fs::write(&fixture, &document).expect("write the fixture");
    }

    let mut wrong = Vec::new();
    for FleetRefusal { arguments, line, output, .. } in &printed {
        let expected = format!("{line}\n");
        if output.status.code() != Some(1) || !output.stdout.is_empty() || stderr(output) != expected {
            wrong.push(format!(
                "{arguments:?}: exit {:?}, stdout {:?}\n  stderr   {:?}\n  expected {expected:?}",
                output.status.code(),
                stdout(output),
                stderr(output)
            ));
        }
    }
    assert!(wrong.is_empty(), "refusals not in the contracted form:\n{}", wrong.join("\n"));
    let codes: std::collections::BTreeSet<&str> =
        printed.iter().map(|refusal| refusal.line.split(": ").next().expect("code")).collect();
    assert_eq!(codes.len(), 11, "every code reached: {codes:?}");
    assert_eq!(hub.endpoint.connections(), 0, "nothing was sent");
    assert!(unchanged, "{FLEET_REFUSAL_FIXTURE} did not hold the lines the binary prints; it now does: commit it");
}
