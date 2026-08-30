// SPDX-License-Identifier: AGPL-3.0-only

fn observation_command_error(
    command: &str,
    car_id: i64,
    error: ObservationVerificationError,
) -> Box<dyn std::error::Error> {
    println!(
        "{}",
        serde_json::json!({
            "status": "error",
            "command": command,
            "carId": car_id,
            "errorCode": error.code(),
        })
    );
    Box::new(std::io::Error::other(error.to_string()))
}

fn render_pairing_qr(
    pairing_uri: &str,
) -> Result<zeroize::Zeroizing<String>, qrcode::types::QrError> {
    Ok(zeroize::Zeroizing::new(
        QrCode::new(pairing_uri.as_bytes())?
            .render::<Dense1x2>()
            .quiet_zone(true)
            .build(),
    ))
}

const MAX_TLS_CERTIFICATE_CHAIN_BYTES: usize = server::MAX_TLS_CERTIFICATE_CHAIN_BYTES;
const MAX_TLS_PRIVATE_KEY_BYTES: usize = server::MAX_TLS_PRIVATE_KEY_BYTES;

#[derive(Debug, thiserror::Error)]
enum PairingCommandError {
    #[error("TLS certificate cannot be validated")]
    Certificate(#[source] std::io::Error),
    #[error("pairing expiry is invalid")]
    InvalidExpiry,
    #[error("pairing presentation cannot be constructed")]
    Presentation,
    #[error("pairing invitation cannot be persisted")]
    Persist(#[source] Box<teslatlas_hub::db::StoreError>),
    #[error("pairing invitation persistence failed and revocation could not be confirmed")]
    PersistAndRevoke {
        persist: Box<teslatlas_hub::db::StoreError>,
        revoke: Box<teslatlas_hub::db::StoreError>,
    },
    #[error("pairing presentation failed; invitation was revoked ({kind:?})")]
    Present { kind: std::io::ErrorKind },
    #[error("pairing presentation failed and invitation revocation failed ({kind:?})")]
    PresentAndRevoke {
        kind: std::io::ErrorKind,
        #[source]
        revoke: Box<teslatlas_hub::db::StoreError>,
    },
}

struct PairingCommandInput<'a> {
    label: &'a str,
    expires_in_seconds: u64,
    json: bool,
    public_url: &'a str,
    certificate_path: &'a Path,
    private_key_path: &'a Path,
    created_at_ms: i64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PairingJsonPresentation<'a> {
    pairing_id: uuid::Uuid,
    secret: &'a str,
    expires_at_ms: i64,
    endpoint: &'a str,
    tls_pin: &'a str,
    pairing_uri: &'a str,
}

async fn execute_pairing_at<W: Write>(
    store: &HubStore,
    input: PairingCommandInput<'_>,
    writer: &mut W,
) -> Result<(), PairingCommandError> {
    // Read and validate the exact configured identity before generating any
    // one-time secret. These bytes take the same Rustls construction path as
    // Serve, after the pairing-specific no-follow and identity checks.
    let certificate_pem = read_tls_identity_file(
        input.certificate_path,
        MAX_TLS_CERTIFICATE_CHAIN_BYTES,
        false,
    )
    .map_err(PairingCommandError::Certificate)?;
    let private_key_pem =
        read_tls_identity_file(input.private_key_path, MAX_TLS_PRIVATE_KEY_BYTES, true)
            .map_err(PairingCommandError::Certificate)?;
    let tls_pin = leaf_certificate_sha256_from_pem(&certificate_pem)
        .map_err(PairingCommandError::Certificate)?;
    teslatlas_hub::server::rustls_config_from_pem_identity(certificate_pem, private_key_pem)
        .await
        .map_err(|_| {
            PairingCommandError::Certificate(std::io::Error::other(
                "TLS identity cannot be validated",
            ))
        })?;
    if input.expires_in_seconds == 0 {
        return Err(PairingCommandError::InvalidExpiry);
    }
    let ttl_ms = input
        .expires_in_seconds
        .checked_mul(1_000)
        .and_then(|value| i64::try_from(value).ok())
        .ok_or(PairingCommandError::InvalidExpiry)?;
    let expires_at_ms = input
        .created_at_ms
        .checked_add(ttl_ms)
        .ok_or(PairingCommandError::InvalidExpiry)?;
    let invitation = store
        .prepare_pairing(input.label, input.created_at_ms, expires_at_ms)
        .map_err(|error| PairingCommandError::Persist(Box::new(error)))?;
    let pairing_uri = pairing_uri(
        input.public_url,
        &tls_pin,
        invitation.pairing_id,
        invitation.secret(),
    )
    .map_err(|_| PairingCommandError::Presentation)?;
    let presentation = build_pairing_presentation(
        input.json,
        input.public_url,
        &tls_pin,
        &pairing_uri,
        &invitation,
        input.expires_in_seconds,
    )?;
    let pairing_id = invitation.pairing_id;
    persist_and_present_pairing(
        writer,
        &presentation,
        || store.persist_pairing(input.label, &invitation),
        || store.revoke_pairing(pairing_id),
    )
}

fn build_pairing_presentation(
    json: bool,
    public_url: &str,
    tls_pin: &str,
    pairing_uri: &str,
    invitation: &teslatlas_hub::db::PairingInvitation,
    expires_in_seconds: u64,
) -> Result<zeroize::Zeroizing<Vec<u8>>, PairingCommandError> {
    let mut output = zeroize::Zeroizing::new(Vec::new());
    if json {
        let maximum_json_bytes = public_url
            .len()
            .saturating_add(tls_pin.len())
            .saturating_add(pairing_uri.len())
            .saturating_add(invitation.secret().len())
            .saturating_mul(6)
            .saturating_add(512);
        output
            .try_reserve_exact(maximum_json_bytes)
            .map_err(|_| PairingCommandError::Presentation)?;
        let document = PairingJsonPresentation {
            pairing_id: invitation.pairing_id,
            secret: invitation.secret(),
            expires_at_ms: invitation.expires_at_ms,
            endpoint: public_url,
            tls_pin,
            pairing_uri,
        };
        serde_json::to_writer(&mut *output, &document)
            .map_err(|_| PairingCommandError::Presentation)?;
        output.push(b'\n');
    } else {
        let qr = render_pairing_qr(pairing_uri).map_err(|_| PairingCommandError::Presentation)?;
        output
            .try_reserve_exact(qr.len().saturating_add(128))
            .map_err(|_| PairingCommandError::Presentation)?;
        write!(
            output,
            "Scan with Teslatlas:\n{}\nExpires in {expires_in_seconds} seconds.\n",
            qr.as_str()
        )
        .map_err(|_| PairingCommandError::Presentation)?;
    }
    Ok(output)
}

fn persist_and_present_pairing<W, P, R>(
    writer: &mut W,
    presentation: &[u8],
    persist: P,
    revoke: R,
) -> Result<(), PairingCommandError>
where
    W: Write,
    P: FnOnce() -> Result<(), teslatlas_hub::db::StoreError>,
    R: FnOnce() -> Result<(), teslatlas_hub::db::StoreError>,
{
    if let Err(persist) = persist() {
        return match revoke() {
            Ok(()) => Err(PairingCommandError::Persist(Box::new(persist))),
            Err(revoke) => Err(PairingCommandError::PersistAndRevoke {
                persist: Box::new(persist),
                revoke: Box::new(revoke),
            }),
        };
    }
    if let Err(error) = writer.write_all(presentation).and_then(|()| writer.flush()) {
        let kind = error.kind();
        return match revoke() {
            Ok(()) => Err(PairingCommandError::Present { kind }),
            Err(revoke) => Err(PairingCommandError::PresentAndRevoke {
                kind,
                revoke: Box::new(revoke),
            }),
        };
    }
    Ok(())
}

/// SHA-256 pin for the first PEM certificate (the server leaf) in the active
/// TLS chain. Pairing carries this non-secret value so an iPhone can pin the
/// exact identity before it sends its single-use claim secret.
#[cfg(test)]
fn leaf_certificate_sha256(certificate_path: &std::path::Path) -> Result<String, std::io::Error> {
    leaf_certificate_sha256_after_open(certificate_path, || {})
}

#[cfg(test)]
fn leaf_certificate_sha256_after_open(
    certificate_path: &Path,
    after_open: impl FnOnce(),
) -> Result<String, std::io::Error> {
    let pem = read_tls_identity_file_after_open(
        certificate_path,
        MAX_TLS_CERTIFICATE_CHAIN_BYTES,
        false,
        after_open,
    )?;
    leaf_certificate_sha256_from_pem(&pem)
}

fn read_tls_identity_file(
    path: &Path,
    maximum: usize,
    private: bool,
) -> Result<zeroize::Zeroizing<Vec<u8>>, std::io::Error> {
    server::read_tls_identity_file(path, maximum, private)
}

#[cfg(test)]
fn read_tls_identity_file_after_open(
    path: &Path,
    maximum: usize,
    private: bool,
    after_open: impl FnOnce(),
) -> Result<zeroize::Zeroizing<Vec<u8>>, std::io::Error> {
    server::read_tls_identity_file_after_open(path, maximum, private, after_open)
}

fn leaf_certificate_sha256_from_pem(pem: &[u8]) -> Result<String, std::io::Error> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};

    let leaf = CertificateDer::pem_slice_iter(pem)
        .next()
        .transpose()
        .map_err(|_| std::io::Error::other("TLS certificate PEM is invalid"))?
        .ok_or_else(|| std::io::Error::other("TLS certificate has no PEM leaf"))?;
    Ok(hex::encode(Sha256::digest(leaf.as_ref())))
}

