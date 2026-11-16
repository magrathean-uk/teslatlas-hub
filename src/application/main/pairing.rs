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

/// The longest pairing link the text presentation prints. The App takes the link
/// pasted into one field, so an endpoint that makes it longer is refused before any
/// invitation is stored.
const MAX_PAIRING_LINK_BYTES: usize = 4 * 1024;

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
    #[error("pairing scope is invalid")]
    Scope(#[source] Box<teslatlas_hub::db::StoreError>),
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
    scope: teslatlas_hub::db::PairingScope,
}

/// `pair` and `control pair`: one invitation against the configured TLS
/// identity and public endpoint, written to `writer`. The two commands differ
/// only in how they reach the store (`pair` under the instance lock after
/// migrating, `control pair` through `open_existing` while the Hub serves).
async fn pair_with_configured_tls<W: Write>(
    store: &HubStore,
    tls: Option<&teslatlas_hub::config::TlsListenerConfig>,
    label: &str,
    expires_in_seconds: u64,
    json: bool,
    scope: teslatlas_hub::db::PairingScope,
    writer: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    let tls = tls.ok_or("device pairing requires configured TLS")?;
    let created_at_ms = current_epoch_ms()?;
    execute_pairing_at(
        store,
        PairingCommandInput {
            label,
            expires_in_seconds,
            json,
            public_url: &tls.public_url,
            certificate_path: &tls.certificate_path,
            private_key_path: &tls.private_key_path,
            created_at_ms,
            scope,
        },
        writer,
    )
    .await?;
    Ok(())
}

/// The scope the pairing flags ask for: the named vehicles, or (with
/// `--all-vehicles` or no flag) every vehicle active when the invitation is
/// claimed. clap rejects the two flags together.
fn pairing_scope_from_flags(
    vehicles: Vec<uuid::Uuid>,
    all_vehicles: bool,
) -> teslatlas_hub::db::PairingScope {
    if all_vehicles || vehicles.is_empty() {
        teslatlas_hub::db::PairingScope::AllVehicles
    } else {
        teslatlas_hub::db::PairingScope::Vehicles(vehicles)
    }
}

/// Whether `c` could hide or reorder text on the operator's terminal: a control
/// character (Cc), a Unicode format character (Cf: the bidirectional marks,
/// embeddings, overrides and isolates U+200E, U+200F, U+202A..U+202E and
/// U+2066..U+2069, zero-width and joiner characters, the soft hyphen, tag
/// characters and the other invisible formatters) or a line or paragraph
/// separator (U+2028, U+2029). `char` has no general-category test, so the
/// Cf ranges are listed (Unicode 16).
fn hidden_in_terminal(c: char) -> bool {
    c.is_control()
        || matches!(
            u32::from(c),
            0xAD | 0x600..=0x605
                | 0x61C
                | 0x6DD
                | 0x70F
                | 0x890..=0x891
                | 0x8E2
                | 0x180E
                | 0x200B..=0x200F
                | 0x2028..=0x202E
                | 0x2060..=0x2064
                | 0x2066..=0x206F
                | 0xFEFF
                | 0xFFF9..=0xFFFB
                | 0x110BD
                | 0x110CD
                | 0x13430..=0x1343F
                | 0x1BCA0..=0x1BCA3
                | 0x1D173..=0x1D17A
                | 0xE0001
                | 0xE0020..=0xE007F
        )
}

