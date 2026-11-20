// SPDX-License-Identifier: AGPL-3.0-only

//! One explicit Rustls cryptography provider for every Hub TLS path.
//!
//! Hub uses `ring` rather than compiling the larger AWS-LC provider on each
//! small native host. Installing it here also keeps TLS setup deterministic
//! when the HTTP listener and source database PostgreSQL reader run in one process.

/// Install the Hub's sole Rustls provider. Repeated calls are harmless: the
/// first call wins and later calls observe the same provider already present.
pub fn install_default_provider() {
    let _already_installed = rustls::crypto::ring::default_provider().install_default();
}

/// A client configuration for the Hub's own readiness probes that trusts exactly
/// one certificate: the first (end-entity) certificate of the configured listener
/// PEM, compared byte for byte, valid for the requested host name, with the
/// handshake signature checked by the Hub's provider. No other certificate is
/// trusted, so a listener that presents anything else is refused. Unlike a
/// root-store client it also admits the configured certificate when an older
/// `lan-tls` made it as a CA-style self-signed certificate (basicConstraints
/// CA:TRUE), which the WebPKI path refuses as `CaUsedAsEndEntity`.
pub fn pinned_listener_client_config(
    certificate_pem: &[u8],
) -> std::io::Result<rustls::ClientConfig> {
    let pinned = first_pem_certificate(certificate_pem)?;
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let verifier = PinnedCertificateVerifier {
        pinned,
        algorithms: provider.signature_verification_algorithms,
    };
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(std::io::Error::other)?
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(verifier))
        .with_no_client_auth())
}

/// True when the first certificate of `certificate_pem` carries basicConstraints
/// with `cA` set: a LAN certificate an older `lan-tls` made. False for a leaf, an
/// unreadable PEM or a certificate without the extension.
pub fn listener_certificate_is_ca(certificate_pem: &[u8]) -> bool {
    first_pem_certificate(certificate_pem).is_ok_and(|der| der_certificate_is_ca(der.as_ref()))
}

/// True when `error` (or an error it wraps, including inside nested `io::Error`
/// values, whose `source` skips the wrapped error itself) is a TLS certificate
/// refusal.
pub fn is_certificate_refusal(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if matches!(
            error.downcast_ref::<rustls::Error>(),
            Some(rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented)
        ) {
            return true;
        }
        current = match error.downcast_ref::<std::io::Error>() {
            Some(io) => io
                .get_ref()
                .map(|inner| inner as &(dyn std::error::Error + 'static)),
            None => error.source(),
        };
    }
    false
}

fn first_pem_certificate(
    certificate_pem: &[u8],
) -> std::io::Result<rustls::pki_types::CertificateDer<'static>> {
    use rustls::pki_types::{CertificateDer, pem::PemObject as _};
    CertificateDer::pem_slice_iter(certificate_pem)
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no certificate"))?
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

#[derive(Debug)]
struct PinnedCertificateVerifier {
    pinned: rustls::pki_types::CertificateDer<'static>,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl rustls::client::danger::ServerCertVerifier for PinnedCertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() != self.pinned.as_ref() {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ));
        }
        let parsed = rustls::server::ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_name(&parsed, server_name)?;
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// One DER element: its tag, its contents and what follows it.
fn der_element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let (bytes, rest) = rest.split_at(count);
        (bytes.iter().fold(0usize, |length, byte| (length << 8) | usize::from(*byte)), rest)
    };
    (rest.len() >= length).then(|| (tag, &rest[..length], &rest[length..]))
}

/// Whether an X.509 certificate's basicConstraints sets `cA` (RFC 5280 4.2.1.9).
fn der_certificate_is_ca(certificate: &[u8]) -> bool {
    const SEQUENCE: u8 = 0x30;
    const BASIC_CONSTRAINTS: [u8; 3] = [0x55, 0x1d, 0x13];
    let found = || -> Option<bool> {
        let (SEQUENCE, certificate, _) = der_element(certificate)? else { return None };
        let (SEQUENCE, mut fields, _) = der_element(certificate)? else { return None };
        while !fields.is_empty() {
            let (tag, field, rest) = der_element(fields)?;
            fields = rest;
            if tag != 0xa3 {
                continue;
            }
            let (SEQUENCE, mut extensions, _) = der_element(field)? else { return None };
            while !extensions.is_empty() {
                let (SEQUENCE, extension, rest) = der_element(extensions)? else { return None };
                extensions = rest;
                let (0x06, oid, rest) = der_element(extension)? else { return None };
                if oid != BASIC_CONSTRAINTS {
                    continue;
                }
                // An optional `critical` BOOLEAN precedes the OCTET STRING value.
                let (tag, value, after) = der_element(rest)?;
                let (tag, value) = if tag == 0x01 {
                    let (tag, value, _) = der_element(after)?;
                    (tag, value)
                } else {
                    (tag, value)
                };
                if tag != 0x04 {
                    return None;
                }
                let (SEQUENCE, constraints, _) = der_element(value)? else { return None };
                if constraints.is_empty() {
                    return Some(false);
                }
                let (tag, ca, _) = der_element(constraints)?;
                return Some(tag == 0x01 && ca.iter().any(|byte| *byte != 0));
            }
            return Some(false);
        }
        Some(false)
    };
    found().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::install_default_provider;

    fn self_signed(is_ca: rcgen::IsCa) -> String {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["hub.test.local".to_owned()]).unwrap();
        params.is_ca = is_ca;
        params.self_signed(&key).unwrap().pem()
    }

    #[test]
    fn only_a_ca_style_listener_certificate_is_reported_as_one() {
        use super::listener_certificate_is_ca;
        assert!(listener_certificate_is_ca(
            self_signed(rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained)).as_bytes()
        ));
        assert!(listener_certificate_is_ca(
            self_signed(rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0))).as_bytes()
        ));
        assert!(!listener_certificate_is_ca(self_signed(rcgen::IsCa::ExplicitNoCa).as_bytes()));
        assert!(!listener_certificate_is_ca(self_signed(rcgen::IsCa::NoCa).as_bytes()));
        assert!(!listener_certificate_is_ca(b"not a certificate"));
    }

    #[test]
    fn ring_provider_is_available_to_tls_callers() {
        install_default_provider();
        let _config = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
    }
}