fn pairing_uri(
    endpoint: &str,
    tls_pin: &str,
    pairing_id: uuid::Uuid,
    secret: &str,
) -> Result<zeroize::Zeroizing<String>, std::collections::TryReserveError> {
    const PREFIX: &str = "teslatlas-hub://pair?";
    let pairing_id = pairing_id.to_string();
    let maximum_encoded_values = endpoint
        .len()
        .saturating_add(pairing_id.len())
        .saturating_add(secret.len())
        .saturating_add(tls_pin.len())
        .saturating_mul(3);
    let maximum_uri_bytes = PREFIX
        .len()
        .saturating_add("endpoint=".len())
        .saturating_add("&pairing_id=".len())
        .saturating_add("&secret=".len())
        .saturating_add("&tls_pin=".len())
        .saturating_add(maximum_encoded_values);
    let mut allocation = String::new();
    allocation.try_reserve_exact(maximum_uri_bytes)?;
    let mut uri = zeroize::Zeroizing::new(allocation);
    uri.push_str(PREFIX);
    {
        let mut query = url::form_urlencoded::Serializer::for_suffix(&mut *uri, PREFIX.len());
        query.append_pair("endpoint", endpoint);
        query.append_pair("pairing_id", &pairing_id);
        query.append_pair("secret", secret);
        query.append_pair("tls_pin", tls_pin);
        let _ = query.finish();
    }
    debug_assert!(uri.len() <= maximum_uri_bytes);
    Ok(uri)
}

fn current_epoch_ms() -> Result<i64, std::io::Error> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(std::io::Error::other)?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| std::io::Error::other("system clock exceeds epoch milliseconds"))
}
