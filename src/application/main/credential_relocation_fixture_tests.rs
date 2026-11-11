// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

#[cfg(not(feature = "credential-relocation-fixture"))]
#[test]
fn ordinary_binary_rejects_fixture_protocol() {
    for args in [
        vec!["hub", "--fixture-capability"],
        vec![
            "hub",
            "--fixture-context",
            "/synthetic/fixture-context.json",
            "status",
        ],
    ] {
        assert!(
            Cli::try_parse_from(args).is_err(),
            "ordinary grammar exposes no fixture capability or context"
        );
    }
}

#[cfg(feature = "credential-relocation-fixture")]
use super::credential_relocation_fixture as fixture;
#[cfg(feature = "credential-relocation-fixture")]
use std::{ffi::OsString, os::unix::fs::PermissionsExt};

#[cfg(feature = "credential-relocation-fixture")]
struct Harness {
    root: tempfile::TempDir,
    context: serde_json::Value,
}
#[cfg(feature = "credential-relocation-fixture")]
impl Harness {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("v2-fixture-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("synthetic root");
        let path = root.path().canonicalize().expect("canonical root");
        for name in ["data", "session", "service"] {
            fs::create_dir(path.join(name)).expect("synthetic directory");
            fs::set_permissions(path.join(name), fs::Permissions::from_mode(0o700))
                .expect("directory mode");
        }
        drop(HubStore::initialize(&path.join("data")).expect("synthetic store"));
        let run = uuid::Uuid::new_v4().simple().to_string();
        let label = format!("fixture.teslatlas-relocation.{run}");
        fs::write(path.join(".fixture-run"), format!("{run}\n")).expect("run marker");
        fs::write(
            path.join("hub.toml"),
            format!(
                "data_dir = {:?}\nbind = '127.0.0.1:18443'\n[collector]\ninterval_seconds = 0\n",
                path.join("data")
            ),
        )
        .expect("synthetic configuration");
        fs::write(
            path.join("recording-state.json"),
            br#"{"loaded":true,"disabled":false,"pid":900000001}"#,
        )
        .expect("recording state");
        fs::write(
            path.join("service").join(format!("{label}.plist")),
            b"synthetic plist",
        )
        .expect("synthetic plist");
        let context = serde_json::json!({
            "contract": "teslatlas-hub credential-relocation-fixture", "contract_version": 1,
            "contract_digest": "607c465b60da6019ae61e93c794d61e12aaa0e2cab762b283004ee1ff9f363ed",
            "run_id": run, "executable_kind": "fixture", "synthetic_root": path,
            "data_dir": path.join("data"), "config_path": path.join("hub.toml"), "session_dir": path.join("session"),
            "recording_state_path": path.join("recording-state.json"), "trace_path": path.join("trace.jsonl"),
            "service_identity": {"label": label, "domain": format!("fixture/{run}"), "plist_path": path.join("service").join(format!("{label}.plist"))},
            "permitted_endpoints": [], "permitted_faults": []
        });
        let harness = Self { root, context };
        harness.write_context();
        harness
    }
    fn path(&self, key: &str) -> PathBuf {
        PathBuf::from(self.context[key].as_str().expect("context path"))
    }
    fn context_path(&self) -> PathBuf {
        self.root
            .path()
            .canonicalize()
            .expect("canonical root")
            .join("fixture-context.json")
    }
    fn write_context(&self) {
        fs::write(
            self.context_path(),
            serde_json::to_vec(&self.context).expect("context JSON"),
        )
        .expect("context file");
        fs::set_permissions(self.context_path(), fs::Permissions::from_mode(0o600))
            .expect("private context");
    }
    fn arguments(&self, command: &[&str]) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("hub"),
            OsString::from("--fixture-context"),
            self.context_path().into_os_string(),
        ];
        args.extend(command.iter().map(|argument| OsString::from(*argument)));
        args
    }
    fn refuse(&self, command: &[&str]) -> &'static str {
        match fixture::prepare(self.arguments(command)) {
            Err(error) => error.0,
            Ok(_) => panic!("invalid fixture admitted"),
        }
    }
    fn execute(&self, command: &[&str], fault: bool) -> Result<(), Box<dyn std::error::Error>> {
        let prepared = if fault {
            fixture::prepare_fault_for_test(self.arguments(command))
        } else {
            fixture::prepare(self.arguments(command))
        }?;
        let fixture::Prepared::Operation(operation) = prepared else {
            panic!("operational command became capability")
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fixture runtime")
            .block_on(fixture::execute(operation))
    }
    fn trace(&self) -> Vec<serde_json::Value> {
        let lines = fs::read_to_string(self.path("trace_path")).expect("recorded trace");
        let rows: Vec<serde_json::Value> = lines
            .lines()
            .map(|line| serde_json::from_str(line).expect("trace JSON"))
            .collect();
        assert_eq!(
            rows.last(),
            Some(
                &serde_json::json!({"op":"run_end", "systemExecutorSelected":false, "ownerTargetSubmitted":false, "childrenSpawned":0})
            )
        );
        for secret in ["access-0001", "refresh-0001", "fleet-refresh-0001"] {
            assert!(!lines.contains(secret));
        }
        rows
    }
    fn counts(&self) -> Vec<i64> {
        let connection = rusqlite::Connection::open(self.path("data_dir").join("hub.sqlite"))
            .expect("synthetic SQL");
        [
            "credential_pairs",
            "credential_refresh_fences",
            "credential_refresh_attempts",
            "credential_key_usage",
        ]
        .iter()
        .map(|table| {
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count")
        })
        .collect()
    }
}

