//! One explicit Rustls cryptography provider for every Hub TLS path.
//!
//! Hub uses `ring` rather than compiling the larger AWS-LC provider on each
//! small native host. Installing it here also keeps TLS setup deterministic
//! when the HTTP listener and TeslaMate PostgreSQL reader run in one process.

/// Install the Hub's sole Rustls provider. Repeated calls are harmless: the
/// first call wins and later calls observe the same provider already present.
pub fn install_default_provider() {
    let _already_installed = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::install_default_provider;

    #[test]
    fn ring_provider_is_available_to_tls_callers() {
        install_default_provider();
        let _config = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
    }
}
