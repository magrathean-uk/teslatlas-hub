#![forbid(unsafe_code)]

use std::fs::OpenOptions;
use std::io::Write;
use std::net::TcpListener;
use std::time::Duration;

use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use tempfile::TempDir;
use teslatlas_edge::config::EdgeConfig;
use teslatlas_edge::runtime::run_until_shutdown;
use teslatlas_edge::spool::{Spool, SpoolKey};
use tokio::sync::oneshot;

fn unused_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn private_file(path: &std::path::Path, bytes: &[u8]) {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
}

fn make_server_identity(temp: &TempDir) {
    let mut ca_params = CertificateParams::new(Vec::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let ca_key = KeyPair::generate().unwrap();
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);

    let mut server_params =
        CertificateParams::new(vec!["localhost".to_owned(), "127.0.0.1".to_owned()]).unwrap();
    server_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server_key = KeyPair::generate().unwrap();
    let server = server_params.signed_by(&server_key, &issuer).unwrap();

    std::fs::write(temp.path().join("server.crt"), server.pem()).unwrap();
    private_file(
        &temp.path().join("server.key"),
        server_key.serialize_pem().as_bytes(),
    );
    std::fs::write(temp.path().join("hub-ca.crt"), ca.pem()).unwrap();
}

fn make_config(temp: &TempDir, receiver_port: u16, hub_port: u16) -> EdgeConfig {
    private_file(
        &temp.path().join("receiver-token"),
        b"receiver-token-0123456789",
    );
    private_file(&temp.path().join("spool-key"), &[0x31; 32]);
    private_file(
        &temp.path().join("credentials.json"),
        b"{\"version\":1,\"credentials\":[]}",
    );
    make_server_identity(temp);
    let root = temp.path();
    let text = format!(
        r#"version = 1
state_directory = "{}"
receiver_bind = "127.0.0.1:{receiver_port}"
hub_bind = "0.0.0.0:{hub_port}"
receiver_bearer_path = "{}"
spool_key_path = "{}"
credential_store_path = "{}"
hub_server_certificate_path = "{}"
hub_server_private_key_path = "{}"
hub_client_ca_path = "{}"

[spool]
max_bytes = 1048576
max_records = 8
retention_seconds = 604800
batch_max_bytes = 262144
batch_max_records = 8
"#,
        root.display(),
        root.join("receiver-token").display(),
        root.join("spool-key").display(),
        root.join("credentials.json").display(),
        root.join("server.crt").display(),
        root.join("server.key").display(),
        root.join("hub-ca.crt").display(),
    );
    std::fs::write(root.join("config.toml"), &text).unwrap();
    EdgeConfig::from_toml(text.as_bytes()).unwrap()
}

#[tokio::test]
async fn bounded_shutdown_closes_idle_connection_and_keeps_admitted_record() {
    let temp = TempDir::new().unwrap();
    let receiver_port = unused_port();
    let mut hub_port = unused_port();
    while hub_port == receiver_port {
        hub_port = unused_port();
    }
    let config = make_config(&temp, receiver_port, hub_port);
    let spool_config = config.spool_config().unwrap();
    let doctor = tokio::process::Command::new(env!("CARGO_BIN_EXE_teslatlas-edge"))
        .arg("--config")
        .arg(temp.path().join("config.toml"))
        .arg("doctor")
        .output()
        .await
        .unwrap();
    assert!(doctor.status.success());
    assert_eq!(doctor.stdout, b"ok\n");
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let runtime_config = config.clone();
    let runtime = tokio::spawn(async move {
        run_until_shutdown(runtime_config, async move {
            let _ = shutdown_rx.await;
        })
        .await
    });

    let client = reqwest::Client::new();
    let health_url = format!("http://127.0.0.1:{receiver_port}/healthz");
    for _ in 0..100 {
        if client.get(&health_url).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        client
            .get(&health_url)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let _idle = tokio::net::TcpStream::connect(("127.0.0.1", receiver_port))
        .await
        .unwrap();

    let envelope = serde_json::json!({
        "version": 1,
        "vin": "5YJ3E1EA7KF000001",
        "txid": "runtime-1",
        "tx_type": "V",
        "received_at_ms": 1_800_000_000_000_i64,
        "timestamp_ms": 1_800_000_000_000_i64,
        "payload": {"Soc": 71}
    });
    assert_eq!(
        client
            .post(format!(
                "http://127.0.0.1:{receiver_port}/v1/internal/fleet-telemetry"
            ))
            .bearer_auth("receiver-token-0123456789")
            .json(&envelope)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::NO_CONTENT
    );

    shutdown_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(6), runtime)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let spool = Spool::open(spool_config, SpoolKey::from_bytes([0x31; 32]), now_ms()).unwrap();
    assert_eq!(spool.next_batch(now_ms()).unwrap().records.len(), 1);
}

fn now_ms() -> i64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(elapsed.as_millis()).unwrap()
}
