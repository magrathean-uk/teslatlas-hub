use std::io::Cursor;
use std::sync::Arc;

use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TlsError {
    #[error("invalid server certificate chain")]
    InvalidServerCertificate,
    #[error("invalid server private key")]
    InvalidServerPrivateKey,
    #[error("invalid Hub client CA")]
    InvalidClientAuthority,
    #[error("cannot construct mTLS server configuration")]
    InvalidConfiguration,
}

pub fn mtls_server_config_from_pem(
    server_certificate_pem: &[u8],
    server_private_key_pem: &[u8],
    hub_client_ca_pem: &[u8],
) -> Result<Arc<ServerConfig>, TlsError> {
    let server_certificates = rustls_pemfile::certs(&mut Cursor::new(server_certificate_pem))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TlsError::InvalidServerCertificate)?;
    if server_certificates.is_empty() {
        return Err(TlsError::InvalidServerCertificate);
    }
    let server_private_key = rustls_pemfile::private_key(&mut Cursor::new(server_private_key_pem))
        .map_err(|_| TlsError::InvalidServerPrivateKey)?
        .ok_or(TlsError::InvalidServerPrivateKey)?;
    let client_authorities = rustls_pemfile::certs(&mut Cursor::new(hub_client_ca_pem))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TlsError::InvalidClientAuthority)?;
    if client_authorities.is_empty() {
        return Err(TlsError::InvalidClientAuthority);
    }
    let mut roots = RootCertStore::empty();
    for authority in client_authorities {
        roots
            .add(authority)
            .map_err(|_| TlsError::InvalidClientAuthority)?;
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let client_verifier =
        WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(|_| TlsError::InvalidConfiguration)?;
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| TlsError::InvalidConfiguration)?
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(server_certificates, server_private_key)
        .map_err(|_| TlsError::InvalidConfiguration)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}