#[cfg(feature = "credential-relocation-fixture")]
#[test]
fn fixture_context_is_required_before_effects() {
    assert!(matches!(
        fixture::prepare(vec!["hub".into(), "status".into()]),
        Err(fixture::Refusal("fixture_context_missing"))
    ));
    let mut harness = Harness::new();
    assert!(harness.root.path().exists());
    let database = fs::read(harness.path("data_dir").join("hub.sqlite")).expect("database");
    let pristine = harness.context.clone();
    for key in pristine.as_object().expect("object").keys() {
        harness.context = pristine.clone();
        harness.context.as_object_mut().expect("object").remove(key);
        harness.write_context();
        assert_eq!(
            harness.refuse(&["status"]),
            "fixture_context_malformed",
            "{key}"
        );
    }
    harness.context = pristine.clone();
    harness.context["unexpected"] = true.into();
    harness.write_context();
    assert_eq!(harness.refuse(&["status"]), "fixture_context_malformed");
    harness.context = pristine.clone();
    harness.context["contract_digest"] = "incorrect".into();
    harness.write_context();
    assert_eq!(harness.refuse(&["status"]), "fixture_context_mismatch");
    harness.context = pristine;
    harness.write_context();
    fs::set_permissions(harness.context_path(), fs::Permissions::from_mode(0o644))
        .expect("unsafe mode");
    assert_eq!(harness.refuse(&["status"]), "fixture_context_unsafe");
    assert!(!harness.path("trace_path").exists());
    assert_eq!(
        fs::read(harness.path("data_dir").join("hub.sqlite")).expect("database"),
        database
    );
}

#[cfg(feature = "credential-relocation-fixture")]
#[test]
fn fixture_paths_cannot_escape_synthetic_root() {
    use std::os::unix::fs::symlink;
    for key in [
        "data_dir",
        "config_path",
        "trace_path",
        "session_dir",
        "recording_state_path",
    ] {
        let mut harness = Harness::new();
        let escape = harness
            .path("synthetic_root")
            .join("data")
            .join("..")
            .join("escape");
        harness.context[key] = escape.to_string_lossy().into_owned().into();
        harness.write_context();
        assert_eq!(
            harness.refuse(&["status"]),
            "fixture_path_escape",
            "lexical {key}"
        );
        let mut harness = Harness::new();
        let target = harness.path(key);
        let link = harness.path("synthetic_root").join("linked");
        symlink(&target, &link).expect("synthetic link");
        harness.context[key] = link.to_string_lossy().into_owned().into();
        harness.write_context();
        assert_eq!(
            harness.refuse(&["status"]),
            "fixture_path_escape",
            "link {key}"
        );
        let mut harness = Harness::new();
        harness.context[key] = "/synthetic-outside/refused".into();
        harness.write_context();
        assert_eq!(
            harness.refuse(&["status"]),
            "fixture_path_escape",
            "outside {key}"
        );
    }
}

