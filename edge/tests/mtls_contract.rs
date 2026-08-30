#![forbid(unsafe_code)]

use std::net::SocketAddr;
use std::sync::Arc;

use axum_server::Handle;
use axum_server::tls_rustls::RustlsConfig;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer,
    KeyPair, KeyUsagePurpose,
};
use reqwest::{Certificate as ClientRoot, Identity};
use tempfile::TempDir;
use teslatlas_edge::admission::Clock;
use teslatlas_edge::credentials::CredentialStore;
use teslatlas_edge::delivery::DeliveryService;
use teslatlas_edge::spool::{Spool, SpoolConfig, SpoolKey};
use teslatlas_edge::tls::mtls_server_config_from_pem;

const T0: i64 = 1_800_000_000_000;

struct FixedClock;

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        T0
    }
}

struct Authority {
    certificate: Certificate,
    issuer: Issuer<'static, KeyPair>,
}

struct Leaf {
    certificate_pem: String,
    key_pem: String,
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

fn client(root: &str, identity: Option<&Leaf>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .add_root_certificate(ClientRoot::from_pem(root.as_bytes()).unwrap())
        .https_only(true);
    if let Some(identity) = identity {
        let combined = format!("{}{}", identity.certificate_pem, identity.key_pem);
        builder = builder.identity(Identity::from_pem(combined.as_bytes()).unwrap());
    }
    builder.build().unwrap()
}

#[tokio::test]
async fn hub_endpoint_requires_trusted_client_certificate_and_active_bearer() {
    let temp = TempDir::new().unwrap();
    let spool = Spool::open(
        SpoolConfig {
            directory: temp.path().join("spool"),
            max_bytes: 1_048_576,
            max_records: 8,
            retention_ms: 60_000,
            batch_max_bytes: 262_144,
            batch_max_records: 8,
        },
        SpoolKey::from_bytes([0x77; 32]),
        T0,
    )
    .unwrap();
    let credentials = CredentialStore::open(temp.path().join("credentials.json")).unwrap();
    let issued = credentials.enrol("home-hub", T0, 60_000).unwrap();
    let service = DeliveryService::new(spool, credentials, Arc::new(FixedClock));

    let trusted = authority();
    let untrusted = authority();
    let server_leaf = leaf(&trusted, true);
    let trusted_client = leaf(&trusted, false);
    let untrusted_client = leaf(&untrusted, false);
    let server_config = mtls_server_config_from_pem(
        server_leaf.certificate_pem.as_bytes(),
        server_leaf.key_pem.as_bytes(),
        trusted.certificate.pem().as_bytes(),
    )
    .unwrap();
    let handle = Handle::<SocketAddr>::new();
    let task_handle = handle.clone();
    let server = tokio::spawn(async move {
        axum_server::bind_rustls(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            RustlsConfig::from_config(server_config),
        )
        .handle(task_handle)
        .serve(service.router().into_make_service())
        .await
    });
    let address = handle.listening().await.unwrap();
    let url = format!("https://127.0.0.1:{}/v1/hub/batches/next", address.port());
    let server_root = trusted.certificate.pem();

    assert!(client(&server_root, None).get(&url).send().await.is_err());
    assert!(
        client(&server_root, Some(&untrusted_client))
            .get(&url)
            .send()
            .await
            .is_err()
    );
    let authorized_client = client(&server_root, Some(&trusted_client));
    assert_eq!(
        authorized_client
            .get(&url)
            .bearer_auth("invalid")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        authorized_client
            .get(&url)
            .bearer_auth(issued.token())
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );

    handle.graceful_shutdown(Some(std::time::Duration::from_secs(1)));
    server.await.unwrap().unwrap();
}
