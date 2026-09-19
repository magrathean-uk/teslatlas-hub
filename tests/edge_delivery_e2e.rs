// SPDX-License-Identifier: AGPL-3.0-only
//! Real Edge process, mTLS transport, production Hub consumer, and SQLite store.

use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    net::TcpListener,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer,
    KeyPair, KeyUsagePurpose,
};
use serde_json::{Value, json};
#[cfg(not(feature = "edge-test-faults"))]
use teslatlas_hub::db::{EdgeBinding, VerifiedEdgeItem, VerifiedEdgeRecord};
use teslatlas_hub::{
    config::EdgeCollectorConfig,
    db::{HubStore, SourceDescriptor},
    edge_delivery::{EdgeConsumer, EdgeDeliveryError},
};

#[path = "interop/seed.rs"]
#[allow(dead_code)]
mod seed;

const VIN: &str = "5YJ3E1EA7KF000001";
const STABLE_ID: &str = "37b6ac0f3288d84fc45bea42e645d8fdc44e4ba796e7e66891573487a9e19fca";
const PROCESS_STABLE_ID: &str = "9f69b3f155de02e48e6bf497306022f5fe117e69a6e023d48498c7cfef0794c5";
const UNSUPPORTED_STABLE_ID: &str =
    "9aa7740cfa754eda4b022c85024570f7b641cfab90fe22371d2c2679179f3f5d";
const UNSUPPORTED_PAYLOAD_SHA256: &str =
    "469a95a6e10a6ded5929d9f5e8a871b0d76726852019eca9b41c66b0096a8e44";
const GAP_SOURCE_STABLE_ID: &str =
    "f05a103c7e5e24b14a60935de35d3021a6b047a984bd0392dd17569e16c2b0dd";
const GAP_SOURCE_LEGACY_ID: &str =
    "fb77b0ccac7be19ac00d7e3754aaa50277e61cb48a41b68f69530f57d28328aa";
const GAP_EVIDENCE_SHA256: &str =
    "f04c2080bb9f7877a39003b0ad9cc458431e47e7af9777e96ae0c6b9b3290c6f";
const GAP_NOTICE_ID: &str = "bd331d62a7d1cbe93a7eb3258b0469b9aaa7fa690c74f18b7d1cb4559c9150e1";
const STALE_SOC_STABLE_ID: &str =
    "3151fed2cdfd5fb4998b8bd4d274de75111e18049f7e89a52cecdc3ddb9e0cdb";
const GAP_BATCH_ID: &str = "8aab0c0de41572fc0cb41d5f0f9685832d5206e13be93e9aae0464ed28704a85";

struct Authority {
    certificate: Certificate,
    issuer: Issuer<'static, KeyPair>,
}

struct Leaf {
    certificate_pem: String,
    key_pem: String,
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn unused_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
#[ignore = "requires an explicitly authorized empty-spool Edge input file"]
async fn external_empty_spool_edge_consumer_probe_uses_one_v2_pull() {
    let config_path = std::env::var_os("TESLATLAS_EDGE_EMPTY_SPOOL_PROBE_CONFIG")
        .expect("authorized private probe config path");
    let config: EdgeCollectorConfig =
        serde_json::from_slice(&fs::read(config_path).expect("authorized private probe config"))
            .expect("valid Edge collector config");
    let temporary = tempfile::tempdir().expect("private temporary store");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("empty Hub store");
    let consumer = EdgeConsumer::from_config(&config, Duration::from_secs(900))
        .expect("configured Edge consumer");

    let report = consumer.poll_once(&store).await.expect("empty Edge batch");

    assert!(!report.batch_id.is_empty());
    assert!(report.accepted.is_empty());
    assert_eq!(
        store
            .edge_ledger_counts(&config.installation_id, &config.lineage)
            .expect("zero Edge ledger"),
        teslatlas_hub::db::EdgeLedgerCounts {
            applications: 0,
            sequences: 0,
            pending_publications: 0,
            ack_frontier: None,
        }
    );
}

fn private_file(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).unwrap();
    fs::set_permissions(destination, fs::metadata(source).unwrap().permissions()).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().unwrap();
        assert!(
            !file_type.is_symlink(),
            "parity fixture contains no symlinks"
        );
        if file_type.is_dir() {
            copy_tree(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).unwrap();
            fs::set_permissions(
                &destination_path,
                fs::metadata(&source_path).unwrap().permissions(),
            )
            .unwrap();
        }
    }
}

fn authority() -> Authority {
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let key = KeyPair::generate().unwrap();
    let certificate = params.self_signed(&key).unwrap();
    Authority {
        certificate,
        issuer: Issuer::new(params, key),
    }
}

fn leaf(authority: &Authority, server: bool) -> Leaf {
    let names = if server {
        vec!["localhost".to_owned(), "127.0.0.1".to_owned()]
    } else {
        Vec::new()
    };
    let mut params = CertificateParams::new(names).unwrap();
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![if server {
        ExtendedKeyUsagePurpose::ServerAuth
    } else {
        ExtendedKeyUsagePurpose::ClientAuth
    }];
    let key = KeyPair::generate().unwrap();
    let certificate = params.signed_by(&key, &authority.issuer).unwrap();
    Leaf {
        certificate_pem: certificate.pem(),
        key_pem: key.serialize_pem(),
    }
}

fn edge_binary() -> PathBuf {
    std::env::var_os("TESLATLAS_EDGE_BIN").map_or_else(
        || {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../teslatlas-edge/target/debug/teslatlas-edge")
        },
        PathBuf::from,
    )
}

