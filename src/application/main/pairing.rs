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
    Scope(#[source] Box<tesatlas_hub::db::StoreError>),
    #[error("pairing invitation cannot be persisted")]
    Persist(#[source] Box<tesatlas_hub::db::StoreError>),
    #[error("pairing invitation persistence failed and revocation could not be confirmed")]
    PersistAndRevoke {
        persist: Box<tesatlas_hub::db::StoreError>,
        revoke: Box<tesatlas_hub::db::StoreError>,
    },
    #[error("pairing presentation failed; invitation was revoked ({kind:?})")]
    Present { kind: std::io::ErrorKind },
    #[error("pairing presentation failed and invitation revocation failed ({kind:?})")]
    PresentAndRevoke {
        kind: std::io::ErrorKind,
        #[source]
        revoke: Box<tesatlas_hub::db::StoreError>,
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
    scope: tesatlas_hub::db::PairingScope,
}

/// `pair` and `control pair`: one invitation against the configured TLS
/// identity and public endpoint, written to `writer`. The two commands differ
/// only in how they reach the store (`pair` under the instance lock after
/// migrating, `control pair` through `open_existing` while the Hub serves).
async fn pair_with_configured_tls<W: Write>(
    store: &HubStore,
    tls: Option<&tesatlas_hub::config::TlsListenerConfig>,
    label: &str,
    expires_in_seconds: u64,
    json: bool,
    scope: tesatlas_hub::db::PairingScope,
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
) -> tesatlas_hub::db::PairingScope {
    if all_vehicles || vehicles.is_empty() {
        tesatlas_hub::db::PairingScope::AllVehicles
    } else {
        tesatlas_hub::db::PairingScope::Vehicles(vehicles)
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
fn pairing_grants_text(named: Option<&[tesatlas_hub::db::PublishedVehicle]>) -> String {
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
    tesatlas_hub::server::rustls_config_from_pem_identity(certificate_pem, private_key_pem)
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
        tesatlas_hub::db::PairingScope::AllVehicles => None,
        tesatlas_hub::db::PairingScope::Vehicles(vehicles) => Some(
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
    invitation: &tesatlas_hub::db::PairingInvitation,
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
    P: FnOnce() -> Result<(), tesatlas_hub::db::StoreError>,
    R: FnOnce() -> Result<(), tesatlas_hub::db::StoreError>,
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
/// The folder inside the Hub data folder that holds the identity.
const LAN_TLS_DIRECTORY: &str = "tls";

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

/// Creates the owner-only `tls/` folder (true), or admits an existing one (false)
/// only when it is a real folder owned by this user and closed to group and others.
#[cfg(unix)]
fn prepare_lan_tls_directory(directory: &Path) -> Result<bool, Box<dyn std::error::Error>> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(directory)?;
            if !metadata.file_type().is_dir()
                || metadata.uid() != getuid().as_raw()
                || metadata.mode() & 0o077 != 0
            {
                return Err("the Hub TLS folder is not a private folder of this user".into());
            }
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

/// The `tls/` file names of one `lan-tls` run. Every run has its own id, so a
/// retry never meets, replaces or adopts a file an earlier run left behind.
const LAN_TLS_FILE_PREFIX: &str = "lan-tls-";
const LAN_TLS_PENDING_SUFFIX: &str = ".pending";
const LAN_TLS_CERTIFICATE_SUFFIX: &str = "-certificate.pem";
const LAN_TLS_PRIVATE_KEY_SUFFIX: &str = "-private-key.pem";

/// The certificate, private key and pending marker of the run `id`.
fn lan_tls_run_paths(directory: &Path, id: &str) -> [PathBuf; 3] {
    [
        directory.join(format!(
            "{LAN_TLS_FILE_PREFIX}{id}{LAN_TLS_CERTIFICATE_SUFFIX}"
        )),
        directory.join(format!(
            "{LAN_TLS_FILE_PREFIX}{id}{LAN_TLS_PRIVATE_KEY_SUFFIX}"
        )),
        directory.join(format!("{LAN_TLS_FILE_PREFIX}{id}{LAN_TLS_PENDING_SUFFIX}")),
    ]
}

/// The run id of a pending marker name, when the name is exactly one.
fn lan_tls_pending_id(name: &str) -> Option<&str> {
    let id = name
        .strip_prefix(LAN_TLS_FILE_PREFIX)?
        .strip_suffix(LAN_TLS_PENDING_SUFFIX)?;
    (id.len() == 32
        && id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then_some(id)
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> std::io::Result<()> {
    fs::File::open(directory)?.sync_all()
}

/// Creates one new owner-only file and records it in `created` as soon as the
/// exclusive create succeeds, before anything is written: a failed write or sync
/// still leaves the file in the list this run removes. `create_new` (O_CREAT |
/// O_EXCL) refuses an existing file or link, so nothing is replaced or followed.
#[cfg(unix)]
fn write_new_private_file(
    path: &Path,
    bytes: &[u8],
    created: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    created.push(path.to_path_buf());
    file.write_all(bytes)?;
    file.sync_all()
}

/// The configuration bytes as `Serve` admits them, with the identity they were read
/// from: a no-follow, non-blocking open (a FIFO never blocks), a regular file owned
/// by this user or root and not writable by group or others, one bounded read, and
/// the same file still at the path afterwards.
#[cfg(unix)]
struct AdmittedConfigBytes {
    bytes: Vec<u8>,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn read_admitted_config(path: &Path) -> Result<AdmittedConfigBytes, Box<dyn std::error::Error>> {
    const MAX_CONFIG_BYTES: usize = 1024 * 1024;
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| "the Hub configuration cannot be opened")?;
    let held = fstat(&descriptor).map_err(|_| "the Hub configuration cannot be read")?;
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || (held.st_uid != getuid().as_raw() && held.st_uid != 0)
        || (held.st_mode as u32 & 0o022) != 0
    {
        return Err("the Hub configuration is not a private file of this user".into());
    }
    let flags = fcntl_getfl(&descriptor).map_err(|_| "the Hub configuration cannot be read")?;
    fcntl_setfl(&descriptor, flags & !OFlags::NONBLOCK)
        .map_err(|_| "the Hub configuration cannot be read")?;
    let mut bytes = Vec::new();
    fs::File::from(descriptor)
        .take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err("the Hub configuration is too large".into());
    }
    let current = fs::symlink_metadata(path)?;
    if current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.uid() != held.st_uid
        || current.dev() != held.st_dev as u64
        || current.ino() != held.st_ino
        || (current.mode() & 0o022) != 0
    {
        return Err("the Hub configuration changed while it was read".into());
    }
    Ok(AdmittedConfigBytes {
        bytes,
        device: current.dev(),
        inode: current.ino(),
    })
}

/// The installation's configuration writer lock, beside the configuration.
#[cfg(unix)]
const CONFIG_WRITER_LOCK: &str = ".config-writer.lock";
/// How long `lan-tls` waits for another setup of the same installation.
#[cfg(unix)]
const LAN_TLS_LOCK_WAIT: Duration = Duration::from_secs(30);

/// Takes the installation's exclusive configuration writer lock (`flock` on an
/// owner-only file beside the configuration), waiting up to `wait`. `lan-tls` holds
/// it from its first read of the configuration through recovery, identity creation,
/// the final comparison and publication. The lock ends with its descriptor or its
/// process, so a pending marker found while it is held belongs to a run that can no
/// longer publish, and no other setup can replace the configuration after the
/// comparison.
#[cfg(unix)]
fn lock_config_writer(
    config_path: &Path,
    wait: Duration,
) -> Result<rustix::fd::OwnedFd, Box<dyn std::error::Error>> {
    use rustix::fs::{FlockOperation, flock};
    let folder = config_path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let descriptor = open(
        folder.join(CONFIG_WRITER_LOCK),
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| "the Hub configuration lock cannot be opened")?;
    let held = fstat(&descriptor).map_err(|_| "the Hub configuration lock cannot be read")?;
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || held.st_uid != getuid().as_raw()
        || (held.st_mode as u32 & 0o077) != 0
    {
        return Err("the Hub configuration lock is not a private file of this user".into());
    }
    let deadline = Instant::now() + wait;
    loop {
        match flock(&descriptor, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(descriptor),
            Err(Errno::WOULDBLOCK) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(Errno::WOULDBLOCK) => {
                return Err(
                    "another setup of this Hub's configuration is still running; try again".into(),
                );
            }
            Err(_) => return Err("the Hub configuration lock cannot be taken".into()),
        }
    }
}

/// Settles what earlier runs left in `tls/`: a run whose files the configuration
/// does not name lost before its configuration was published, so its files and
/// marker go; a run the configuration names only loses its marker. A file without a
/// marker of ours is never touched.
#[cfg(unix)]
fn recover_lan_tls_runs(
    directory: &Path,
    referenced: &[&Path],
) -> Result<(), Box<dyn std::error::Error>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut changed = false;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(lan_tls_pending_id) else {
            continue;
        };
        let [certificate, private_key, marker] = lan_tls_run_paths(directory, id);
        let named = referenced
            .iter()
            .any(|path| *path == certificate.as_path() || *path == private_key.as_path());
        if !named {
            for path in [&certificate, &private_key] {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        fs::remove_file(&marker)?;
        changed = true;
    }
    if changed {
        sync_directory(directory)?;
    }
    Ok(())
}

/// `lan-tls` (DEC-152): under the installation's configuration writer lock
/// ([`lock_config_writer`]), when the configuration has no `[tls]`, makes a self-signed
/// P-256 identity for `host` in the data folder's `tls/` (folder 0700, files 0600),
/// then sets `bind = "0.0.0.0:8443"` and `[tls]` with `https://<host>:8443/`.
///
/// It is one recoverable transaction: a pending marker claims the run's own file
/// names before any identity file exists; the key and certificate are written and
/// synced, with their folder, before the configuration names them; the new
/// configuration is parsed and its identity checked as `Serve` checks it, and it
/// replaces the file only when the file still holds the bytes that were read. On
/// any failure before that the run's files are removed and the configuration is
/// unchanged; a run lost to a crash is settled by the next run. With `[tls]`
/// already set the identity is checked and nothing is written. The private key is
/// never printed.
#[cfg(unix)]
fn setup_lan_tls(
    config_path: &Path,
    host: &str,
    now: SystemTime,
) -> Result<LanTlsReport, Box<dyn std::error::Error>> {
    setup_lan_tls_observed(config_path, host, now, &mut || {}, LAN_TLS_LOCK_WAIT)
}

/// `setup_lan_tls` with `before_replace` run just before the final check of the
/// configuration file (a test seam for a concurrent writer), waiting up to
/// `lock_wait` for the writer lock.
#[cfg(unix)]
fn setup_lan_tls_observed(
    config_path: &Path,
    host: &str,
    now: SystemTime,
    before_replace: &mut dyn FnMut(),
    lock_wait: Duration,
) -> Result<LanTlsReport, Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt as _;
    if !valid_lan_host(host) {
        return Err(
            "--host must be this Mac's lowercase .local name, such as office-mac.local".into(),
        );
    }
    // Held to the end of this function, every return included.
    let _writer = lock_config_writer(config_path, lock_wait)?;
    // One admitted read: the bytes that are checked are the bytes that are edited.
    let admitted = read_admitted_config(config_path)?;
    let (config, _) = HubConfig::from_exact_bytes(&admitted.bytes)?;
    let directory = config.data_dir.join(LAN_TLS_DIRECTORY);
    if let Some(tls) = &config.tls {
        server::validate_tls_identity(tls)?;
        // Markers of a run whose configuration was published are retired; this is
        // housekeeping only and never blocks the check.
        let _ = recover_lan_tls_runs(
            &directory,
            &[
                tls.certificate_path.as_path(),
                tls.private_key_path.as_path(),
            ],
        );
        return Ok(LanTlsReport {
            status: "existing",
            public_url: tls.public_url.clone(),
            bind: config.bind.to_string(),
            certificate_expires_at_ms: None,
        });
    }
    let mut table: toml::Table = std::str::from_utf8(&admitted.bytes)?.parse()?;

    let created_directory = prepare_lan_tls_directory(&directory)?;
    if created_directory {
        sync_directory(&config.data_dir)?;
    }
    recover_lan_tls_runs(&directory, &[])?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let [certificate_path, private_key_path, marker_path] = lan_tls_run_paths(&directory, &id);
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
    table.insert(
        "bind".to_owned(),
        toml::Value::String(LAN_TLS_BIND.to_owned()),
    );
    let mut tls = toml::Table::new();
    tls.insert(
        "certificate_path".to_owned(),
        toml::Value::String(certificate_text.to_owned()),
    );
    tls.insert(
        "private_key_path".to_owned(),
        toml::Value::String(private_key_text.to_owned()),
    );
    tls.insert(
        "public_url".to_owned(),
        toml::Value::String(public_url.clone()),
    );
    table.insert("tls".to_owned(), toml::Value::Table(tls));
    let updated = toml::to_string(&table)?;

    let folder = config_path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staged = folder.join(format!(".config.{}.tmp", uuid::Uuid::new_v4()));
    let mut created: Vec<PathBuf> = Vec::with_capacity(3);
    let prepared = (|| -> Result<(), Box<dyn std::error::Error>> {
        // The marker claims the run before any identity file exists.
        write_new_private_file(&marker_path, b"", &mut created)?;
        sync_directory(&directory)?;
        write_new_private_file(&private_key_path, private_key_pem.as_bytes(), &mut created)?;
        write_new_private_file(
            &certificate_path,
            certificate.pem().as_bytes(),
            &mut created,
        )?;
        sync_directory(&directory)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)?;
        created.push(staged.clone());
        file.write_all(updated.as_bytes())?;
        file.sync_all()?;
        let staged_config = HubConfig::load(&staged)?;
        let staged_tls = staged_config
            .tls
            .as_ref()
            .ok_or("the new configuration names no TLS identity")?;
        server::validate_tls_identity(staged_tls)?;
        // Another writer's change is never lost: the file must still be the one
        // read, with the same bytes, immediately before it is replaced.
        before_replace();
        let current = read_admitted_config(config_path)?;
        if current.device != admitted.device
            || current.inode != admitted.inode
            || current.bytes != admitted.bytes
        {
            return Err(
                "the Hub configuration changed while the LAN identity was prepared; nothing was changed, try again"
                    .into(),
            );
        }
        fs::rename(&staged, config_path)?;
        Ok(())
    })();
    if let Err(error) = prepared {
        for path in created.iter().rev() {
            let _ = fs::remove_file(path);
        }
        let _ = sync_directory(&directory);
        return Err(error);
    }
    // The configuration now names the identity, whose files are already durable. A
    // failed sync of its folder is reported; the files stay, and the next run
    // retires the marker.
    sync_directory(folder)
        .map_err(|_| "the new Hub configuration could not be confirmed on disk; check the disk, then try again")?;
    if fs::remove_file(&marker_path).is_ok() {
        let _ = sync_directory(&directory);
    }
    Ok(LanTlsReport {
        status: "created",
        public_url,
        bind: LAN_TLS_BIND.to_owned(),
        certificate_expires_at_ms: expires_at_ms,
    })
}

/// What `serving-check` reports: the installed Hub's process serves the
/// configuration on disk on its configured listener. `readiness` is `ready`, or
/// `between_windows` when the only unmet readiness check is the absent collector of
/// a closed collection window. Every other `/readyz` reason is a failure.
#[cfg(any(test, target_os = "macos"))]
#[derive(Debug, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ServingReport {
    status: &'static str,
    readiness: &'static str,
}

#[cfg(any(test, target_os = "macos"))]
/// Consecutive matching observations of one process that count as serving.
const SERVING_STABLE_OBSERVATIONS: usize = 3;
#[cfg(any(test, target_os = "macos"))]
const SERVING_POLL: Duration = Duration::from_millis(200);
#[cfg(any(test, target_os = "macos"))]
/// The longest `serving-check --wait-seconds` accepts.
const SERVING_MAX_WAIT_SECONDS: u64 = 300;

#[cfg(any(test, target_os = "macos"))]
/// The running process of a `launchctl print` answer: `state = running` and a
/// positive `pid`.
fn launchctl_running_pid(output: &str) -> Option<u32> {
    let running = output.lines().any(|line| line.trim() == "state = running");
    let pid = output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("pid = ")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|pid| *pid > 0)
    });
    running.then_some(pid).flatten()
}

#[cfg(any(test, target_os = "macos"))]
/// The judged `/readyz` answer of the expected process.
#[derive(Debug, PartialEq, Eq)]
enum ServingAnswer {
    /// Serving: `ready`, or `between_windows` (only the collector of a closed window
    /// is absent; catalogue, lifecycle, published content and credentials are sound).
    Serving(&'static str),
    /// The expected process answered but cannot serve: the redacted `/readyz` reason
    /// (credential store, catalogue, quarantine, unservable content, a stale or
    /// signed-out collector), or `unknown` when it named none.
    Refused(String),
    /// The listener presented a TLS certificate other than the configured one.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    CertificateRefused,
}

#[cfg(any(test, target_os = "macos"))]
/// The readiness answer of `pid` serving the configuration `digest`: a 200 `ready`,
/// or a 503 `not_ready` whose reason is judged. `None` when the native headers do not
/// name exactly that process and configuration, or the answer is malformed.
fn serving_answer(
    status: u16,
    process_id: Option<&str>,
    config_digest: Option<&str>,
    body: &[u8],
    pid: u32,
    digest: &str,
) -> Option<ServingAnswer> {
    #[derive(serde::Deserialize)]
    struct Answer {
        status: String,
        #[serde(default)]
        reason: Option<String>,
    }
    if process_id != Some(pid.to_string().as_str()) || config_digest != Some(digest) {
        return None;
    }
    let answer: Answer = serde_json::from_slice(body).ok()?;
    match (status, answer.status.as_str(), answer.reason.as_deref()) {
        (200, "ready", None) => Some(ServingAnswer::Serving("ready")),
        (503, "not_ready", Some("collector_absent")) => {
            Some(ServingAnswer::Serving("between_windows"))
        }
        (503, "not_ready", reason) => {
            let reason = reason
                .filter(|reason| {
                    !reason.is_empty()
                        && reason.len() <= 64
                        && reason
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                })
                .unwrap_or("unknown");
            Some(ServingAnswer::Refused(reason.to_owned()))
        }
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn installed_service_pid() -> Result<Option<u32>, Box<dyn std::error::Error>> {
    let service = format!(
        "gui/{}/com.tesatlas.hub",
        rustix::process::geteuid().as_raw()
    );
    let output = std::process::Command::new("/bin/launchctl")
        .arg("print")
        .arg(&service)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(launchctl_running_pid(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

/// One bounded `/readyz` request to the configured listener (loopback for an
/// all-interfaces bind), trusting over TLS only the exact configured certificate
/// (pinned by its bytes and checked for the public URL's host), so an older
/// CA-style LAN certificate is admitted and every other certificate is refused.
#[cfg(target_os = "macos")]
async fn serving_observation(
    config: &HubConfig,
    digest: &str,
    pid: u32,
) -> Result<Option<ServingAnswer>, Box<dyn std::error::Error>> {
    let address = match config.bind {
        std::net::SocketAddr::V4(bind) if bind.ip().is_unspecified() => {
            std::net::SocketAddr::from(([127, 0, 0, 1], bind.port()))
        }
        std::net::SocketAddr::V6(bind) if bind.ip().is_unspecified() => {
            std::net::SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], bind.port()))
        }
        bind => bind,
    };
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_millis(500))
        .timeout(Duration::from_secs(2));
    let endpoint = if let Some(tls) = &config.tls {
        let mut pem = Vec::new();
        fs::File::open(&tls.certificate_path)?
            .take(MAX_TLS_CERTIFICATE_CHAIN_BYTES as u64 + 1)
            .read_to_end(&mut pem)?;
        if pem.len() > MAX_TLS_CERTIFICATE_CHAIN_BYTES {
            return Err("the Hub certificate is too large".into());
        }
        let pinned = tesatlas_hub::crypto::pinned_listener_client_config(&pem)
            .map_err(|_| "the Hub certificate is empty or unreadable")?;
        let mut endpoint = reqwest::Url::parse(&tls.public_url)?;
        let host = endpoint
            .host_str()
            .ok_or("the Hub public URL has no host")?
            .to_owned();
        builder = builder
            .https_only(true)
            .tls_backend_preconfigured(pinned)
            .resolve(&host, address);
        endpoint
            .set_port(Some(address.port()))
            .map_err(|_| "the Hub public URL port is invalid")?;
        endpoint.set_path("/readyz");
        endpoint
    } else {
        if !address.ip().is_loopback() {
            return Err("a plain listener must be loopback".into());
        }
        reqwest::Url::parse(&format!("http://{address}/readyz"))?
    };
    let client = builder.build()?;
    let response = match client
        .get(endpoint)
        .header("accept-encoding", "identity")
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) if tesatlas_hub::crypto::is_certificate_refusal(&error) => {
            return Ok(Some(ServingAnswer::CertificateRefused));
        }
        Err(_) => return Ok(None),
    };
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let process_id = header("x-teslatlas-native-process-id");
    let config_digest = header(server::NATIVE_CONFIG_DIGEST_HEADER);
    let Ok(body) = response.bytes().await else {
        return Ok(None);
    };
    if body.len() > 16 * 1024 {
        return Ok(None);
    }
    Ok(serving_answer(
        status,
        process_id.as_deref(),
        config_digest.as_deref(),
        &body,
        pid,
        digest,
    ))
}

/// The plain reason `serving-check` gives when the listener's certificate is not
/// the configured one (the App shows it as the hand-over's reason).
#[cfg(target_os = "macos")]
const SERVING_CERTIFICATE_REFUSED: &str = "the Hub service answers on its listener with a different TLS certificate from the one in its configuration; restart the Hub service, and if that does not help, renew the LAN certificate";

/// `serving-check`: waits up to `wait` for the installed Hub job's process to hold
/// the Hub lifetime lock and answer on its configured listener for exactly the
/// configuration on disk, the same process in consecutive observations. It never
/// needs an open collection window or any Tesla request.
#[cfg(target_os = "macos")]
async fn check_serving(
    config_path: &Path,
    wait: Duration,
) -> Result<ServingReport, Box<dyn std::error::Error>> {
    tesatlas_hub::crypto::install_default_provider();
    let (config, digest) = HubConfig::load_with_digest(config_path)?;
    let digest = digest.to_string();
    let deadline = Instant::now() + wait;
    let mut last: Option<u32> = None;
    let mut consecutive = 0;
    let mut refused: Option<String>;
    let mut certificate_refused: bool;
    loop {
        let observed = match installed_service_pid()? {
            None => None,
            Some(pid) => match AdmittedUserHub::admit(&config.data_dir) {
                Err(tesatlas_hub::hub_user_process::UserLifetimeLockError::AlreadyRunning) => {
                    serving_observation(&config, &digest, pid)
                        .await?
                        .map(|readiness| (pid, readiness))
                }
                Ok(admission) => {
                    drop(admission);
                    None
                }
                Err(error) => return Err(error.into()),
            },
        };
        match observed {
            Some((_, ServingAnswer::Refused(reason))) => {
                consecutive = 0;
                last = None;
                refused = Some(reason);
                certificate_refused = false;
            }
            Some((_, ServingAnswer::CertificateRefused)) => {
                consecutive = 0;
                last = None;
                refused = None;
                certificate_refused = true;
            }
            Some((pid, ServingAnswer::Serving(readiness))) => {
                refused = None;
                certificate_refused = false;
                consecutive = if last == Some(pid) {
                    consecutive + 1
                } else {
                    1
                };
                last = Some(pid);
                if consecutive >= SERVING_STABLE_OBSERVATIONS {
                    return Ok(ServingReport {
                        status: "serving",
                        readiness,
                    });
                }
            }
            None => {
                consecutive = 0;
                last = None;
                refused = None;
                certificate_refused = false;
            }
        }
        if Instant::now() + SERVING_POLL >= deadline {
            return Err(match refused {
                Some(reason) => {
                    format!("the Hub service is running but cannot serve its history ({reason})")
                        .into()
                }
                None if certificate_refused => SERVING_CERTIFICATE_REFUSED.into(),
                None => "the Hub service is not serving its configuration on its listener".into(),
            });
        }
        tokio::time::sleep(SERVING_POLL).await;
    }
}
