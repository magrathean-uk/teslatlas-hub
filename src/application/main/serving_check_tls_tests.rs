// SPDX-License-Identifier: AGPL-3.0-only

//! FIX-HANDOVER-TLS: `serving-check` against a listener whose configured LAN
//! certificate an older `lan-tls` made as a CA-style self-signed certificate.

use std::{fs, os::unix::fs::PermissionsExt as _};

use super::super::{HubConfig, ServingAnswer, serving_observation};

const HOST: &str = "older-hub.local";

fn identity(is_ca: rcgen::IsCa) -> (String, String) {
    let key = rcgen::KeyPair::generate().expect("key");
    let mut params = rcgen::CertificateParams::new(vec![HOST.to_owned()]).expect("params");
    params.is_ca = is_ca;
    (params.self_signed(&key).expect("certificate").pem(), key.serialize_pem())
}

fn ca_style() -> rcgen::IsCa {
    rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained)
}

/// Serves `/readyz` as Hub process 42 for the configuration whose certificate
/// file holds `configured_pem` and whose public URL names `host`; the listener
/// itself presents `served`. Returns the configuration and its digest.
async fn serving(
    temporary: &std::path::Path,
    configured_pem: &str,
    served: (String, String),
    host: &str,
) -> (HubConfig, String, tokio::task::JoinHandle<()>) {
    tesatlas_hub::crypto::install_default_provider();
    let certificate = temporary.join("configured.pem");
    fs::write(&certificate, configured_pem).expect("certificate");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    listener.set_nonblocking(true).expect("non-blocking");
    let config_path = temporary.join("config.toml");
    fs::write(
        &config_path,
        format!(
            "data_dir = '{}'\nbind = '{}'\n[collector]\ninterval_seconds = 0\n[tls]\ncertificate_path = '{}'\nprivate_key_path = '{}'\npublic_url = 'https://{host}:{}/'\n",
            temporary.join("hub").display(),
            address,
            certificate.display(),
            temporary.join("unused-key.pem").display(),
            address.port()
        ),
    )
    .expect("config");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).expect("mode");
    let (config, digest) = HubConfig::load_with_digest(&config_path).expect("load");
    let digest = digest.to_string();
    let answered = digest.clone();
    let app = axum::Router::new().route(
        "/readyz",
        axum::routing::get(move || {
            let digest = answered.clone();
            async move {
                (
                    [
                        ("x-teslatlas-native-process-id", "42".to_owned()),
                        (tesatlas_hub::server::NATIVE_CONFIG_DIGEST_HEADER, digest),
                    ],
                    axum::Json(serde_json::json!({"status": "ready"})),
                )
            }
        }),
    );
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        served.0.into_bytes(),
        served.1.into_bytes(),
    )
    .await
    .expect("served identity");
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, tls)
            .expect("server")
            .serve(app.into_make_service())
            .await
            .expect("serve");
    });
    (config, digest, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_older_ca_style_configured_certificate_is_served_and_seen() {
    let temporary = tempfile::tempdir().expect("temporary");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).expect("mode");
    let older = identity(ca_style());
    let (config, digest, server) =
        serving(temporary.path(), &older.0, older.clone(), HOST).await;
    assert!(tesatlas_hub::crypto::listener_certificate_is_ca(older.0.as_bytes()));

    // Before the fix: a root-store client refuses the configured certificate
    // (rustls CaUsedAsEndEntity), so the check never saw an answer.
    let address = config.bind;
    let roots = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .tls_certs_only(reqwest::Certificate::from_pem_bundle(older.0.as_bytes()).expect("pem"))
        .resolve(HOST, address)
        .build()
        .expect("client");
    let refused = roots
        .get(format!("https://{HOST}:{}/readyz", address.port()))
        .send()
        .await
        .expect_err("the WebPKI path refuses a CA certificate as the server's");
    assert!(tesatlas_hub::crypto::is_certificate_refusal(&refused), "{refused:?}");

    // After: the exact configured certificate is pinned and admitted.
    assert_eq!(
        serving_observation(&config, &digest, 42).await.expect("observation"),
        Some(ServingAnswer::Serving("ready"))
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_listener_with_any_other_certificate_is_still_refused() {
    let temporary = tempfile::tempdir().expect("temporary");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).expect("mode");
    // Configured: one CA-style certificate. Served: another for the same name.
    let configured = identity(ca_style());
    let (config, digest, server) =
        serving(temporary.path(), &configured.0, identity(ca_style()), HOST).await;
    assert_eq!(
        serving_observation(&config, &digest, 42).await.expect("observation"),
        Some(ServingAnswer::CertificateRefused)
    );
    server.abort();

    // A leaf for the same name that is not the configured one is refused too.
    let temporary = tempfile::tempdir().expect("temporary");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).expect("mode");
    let (config, digest, server) = serving(
        temporary.path(),
        &configured.0,
        identity(rcgen::IsCa::ExplicitNoCa),
        HOST,
    )
    .await;
    assert_eq!(
        serving_observation(&config, &digest, 42).await.expect("observation"),
        Some(ServingAnswer::CertificateRefused)
    );
    server.abort();

    // The configured certificate under a public URL it does not name is refused.
    let temporary = tempfile::tempdir().expect("temporary");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).expect("mode");
    let (config, digest, server) =
        serving(temporary.path(), &configured.0, configured.clone(), "other-hub.local").await;
    assert_eq!(
        serving_observation(&config, &digest, 42).await.expect("observation"),
        Some(ServingAnswer::CertificateRefused)
    );
    server.abort();
}