#[cfg(feature = "credential-relocation-fixture")]
#[test]
fn fixture_sign_out_uses_recording_service_backend() {
    use teslatlas_hub::auth::credential_sign_in as credentials;
    let harness = Harness::new();
    let data = harness.path("data_dir");
    let owner = credentials::seed_owner_pair_for_test(&data, "access-0001", "refresh-0001", None)
        .expect("owner seed");
    let fleet = credentials::seed_fleet_pair_for_test(
        &data,
        &teslatlas_hub::fleet_credentials::FleetSetupCredentials::new(
            "e30.eyJzY3AiOlsidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iXX0.sig".into(),
            "fleet-refresh-0001".into(),
            "client-0001".into(),
            teslatlas_hub::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            3600,
        )
        .expect("fleet pair"),
    )
    .expect("fleet seed");
    credentials::begin_refresh_for_test(&data, false, owner).expect("owner fence");
    credentials::begin_refresh_for_test(&data, true, fleet).expect("fleet fence");
    let before = harness.counts();
    let key_path = data.join("secrets").join("credential-keys");
    let key = fs::read(&key_path).expect("keyring");
    harness
        .execute(&["control", "sign-out"], false)
        .expect("real sign-out");
    let after = harness.counts();
    assert_eq!(after[0], 0);
    assert_eq!(&after[1..], &before[1..]);
    assert_eq!(fs::read(key_path).expect("retained keyring"), key);
    let trace = harness.trace();
    let ops: Vec<&str> = trace.iter().filter_map(|row| row["op"].as_str()).collect();
    assert_eq!(
        &ops[..3],
        &["disable", "bootout", "unload_wait_observation"]
    );
    assert!(
        ops[3..ops.len() - 1]
            .iter()
            .all(|op| *op == "secrets_exclusion")
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(harness.path("recording_state_path")).expect("state"))
            .expect("state JSON");
    assert_eq!(
        state,
        serde_json::json!({"loaded":false,"disabled":true,"pid":null})
    );
}

#[cfg(feature = "credential-relocation-fixture")]
#[test]
fn fixture_service_queries_have_no_system_fallback() {
    use teslatlas_hub::platform::{
        r78_lifecycle::{ServiceBudgets, ServiceEffects},
        r78_recording::{
            FixtureBackendGuard, RecordingBackend, RecordingState, SyntheticServiceIdentity,
            with_fixture_backend,
        },
        r78_service::InstalledServiceEffects,
    };
    let harness = Harness::new();
    harness
        .execute(&["status"], false)
        .expect("status through real dispatch");
    let trace = harness.trace();
    assert_eq!(
        trace
            .iter()
            .filter_map(|row| row["op"].as_str())
            .collect::<Vec<_>>(),
        ["loaded_query", "pid_query", "plist_decode", "run_end"]
    );
    let identity: SyntheticServiceIdentity =
        serde_json::from_value(harness.context["service_identity"].clone()).expect("identity");
    let backend = RecordingBackend::open(
        &harness.path("data_dir"),
        identity,
        RecordingState {
            loaded: true,
            disabled: false,
            pid: Some(900_000_001),
        },
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(harness.path("recording_state_path"))
            .expect("state"),
        fs::File::create(harness.path("synthetic_root").join("queries.jsonl")).expect("trace"),
        harness.context["run_id"].as_str().expect("run"),
    )
    .expect("recorder");
    let mut guard = FixtureBackendGuard::install(backend).expect("select recorder");
    let mut service =
        InstalledServiceEffects::new(&harness.path("data_dir"), ServiceBudgets::SCALED_TEST);
    assert!(
        service
            .loaded(Instant::now() + Duration::from_secs(1))
            .expect("recorded loaded query")
    );
    assert_eq!(
        with_fixture_backend(|backend| backend.pid_query()).expect("recorded PID"),
        Some(900_000_001)
    );
    assert!(with_fixture_backend(|backend| backend.readiness_query()).expect("recorded readiness"));
    guard.finish().expect("end");
    drop(guard);
    assert!(with_fixture_backend(|backend| backend.loaded_query()).is_err());
    assert!(
        service
            .loaded(Instant::now() + Duration::from_secs(1))
            .is_err(),
        "removed backend cannot select a system executor"
    );
}

