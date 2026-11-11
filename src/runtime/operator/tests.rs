// SPDX-License-Identifier: AGPL-3.0-only

//! Operator wiring (M02-T10). Names carry the `collection_window` prefix so
//! the milestone's `collection_window` filter runs them. The service-side
//! controller here has its own gate on a manual clock; the process-wide gate
//! wiring is proven in `tests/cli_admission.rs`, in a process of its own, so
//! that no test here ever opens the gate other library tests read.

use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::{
    admission::{self, OperatorRefusal, Route},
    authority::{OperatorService, ServiceBackend},
    channel::{self, ChannelServer, OperatorRequest, SetupMaterial},
    cli::{Cli, Command, WindowCommand},
};
use crate::collection::window::{
    CloseReason, EgressGate, IssueRequest, Issuer, ManualClock, ManualProbe, MemoryAnchor,
    NoHighWater, PermitKind, Provider, WindowClock, WindowConfig, WindowController,
    WindowDeps,
};

const T0: i64 = 1_800_000_000;
const HOST: &str = "host-operator-test";
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A temporary directory short enough for a Unix socket path.
fn short_tempdir() -> TempDir {
    let base = std::env::temp_dir();
    if base.as_os_str().len() <= 50 {
        TempDir::new_in(base).expect("temp dir")
    } else {
        TempDir::new_in("/tmp").expect("temp dir")
    }
}

#[derive(Default)]
struct Calls(Mutex<Vec<String>>);

impl Calls {
    fn push(&self, call: &str) {
        self.0.lock().expect("calls").push(call.to_owned());
    }

    fn all(&self) -> Vec<String> {
        self.0.lock().expect("calls").clone()
    }
}

struct FakeBackend(Arc<Calls>);

impl ServiceBackend for FakeBackend {
    fn setup(
        &self,
        material: SetupMaterial,
        vehicle_id: Option<i64>,
        all_vehicles: bool,
    ) -> Result<Value, OperatorRefusal> {
        self.0.push(&format!(
            "setup:{}:{}:{vehicle_id:?}:{all_vehicles}",
            material.code.len(),
            material.redirect_uri
        ));
        Ok(json!({ "status": "configured" }))
    }

    fn recovery_export(&self, destination: &Path, _: &Path) -> Result<Value, OperatorRefusal> {
        self.0.push(&format!("export:{}", destination.display()));
        Ok(json!({ "status": "exported" }))
    }

    fn recovery_restore(&self, source: &Path, _: &Path) -> Result<Value, OperatorRefusal> {
        self.0.push(&format!("restore:{}", source.display()));
        Ok(json!({ "status": "restored" }))
    }
}

struct Rig {
    tmp: TempDir,
    clock: Arc<ManualClock>,
    issuer: Issuer,
    key: std::path::PathBuf,
    trust: Vec<u8>,
    controller: Arc<WindowController>,
    calls: Arc<Calls>,
    kicks: Arc<Mutex<u32>>,
}

impl Rig {
    fn new() -> Self {
        let tmp = short_tempdir();
        let clock = Arc::new(ManualClock::new(T0));
        let anchor = Arc::new(MemoryAnchor::new());
        let issuer = Issuer::new(
            &tmp.path().join("issuer"),
            "host-operator-issuer",
            anchor.clone(),
            clock.clone(),
        );
        let key = tmp.path().join("issuer.key");
        let trust = issuer.init(&key).expect("issuer init").to_bytes().expect("trust bytes");
        let (controller, _) = WindowController::start(
            WindowConfig {
                dir: super::window_dir(&tmp.path().join("data")),
                claim_path: tmp.path().join("claim/host-claim.lock"),
                host_id: HOST.to_owned(),
            },
            WindowDeps {
                clock: clock.clone(),
                anchor,
                probe: Arc::new(ManualProbe::new(0.2)),
                gate: EgressGate::new(clock.clone()),
                handback: Arc::new(NoHighWater),
            },
        );
        Self {
            tmp,
            clock,
            issuer,
            key,
            trust,
            controller: Arc::new(controller),
            calls: Arc::new(Calls::default()),
            kicks: Arc::new(Mutex::new(0)),
        }
    }

    fn data(&self) -> std::path::PathBuf {
        self.tmp.path().join("data")
    }

    fn service(&self) -> OperatorService {
        let kicks = self.kicks.clone();
        OperatorService::new(
            self.controller.clone(),
            Arc::new(FakeBackend(self.calls.clone())),
            Box::new(move || *kicks.lock().expect("kicks") += 1),
        )
    }

