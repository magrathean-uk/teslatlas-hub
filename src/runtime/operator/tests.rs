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
    trust::{OwnerConfirmation, TrustAuthority},
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

    fn refused_credentials(&self) -> Result<Value, OperatorRefusal> {
        self.0.push("refused_list");
        Ok(json!({ "mode": "list" }))
    }

    fn purge_refused_credentials(&self) -> Result<Value, OperatorRefusal> {
        self.0.push("refused_purge");
        Ok(json!({ "mode": "purge" }))
    }
}

/// The owner at the Hub Mac: answers the trust confirmation as told and
/// counts the questions.
#[derive(Default)]
struct Owner {
    declines: std::sync::atomic::AtomicBool,
    asked: std::sync::atomic::AtomicU32,
}

impl OwnerConfirmation for Owner {
    fn confirm_issuer(&self, _: &str, _: u64, _: Option<&str>) -> bool {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        !self.declines.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Owner {
    fn decline(&self, declines: bool) {
        self.declines.store(declines, std::sync::atomic::Ordering::SeqCst);
    }

    fn asked(&self) -> u32 {
        self.asked.load(std::sync::atomic::Ordering::SeqCst)
    }
}

struct Rig {
    probe: Arc<ManualProbe>,
    owner: Arc<Owner>,
    pin: Arc<MemoryAnchor>,
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
        Self::with_handback(Arc::new(NoHighWater))
    }

    fn with_handback(handback: Arc<dyn crate::collection::window::HandbackSource>) -> Self {
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
        let probe = Arc::new(ManualProbe::new(0.2));
        let (controller, _) = WindowController::start(
            WindowConfig {
                dir: super::window_dir(&tmp.path().join("data")),
                claim_path: crate::collection::window::HostClaim::default_path(&tmp.path().join("home")),
                host_id: HOST.to_owned(),
            },
            WindowDeps {
                clock: clock.clone(),
                anchor,
                probe: probe.clone(),
                gate: EgressGate::new(clock.clone()),
                handback,
            },
        );
        Self {
            probe,
            owner: Arc::new(Owner::default()),
            pin: Arc::new(MemoryAnchor::new()),
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

    fn home(&self) -> std::path::PathBuf {
        self.tmp.path().join("home")
    }

    fn admit(&self) -> Result<admission::ImportAdmission, OperatorRefusal> {
        admission::admit_import(Some(&self.data()), &self.home())
    }

    fn service(&self) -> OperatorService {
        let kicks = self.kicks.clone();
        OperatorService::new(
            self.controller.clone(),
            Arc::new(FakeBackend(self.calls.clone())),
            Box::new(move || *kicks.lock().expect("kicks") += 1),
            TrustAuthority::new(self.pin.clone(), HOST, self.owner.clone()),
            &self.data(),
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
    rig.admit().expect("fresh Hub admits import");
    rig.controller
        .trust(&crate::collection::window::TrustDocument::parse(&rig.trust).expect("trust"))
        .expect("trusted");
    let held = rig.admit().expect("Closed admits import");
    drop(held);
    rig.controller.open(&rig.grant(600)).expect("open");
    assert_eq!(
        rig.admit().unwrap_err().code,
        "window_active"
    );
    rig.controller.close(CloseReason::OperatorClose);
    assert_eq!(
        rig.admit().unwrap_err().code,
        "window_active",
        "Closing still refuses"
    );
    rig.controller.finish_close().expect("handback");
    let held = rig.admit().expect("admitted after handback");
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
        rig.admit().unwrap_err().code,
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

/// Finding 3: a process of the Hub's own user with an issuer key of its own
/// cannot install that key through the socket, before or after the owner's
/// bootstrap, so it cannot open a window; the pinned issuer's endorsed new
/// epoch is accepted without asking the owner.
#[test]
fn collection_window_operator_trust_needs_the_owner_or_the_pinned_issuer() {
    let rig = Rig::new();
    std::fs::create_dir_all(rig.data()).expect("data dir");
    let _server = rig.serve();
    let attacker = Issuer::new(
        &rig.tmp.path().join("attacker"),
        "host-attacker",
        Arc::new(MemoryAnchor::new()),
        rig.clock.clone(),
    );
    let attacker_key = rig.tmp.path().join("attacker.key");
    attacker.init(&attacker_key).expect("attacker issuer");
    let attacker_grant = || {
        let out = rig.tmp.path().join(format!("attacker-grant-{}.json", rig.clock.wall()));
        attacker
            .issue(
                &attacker_key,
                &IssueRequest {
                    host_id: HOST.to_owned(),
                    starts_at: rig.clock.wall(),
                    expires_at: rig.clock.wall() + 600,
                    approver: "G".to_owned(),
                    reason: "not approved".to_owned(),
                    approval_ref: "none".to_owned(),
                },
                &out,
            )
            .expect("attacker grant");
        encoded(&std::fs::read(out).expect("grant bytes"))
    };
    let trust = |bytes: &[u8]| {
        rig.forward(&OperatorRequest::WindowTrust {
            document: encoded(bytes),
        })
    };

    // A fresh Hub: the attacker's bootstrap is not the owner's.
    rig.owner.decline(true);
    let own = attacker.bump_trust_epoch(&attacker_key).expect("epoch 2");
    let refused = trust(&own.to_bytes().expect("bytes")).unwrap_err();
    assert_eq!(refused.code, "owner_confirmation_refused");
    assert_eq!(rig.controller.status().trusted, false);
    let refused = rig
        .forward(&OperatorRequest::WindowOpen { grant: attacker_grant() })
        .unwrap_err();
    assert_eq!(refused.code, "trust_missing");

    // The owner bootstraps G's issuer.
    rig.owner.decline(false);
    trust(&rig.trust).expect("owner bootstrap");
    assert_eq!(rig.owner.asked(), 2);

    // After it, neither a plain nor a self-endorsed newer document of the
    // attacker's key is accepted.
    rig.owner.decline(true);
    let newer = attacker.bump_trust_epoch(&attacker_key).expect("epoch 3");
    let plain = trust(&newer.to_bytes().expect("bytes")).unwrap_err();
    assert_eq!(plain.code, "owner_confirmation_refused");
    let endorsed = super::trust::endorse(&newer, &attacker_key).expect("self-endorsed");
    let refused = trust(&endorsed).unwrap_err();
    assert_eq!(refused.code, "owner_confirmation_refused");
    // A plain newer document of G's own key needs the owner as well.
    let mut bumped = crate::collection::window::TrustDocument::parse(&rig.trust).expect("trust");
    bumped.trust_epoch = 9;
    assert_eq!(
        trust(&bumped.to_bytes().expect("bytes")).unwrap_err().code,
        "owner_confirmation_refused"
    );
    assert_eq!(rig.owner.asked(), 5);
    // The attacker's issuer keeps its own gap between grants.
    rig.clock.advance(2_000);
    let refused = rig
        .forward(&OperatorRequest::WindowOpen { grant: attacker_grant() })
        .unwrap_err();
    assert_eq!(refused.code, "key_id_mismatch");
    assert!(!rig.controller.gate().is_open());

    // G's rotation, endorsed by the pinned key, needs no confirmation.
    let rotated = rig.issuer.bump_trust_epoch(&rig.key).expect("G's epoch 2");
    let status = trust(&super::trust::endorse(&rotated, &rig.key).expect("endorsed"))
        .expect("pinned issuer's rotation");
    assert_eq!(status["trusted"], true);
    assert_eq!(rig.owner.asked(), 5, "no confirmation asked");
    let status = rig
        .forward(&OperatorRequest::WindowOpen {
            grant: encoded(&rig.grant(600)),
        })
        .expect("G's grant of the new epoch opens");
    assert_eq!(status["phase"], "open");
}

/// Finding 4: the import pauses after it holds its locks and before it reads
/// the window state; a `window open` in that pause is refused, so the import
/// is never admitted while a window is open. The first use of a Hub with no
/// window directory is locked the same way.
#[test]
fn collection_window_operator_import_admission_locks_before_it_reads_the_state() {
    let rig = Rig::new();
    std::fs::create_dir_all(rig.data()).expect("data dir");
    // First use: no window directory yet.
    let mut opened = None;
    let held = admission::admit_import_observed(Some(&rig.data()), &rig.home(), &mut || {
        rig.controller
            .trust(&crate::collection::window::TrustDocument::parse(&rig.trust).expect("trust"))
            .expect_err("no trust while the import holds the window lock");
        opened = Some(());
    })
    .expect("a fresh Hub admits the import");
    assert!(opened.is_some());
    drop(held);
    rig.controller
        .trust(&crate::collection::window::TrustDocument::parse(&rig.trust).expect("trust"))
        .expect("trusted");
    let grant = rig.grant(600);
    let mut refusal = None;
    let held = admission::admit_import_observed(Some(&rig.data()), &rig.home(), &mut || {
        refusal = Some(rig.controller.open(&grant).unwrap_err().code());
    })
    .expect("admitted: the window could not open in the pause");
    assert_eq!(refusal, Some("import_running"));
    assert_eq!(rig.controller.status().phase, crate::collection::window::Phase::Closed);
    assert!(!rig.controller.gate().is_open());
    drop(held);
    rig.controller.open(&grant).expect("opens once the import ends");
    assert_eq!(rig.admit().unwrap_err().code, "window_active");
}

/// Finding 8: a restore with no configuration, or with the configuration of
/// another, Closed Hub, is bound to this host's collection authority: it is
/// refused while any window is open on the host and holds the host claim
/// while it runs.
#[test]
fn collection_window_operator_restore_admission_follows_the_host_claim() {
    let rig = Rig::new();
    std::fs::create_dir_all(rig.data()).expect("data dir");
    let other = rig.tmp.path().join("other-data");
    std::fs::create_dir_all(&other).expect("other data dir");
    rig.controller
        .trust(&crate::collection::window::TrustDocument::parse(&rig.trust).expect("trust"))
        .expect("trusted");
    rig.controller.open(&rig.grant(600)).expect("open");
    for data in [None, Some(other.as_path())] {
        assert_eq!(
            admission::admit_import(data, &rig.home()).unwrap_err().code,
            "window_active",
            "{data:?}"
        );
    }
    rig.controller.close(CloseReason::OperatorClose);
    rig.controller.finish_close().expect("handback");
    let held = admission::admit_import(None, &rig.home()).expect("no window on the host");
    rig.clock.advance(2_000);
    assert_eq!(
        rig.controller.open(&rig.grant(600)).unwrap_err().code(),
        "collector_busy",
        "no window opens while the configuration-free restore runs"
    );
    drop(held);
    let held = admission::admit_import(Some(&other), &rig.home()).expect("other Hub");
    rig.clock.advance(2_000);
    assert_eq!(rig.controller.open(&rig.grant(600)).unwrap_err().code(), "collector_busy");
    drop(held);
}

/// Records when the handback reads the high-water marks.
struct RecordingHandback(Arc<Calls>);

impl crate::collection::window::HandbackSource for RecordingHandback {
    fn high_water(&self) -> crate::collection::window::HighWater {
        self.0.push("handback");
        crate::collection::window::HighWater::default()
    }
}

/// Finding 5: a long observation is supervised for its whole life. When
/// the network time check fails mid-observation the window closes, the
/// observation is told to stop, and the handback is written only after the
/// observation has released its collector lease.
#[tokio::test]
async fn collection_window_operator_observation_is_supervised_for_its_whole_life() {
    let calls = Arc::new(Calls::default());
    let rig = Rig::with_handback(Arc::new(RecordingHandback(calls.clone())));
    rig.controller
        .trust(&crate::collection::window::TrustDocument::parse(&rig.trust).expect("trust"))
        .expect("trusted");
    rig.controller.open(&rig.grant(3_600)).expect("open");
    let controller = rig.controller.clone();
    let observed = calls.clone();
    let supervised = tokio::spawn(super::authority::supervise_observation(
        controller,
        std::time::Duration::from_millis(10),
        move |window_left| async move {
            // An observation far longer than the test: it ends only when told.
            let _ = window_left.await;
            observed.push("lease_released");
            "observed"
        },
    ));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(rig.controller.status().phase, crate::collection::window::Phase::Open);
    // The time server stops answering; the next check is due.
    rig.probe.set(Err(crate::collection::window::Refusal::ClockOffset));
    rig.clock.advance(crate::collection::window::NTP_RECHECK_SECONDS + 1);
    let (result, handback) = tokio::time::timeout(std::time::Duration::from_secs(10), supervised)
        .await
        .expect("the observation was stopped by the supervision")
        .expect("joined");
    assert_eq!(result, "observed");
    let handback = handback.expect("handback").expect("this run closed the window");
    assert_eq!(handback.reason, CloseReason::ClockInvalid);
    assert_eq!(calls.all(), vec!["lease_released".to_owned(), "handback".to_owned()]);
    assert_eq!(rig.controller.status().phase, crate::collection::window::Phase::Closed);
    assert!(!rig.controller.gate().is_open());
}

/// Finding 6: a client that trickles its request is cut off at the
/// absolute deadline and never stands between the operator and `window
/// close`; stopping the service interrupts it; the service never reads its
/// standard input or a relative path for recovery.
#[test]
fn collection_window_operator_channel_has_a_deadline_and_stops_promptly() {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    let rig = Rig::new();
    std::fs::create_dir_all(rig.data()).expect("data dir");
    let server = ChannelServer::bind_with_deadline(
        &rig.data(),
        Arc::new(rig.service()),
        Duration::from_millis(600),
    )
    .expect("channel bound");
    rig.forward(&OperatorRequest::WindowTrust {
        document: encoded(&rig.trust),
    })
    .expect("trust");
    rig.forward(&OperatorRequest::WindowOpen {
        grant: encoded(&rig.grant(600)),
    })
    .expect("open");

    // A slow client sends one byte every 100 ms and never finishes.
    let mut slow = channel::connect(&rig.data()).expect("slow client");
    let trickle = {
        let mut writer = slow.try_clone().expect("clone");
        std::thread::spawn(move || {
            for _ in 0..100 {
                if writer.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };
    // The operator's close is answered while the slow request is pending.
    let started = Instant::now();
    let status = rig.forward(&OperatorRequest::WindowClose).expect("close");
    assert_eq!(status["phase"], "closing");
    assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
    // The slow request is cut off at its deadline, not when it stops.
    slow.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut answer = String::new();
    let _ = slow.read_to_string(&mut answer);
    assert!(answer.contains("in time"), "{answer}");
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    trickle.join().expect("trickle");

    // A stop interrupts a request in progress instead of waiting for it.
    let mut pending = channel::connect(&rig.data()).expect("pending client");
    pending.write_all(b"{").expect("partial");
    std::thread::sleep(Duration::from_millis(100));
    let stopping = Instant::now();
    drop(server);
    assert!(stopping.elapsed() < Duration::from_millis(500), "{:?}", stopping.elapsed());
    // The service shut the connection down: the client reads its end at once.
    let _ = pending.set_read_timeout(Some(Duration::from_secs(5)));
    let mut rest = Vec::new();
    let _ = pending.read_to_end(&mut rest);

    // Recovery never reads the service's standard input or a relative path.
    rig.controller.finish_close().expect("handback");
    let service = rig.service();
    use super::channel::OperatorHandler;
    for (source, key) in [
        ("-", "/abs/recovery.key"),
        ("/abs/export", "-"),
        ("relative/export", "/abs/recovery.key"),
        ("/abs/export", "recovery.key"),
    ] {
        let refused = service
            .handle(OperatorRequest::RecoveryRestore {
                source: source.into(),
                recovery_key_file: key.into(),
            })
            .unwrap_err();
        assert_eq!(refused.code, "request_invalid", "{source} {key}");
        let refused = service
            .handle(OperatorRequest::RecoveryExport {
                destination: source.into(),
                recovery_key_file: key.into(),
            })
            .unwrap_err();
        assert_eq!(refused.code, "request_invalid", "{source} {key}");
    }
    assert!(rig.calls.all().is_empty(), "the backend was never asked");
}

/// Restores the original cursor key, as the vault's credential restore does.
struct RestoringBackend {
    data: std::path::PathBuf,
    key: [u8; 32],
    calls: Arc<Calls>,
}

impl ServiceBackend for RestoringBackend {
    fn setup(&self, _: SetupMaterial, _: Option<i64>, _: bool) -> Result<Value, OperatorRefusal> {
        self.calls.push("setup");
        Ok(json!({}))
    }

    fn recovery_export(&self, _: &Path, _: &Path) -> Result<Value, OperatorRefusal> {
        self.calls.push("export");
        Ok(json!({}))
    }

    fn recovery_restore(&self, _: &Path, _: &Path) -> Result<Value, OperatorRefusal> {
        use std::os::unix::fs::OpenOptionsExt;
        self.calls.push("restore");
        let secrets = self.data.join("secrets");
        std::fs::create_dir_all(&secrets).expect("secrets");
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o700)).expect("mode");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(crate::legacy_owner_credentials::cursor_key_path(&self.data))
            .map_err(|_| OperatorRefusal::new("recovery_failed", "a key already exists"))?;
        std::io::Write::write_all(&mut file, &self.key).expect("key");
        Ok(json!({ "status": "restored", "cursorKeyIncluded": true }))
    }

    fn refused_credentials(&self) -> Result<Value, OperatorRefusal> {
        self.calls.push("refused_list");
        Ok(json!({}))
    }

    fn purge_refused_credentials(&self) -> Result<Value, OperatorRefusal> {
        self.calls.push("refused_purge");
        Ok(json!({}))
    }
}

/// Finding 2: after a data-only restore the service waits in its recovery
/// state. Nothing that could create a key or sign data runs; the credential
/// restore then writes the original key, and the API's key loader returns
/// that key instead of a new one.
#[tokio::test]
async fn collection_window_operator_recovery_state_restores_keys_before_any_is_created() {
    let rig = Rig::new();
    let data = rig.data();
    crate::db::HubStore::initialize(&data).expect("store");
    assert!(!super::authority::credential_recovery_pending(&data), "nothing signed yet");
    // A data-only restore: signed history, no cursor key.
    // The head alone is enough for the check; its parents are not needed.
    let connection = rusqlite::Connection::open(data.join("hub.sqlite")).expect("database");
    connection.execute_batch("PRAGMA foreign_keys = OFF;").expect("no parents");
    connection
        .execute(
            "INSERT INTO sync_heads (vehicle_id, base_snapshot_id, head_sequence, head_digest, terminal_cursor) VALUES ('v', 's', 1, ?1, 'c')",
            [&"a".repeat(64)],
        )
        .expect("signed head");
    drop(connection);
    assert!(super::authority::credential_recovery_pending(&data));

    let gate = super::authority::RecoveryGate::new(true);
    let original = [7_u8; 32];
    let kicks = rig.kicks.clone();
    let service = OperatorService::new(
        rig.controller.clone(),
        Arc::new(RestoringBackend {
            data: data.clone(),
            key: original,
            calls: rig.calls.clone(),
        }),
        Box::new(move || *kicks.lock().expect("kicks") += 1),
        TrustAuthority::new(rig.pin.clone(), HOST, rig.owner.clone()),
        &data,
    )
    .with_recovery(gate.clone());
    let _server = ChannelServer::bind(&data, Arc::new(service)).expect("channel bound");

    let status = rig.forward(&OperatorRequest::WindowStatus).expect("status");
    assert_eq!(status["credentialRecoveryPending"], true);
    for request in [
        OperatorRequest::WindowTrust { document: encoded(&rig.trust) },
        OperatorRequest::WindowOpen { grant: encoded(b"{}") },
        OperatorRequest::Setup { material: material(), vehicle_id: None, all_vehicles: true },
        OperatorRequest::RecoveryExport {
            destination: "/abs/export".into(),
            recovery_key_file: "/abs/recovery.key".into(),
        },
    ] {
        assert_eq!(rig.forward(&request).unwrap_err().code, "credential_recovery_pending");
    }
    assert!(rig.calls.all().is_empty());
    assert!(
        std::fs::symlink_metadata(crate::legacy_owner_credentials::cursor_key_path(&data)).is_err(),
        "no key was created while waiting"
    );

    rig.forward(&OperatorRequest::RecoveryRestore {
        source: "/abs/export".into(),
        recovery_key_file: "/abs/recovery.key".into(),
    })
    .expect("restore");
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.restored())
        .await
        .expect("the recovery state ended");
    assert!(!super::authority::credential_recovery_pending(&data));
    // The API now starts with the recovered key, not a new one.
    crate::legacy_owner_credentials::load_or_create_cursor_key(&data).expect("key loads");
    assert_eq!(
        std::fs::read(crate::legacy_owner_credentials::cursor_key_path(&data)).expect("key file"),
        original,
        "the API's key is the restored one"
    );
    let status = rig.forward(&OperatorRequest::WindowStatus).expect("status");
    assert_eq!(status["credentialRecoveryPending"], false);
    rig.forward(&OperatorRequest::WindowTrust { document: encoded(&rig.trust) })
        .expect("window verbs answer again");
}

/// M02-T11 acceptance 1 and 3: the purge command parses with a listing mode
/// and a confirmation flag, is forwarded to the running service, lists
/// without the confirmation (deleting nothing) and purges only once the
/// window is Closed, holding the import lock.
#[test]
fn refused_credential_purge_is_forwarded_lists_without_confirmation_and_waits_for_the_window() {
    for (arguments, confirm) in [
        (&["teslatlas-hub", "purge-refused-credentials"][..], false),
        (&["teslatlas-hub", "purge-refused-credentials", "--confirm"][..], true),
    ] {
        let cli = Cli::try_parse_from(arguments.iter().copied()).expect("parses");
        assert!(
            matches!(cli.command, Command::PurgeRefusedCredentials { confirm: parsed } if parsed == confirm),
            "{arguments:?}"
        );
        assert_eq!(admission::route(&cli.command), Route::Service, "forwarded to the service");
        assert!(!admission::requires_user_hub_admission(&cli.command), "never takes the lifetime lock");
    }
    assert!(Cli::try_parse_from(["teslatlas-hub", "purge-refused-credentials", "--yes"]).is_err());

    let rig = Rig::new();
    std::fs::create_dir_all(rig.data()).expect("data dir");
    let _server = rig.serve();
    let list = OperatorRequest::PurgeRefusedCredentials { confirm: false };
    let purge = OperatorRequest::PurgeRefusedCredentials { confirm: true };
    assert_eq!(rig.forward(&list).expect("listing")["mode"], "list");
    assert_eq!(rig.calls.all(), vec!["refused_list".to_owned()], "without --confirm only the listing runs");

    rig.forward(&OperatorRequest::WindowTrust {
        document: encoded(&rig.trust),
    })
    .expect("trust");
    rig.forward(&OperatorRequest::WindowOpen {
        grant: encoded(&rig.grant(600)),
    })
    .expect("open");
    let refused = rig.forward(&purge).unwrap_err();
    assert_eq!(refused.code, "window_active", "no purge inside a window");
    rig.forward(&list).expect("the listing is read-only, so it runs in a window");
    rig.forward(&OperatorRequest::WindowClose).expect("close");
    assert_eq!(rig.forward(&purge).unwrap_err().code, "window_active", "not while Closing");
    rig.controller.finish_close().expect("handback");
    assert_eq!(rig.forward(&purge).expect("purge")["mode"], "purge");
    assert_eq!(
        rig.calls.all(),
        vec!["refused_list".to_owned(), "refused_list".to_owned(), "refused_purge".to_owned()],
        "the purge reached the backend once, after the window closed"
    );
}

// Deprecated aliases (M05-T05). Names carry `deprecated_alias` so the
// milestone's filter runs them. An alias only maps an earlier name to a
// command that still exists; inputs removed on purpose stay refused.

const EARLIER_CHECK_VERB: &str = crate::runtime::legacy_names::DEPRECATED_VERB_SOURCE_CHECK;

fn check_arguments(command: &str, flag: &str) -> Vec<String> {
    let mut arguments = [
        "teslatlas-hub",
        command,
        "--source",
        "postgresql://reader@source.example/sourcedb",
        "--car-id",
        "42",
        "--postgres-password-file",
        "fictional-password",
        flag,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    if command == "migrate" {
        arguments.extend(["--history-only".to_owned(), "--online-snapshot".to_owned()]);
    }
    arguments
}

#[test]
fn deprecated_alias_schema_flag_works_warns_and_is_hidden() {
    for command in ["source-check", "migrate"] {
        let mut warnings = Vec::new();
        let cli = Cli::parse_with_schema_warning(
            check_arguments(command, "--acknowledge-v4-2-compatible-schema"),
            &mut warnings,
        )
        .expect("the earlier flag parses");
        match cli.command {
            Command::SourceCheck { accept_schema_evidence, .. }
            | Command::Migrate { accept_schema_evidence, .. } => assert!(accept_schema_evidence),
            _ => panic!("wrong command"),
        }
        let warnings = String::from_utf8(warnings).expect("utf8");
        assert_eq!(warnings.lines().count(), 1, "{warnings}");
        assert!(warnings.contains("--accept-schema-evidence"));
        assert!(warnings.contains("a later version"));
        let help = Cli::try_parse_from(["teslatlas-hub", command, "--help"])
            .unwrap_err()
            .to_string();
        assert!(help.contains("--accept-schema-evidence"));
        assert!(!help.contains("--acknowledge-v4-2-compatible-schema"));
    }
}

#[test]
fn deprecated_alias_check_verb_works_warns_once_and_is_hidden() {
    let mut earlier = check_arguments("source-check", "--accept-schema-evidence");
    earlier[1] = EARLIER_CHECK_VERB.to_owned();
    for with_config in [false, true] {
        let mut arguments = earlier.clone();
        if with_config {
            arguments.splice(1..1, ["--config".to_owned(), "fictional.toml".to_owned()]);
        }
        let mut warnings = Vec::new();
        let cli = Cli::parse_with_schema_warning(arguments, &mut warnings)
            .expect("the earlier verb parses");
        assert!(matches!(
            cli.command,
            Command::SourceCheck { car_id: 42, accept_schema_evidence: true, .. }
        ));
        assert_eq!(cli.config.is_some(), with_config);
        let warnings = String::from_utf8(warnings).expect("utf8");
        assert_eq!(warnings.lines().count(), 1, "{warnings}");
        assert!(warnings.contains(EARLIER_CHECK_VERB));
        assert!(warnings.contains("use source-check"));
        assert!(warnings.contains("a later version"));
    }
    // The verb and the earlier flag each warn once, together.
    let mut warnings = Vec::new();
    Cli::parse_with_schema_warning(
        check_arguments(EARLIER_CHECK_VERB, "--acknowledge-v4-2-compatible-schema"),
        &mut warnings,
    )
    .expect("both earlier names parse");
    assert_eq!(String::from_utf8(warnings).expect("utf8").lines().count(), 2);
    // The current name stays silent, and the earlier verb is not in help.
    let mut warnings = Vec::new();
    Cli::parse_with_schema_warning(
        check_arguments("source-check", "--accept-schema-evidence"),
        &mut warnings,
    )
    .expect("current name parses");
    assert!(warnings.is_empty());
    let help = Cli::try_parse_from(["teslatlas-hub", "--help"]).unwrap_err().to_string();
    assert!(help.contains("source-check"));
    assert!(!help.contains(EARLIER_CHECK_VERB), "{help}");
    let command_help = Cli::try_parse_from(["teslatlas-hub", "source-check", "--help"])
        .unwrap_err()
        .to_string();
    assert!(!command_help.contains(EARLIER_CHECK_VERB), "{command_help}");
}

#[test]
fn deprecated_alias_check_verb_keeps_removed_inputs_refused() {
    for removed in ["--access-token-file", "--refresh-token-file", "--encryption-key-file"] {
        let mut arguments = check_arguments(EARLIER_CHECK_VERB, "--accept-schema-evidence");
        arguments.extend([removed.to_owned(), "fictional-token".to_owned()]);
        let error = Cli::try_parse_from(arguments).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument, "{removed}");
    }
}