/// The scope line of the text presentation. The invitation URI and the JSON
/// payload never carry the scope.
fn pairing_grants_text(named: Option<&[teslatlas_hub::db::PublishedVehicle]>) -> String {
    let Some(vehicles) = named else {
        return "Grants: every vehicle that is active when this invitation is claimed.".to_owned();
    };
    let names: Vec<String> = vehicles
        .iter()
        .map(|vehicle| {
            vehicle
                .display_name
                .as_deref()
                .filter(|name| !name.is_empty())
                .map_or_else(
                    || vehicle.vehicle_id.to_string(),
                    |name| {
                        name.chars()
                            .map(|c| if hidden_in_terminal(c) { '?' } else { c })
                            .collect()
                    },
                )
        })
        .collect();
    format!("Grants: {}.", names.join(", "))
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
    // Named vehicles must exist and be active before any one-time secret is
    // generated; persisting checks retirement again inside its transaction.
    let named = match &input.scope {
        teslatlas_hub::db::PairingScope::AllVehicles => None,
        teslatlas_hub::db::PairingScope::Vehicles(vehicles) => Some(
            store
                .pairing_scope_vehicles(vehicles)
                .map_err(|error| PairingCommandError::Scope(Box::new(error)))?,
        ),
    };
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
        &pairing_grants_text(named.as_deref()),
    )?;
    let pairing_id = invitation.pairing_id;
    persist_and_present_pairing(
        writer,
        &presentation,
        || store.persist_pairing_with_scope(input.label, &invitation, &input.scope),
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
    grants_text: &str,
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
        if pairing_uri.len() > MAX_PAIRING_LINK_BYTES {
            return Err(PairingCommandError::Presentation);
        }
        output
            .try_reserve_exact(
                pairing_uri
                    .len()
                    .saturating_add(grants_text.len())
                    .saturating_add(128),
            )
            .map_err(|_| PairingCommandError::Presentation)?;
        write!(
            output,
            "Paste this link in Tesatlas:\n{pairing_uri}\n{grants_text}\nExpires in {expires_in_seconds} seconds.\n",
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

/// The LAN listener the `lan-tls` verb sets up (DEC-152): every interface, port 8443.
const LAN_TLS_BIND: &str = "0.0.0.0:8443";
const LAN_TLS_PORT: u16 = 8443;
/// The self-signed identity's lifetime: within the 398-day ceiling clients apply.
const LAN_TLS_VALIDITY_DAYS: i64 = 397;
/// The folder inside the Hub data folder that holds the identity, and its files.
const LAN_TLS_DIRECTORY: &str = "tls";
const LAN_TLS_CERTIFICATE_FILE: &str = "certificate.pem";
const LAN_TLS_PRIVATE_KEY_FILE: &str = "private-key.pem";

/// What `lan-tls` reports. It names no secret: the public URL and the certificate
/// expiry only.
#[derive(Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct LanTlsReport {
    /// `created` when this run made the identity, `existing` when `[tls]` was set.
    status: &'static str,
    public_url: String,
    bind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    certificate_expires_at_ms: Option<i64>,
}

/// A single-label Bonjour name: lowercase letters, digits and hyphens, then `.local`.
fn valid_lan_host(host: &str) -> bool {
    let Some(label) = host.strip_suffix(".local") else {
        return false;
    };
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The certificate parameters for `host`: the name as subject and only SAN, a
/// server-authentication leaf that is not a CA, valid from `now` for 397 days.
fn lan_tls_certificate_params(
    host: &str,
    now: SystemTime,
) -> Result<rcgen::CertificateParams, Box<dyn std::error::Error>> {
    let mut params = rcgen::CertificateParams::new(vec![host.to_owned()])?;
    let mut name = rcgen::DistinguishedName::new();
    name.push(rcgen::DnType::CommonName, host);
    params.distinguished_name = name;
    params.is_ca = rcgen::IsCa::ExplicitNoCa;
    params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let not_before = time::OffsetDateTime::from(now);
    params.not_before = not_before;
    params.not_after = not_before + time::Duration::days(LAN_TLS_VALIDITY_DAYS);
    Ok(params)
}

/// Creates the owner-only `tls/` folder, or admits an existing one only when it is a
/// real folder owned by this user and closed to group and others.
#[cfg(unix)]
fn prepare_lan_tls_directory(directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(directory)?;
            if !metadata.file_type().is_dir()
                || metadata.uid() != getuid().as_raw()
                || metadata.mode() & 0o077 != 0
            {
                return Err("the Hub TLS folder is not a private folder of this user".into());
            }
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// Writes one new owner-only file. `create_new` (O_CREAT | O_EXCL) refuses an
/// existing file or link, so nothing is replaced or followed.
#[cfg(unix)]
fn write_new_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// `lan-tls` (DEC-152): when the configuration has no `[tls]`, makes a self-signed
/// P-256 identity for `host` in the data folder's `tls/` (folder 0700, files 0600),
/// then sets `bind = "0.0.0.0:8443"` and `[tls]` with `https://<host>:8443/`. The
/// new configuration is parsed and its identity checked as `Serve` checks it before
/// it replaces the file; on any failure the files this run wrote are removed and the
/// configuration is unchanged. With `[tls]` already set the identity is checked and
/// nothing is written. The private key is never printed.
#[cfg(unix)]
fn setup_lan_tls(
    config_path: &Path,
    host: &str,
    now: SystemTime,
) -> Result<LanTlsReport, Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt as _;
    if !valid_lan_host(host) {
        return Err("--host must be this Mac's lowercase .local name, such as office-mac.local".into());
    }
    // One no-follow read: the bytes that are checked are the bytes that are edited.
    let descriptor = open(
        config_path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| "the Hub configuration cannot be opened")?;
    let mut source = Vec::new();
    fs::File::from(descriptor)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut source)?;
    let (config, _) = HubConfig::from_exact_bytes(&source)?;
    if let Some(tls) = &config.tls {
        server::validate_tls_identity(tls)?;
        return Ok(LanTlsReport {
            status: "existing",
            public_url: tls.public_url.clone(),
            bind: config.bind.to_string(),
            certificate_expires_at_ms: None,
        });
    }
    let mut table: toml::Table = std::str::from_utf8(&source)?.parse()?;

    let directory = config.data_dir.join(LAN_TLS_DIRECTORY);
    prepare_lan_tls_directory(&directory)?;
    let certificate_path = directory.join(LAN_TLS_CERTIFICATE_FILE);
    let private_key_path = directory.join(LAN_TLS_PRIVATE_KEY_FILE);
    for path in [&certificate_path, &private_key_path] {
        if fs::symlink_metadata(path).is_ok() {
            return Err(format!(
                "{} already exists but the configuration names no TLS identity; move it aside and try again",
                path.display()
            )
            .into());
        }
    }
    let (Some(certificate_text), Some(private_key_text)) =
        (certificate_path.to_str(), private_key_path.to_str())
    else {
        return Err("the Hub data folder path must be UTF-8".into());
    };

    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let params = lan_tls_certificate_params(host, now)?;
    let expires_at_ms = params.not_after.unix_timestamp().checked_mul(1_000);
    let certificate = params.self_signed(&key)?;
    let private_key_pem = zeroize::Zeroizing::new(key.serialize_pem());

    let public_url = format!("https://{host}:{LAN_TLS_PORT}/");
    table.insert("bind".to_owned(), toml::Value::String(LAN_TLS_BIND.to_owned()));
    let mut tls = toml::Table::new();
    tls.insert(
        "certificate_path".to_owned(),
        toml::Value::String(certificate_text.to_owned()),
    );
    tls.insert(
        "private_key_path".to_owned(),
        toml::Value::String(private_key_text.to_owned()),
    );
    tls.insert("public_url".to_owned(), toml::Value::String(public_url.clone()));
    table.insert("tls".to_owned(), toml::Value::Table(tls));
    let updated = toml::to_string(&table)?;

    let folder = config_path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staged = folder.join(format!(".config.{}.tmp", uuid::Uuid::new_v4()));
    let mut created: Vec<&Path> = Vec::with_capacity(2);
    let written = (|| -> Result<(), Box<dyn std::error::Error>> {
        write_new_private_file(&private_key_path, private_key_pem.as_bytes())?;
        created.push(&private_key_path);
        write_new_private_file(&certificate_path, certificate.pem().as_bytes())?;
        created.push(&certificate_path);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)?;
        file.write_all(updated.as_bytes())?;
        file.sync_all()?;
        let staged_config = HubConfig::load(&staged)?;
        let staged_tls = staged_config
            .tls
            .as_ref()
            .ok_or("the new configuration names no TLS identity")?;
        server::validate_tls_identity(staged_tls)?;
        fs::rename(&staged, config_path)?;
        Ok(())
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&staged);
        for path in created {
            let _ = fs::remove_file(path);
        }
        return Err(error);
    }
    // The new configuration is in place and names the identity: a failed folder sync
    // is not undone by removing the files it names.
    let _ = fs::File::open(folder).and_then(|folder| folder.sync_all());
    Ok(LanTlsReport {
        status: "created",
        public_url,
        bind: LAN_TLS_BIND.to_owned(),
        certificate_expires_at_ms: expires_at_ms,
    })
}