    fn serve(&self) -> ChannelServer {
        ChannelServer::bind(&self.data(), Arc::new(self.service())).expect("channel bound")
    }

    fn grant(&self, length: i64) -> Vec<u8> {
        let now = self.clock.wall();
        let out = self.tmp.path().join(format!("grant-{now}.json"));
        self.issuer
            .issue(
                &self.key,
                &IssueRequest {
                    host_id: HOST.to_owned(),
                    starts_at: now,
                    expires_at: now + length,
                    approver: "G".to_owned(),
                    reason: "operator test window".to_owned(),
                    approval_ref: "approval-operator-test".to_owned(),
                },
                &out,
            )
            .expect("grant issued");
        std::fs::read(out).expect("grant bytes")
    }

    fn forward(&self, request: &OperatorRequest) -> Result<Value, OperatorRefusal> {
        channel::forward(&self.data(), request, TIMEOUT)
    }
}

fn encoded(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn material() -> SetupMaterial {
    SetupMaterial {
        code: "code-0001".to_owned(),
        code_verifier: "v".repeat(43),
        redirect_uri: "https://auth.tesla.com/void/callback".to_owned(),
    }
}

#[test]
fn collection_window_operator_channel_forwards_window_verbs_to_the_service() {
    let rig = Rig::new();
    std::fs::create_dir_all(rig.data()).expect("data dir");
    let _server = rig.serve();
    let status = rig.forward(&OperatorRequest::WindowStatus).expect("status");
    assert_eq!(status["phase"], "closed");
    assert_eq!(status["trusted"], false);

    let status = rig
        .forward(&OperatorRequest::WindowTrust {
            document: encoded(&rig.trust),
        })
        .expect("trust");
    assert_eq!(status["trusted"], true);

    // Sign-in is refused outside a window, before the backend is asked.
    let refused = rig
        .forward(&OperatorRequest::Setup {
            material: material(),
            vehicle_id: None,
            all_vehicles: true,
        })
        .unwrap_err();
    assert_eq!(refused.code, "window_closed");
    assert!(rig.calls.all().is_empty());

    let gate = rig.controller.gate().clone();
    assert_eq!(
        gate.permit(Provider::Legacy, PermitKind::Request).unwrap_err().code(),
        "window_closed"
    );
    let status = rig
        .forward(&OperatorRequest::WindowOpen {
            grant: encoded(&rig.grant(3_600)),
        })
        .expect("open");
    assert_eq!(status["phase"], "open");
    assert_eq!(status["deadline"], T0 + 3_600);
    assert!(gate.permit(Provider::Legacy, PermitKind::Request).is_ok());
    assert_eq!(
        gate.permit(Provider::Fleet, PermitKind::Request).unwrap_err().code(),
        "provider_parked"
    );

    // Inside the window the bounded code material reaches the backend.
    rig.forward(&OperatorRequest::Setup {
        material: material(),
        vehicle_id: Some(7),
        all_vehicles: false,
    })
    .expect("setup forwarded");
    assert_eq!(
        rig.calls.all(),
        vec!["setup:9:https://auth.tesla.com/void/callback:Some(7):false".to_owned()]
    );

    // Recovery waits for the window to close.
    let refused = rig
        .forward(&OperatorRequest::RecoveryExport {
            destination: rig.tmp.path().join("export"),
            recovery_key_file: rig.tmp.path().join("recovery.key"),
        })
        .unwrap_err();
    assert_eq!(refused.code, "window_active");

    let status = rig.forward(&OperatorRequest::WindowClose).expect("close");
    assert_eq!(status["phase"], "closing");
    assert!(gate.permit(Provider::Legacy, PermitKind::Request).is_err());
    assert_eq!(*rig.kicks.lock().expect("kicks"), 3, "trust, open and close kick");

    rig.controller.finish_close().expect("handback");
    rig.forward(&OperatorRequest::RecoveryRestore {
        source: rig.tmp.path().join("export"),
        recovery_key_file: rig.tmp.path().join("recovery.key"),
    })
    .expect("restore forwarded once Closed");
    assert_eq!(rig.calls.all().len(), 2);
}

#[test]
fn collection_window_operator_channel_refuses_a_replayed_grant_and_bad_material() {
    let rig = Rig::new();
    std::fs::create_dir_all(rig.data()).expect("data dir");
    let _server = rig.serve();
    rig.forward(&OperatorRequest::WindowTrust {
        document: encoded(&rig.trust),
    })
    .expect("trust");
    let grant = rig.grant(600);
    rig.forward(&OperatorRequest::WindowOpen {
        grant: encoded(&grant),
    })
    .expect("open");
    rig.controller.close(CloseReason::OperatorClose);
    rig.controller.finish_close().expect("closed");
    let refused = rig
        .forward(&OperatorRequest::WindowOpen {
            grant: encoded(&grant),
        })
        .unwrap_err();
    assert_eq!(refused.code, "grant_consumed");
    let refused = rig
        .forward(&OperatorRequest::WindowOpen {
            grant: "not base64!".to_owned(),
        })
        .unwrap_err();
    assert_eq!(refused.code, "request_invalid");
    let refused = rig
        .forward(&OperatorRequest::WindowOpen {
            grant: encoded(&vec![b' '; crate::collection::window::GRANT_MAX_BYTES + 1]),
        })
        .unwrap_err();
    assert_eq!(refused.code, "request_invalid");

    // A token-shaped request is not understood by the service at all.
    let stream = channel::connect(&rig.data()).expect("connected");
    let mut stream = stream;
    use std::io::{Read, Write};
    stream
        .write_all(br#"{"op":"setup","material":{"code":"c","code_verifier":"v","redirect_uri":"r","accessToken":"a"},"vehicle_id":null,"all_vehicles":false}"#)
        .expect("written");
    stream.shutdown(std::net::Shutdown::Write).expect("shutdown");
    let mut answer = String::new();
    stream.read_to_string(&mut answer).expect("answer");
    assert!(answer.contains("request_invalid"), "{answer}");
    assert!(rig.calls.all().is_empty());
}

#[test]
fn collection_window_operator_refuses_service_verbs_when_no_service_runs() {
    let tmp = short_tempdir();
    let data = tmp.path().join("data");
    // No data directory, then no channel directory, then no listener.
    for prepare in [0, 1, 2] {
        if prepare >= 1 {
            std::fs::create_dir_all(&data).expect("data dir");
        }
        if prepare == 2 {
            let dir = channel::channel_dir(&data);
            std::fs::create_dir_all(&dir).expect("channel dir");
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("mode");
            let listener =
                std::os::unix::net::UnixListener::bind(channel::socket_path(&data)).expect("bind");
            drop(listener);
        }
        let refused = channel::forward(&data, &OperatorRequest::WindowStatus, TIMEOUT).unwrap_err();
        assert_eq!(refused.code, "service_not_running", "case {prepare}");
    }
    // A channel directory open to others is refused, not trusted.
    std::fs::set_permissions(
        channel::channel_dir(&data),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("mode");
    let refused = channel::forward(&data, &OperatorRequest::WindowStatus, TIMEOUT).unwrap_err();
    assert_eq!(refused.code, "control_channel_unsafe");
}

#[test]
fn collection_window_operator_import_admission_follows_the_window() {
    let rig = Rig::new();
    // A Hub that never trusted an issuer has no window to wait for.
    admission::admit_import(&rig.data()).expect("fresh Hub admits import");
    rig.controller
        .trust(&crate::collection::window::TrustDocument::parse(&rig.trust).expect("trust"))
        .expect("trusted");
    let held = admission::admit_import(&rig.data()).expect("Closed admits import");
    drop(held);
    rig.controller.open(&rig.grant(600)).expect("open");
    assert_eq!(
        admission::admit_import(&rig.data()).unwrap_err().code,
        "window_active"
    );
    rig.controller.close(CloseReason::OperatorClose);
    assert_eq!(
        admission::admit_import(&rig.data()).unwrap_err().code,
        "window_active",
        "Closing still refuses"
    );
    rig.controller.finish_close().expect("handback");
    let held = admission::admit_import(&rig.data()).expect("admitted after handback");
    // The issuer keeps its gap between grants.
    rig.clock.advance(2_000);
    // While the import holds the lock no window opens.
    assert_eq!(
        rig.controller.open(&rig.grant(600)).unwrap_err().code(),
        "import_running"
    );
    drop(held);
    // A corrupt state file fails closed.
    std::fs::write(super::window_dir(&rig.data()).join("state.json"), b"{").expect("corrupt");
    assert_eq!(
        admission::admit_import(&rig.data()).unwrap_err().code,
        "state_unverifiable"
    );
}

fn parsed(arguments: &[&str]) -> Result<Command, String> {
    let mut all = vec!["teslatlas-hub"];
    all.extend_from_slice(arguments);
    Cli::try_parse_from(all)
        .map(|cli| cli.command)
        .map_err(|error| error.to_string())
}

#[test]
fn collection_window_operator_routes_every_operator_command() {
    let route = |arguments: &[&str]| admission::route(&parsed(arguments).expect("parsed"));
    assert_eq!(route(&["setup", "--all-vehicles"]), Route::Service);
    assert_eq!(route(&["setup-fleet"]), Route::Refused("provider_parked"));
    assert_eq!(route(&["configure-fleet-telemetry"]), Route::Refused("provider_parked"));
    for verb in [
        vec!["window", "status"],
        vec!["window", "close"],
        vec!["window", "trust", "trust.json"],
        vec!["window", "open", "--grant", "grant.json"],
        vec!["export-recovery-credentials", "--destination", "d", "--recovery-key-file", "k"],
        vec!["restore-recovery-credentials", "--source", "s", "--recovery-key-file", "k"],
    ] {
        assert_eq!(route(&verb), Route::Service, "{verb:?}");
    }
    for verb in [
        vec!["window", "host-id"],
        vec!["window", "issuer-init", "--key-out", "k", "--trust-out", "t"],
        vec!["window", "issuer-trust", "--key-file", "k", "--trust-out", "t"],
        vec!["window", "void", "--key-file", "k", "--seq", "1", "--reason", "r"],
    ] {
        assert_eq!(route(&verb), Route::Issuer, "{verb:?}");
    }
    assert_eq!(route(&["restore-data", "--source", "s", "--destination", "d"]), Route::Import);
    assert_eq!(route(&["doctor"]), Route::Local);
    // Forwarded commands never take the lifetime lock (ruling R5).
    for verb in [
        vec!["setup"],
        vec!["window", "open", "--grant", "g"],
        vec!["export-recovery-credentials", "--destination", "d", "--recovery-key-file", "k"],
    ] {
        assert!(!admission::requires_user_hub_admission(&parsed(&verb).expect("parsed")));
    }
    assert!(admission::requires_user_hub_admission(&parsed(&["serve"]).expect("parsed")));
    assert!(matches!(
        parsed(&["window", "open", "--grant", "g"]).expect("parsed"),
        Command::Window { command: WindowCommand::Open { .. } }
    ));
}

#[test]
fn collection_window_operator_removed_inputs_are_rejected_by_the_parser() {
    for removed in [
        vec!["setup", "--tokens-stdin"],
        vec!["setup", "--access-token-file", "a", "--refresh-token-file", "r"],
        vec!["setup", "--sign-in"],
        vec!["setup-fleet", "--sign-in"],
        vec!["source-check", "--source", "postgres://h/db", "--car-id", "1", "--postgres-password-file", "p", "--history-only"],
    ] {
        let error = parsed(&removed).unwrap_err();
        assert!(error.contains("unexpected argument"), "{removed:?}: {error}");
    }
    for removed in [
        "--source-token-key-file",
        "--encryption-key-file",
        "--access-token-file",
        "--refresh-token-file",
        "--replace-credentials",
        "--preserve-existing-credentials",
    ] {
        let error = parsed(&[
            "migrate", "--source", "postgres://h/db", "--car-id", "1",
            "--postgres-password-file", "p", "--accept-schema-evidence", removed, "x",
        ])
        .unwrap_err();
        assert!(error.contains("unexpected argument"), "{removed}: {error}");
    }
}

#[test]
fn collection_window_operator_host_id_and_times() {
    let ioreg = "  \"IOPlatformSerialNumber\" = \"XYZ\"\n  \"IOPlatformUUID\" = \"00000000-1111-2222-3333-444444444444\"\n";
    let uuid = super::host::parse_platform_uuid(ioreg).expect("uuid");
    assert_eq!(uuid, "00000000-1111-2222-3333-444444444444");
    let id = super::host::host_id_for(&uuid);
    assert!(id.starts_with("host-") && id.len() == 37, "{id}");
    assert_ne!(id, super::host::host_id_for("00000000-1111-2222-3333-444444444445"));
    assert!(!id.contains("1111"), "the raw identifier is not carried");
    assert_eq!(super::issuer::parse_time("1800000000").expect("seconds"), 1_800_000_000);
    assert_eq!(
        super::issuer::parse_time("2027-01-15T08:00:00Z").expect("rfc3339"),
        1_800_000_000
    );
    for bad in ["2027-01-15 08:00:00", "2027-13-01T00:00:00Z", "2027-01-15T08:00:00+01:00", "soon"] {
        assert_eq!(super::issuer::parse_time(bad).unwrap_err().code, "time_invalid", "{bad}");
    }
}