fn run(edge: &Path, config: &Path, args: &[&str]) -> std::process::Output {
    let output = Command::new(edge)
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Edge {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn envelope(received_at_ms: i64) -> Value {
    json!({
        "version": 1,
        "vin": VIN,
        "txid": "task9-projected-0001",
        "tx_type": "V",
        "received_at_ms": received_at_ms,
        "timestamp_ms": 1_788_566_400_000_i64,
        "payload": {
            "vin": VIN,
            "createdAt": "2026-09-05T00:00:00Z",
            "data": {"Soc": {"intValue": "81"}}
        }
    })
}

fn process_envelope() -> Value {
    json!({
        "version": 1,
        "vin": VIN,
        "txid": "task9-process-0002",
        "tx_type": "V",
        "received_at_ms": 1_788_566_401_100_i64,
        "timestamp_ms": 1_788_566_401_000_i64,
        "payload": {
            "vin": VIN,
            "createdAt": "2026-09-05T00:00:01Z",
            "data": {"Soc": {"intValue": "82"}}
        }
    })
}

fn unsupported_envelope() -> Value {
    json!({
        "version": 1,
        "vin": VIN,
        "txid": "task9-unsupported-0003",
        "tx_type": "V",
        "received_at_ms": 1_788_566_402_100_i64,
        "timestamp_ms": 1_788_566_401_500_i64,
        "payload": {"futureObject": {"futureValue": "bounded-public-event"}}
    })
}

fn delayed_temperature_envelope() -> Value {
    json!({
        "version": 1,
        "vin": VIN,
        "txid": "task9-delayed-temperature-0004",
        "tx_type": "V",
        "received_at_ms": 1_788_566_402_200_i64,
        "timestamp_ms": 1_788_566_399_000_i64,
        "payload": {
            "vin": VIN,
            "createdAt": "2026-09-04T23:59:59Z",
            "data": {"InsideTemp": {"doubleValue": 22.75}}
        }
    })
}

fn gap_source_envelope() -> Value {
    json!({
        "version": 1,
        "vin": VIN,
        "txid": "task9-gap-source-0009",
        "tx_type": "V",
        "received_at_ms": 1_788_566_404_200_i64,
        "timestamp_ms": 1_788_566_404_150_i64,
        "payload": {
            "vin": VIN,
            "createdAt": "2026-09-05T00:00:04.150Z",
            "data": {"Soc": {"intValue": "83"}}
        }
    })
}

fn stale_soc_envelope() -> Value {
    json!({
        "version": 1,
        "vin": VIN,
        "txid": "task9-stale-soc-0010",
        "tx_type": "V",
        "received_at_ms": 1_788_566_405_000_i64,
        "timestamp_ms": 1_788_566_400_500_i64,
        "payload": {
            "vin": VIN,
            "createdAt": "2026-09-05T00:00:00.500Z",
            "data": {"Soc": {"intValue": "70"}}
        }
    })
}

fn pack_envelope(txid: &str, received_at_ms: i64, timestamp_ms: i64, field: &str) -> Value {
    let data = match field {
        "PackVoltage" => json!({"PackVoltage": {"doubleValue": 400.0}}),
        "PackCurrent" => json!({"PackCurrent": {"doubleValue": 10.0}}),
        _ => unreachable!(),
    };
    json!({
        "version": 1,
        "vin": VIN,
        "txid": txid,
        "tx_type": "V",
        "received_at_ms": received_at_ms,
        "timestamp_ms": timestamp_ms,
        "payload": {
            "vin": VIN,
            "createdAt": "2026-09-05T00:00:03Z",
            "data": data
        }
    })
}

fn actual_parity_envelopes() -> Vec<Value> {
    [
        (
            "task9-parity-soc-1",
            1_788_566_412_100_i64,
            1_788_566_412_000_i64,
            json!({"Soc": {"intValue": "80"}}),
        ),
        (
            "task9-parity-delayed-temp-2",
            1_788_566_412_200_i64,
            1_788_566_411_000_i64,
            json!({"InsideTemp": {"doubleValue": 22.75}}),
        ),
        (
            "task9-parity-voltage-3",
            1_788_566_413_100_i64,
            1_788_566_413_000_i64,
            json!({"PackVoltage": {"doubleValue": 400.0}}),
        ),
        (
            "task9-parity-stale-soc-4",
            1_788_566_413_200_i64,
            1_788_566_411_500_i64,
            json!({"Soc": {"intValue": "70"}}),
        ),
        (
            "task9-parity-current-5",
            1_788_566_413_300_i64,
            1_788_566_413_100_i64,
            json!({"PackCurrent": {"doubleValue": 10.0}}),
        ),
    ]
    .into_iter()
    .map(|(txid, received_at_ms, timestamp_ms, data)| {
        json!({
            "version": 1,
            "vin": VIN,
            "txid": txid,
            "tx_type": "V",
            "received_at_ms": received_at_ms,
            "timestamp_ms": timestamp_ms,
            "payload": {
                "vin": VIN,
                "createdAt": "2026-09-05T00:00:10Z",
                "data": data,
            }
        })
    })
    .collect()
}

fn install_edge_config(
    edge_root: &Path,
    receiver_port: u16,
    delivery_port: u16,
    batch_max_records: usize,
) {
    let config = format!(
        "version = 1\nstate_directory = {state:?}\nreceiver_bind = {receiver:?}\nhub_bind = {delivery:?}\nreceiver_bearer_path = {receiver_token:?}\nspool_key_path = {spool_key:?}\ncredential_store_path = {credentials:?}\nhub_server_certificate_path = {server_cert:?}\nhub_server_private_key_path = {server_key:?}\nhub_client_ca_path = {client_ca:?}\n\n[spool]\nmax_bytes = 1048576\nmax_records = 8\nretention_seconds = 604800\nbatch_max_bytes = 262144\nbatch_max_records = {batch_max_records}\n",
        state = edge_root.join("state"),
        receiver = format!("127.0.0.1:{receiver_port}"),
        delivery = format!("0.0.0.0:{delivery_port}"),
        receiver_token = edge_root.join("state/receiver-token"),
        spool_key = edge_root.join("state/spool-key"),
        credentials = edge_root.join("state/credentials.json"),
        server_cert = edge_root.join("server.crt"),
        server_key = edge_root.join("server.key"),
        client_ca = edge_root.join("client-ca.crt"),
    );
    fs::write(edge_root.join("config.toml"), config).unwrap();
    fs::set_permissions(
        edge_root.join("config.toml"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
}

fn install_hub_config(
    prepared: &seed::PreparedFixture,
    fixture_root: &Path,
    edge_root: &Path,
    hub_port: u16,
    delivery_port: u16,
    source_id: uuid::Uuid,
) {
    let hub_config = format!(
        "data_dir = {data:?}\nbind = {bind:?}\n[tls]\ncertificate_path = {cert:?}\nprivate_key_path = {key:?}\npublic_url = {endpoint:?}\n[collector]\nprovider = 'fleet'\ninterval_seconds = 0\n[collector.edge]\nbase_url = {edge_url:?}\nca_certificate_path = {edge_ca:?}\nclient_certificate_path = {edge_cert:?}\nclient_private_key_path = {edge_key:?}\nbearer_token_path = {edge_token:?}\ninstallation_id = 'task9-edge'\nlineage = 'spool-2026-09-05'\nsource_id = {source_id_string:?}\nvehicle_id = '11111111-1111-4111-8111-111111111111'\nvin = '5YJ3E1EA7KF000001'\ncar_id = 9\npoll_milliseconds = 20\ntimeout_seconds = 3\nmax_backoff_seconds = 1\n[terrain]\nenabled = false\n[geocoder]\nenabled = false\n",
        data = fixture_root.join("hub"),
        bind = format!("127.0.0.1:{hub_port}"),
        cert = prepared.certificate_path,
        key = fixture_root.join("server-key.pem"),
        endpoint = prepared.endpoint,
        edge_url = format!("https://127.0.0.1:{delivery_port}/"),
        edge_ca = edge_root.join("server-ca.crt"),
        edge_cert = edge_root.join("client.crt"),
        edge_key = edge_root.join("client.key"),
        edge_token = edge_root.join("delivery-token"),
        source_id_string = source_id.to_string(),
    );
    fs::write(&prepared.config_path, hub_config).unwrap();
    fs::set_permissions(&prepared.config_path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn spawn_hub(config_path: &Path, log_path: &Path) -> ChildGuard {
    let log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(log_path)
        .unwrap();
    let error_log = log.try_clone().unwrap();
    ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_teslatlas-hub"))
            .arg("--config")
            .arg(config_path)
            .arg("serve")
            .stdout(log)
            .stderr(error_log)
            .spawn()
            .unwrap(),
    )
}

async fn stop_hub(hub: &mut ChildGuard) {
    Command::new("kill")
        .args(["-TERM", &hub.0.id().to_string()])
        .status()
        .unwrap();
    for _ in 0..200 {
        if hub.0.try_wait().unwrap().is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("Hub ignored SIGTERM");
}

async fn wait_for_edge_frontier(store: &HubStore, target: u64, require_published: bool) {
    for _ in 0..500 {
        let counts = store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap();
        if counts.ack_frontier == Some(target)
            && (!require_published || counts.pending_publications == 0)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("actual parity Hub did not reach frontier {target}");
}

async fn run_actual_parity_lane(
    lane_root: &Path,
    edge: &Path,
    template: &seed::PreparedFixture,
    source_id: uuid::Uuid,
    restart_after_each_record: bool,
) -> Value {
    let edge_root = lane_root.join("edge");
    let fixture_root = lane_root.join("hub-fixture");
    let receiver_port = unused_port();
    let delivery_port = unused_port();
    let hub_port = unused_port();
    install_edge_config(&edge_root, receiver_port, delivery_port, 1);
    let prepared = seed::PreparedFixture {
        schema_version: template.schema_version,
        config_path: fixture_root.join("config.toml"),
        certificate_path: fixture_root.join("server.pem"),
        invitation_path: fixture_root.join("invitation.json"),
        hub_id: template.hub_id,
        source_id,
        endpoint: format!("https://localhost:{hub_port}"),
        vehicle_ids: template.vehicle_ids,
    };
    install_hub_config(
        &prepared,
        &fixture_root,
        &edge_root,
        hub_port,
        delivery_port,
        source_id,
    );
    let hub_config = fs::read_to_string(&prepared.config_path)
        .unwrap()
        .replace("poll_milliseconds = 20", "poll_milliseconds = 1000");
    fs::write(&prepared.config_path, hub_config).unwrap();
    fs::set_permissions(&prepared.config_path, fs::Permissions::from_mode(0o600)).unwrap();

    let mut edge_process = ChildGuard(
        Command::new(edge)
            .arg("--config")
            .arg(edge_root.join("config.toml"))
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let receiver = reqwest::Client::new();
    let receiver_url = format!("http://127.0.0.1:{receiver_port}/healthz");
    for _ in 0..200 {
        if receiver.get(&receiver_url).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(edge_process.0.try_wait().unwrap().is_none());

    let store = HubStore::initialize(fixture_root.join("hub")).unwrap();
    if restart_after_each_record {
        for target in 1..=5 {
            let log_path = lane_root.join(format!("hub-restart-{target}.log"));
            let mut hub = spawn_hub(&prepared.config_path, &log_path);
            wait_for_edge_frontier(&store, target, false).await;
            stop_hub(&mut hub).await;
        }
        let log_path = lane_root.join("hub-final-publication.log");
        let mut hub = spawn_hub(&prepared.config_path, &log_path);
        wait_for_edge_frontier(&store, 5, true).await;
        stop_hub(&mut hub).await;
    } else {
        let log_path = lane_root.join("hub-uninterrupted.log");
        let mut hub = spawn_hub(&prepared.config_path, &log_path);
        wait_for_edge_frontier(&store, 5, true).await;
        stop_hub(&mut hub).await;
    }

    let root = reqwest::Certificate::from_pem(&fs::read(edge_root.join("server-ca.crt")).unwrap())
        .unwrap();
    let identity = reqwest::Identity::from_pem(
        [
            fs::read(edge_root.join("client.crt")).unwrap(),
            fs::read(edge_root.join("client.key")).unwrap(),
        ]
        .concat()
        .as_slice(),
    )
    .unwrap();
    let delivery = reqwest::Client::builder()
        .https_only(true)
        .tls_certs_only([root])
        .identity(identity)
        .build()
        .unwrap();
    let delivery_token = fs::read_to_string(edge_root.join("delivery-token")).unwrap();
    let batch: Value = delivery
        .get(format!(
            "https://127.0.0.1:{delivery_port}/v2/hub/batches/next"
        ))
        .bearer_auth(delivery_token.trim())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(batch["records"], json!([]));
    assert_eq!(batch["gaps"], json!([]));
    edge_process.0.kill().unwrap();
    let _ = edge_process.0.wait().unwrap();

    let vehicle_id = template.vehicle_ids[0];
    let current = store
        .current_observations_for_vehicle(vehicle_id)
        .unwrap()
        .into_iter()
        .filter(|row| row.payload["record_type"] == "fleet_api_vehicle_data_v1")
        .map(|row| {
            json!({
                "observation_id": row.observation_id,
                "source_id": row.source_id,
                "vehicle_id": row.vehicle_id,
                "observed_at_ms": row.observed_at_ms,
                "received_at_ms": row.received_at_ms,
                "payload_sha256": row.payload_sha256.to_string(),
                "payload": row.payload,
            })
        })
        .collect::<Vec<_>>();
    let retained_observations = store
        .observations_for_vehicle(
            vehicle_id,
            teslatlas_hub::db::ObservationQuery::from_start(100),
        )
        .unwrap()
        .into_iter()
        .map(|row| {
            json!({
                "observation_id": row.observation_id,
                "source_id": row.source_id,
                "vehicle_id": row.vehicle_id,
                "observed_at_ms": row.observed_at_ms,
                "received_at_ms": row.received_at_ms,
                "payload_sha256": row.payload_sha256.to_string(),
                "payload": row.payload,
            })
        })
        .collect::<Vec<_>>();
    let lifecycle = store.load_lifecycle_state(vehicle_id).unwrap().map(|row| {
        json!({
            "vehicle_id": row.vehicle_id,
            "car_id": row.car_id,
            "last_observation_id": row.last_observation_id,
            "open_session_json": row.open_session_json,
            "quarantined": row.quarantined,
            "updated_at_ms": row.updated_at_ms,
        })
    });
    let connection = rusqlite::Connection::open(store.database_path()).unwrap();
    let accumulator: Vec<u8> = connection
        .query_row(
            "SELECT state_json FROM edge_accumulator_states
              WHERE installation_id = 'task9-edge' AND lineage = 'spool-2026-09-05'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let rows = |sql: &str, width: usize| -> Vec<Value> {
        let mut statement = connection.prepare(sql).unwrap();
        statement
            .query_map([], |row| {
                let mut values = Vec::with_capacity(width);
                for column in 0..width {
                    let value = match row.get_ref(column)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(value) => Value::from(value),
                        rusqlite::types::ValueRef::Real(value) => Value::from(value),
                        rusqlite::types::ValueRef::Text(value) => {
                            Value::String(String::from_utf8(value.to_vec()).unwrap())
                        }
                        rusqlite::types::ValueRef::Blob(value) => {
                            Value::Array(value.iter().copied().map(Value::from).collect::<Vec<_>>())
                        }
                    };
                    values.push(value);
                }
                Ok(Value::Array(values))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let applications = rows(
        "SELECT stable_record_id, payload_sha256, disposition,
                first_spool_seq, observation_id
           FROM edge_applications
          WHERE installation_id = 'task9-edge' AND lineage = 'spool-2026-09-05'
          ORDER BY first_spool_seq",
        5,
    );
    let dispositions = rows(
        "SELECT spool_seq, item_kind, item_id, legacy_record_id,
                payload_sha256, category, reason, occurred_at_ms, evidence_sha256
           FROM edge_sequence_dispositions
          WHERE installation_id = 'task9-edge' AND lineage = 'spool-2026-09-05'
          ORDER BY spool_seq",
        9,
    );
    let materialised_states = rows(
        "SELECT state_id, car_id, state_json FROM materialised_states
          WHERE vehicle_id = '11111111-1111-4111-8111-111111111111' ORDER BY state_id",
        3,
    );
    let open_rows = rows(
        "SELECT source_id, source_table, source_row_id, vehicle_id, car_id,
                domain, parent_source_row_id, row_json
           FROM lifecycle_open_rows
          WHERE vehicle_id = '11111111-1111-4111-8111-111111111111'
          ORDER BY source_table, source_row_id",
        8,
    );
    let sync_mutations = rows(
        "SELECT revision, entity, entity_id, car_id, operation,
                payload_json, published, claimed_until_ms
           FROM sync_mutations
          WHERE vehicle_id = '11111111-1111-4111-8111-111111111111'
          ORDER BY revision",
        8,
    );
    let counts = store
        .edge_ledger_counts("task9-edge", "spool-2026-09-05")
        .unwrap();
    json!({
        "current": current,
        "retained_observations": retained_observations,
        "lifecycle": lifecycle,
        "accumulator": serde_json::from_slice::<Value>(&accumulator).unwrap(),
        "applications": applications,
        "dispositions": dispositions,
        "materialised_states": materialised_states,
        "open_rows": open_rows,
        "sync_mutations": sync_mutations,
        "counts": {
            "applications": counts.applications,
            "sequences": counts.sequences,
            "pending_publications": counts.pending_publications,
            "ack_frontier": counts.ack_frontier,
        },
    })
}

#[cfg(feature = "edge-test-faults")]
async fn expect_instrumented_hub_crash(
    root: &Path,
    config_path: &Path,
    point: &str,
    fail_publication_once: bool,
) {
    let log_path = root.join(format!("hub-{point}.log"));
    let witness_path = root.join(format!("hub-{point}.witness"));
    let log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&log_path)
        .unwrap();
    let error_log = log.try_clone().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_teslatlas-hub"));
    command
        .arg("--config")
        .arg(config_path)
        .arg("serve")
        .env("TESLATLAS_EDGE_TEST_ABORT_POINT", point)
        .env("TESLATLAS_EDGE_TEST_WITNESS_PATH", &witness_path)
        .stdout(log)
        .stderr(error_log);
    if fail_publication_once {
        command
            .env("TESLATLAS_EDGE_TEST_FAIL_PUBLICATION_ONCE", "1")
            .env(
                "TESLATLAS_EDGE_TEST_PUBLICATION_WITNESS_PATH",
                root.join("hub-publication-failure.witness"),
            );
    }
    let mut child = command.spawn().unwrap();
    for _ in 0..400 {
        if let Some(status) = child.try_wait().unwrap() {
            let output = fs::read_to_string(&log_path).unwrap_or_default();
            assert!(
                !status.success(),
                "instrumented Hub did not crash at {point}"
            );
            assert_eq!(
                fs::read_to_string(&witness_path).unwrap_or_default(),
                point,
                "instrumented Hub exited before {point}: {output}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!(
        "instrumented Hub did not reach {point}: {}",
        fs::read_to_string(log_path).unwrap_or_default()
    );
}

#[cfg(feature = "edge-test-faults")]
async fn expect_instrumented_hub_returned_error(
    root: &Path,
    config_path: &Path,
    point: &str,
) -> ChildGuard {
    let log_path = root.join(format!("hub-returned-{point}.log"));
    let witness_path = root.join(format!("hub-returned-{point}.witness"));
    let log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&log_path)
        .unwrap();
    let error_log = log.try_clone().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_teslatlas-hub"))
        .arg("--config")
        .arg(config_path)
        .arg("serve")
        .env("TESLATLAS_EDGE_TEST_RETURN_ERROR_AT", point)
        .env(
            "TESLATLAS_EDGE_TEST_RETURN_ERROR_WITNESS_PATH",
            &witness_path,
        )
        .stdout(log)
        .stderr(error_log)
        .spawn()
        .unwrap();
    let mut hub = ChildGuard(child);
    for _ in 0..400 {
        if witness_path.is_file() {
            assert_eq!(fs::read_to_string(&witness_path).unwrap(), point);
            assert!(
                hub.0.try_wait().unwrap().is_none(),
                "returned-error Hub exited at {point}: {}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
            return hub;
        }
        if let Some(status) = hub.0.try_wait().unwrap() {
            panic!(
                "returned-error Hub exited {status} before {point}: {}",
                fs::read_to_string(&log_path).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "returned-error Hub did not reach {point}: {}",
        fs::read_to_string(log_path).unwrap_or_default()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_uninterrupted_and_restarted_sparse_delivery_have_complete_parity() {
    teslatlas_hub::crypto::install_default_provider();
    let temporary = tempfile::tempdir().unwrap();
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let base_edge = temporary.path().join("base-edge");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&base_edge)
        .unwrap();
    let server_ca = authority();
    let server = leaf(&server_ca, true);
    let client_ca = authority();
    let client = leaf(&client_ca, false);
    fs::write(base_edge.join("server-ca.crt"), server_ca.certificate.pem()).unwrap();
    fs::write(base_edge.join("server.crt"), &server.certificate_pem).unwrap();
    private_file(
        base_edge.join("server.key").as_path(),
        server.key_pem.as_bytes(),
    );
    fs::write(base_edge.join("client-ca.crt"), client_ca.certificate.pem()).unwrap();
    fs::write(base_edge.join("client.crt"), &client.certificate_pem).unwrap();
    private_file(
        base_edge.join("client.key").as_path(),
        client.key_pem.as_bytes(),
    );
    let template_receiver_port = unused_port();
    let template_delivery_port = unused_port();
    install_edge_config(
        &base_edge,
        template_receiver_port,
        template_delivery_port,
        1,
    );
    let edge = edge_binary();
    assert!(edge.is_file());
    run(&edge, &base_edge.join("config.toml"), &["init"]);
    let issued: Value = serde_json::from_slice(
        &run(
            &edge,
            &base_edge.join("config.toml"),
            &["credential", "enrol", "task9-parity-hub"],
        )
        .stdout,
    )
    .unwrap();
    private_file(
        &base_edge.join("delivery-token"),
        issued["token"].as_str().unwrap().as_bytes(),
    );
    let receiver_token = fs::read_to_string(base_edge.join("state/receiver-token")).unwrap();
    let mut template_edge = ChildGuard(
        Command::new(&edge)
            .arg("--config")
            .arg(base_edge.join("config.toml"))
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let receiver = reqwest::Client::new();
    let receiver_url = format!("http://127.0.0.1:{template_receiver_port}/healthz");
    for _ in 0..200 {
        if receiver.get(&receiver_url).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(template_edge.0.try_wait().unwrap().is_none());
    let ingest_url =
        format!("http://127.0.0.1:{template_receiver_port}/v1/internal/fleet-telemetry");
    for envelope in actual_parity_envelopes() {
        let response = receiver
            .post(&ingest_url)
            .bearer_auth(receiver_token.trim())
            .json(&envelope)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    }
    template_edge.0.kill().unwrap();
    let _ = template_edge.0.wait().unwrap();

    let base_fixture_root = temporary.path().join("base-hub-fixture");
    let template = seed::prepare(&base_fixture_root, unused_port()).unwrap();
    let base_store = HubStore::initialize(base_fixture_root.join("hub")).unwrap();
    let source = base_store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            1_788_566_400_000,
        )
        .unwrap();
    base_store
        .open()
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    drop(base_store);

    let uninterrupted_root = temporary.path().join("actual-uninterrupted");
    let restarted_root = temporary.path().join("actual-restarted");
    for lane in [&uninterrupted_root, &restarted_root] {
        fs::DirBuilder::new().mode(0o700).create(lane).unwrap();
        copy_tree(&base_edge, &lane.join("edge"));
        copy_tree(&base_fixture_root, &lane.join("hub-fixture"));
    }
    let uninterrupted = run_actual_parity_lane(
        &uninterrupted_root,
        &edge,
        &template,
        source.source_id,
        false,
    )
    .await;
    let restarted =
        run_actual_parity_lane(&restarted_root, &edge, &template, source.source_id, true).await;
    assert_eq!(restarted, uninterrupted);
    assert_eq!(restarted["counts"]["ack_frontier"], 5);
    assert_eq!(restarted["counts"]["pending_publications"], 0);
    assert_eq!(
        restarted["dispositions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row[2].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "220093f44465aea9823e7ffa41dea302d5b8e098ebf91aa0b2e3ab60f8ddf537",
            "952823be057d16907e062c4aa61994fe744c8d5af7521b8f85fe6c3da985640f",
            "8527da3ed220d4772221d173ef2b9eddf779243de80496db1fb351f550225a37",
            "72fb72ae8902122cf93fc134f25553216114d85228ee90dec5ead4a9cbcca2cf",
            "e46439a04a461b335835bb494619afd97ae07fce2a920d6da1fd3ae5f6bdfa5f",
        ]
    );
    assert_eq!(
        restarted["current"][0]["payload"]["provider_raw_json"]["response"]["charge_state"]["usable_battery_level"],
        80
    );
    assert_eq!(
        restarted["current"][0]["payload"]["provider_raw_json"]["response"]["climate_state"]["inside_temp"],
        22.75
    );
    assert_eq!(
        restarted["current"][0]["payload"]["provider_raw_json"]["response"]["drive_state"]["power"],
        -4.0
    );
    assert_eq!(
        restarted["accumulator"]["field_watermarks"]["soc"],
        1_788_566_412_000_i64
    );
    assert_eq!(
        restarted["accumulator"]["field_watermarks"]["insidetemp"],
        1_788_566_411_000_i64
    );
    assert_eq!(
        restarted["accumulator"]["pack_voltage"],
        json!({"value": 400.0, "timestamp_ms": 1_788_566_413_000_i64})
    );
    assert_eq!(
        restarted["accumulator"]["pack_current"],
        json!({"value": 10.0, "timestamp_ms": 1_788_566_413_100_i64})
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_edge_mtls_delivery_replay_and_reenqueue_are_durable() {
    teslatlas_hub::crypto::install_default_provider();
    let temporary = tempfile::tempdir().unwrap();
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let edge_root = temporary.path().join("edge");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&edge_root)
        .unwrap();

    let server_ca = authority();
    let server = leaf(&server_ca, true);
    let client_ca = authority();
    let client = leaf(&client_ca, false);
    fs::write(edge_root.join("server-ca.crt"), server_ca.certificate.pem()).unwrap();
    fs::write(edge_root.join("server.crt"), &server.certificate_pem).unwrap();
    private_file(
        edge_root.join("server.key").as_path(),
        server.key_pem.as_bytes(),
    );
    fs::write(edge_root.join("client-ca.crt"), client_ca.certificate.pem()).unwrap();
    fs::write(edge_root.join("client.crt"), &client.certificate_pem).unwrap();
    private_file(
        edge_root.join("client.key").as_path(),
        client.key_pem.as_bytes(),
    );

    let receiver_port = unused_port();
    let delivery_port = unused_port();
    let edge_config_path = edge_root.join("config.toml");
    private_file(
        &edge_config_path,
        format!(
            "version = 1\nstate_directory = {state:?}\nreceiver_bind = {receiver:?}\nhub_bind = {delivery:?}\nreceiver_bearer_path = {receiver_token:?}\nspool_key_path = {spool_key:?}\ncredential_store_path = {credentials:?}\nhub_server_certificate_path = {server_cert:?}\nhub_server_private_key_path = {server_key:?}\nhub_client_ca_path = {client_ca:?}\n\n[spool]\nmax_bytes = 1048576\nmax_records = 8\nretention_seconds = 604800\nbatch_max_bytes = 262144\nbatch_max_records = 8\n",
            state = edge_root.join("state"),
            receiver = format!("127.0.0.1:{receiver_port}"),
            delivery = format!("0.0.0.0:{delivery_port}"),
            receiver_token = edge_root.join("state/receiver-token"),
            spool_key = edge_root.join("state/spool-key"),
            credentials = edge_root.join("state/credentials.json"),
            server_cert = edge_root.join("server.crt"),
            server_key = edge_root.join("server.key"),
            client_ca = edge_root.join("client-ca.crt"),
        )
        .as_bytes(),
    );
    let edge = edge_binary();
    assert!(
        edge.is_file(),
        "build the actual Edge binary at {}",
        edge.display()
    );
    run(&edge, &edge_config_path, &["init"]);
    let issued: Value = serde_json::from_slice(
        &run(
            &edge,
            &edge_config_path,
            &["credential", "enrol", "task9-hub"],
        )
        .stdout,
    )
    .unwrap();
    let delivery_token = issued["token"].as_str().unwrap().to_owned();
    let receiver_token = fs::read_to_string(edge_root.join("state/receiver-token")).unwrap();
    let child = Command::new(&edge)
        .arg("--config")
        .arg(&edge_config_path)
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut edge_process = ChildGuard(child);

    let receiver = reqwest::Client::new();
    let receiver_url = format!("http://127.0.0.1:{receiver_port}/healthz");
    for _ in 0..100 {
        if receiver.get(&receiver_url).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(edge_process.0.try_wait().unwrap().is_none());

    let fixture_root = temporary.path().join("hub-fixture");
    let hub_port = unused_port();
    let prepared = seed::prepare(&fixture_root, hub_port).unwrap();
    let store = HubStore::initialize(fixture_root.join("hub")).unwrap();
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            1_788_566_400_000,
        )
        .unwrap();
    let binding_vehicle = uuid::Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
    let token_path = edge_root.join("delivery-token");
    private_file(&token_path, delivery_token.as_bytes());
    let config = EdgeCollectorConfig {
        base_url: format!("https://127.0.0.1:{delivery_port}/"),
        ca_certificate_path: edge_root.join("server-ca.crt"),
        client_certificate_path: edge_root.join("client.crt"),
        client_private_key_path: edge_root.join("client.key"),
        bearer_token_path: token_path,
        installation_id: "task9-edge".to_owned(),
        lineage: "spool-2026-09-05".to_owned(),
        source_id: source.source_id,
        vehicle_id: binding_vehicle,
        vin: VIN.to_owned(),
        car_id: 9,
        poll_milliseconds: 20,
        timeout_seconds: 3,
        max_backoff_seconds: 1,
    };
    let consumer = EdgeConsumer::from_config(&config, Duration::from_secs(900)).unwrap();
    install_hub_config(
        &prepared,
        &fixture_root,
        &edge_root,
        hub_port,
        delivery_port,
        source.source_id,
    );

    let ingest_url = format!("http://127.0.0.1:{receiver_port}/v1/internal/fleet-telemetry");
    let response = receiver
        .post(&ingest_url)
        .bearer_auth(receiver_token.trim())
        .json(&envelope(1_788_566_400_100))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

    let root = reqwest::Certificate::from_pem(&fs::read(edge_root.join("server-ca.crt")).unwrap())
        .unwrap();
    let identity = reqwest::Identity::from_pem(
        format!("{}{}", client.certificate_pem, client.key_pem).as_bytes(),
    )
    .unwrap();
    let raw_delivery = reqwest::Client::builder()
        .https_only(true)
        .tls_certs_only([root])
        .identity(identity)
        .build()
        .unwrap();
    let batch: Value = raw_delivery
        .get(format!(
            "https://127.0.0.1:{delivery_port}/v2/hub/batches/next"
        ))
        .bearer_auth(&delivery_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(batch["records"][0]["record_id"], STABLE_ID);
    assert_eq!(batch["records"][0]["spool_seq"], 1);
    #[cfg(feature = "edge-test-faults")]
    {
        for point in [
            "raw_insert",
            "lifecycle_write",
            "receipt_insert",
            "frontier_update",
            "commit",
        ] {
            let mut returned_error_hub = expect_instrumented_hub_returned_error(
                temporary.path(),
                &prepared.config_path,
                point,
            )
            .await;
            assert_eq!(
                store
                    .edge_ledger_counts("task9-edge", "spool-2026-09-05")
                    .unwrap(),
                teslatlas_hub::db::EdgeLedgerCounts {
                    applications: 0,
                    sequences: 0,
                    pending_publications: 0,
                    ack_frontier: None,
                },
                "returned {point} error must roll back every durable acceptance effect"
            );
            let redelivery: Value = raw_delivery
                .get(format!(
                    "https://127.0.0.1:{delivery_port}/v2/hub/batches/next"
                ))
                .bearer_auth(&delivery_token)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(redelivery["records"][0]["spool_seq"], 1, "{point}");
            assert_eq!(redelivery["records"][0]["record_id"], STABLE_ID, "{point}");
            stop_hub(&mut returned_error_hub).await;
        }
        expect_instrumented_hub_crash(
            temporary.path(),
            &prepared.config_path,
            "before_accept_commit",
            false,
        )
        .await;
        assert_eq!(
            store
                .edge_ledger_counts("task9-edge", "spool-2026-09-05")
                .unwrap(),
            teslatlas_hub::db::EdgeLedgerCounts {
                applications: 0,
                sequences: 0,
                pending_publications: 0,
                ack_frontier: None,
            }
        );
        assert_eq!(
            raw_delivery
                .get(format!(
                    "https://127.0.0.1:{delivery_port}/v2/hub/batches/next"
                ))
                .bearer_auth(&delivery_token)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()["records"][0]["spool_seq"],
            1
        );
        expect_instrumented_hub_crash(
            temporary.path(),
            &prepared.config_path,
            "after_accept_commit_before_ack",
            false,
        )
        .await;
        let committed = store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap();
        assert_eq!(committed.applications, 1);
        assert_eq!(committed.sequences, 1);
        assert_eq!(committed.pending_publications, 1);
        assert_eq!(committed.ack_frontier, Some(1));
        assert_eq!(
            raw_delivery
                .get(format!(
                    "https://127.0.0.1:{delivery_port}/v2/hub/batches/next"
                ))
                .bearer_auth(&delivery_token)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()["records"][0]["spool_seq"],
            1
        );
        expect_instrumented_hub_crash(
            temporary.path(),
            &prepared.config_path,
            "after_ack_accepted_before_response",
            true,
        )
        .await;
        let after_lost_response: Value = raw_delivery
            .get(format!(
                "https://127.0.0.1:{delivery_port}/v2/hub/batches/next"
            ))
            .bearer_auth(&delivery_token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(after_lost_response["records"], json!([]));
        assert_eq!(after_lost_response["gaps"], json!([]));
        assert_eq!(
            store
                .edge_ledger_counts("task9-edge", "spool-2026-09-05")
                .unwrap()
                .pending_publications,
            1
        );
        assert_eq!(
            fs::read_to_string(temporary.path().join("hub-publication-failure.witness")).unwrap(),
            "publication_failed"
        );
    }
    #[cfg(not(feature = "edge-test-faults"))]
    {
        let accepted_without_ack = store
            .accept_verified_edge_item(
                &EdgeBinding {
                    installation_id: "task9-edge".to_owned(),
                    lineage: "spool-2026-09-05".to_owned(),
                    source_id: source.source_id,
                    vehicle_id: binding_vehicle,
                    vin: VIN.to_owned(),
                    car_id: 9,
                },
                &VerifiedEdgeItem::Record(VerifiedEdgeRecord {
                    spool_seq: 1,
                    stable_record_id: STABLE_ID.to_owned(),
                    legacy_record_id:
                        "c0c8716b98cf72be50ea7cdd6868b946b4220a3a2cb0425e5e05bda66dd751d0"
                            .to_owned(),
                    payload_sha256:
                        "9458aec30d7b58765cd3aa3d6ae8b0aa6c9150e43cbb351616766bc18e76b806"
                            .to_owned(),
                    edge_received_at_ms: 1_788_566_400_100,
                    envelope_json: serde_json::to_vec(&envelope(1_788_566_400_100)).unwrap(),
                    non_projection_reason: None,
                }),
                1_788_566_402_000,
                Duration::from_secs(900),
            )
            .unwrap();
        assert_eq!(accepted_without_ack.ack_frontier, 1);
        assert_eq!(
            store
                .edge_ledger_counts("task9-edge", "spool-2026-09-05")
                .unwrap()
                .applications,
            1
        );
    }

    let pack_count_before_recovery = store.v2_lineage_pack_count(binding_vehicle).unwrap();
    edge_process.0.kill().unwrap();
    let _ = edge_process.0.wait().unwrap();
    let recovery_log_path = temporary
        .path()
        .join("hub-offline-publication-recovery.log");
    let mut recovery_hub = spawn_hub(&prepared.config_path, &recovery_log_path);
    let mut publication_recovered = false;
    for _ in 0..300 {
        if store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap()
            .pending_publications
            == 0
            && store.v2_lineage_pack_count(binding_vehicle).unwrap() > pack_count_before_recovery
        {
            publication_recovered = true;
            break;
        }
        if let Some(status) = recovery_hub.0.try_wait().unwrap() {
            panic!(
                "publication-recovery Hub exited {status}: {}",
                fs::read_to_string(&recovery_log_path).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        publication_recovered,
        "restarted Hub must publish durable pending work while Edge remains offline"
    );
    stop_hub(&mut recovery_hub).await;
    assert_eq!(
        store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap()
            .pending_publications,
        0,
        "durable publication must recover while Edge remains offline"
    );
    edge_process = ChildGuard(
        Command::new(&edge)
            .arg("--config")
            .arg(&edge_config_path)
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    for _ in 0..100 {
        if receiver.get(&receiver_url).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(edge_process.0.try_wait().unwrap().is_none());
    let replay = consumer.poll_once(&store).await.unwrap();
    #[cfg(not(feature = "edge-test-faults"))]
    {
        assert_eq!(replay.accepted.len(), 1);
        assert_eq!(replay.accepted[0].item_id, STABLE_ID);
        assert_eq!(replay.accepted[0].category, "projected_telemetry");
    }
    #[cfg(feature = "edge-test-faults")]
    assert!(
        replay.accepted.is_empty(),
        "Edge already accepted the deliberately lost ACK response"
    );
    assert!(
        consumer
            .poll_once(&store)
            .await
            .unwrap()
            .accepted
            .is_empty()
    );

    let bad_token_path = edge_root.join("bad-delivery-token");
    private_file(&bad_token_path, b"tte1.invalid.invalid");
    let bad_config = EdgeCollectorConfig {
        bearer_token_path: bad_token_path,
        ..config.clone()
    };
    let bad_consumer = EdgeConsumer::from_config(&bad_config, Duration::from_secs(900)).unwrap();
    assert!(matches!(
        bad_consumer.poll_once(&store).await,
        Err(EdgeDeliveryError::Authentication)
    ));
    assert_eq!(
        store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap()
            .ack_frontier,
        Some(1)
    );

    let untrusted_client_ca = authority();
    let untrusted_client = leaf(&untrusted_client_ca, false);
    let untrusted_certificate_path = edge_root.join("untrusted-client.crt");
    let untrusted_key_path = edge_root.join("untrusted-client.key");
    fs::write(
        &untrusted_certificate_path,
        &untrusted_client.certificate_pem,
    )
    .unwrap();
    private_file(&untrusted_key_path, untrusted_client.key_pem.as_bytes());
    let untrusted_config = EdgeCollectorConfig {
        client_certificate_path: untrusted_certificate_path,
        client_private_key_path: untrusted_key_path,
        ..config.clone()
    };
    let untrusted_consumer =
        EdgeConsumer::from_config(&untrusted_config, Duration::from_secs(900)).unwrap();
    assert!(matches!(
        untrusted_consumer.poll_once(&store).await,
        Err(EdgeDeliveryError::Transport)
    ));
    assert_eq!(
        store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap()
            .ack_frontier,
        Some(1)
    );

    let response = receiver
        .post(&ingest_url)
        .bearer_auth(receiver_token.trim())
        .json(&envelope(1_788_566_400_200))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    let duplicate = consumer.poll_once(&store).await.unwrap();
    assert_eq!(duplicate.accepted.len(), 1);
    assert_eq!(duplicate.accepted[0].item_id, STABLE_ID);
    assert_eq!(duplicate.accepted[0].category, "duplicate");
    let counts = store
        .edge_ledger_counts("task9-edge", "spool-2026-09-05")
        .unwrap();
    assert_eq!(counts.applications, 1);
    assert_eq!(counts.sequences, 2);
    assert_eq!(counts.ack_frontier, Some(2));
    let observations = store
        .current_observations_for_vehicle(binding_vehicle)
        .unwrap();
    let edge_observation = observations
        .iter()
        .find(|observation| observation.payload["record_type"] == "fleet_api_vehicle_data_v1")
        .expect("Edge current observation");
    assert_eq!(
        edge_observation.payload["provider_raw_json"]["response"]["charge_state"]["usable_battery_level"],
        81
    );

    let response = receiver
        .post(&ingest_url)
        .bearer_auth(receiver_token.trim())
        .json(&process_envelope())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    let hub_log_path = temporary.path().join("hub-first.log");
    let mut hub_process = spawn_hub(&prepared.config_path, &hub_log_path);
    let mut applied = false;
    for _ in 0..200 {
        let counts = store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap();
        if counts.applications == 2 && counts.sequences == 3 && counts.ack_frontier == Some(3) {
            applied = true;
            break;
        }
        if let Some(status) = hub_process.0.try_wait().unwrap() {
            panic!(
                "Hub exited {status}: {}",
                fs::read_to_string(&hub_log_path).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        applied,
        "actual Hub process did not durably consume sequence 3"
    );
    for _ in 0..200 {
        if store
            .edge_diagnostic("task9-edge", "spool-2026-09-05")
            .unwrap()
            .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let status = Command::new(env!("CARGO_BIN_EXE_teslatlas-hub"))
        .arg("--config")
        .arg(&prepared.config_path)
        .arg("status")
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["edgeDelivery"]["configured"], true);
    assert_eq!(status["edgeDelivery"]["ackFrontier"], 3);
    assert!(
        matches!(
            status["edgeDelivery"]["state"].as_str(),
            Some("connected" | "pending_publication")
        ),
        "status: {status}"
    );
    stop_hub(&mut hub_process).await;
    let observations = store
        .current_observations_for_vehicle(binding_vehicle)
        .unwrap();
    let edge_observation = observations
        .iter()
        .find(|observation| observation.payload["record_type"] == "fleet_api_vehicle_data_v1")
        .unwrap();
    assert_eq!(
        edge_observation.payload["provider_raw_json"]["response"]["charge_state"]["usable_battery_level"],
        82
    );

    for next in [
        unsupported_envelope(),
        delayed_temperature_envelope(),
        pack_envelope(
            "task9-pack-voltage-0005",
            1_788_566_403_100,
            1_788_566_403_000,
            "PackVoltage",
        ),
    ] {
        let response = receiver
            .post(&ingest_url)
            .bearer_auth(receiver_token.trim())
            .json(&next)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    }

    let second_hub_log_path = temporary.path().join("hub-second.log");
    let mut second_hub = spawn_hub(&prepared.config_path, &second_hub_log_path);
    let mut applied_through_six = false;
    for _ in 0..300 {
        let counts = store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap();
        if counts.applications == 5 && counts.sequences == 6 && counts.ack_frontier == Some(6) {
            applied_through_six = true;
            break;
        }
        if let Some(status) = second_hub.0.try_wait().unwrap() {
            panic!(
                "restarted Hub exited {status}: {}",
                fs::read_to_string(&second_hub_log_path).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        applied_through_six,
        "restarted Hub did not durably consume unsupported and delayed sequences"
    );
    stop_hub(&mut second_hub).await;

    let catalogue = rusqlite::Connection::open(store.database_path()).unwrap();
    let unsupported_disposition: (String, String, String, String) = catalogue
        .query_row(
            "SELECT category, reason, item_id, payload_sha256
               FROM edge_sequence_dispositions
              WHERE installation_id = 'task9-edge'
                AND lineage = 'spool-2026-09-05' AND spool_seq = 4",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(unsupported_disposition.0, "durable_non_projection_event");
    assert_eq!(unsupported_disposition.1, "projection_unsupported");
    assert_eq!(unsupported_disposition.2, UNSUPPORTED_STABLE_ID);
    assert_eq!(unsupported_disposition.3, UNSUPPORTED_PAYLOAD_SHA256);
    drop(catalogue);

    let observations = store
        .current_observations_for_vehicle(binding_vehicle)
        .unwrap();
    let edge_observation = observations
        .iter()
        .find(|observation| observation.payload["record_type"] == "fleet_api_vehicle_data_v1")
        .unwrap();
    assert_eq!(edge_observation.observed_at_ms, 1_788_566_403_000);
    assert_eq!(
        edge_observation.payload["provider_raw_json"]["response"]["charge_state"]["usable_battery_level"],
        82
    );
    assert_eq!(
        edge_observation.payload["provider_raw_json"]["response"]["climate_state"]["inside_temp"],
        22.75
    );

    for next in [
        pack_envelope(
            "task9-pack-current-0006",
            1_788_566_403_200,
            1_788_566_403_100,
            "PackCurrent",
        ),
        envelope(1_788_566_404_000),
    ] {
        let response = receiver
            .post(&ingest_url)
            .bearer_auth(receiver_token.trim())
            .json(&next)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    }

    let third_hub_log_path = temporary.path().join("hub-third.log");
    let mut third_hub = spawn_hub(&prepared.config_path, &third_hub_log_path);
    let mut applied_through_eight = false;
    for _ in 0..400 {
        let counts = store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap();
        if counts.applications == 6
            && counts.sequences == 8
            && counts.pending_publications == 0
            && counts.ack_frontier == Some(8)
        {
            applied_through_eight = true;
            break;
        }
        if let Some(status) = third_hub.0.try_wait().unwrap() {
            panic!(
                "second restarted Hub exited {status}: {}",
                fs::read_to_string(&third_hub_log_path).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        applied_through_eight,
        "second restarted Hub did not publish and ACK the final sparse sequence"
    );

    let hub_root =
        reqwest::Certificate::from_pem(&fs::read(&prepared.certificate_path).unwrap()).unwrap();
    let public_client = reqwest::Client::builder()
        .https_only(true)
        .tls_certs_only([hub_root])
        .build()
        .unwrap();
    for _ in 0..200 {
        if public_client
            .get(format!("{}/healthz", prepared.endpoint))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let invitation: Value =
        serde_json::from_slice(&fs::read(&prepared.invitation_path).unwrap()).unwrap();
    let claim: Value = public_client
        .post(format!(
            "{}/v1/pairings/{}/claim",
            prepared.endpoint,
            invitation["pairing_id"].as_str().unwrap()
        ))
        .json(&json!({
            "secret": invitation["secret"],
            "device_name": "Task 9 Edge process witness"
        }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let public_current: Value = public_client
        .get(format!(
            "{}/v1/vehicles/{binding_vehicle}/current",
            prepared.endpoint
        ))
        .bearer_auth(claim["access_token"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(public_current["usable_battery_level"], 82);
    assert_eq!(public_current["inside_temp"], 22.75);
    assert_eq!(public_current["power"], -4.0);

    let observations = store
        .current_observations_for_vehicle(binding_vehicle)
        .unwrap();
    let edge_observation = observations
        .iter()
        .find(|observation| observation.payload["record_type"] == "fleet_api_vehicle_data_v1")
        .unwrap();
    assert_eq!(edge_observation.observed_at_ms, 1_788_566_403_100);
    assert_eq!(
        edge_observation.payload["provider_raw_json"]["response"]["charge_state"]["usable_battery_level"],
        82
    );
    assert_eq!(
        edge_observation.payload["provider_raw_json"]["response"]["climate_state"]["inside_temp"],
        22.75
    );
    assert_eq!(
        edge_observation.payload["provider_raw_json"]["response"]["drive_state"]["power"],
        -4.0
    );
    let catalogue = rusqlite::Connection::open(store.database_path()).unwrap();
    let mut sequence_query = catalogue
        .prepare(
            "SELECT spool_seq, category, item_id
               FROM edge_sequence_dispositions
              WHERE installation_id = 'task9-edge' AND lineage = 'spool-2026-09-05'
              ORDER BY spool_seq",
        )
        .unwrap();
    let dispositions = sequence_query
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(dispositions.len(), 8);
    assert_eq!(
        dispositions
            .iter()
            .map(|(sequence, category, _)| (*sequence, category.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (1, "projected_telemetry"),
            (2, "duplicate"),
            (3, "projected_telemetry"),
            (4, "durable_non_projection_event"),
            (5, "projected_telemetry"),
            (6, "projected_telemetry"),
            (7, "projected_telemetry"),
            (8, "duplicate"),
        ]
    );
    assert_eq!(dispositions[0].2, STABLE_ID);
    assert_eq!(dispositions[1].2, STABLE_ID);
    assert_eq!(dispositions[2].2, PROCESS_STABLE_ID);
    assert_eq!(dispositions[3].2, UNSUPPORTED_STABLE_ID);
    assert_eq!(dispositions[7].2, STABLE_ID);
    drop(sequence_query);
    drop(catalogue);
    stop_hub(&mut third_hub).await;

    let response = receiver
        .post(&ingest_url)
        .bearer_auth(receiver_token.trim())
        .json(&gap_source_envelope())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    let gap_source_batch: Value = raw_delivery
        .get(format!(
            "https://127.0.0.1:{delivery_port}/v2/hub/batches/next"
        ))
        .bearer_auth(&delivery_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(gap_source_batch["records"][0]["spool_seq"], 9);
    assert_eq!(
        gap_source_batch["records"][0]["record_id"],
        GAP_SOURCE_STABLE_ID
    );
    edge_process.0.kill().unwrap();
    let _ = edge_process.0.wait().unwrap();
    let pending_directory = edge_root.join("state/spool/pending");
    let pending = fs::read_dir(&pending_directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(
        pending.len(),
        1,
        "only sequence 9 may be pending before corruption"
    );
    assert!(
        pending[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(GAP_SOURCE_LEGACY_ID),
        "pending file must carry the independently expected sequence-9 legacy ID"
    );
    let mut pending_file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&pending[0])
        .unwrap();
    let last_offset = pending_file.metadata().unwrap().len() - 1;
    pending_file.seek(SeekFrom::Start(last_offset)).unwrap();
    let mut last = [0_u8; 1];
    pending_file.read_exact(&mut last).unwrap();
    pending_file.seek(SeekFrom::Start(last_offset)).unwrap();
    pending_file.write_all(&[last[0] ^ 0xff]).unwrap();
    pending_file.sync_all().unwrap();
    drop(pending_file);

    edge_process = ChildGuard(
        Command::new(&edge)
            .arg("--config")
            .arg(&edge_config_path)
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    for _ in 0..100 {
        if receiver.get(&receiver_url).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(edge_process.0.try_wait().unwrap().is_none());
    let response = receiver
        .post(&ingest_url)
        .bearer_auth(receiver_token.trim())
        .json(&stale_soc_envelope())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

    let interleaved: Value = raw_delivery
        .get(format!(
            "https://127.0.0.1:{delivery_port}/v2/hub/batches/next"
        ))
        .bearer_auth(&delivery_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(interleaved["gaps"][0]["spool_seq"], 9);
    assert_eq!(interleaved["gaps"][0]["reason"], "integrity_quarantine");
    assert_eq!(
        interleaved["gaps"][0]["evidence_sha256"],
        GAP_EVIDENCE_SHA256
    );
    assert_eq!(interleaved["gaps"][0]["notice_id"], GAP_NOTICE_ID);
    assert_eq!(interleaved["records"][0]["spool_seq"], 10);
    assert_eq!(interleaved["records"][0]["record_id"], STALE_SOC_STABLE_ID);
    assert_eq!(interleaved["batch_id"], GAP_BATCH_ID);

    let later_only = raw_delivery
        .post(format!("https://127.0.0.1:{delivery_port}/v2/hub/acks"))
        .bearer_auth(&delivery_token)
        .json(&json!({
            "version": 2,
            "batch_id": GAP_BATCH_ID,
            "accepted_record_ids": [STALE_SOC_STABLE_ID],
            "accepted_gap_notice_ids": [],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(later_only.status(), reqwest::StatusCode::BAD_REQUEST);
    let after_rejected_ack: Value = raw_delivery
        .get(format!(
            "https://127.0.0.1:{delivery_port}/v2/hub/batches/next"
        ))
        .bearer_auth(&delivery_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after_rejected_ack["batch_id"], GAP_BATCH_ID);
    assert_eq!(after_rejected_ack["gaps"][0]["notice_id"], GAP_NOTICE_ID);
    assert_eq!(
        after_rejected_ack["records"][0]["record_id"],
        STALE_SOC_STABLE_ID
    );

    let fourth_hub_log_path = temporary.path().join("hub-gap.log");
    let mut fourth_hub = spawn_hub(&prepared.config_path, &fourth_hub_log_path);
    let mut applied_gap_and_later = false;
    for _ in 0..400 {
        let counts = store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap();
        if counts.applications == 7
            && counts.sequences == 10
            && counts.pending_publications == 0
            && counts.ack_frontier == Some(10)
        {
            applied_gap_and_later = true;
            break;
        }
        if let Some(status) = fourth_hub.0.try_wait().unwrap() {
            panic!(
                "gap Hub exited {status}: {}",
                fs::read_to_string(&fourth_hub_log_path).unwrap_or_default()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        applied_gap_and_later,
        "actual Hub did not durably accept the gap before the later record"
    );
    stop_hub(&mut fourth_hub).await;

    let observations = store
        .current_observations_for_vehicle(binding_vehicle)
        .unwrap();
    let edge_observation = observations
        .iter()
        .find(|observation| observation.payload["record_type"] == "fleet_api_vehicle_data_v1")
        .unwrap();
    assert_eq!(edge_observation.observed_at_ms, 1_788_566_403_100);
    assert_eq!(
        edge_observation.payload["provider_raw_json"]["response"]["charge_state"]["usable_battery_level"],
        82,
        "the stale same-field sequence must not overwrite Soc"
    );
    let catalogue = rusqlite::Connection::open(store.database_path()).unwrap();
    let gap_row: (String, String, i64, String) = catalogue
        .query_row(
            "SELECT item_id, reason, occurred_at_ms, evidence_sha256
               FROM edge_sequence_dispositions
              WHERE installation_id = 'task9-edge'
                AND lineage = 'spool-2026-09-05' AND spool_seq = 9",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(gap_row.0, GAP_NOTICE_ID);
    assert_eq!(gap_row.1, "integrity_quarantine");
    assert!(gap_row.2 > 0);
    assert_eq!(gap_row.3, GAP_EVIDENCE_SHA256);
    let mut sequence_query = catalogue
        .prepare(
            "SELECT spool_seq, category, item_id
               FROM edge_sequence_dispositions
              WHERE installation_id = 'task9-edge' AND lineage = 'spool-2026-09-05'
              ORDER BY spool_seq",
        )
        .unwrap();
    let dispositions = sequence_query
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(dispositions.len(), 10);
    assert_eq!(
        dispositions[8],
        (9, "durable_gap".into(), GAP_NOTICE_ID.into())
    );
    assert_eq!(
        dispositions[9],
        (10, "projected_telemetry".into(), STALE_SOC_STABLE_ID.into())
    );
    drop(sequence_query);
    drop(catalogue);
    assert!(
        consumer
            .poll_once(&store)
            .await
            .unwrap()
            .accepted
            .is_empty()
    );
    let final_edge_batch: Value = raw_delivery
        .get(format!(
            "https://127.0.0.1:{delivery_port}/v2/hub/batches/next"
        ))
        .bearer_auth(&delivery_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(final_edge_batch["records"], json!([]));
    assert_eq!(final_edge_batch["gaps"], json!([]));
    if let Some(receipt_path) = std::env::var_os("TESLATLAS_TASK9_RECEIPT_PATH") {
        let receipt_path = PathBuf::from(receipt_path);
        assert!(receipt_path.is_absolute());
        let counts = store
            .edge_ledger_counts("task9-edge", "spool-2026-09-05")
            .unwrap();
        let receipt = json!({
            "schema": 1,
            "instrumented_crash_lane": cfg!(feature = "edge-test-faults"),
            "fault_witnesses": {
                "returned_raw_insert_error": cfg!(feature = "edge-test-faults"),
                "returned_lifecycle_write_error": cfg!(feature = "edge-test-faults"),
                "returned_receipt_insert_error": cfg!(feature = "edge-test-faults"),
                "returned_frontier_update_error": cfg!(feature = "edge-test-faults"),
                "returned_commit_error": cfg!(feature = "edge-test-faults"),
                "abort_before_accept_commit": cfg!(feature = "edge-test-faults"),
                "abort_after_accept_commit_before_ack": cfg!(feature = "edge-test-faults"),
                "abort_after_edge_ack_acceptance_before_response": cfg!(feature = "edge-test-faults"),
                "publisher_failure_after_gate": cfg!(feature = "edge-test-faults"),
                "publication_recovered_while_edge_offline": true,
            },
            "interleaved_gap": {
                "batch_id": GAP_BATCH_ID,
                "gap_spool_seq": 9,
                "gap_notice_id": GAP_NOTICE_ID,
                "gap_evidence_sha256": GAP_EVIDENCE_SHA256,
                "later_record_spool_seq": 10,
                "later_record_id": STALE_SOC_STABLE_ID,
                "later_only_ack_rejected": true,
                "merged_order_durably_accepted": [9, 10],
                "stale_soc_did_not_overwrite": true,
            },
            "sqlite": {
                "applications": counts.applications,
                "sequences": counts.sequences,
                "pending_publications": counts.pending_publications,
                "ack_frontier": counts.ack_frontier,
                "sync_pack_count": store.v2_lineage_pack_count(binding_vehicle).unwrap(),
                "ordered_dispositions": dispositions.iter().map(|(sequence, category, item_id)| json!({
                    "spool_seq": sequence,
                    "category": category,
                    "item_id": item_id,
                })).collect::<Vec<_>>(),
            },
            "public_current": {
                "observed_at_ms": public_current["observed_at_ms"],
                "usable_battery_level": public_current["usable_battery_level"],
                "inside_temp": public_current["inside_temp"],
                "power": public_current["power"],
            },
            "edge_after_ack": {
                "version": final_edge_batch["version"],
                "records": final_edge_batch["records"],
                "gaps": final_edge_batch["gaps"],
            },
            "secret_material_in_receipt": false,
        });
        private_file(
            &receipt_path,
            serde_json::to_vec_pretty(&receipt).unwrap().as_slice(),
        );
    }

    edge_process.0.kill().unwrap();
    assert!(
        edge_process.0.wait().unwrap().success() || edge_process.0.try_wait().unwrap().is_some()
    );
}
