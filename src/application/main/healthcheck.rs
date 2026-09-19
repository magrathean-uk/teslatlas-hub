// SPDX-License-Identifier: AGPL-3.0-only

use futures_util::StreamExt as _;
use serde::Deserialize;

const CONTAINER_HEALTH_PORT: u16 = 8443;
const MAX_HEALTH_CA_BYTES: u64 = 1024 * 1024;
const MAX_HEALTH_RESPONSE_BYTES: usize = 16 * 1024;

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ContainerHealth {
    status: String,
    version: String,
}

async fn run_container_healthcheck(
    ca_file: &Path,
    server_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    run_container_healthcheck_at(
        ca_file,
        server_name,
        std::net::SocketAddr::from(([127, 0, 0, 1], CONTAINER_HEALTH_PORT)),
    )
    .await
}

async fn run_container_healthcheck_at(
    ca_file: &Path,
    server_name: &str,
    connect_address: std::net::SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    validate_container_health_server_name(server_name)?;
    if !ca_file.is_absolute() {
        return Err("healthcheck --ca-file must be absolute".into());
    }
    let metadata = fs::symlink_metadata(ca_file)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_HEALTH_CA_BYTES
    {
        return Err("healthcheck CA must be a non-empty regular file no larger than 1 MiB".into());
    }
    let ca_pem = fs::read(ca_file)?;
    if ca_pem.is_empty() || ca_pem.len() as u64 > MAX_HEALTH_CA_BYTES {
        return Err("healthcheck CA must remain between 1 byte and 1 MiB".into());
    }
    let certificates = reqwest::Certificate::from_pem_bundle(&ca_pem)
        .map_err(|_| "healthcheck CA is not a valid PEM certificate bundle")?;
    if certificates.is_empty() {
        return Err("healthcheck CA bundle is empty".into());
    }

    teslatlas_hub::crypto::install_default_provider();
    let client = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(4))
        .tls_certs_only(certificates)
        .resolve(server_name, connect_address)
        .build()?;
    let health_url = format!(
        "https://{server_name}:{}/healthz",
        connect_address.port()
    );
    let response = client.get(health_url).send().await?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(format!("Hub health endpoint returned {}", response.status()).into());
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if body.len().saturating_add(chunk.len()) > MAX_HEALTH_RESPONSE_BYTES {
            return Err("Hub health response exceeds 16 KiB".into());
        }
        body.extend_from_slice(&chunk);
    }
    validate_container_health(&body)?;
    Ok(())
}

fn validate_container_health_server_name(
    server_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if server_name.len() > 253
        || !server_name.contains('.')
        || server_name.parse::<std::net::IpAddr>().is_ok()
        || server_name.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                || label.starts_with('-')
                || label.ends_with('-')
        })
    {
        return Err("healthcheck --server-name must be a lowercase DNS hostname".into());
    }
    Ok(())
}

fn validate_container_health(body: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let health: ContainerHealth = serde_json::from_slice(body)?;
    if health.status != "ok" || health.version != teslatlas_hub::BUILD_VERSION {
        return Err("Hub health response does not match this binary".into());
    }
    Ok(())
}

#[cfg(test)]
mod container_healthcheck_tests {
    use super::*;

    #[test]
    fn response_requires_ok_and_this_binary_version() {
        let valid = serde_json::json!({
            "status": "ok",
            "version": teslatlas_hub::BUILD_VERSION,
        });
        validate_container_health(valid.to_string().as_bytes()).expect("valid health response");

        for invalid in [
            serde_json::json!({"status": "ready", "version": teslatlas_hub::BUILD_VERSION}),
            serde_json::json!({"status": "ok", "version": "0.0.0"}),
            serde_json::json!({"status": "ok"}),
        ] {
            assert!(validate_container_health(invalid.to_string().as_bytes()).is_err());
        }
    }

    #[tokio::test]
    async fn healthcheck_rejects_relative_ca_path_before_network() {
        let error = run_container_healthcheck(Path::new("ca.pem"), "hub.example.invalid")
            .await
            .expect_err("relative CA path must fail");
        assert_eq!(error.to_string(), "healthcheck --ca-file must be absolute");
    }

    #[test]
    fn healthcheck_rejects_non_dns_server_names() {
        for invalid in [
            "127.0.0.1",
            "localhost",
            "https://hub.example",
            "Hub.example",
            "-hub.example",
            "hub..example",
        ] {
            assert!(validate_container_health_server_name(invalid).is_err());
        }
        validate_container_health_server_name("hub.example.invalid")
            .expect("lowercase DNS name");
    }

    #[tokio::test]
    async fn healthcheck_validates_hostname_over_real_loopback_tls() {
        teslatlas_hub::crypto::install_default_provider();
        let identity = rcgen::generate_simple_self_signed(vec!["health.hub.test".to_owned()])
            .expect("test identity");
        let temporary = tempfile::tempdir().expect("temporary CA directory");
        let ca_file = temporary.path().join("ca.pem");
        fs::write(&ca_file, identity.cert.pem()).expect("write CA");
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            identity.cert.pem().into_bytes(),
            identity.signing_key.serialize_pem().into_bytes(),
        )
        .await
        .expect("TLS config");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener");
        let connect_address = listener.local_addr().expect("listener address");
        let listener = listener.into_std().expect("standard listener");
        let app = axum::Router::new().route(
            "/healthz",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "status": "ok",
                    "version": teslatlas_hub::BUILD_VERSION,
                }))
            }),
        );
        let task = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .expect("test TLS server")
                .serve(app.into_make_service())
                .await
        });

        run_container_healthcheck_at(&ca_file, "health.hub.test", connect_address)
            .await
            .expect("matching DNS certificate");
        assert!(
            run_container_healthcheck_at(&ca_file, "wrong.hub.test", connect_address)
                .await
                .is_err(),
            "hostname mismatch must fail TLS validation"
        );
        task.abort();
        let _ = task.await;
    }
}