#[cfg(feature = "credential-relocation-fixture")]
#[test]
fn fixture_rejects_owner_service_identity() {
    for field in ["label", "domain", "plist_path"] {
        let mut harness = Harness::new();
        harness.context["service_identity"][field] = match field {
            "label" => "com.teslatlas.hub".into(),
            "domain" => format!("gui/{}", rustix::process::geteuid().as_raw()).into(),
            _ => std::env::var_os("HOME")
                .map(PathBuf::from)
                .expect("home for refusal only")
                .join("Library")
                .join("LaunchAgents")
                .join("com.teslatlas.hub.plist")
                .to_string_lossy()
                .into_owned()
                .into(),
        };
        harness.write_context();
        assert_eq!(
            harness.refuse(&["status"]),
            "fixture_owner_identity_refused"
        );
        assert!(!harness.path("trace_path").exists());
    }
}

#[cfg(feature = "credential-relocation-fixture")]
#[test]
fn fixture_rejects_unapproved_child_or_command() {
    for command in [
        vec!["serve"],
        vec!["observe"],
        vec!["setup", "--sign-in"],
        vec!["setup-fleet", "--sign-in"],
        vec!["control", "wake", "--confirm"],
        vec!["control", "rotate-credential-key"],
        vec!["install"],
        vec!["start"],
        vec!["status", "--config", "/synthetic/refused"],
    ] {
        let harness = Harness::new();
        assert_eq!(harness.refuse(&command), "fixture_command_refused");
        assert!(!harness.path("trace_path").exists());
    }
    let error = teslatlas_hub::platform::r78_service::sleeping_helper_for_test(
        Instant::now() + Duration::from_secs(1),
    )
    .expect_err("feature disables all helper children");
    assert_eq!(error.to_string(), "fixture_child_refused");
}

#[cfg(feature = "credential-relocation-fixture")]
#[test]
fn fixture_fault_mode_retains_service_isolation() {
    use teslatlas_hub::auth::credential_sign_in as credentials;
    let mut harness = Harness::new();
    let data = harness.path("data_dir");
    credentials::seed_owner_pair_for_test(&data, "access-0001", "refresh-0001", None)
        .expect("owner seed");
    let candidate = data
        .join("secrets")
        .join("pre-upgrade-owner-api.relocated.key");
    fs::write(&candidate, b"synthetic-retained-key").expect("neutral candidate");
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600)).expect("private candidate");
    harness.context["executable_kind"] = "fault".into();
    harness.context["permitted_faults"] = serde_json::json!(["durability_probe_failure"]);
    harness.write_context();
    assert_eq!(
        harness.refuse(&["bootstrap"]),
        "fixture_context_mismatch",
        "ordinary fixture kind cannot claim to be the fault executable"
    );
    harness
        .execute(&["bootstrap"], true)
        .expect("read-only vault open");
    assert!(candidate.exists(), "fault never removes neutral material");
    let projection = credentials::credential_status(
        &HubStore::open_read_only(&data).expect("status"),
        &data,
        1_800_000_000_000,
    )
    .expect("projection");
    assert_eq!(projection.credential_store.durability, Some("unavailable"));
    assert!(projection.legacy_credentials.present);
    assert!(
        harness
            .trace()
            .iter()
            .filter_map(|row| row["op"].as_str())
            .all(|op| matches!(op, "secrets_exclusion" | "run_end"))
    );
}
