// SPDX-License-Identifier: AGPL-3.0-only

#[cfg(unix)]
use std::future::Future;
use std::{
    collections::HashMap,
    fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path as FsPath,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use rustix::{
    fs::{FileType, Mode, OFlags, fstat, open},
    process::getuid,
};
use serde::{Deserialize, Serialize};
use tower::limit::GlobalConcurrencyLimitLayer;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};
use uuid::Uuid;

#[cfg(unix)]
use crate::config::HubConfig;
use crate::{
    BUILD_VERSION,
    config::TlsListenerConfig,
    corresponding_source_url,
    db::{HubStore, PairedDeviceRecord, PublishedVehicle, ReadinessReasonCode, StoredPack},
    fleet_telemetry::{FleetTelemetryAccumulator, MAX_FLEET_TELEMETRY_INPUT_BYTES, vin_from_json},
    http_range::{parse_single_range, unsatisfied_content_range},
    manifest_signing::ManifestSigning,
    protocol::{
        CursorKey, HUB_PROJECTION_SCHEMA_V1, HUB_PROJECTION_SCHEMA_V2, HUB_PROJECTION_SCHEMA_V3,
        LineageManifestV2, SchemaVersion, Sha256Digest, SyncManifest,
    },
};

pub const MAX_TLS_CERTIFICATE_CHAIN_BYTES: usize = 256 * 1024;
pub const MAX_TLS_PRIVATE_KEY_BYTES: usize = 64 * 1024;
const MAX_IN_FLIGHT_HTTP_REQUESTS: usize = 32;
const MAX_ACTIVE_PACK_STREAMS: usize = 8;
const MAX_ACTIVE_PACK_STREAMS_PER_DEVICE: usize = 2;
const PACK_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_HANDLER_TIMEOUT: Duration = Duration::from_secs(15);
const READINESS_CACHE_TTL: Duration = Duration::from_secs(1);

pub async fn rustls_config_from_identity(
    tls: &TlsListenerConfig,
) -> std::io::Result<axum_server::tls_rustls::RustlsConfig> {
    let certificate_pem = read_tls_identity_file(
        &tls.certificate_path,
        MAX_TLS_CERTIFICATE_CHAIN_BYTES,
        false,
    )?;
    let private_key_pem =
        read_tls_identity_file(&tls.private_key_path, MAX_TLS_PRIVATE_KEY_BYTES, true)?;
    rustls_config_from_pem_identity(certificate_pem, private_key_pem).await
}

/// Read-only validation used by diagnostics. It shares the exact no-follow,
/// ownership, permission, size, parse, and key-pair checks with `Serve`.
pub fn validate_tls_identity(tls: &TlsListenerConfig) -> std::io::Result<()> {
    let certificate_pem = read_tls_identity_file(
        &tls.certificate_path,
        MAX_TLS_CERTIFICATE_CHAIN_BYTES,
        false,
    )?;
    let private_key_pem =
        read_tls_identity_file(&tls.private_key_path, MAX_TLS_PRIVATE_KEY_BYTES, true)?;
    crate::crypto::install_default_provider();
    rustls_server_config_from_pem_identity(&certificate_pem, &private_key_pem).map(drop)
}

/// Read one exact TLS identity inode with fixed bounds and Unix ownership and
/// permission checks. Pairing and Serve deliberately share this function so a
/// device cannot pin different bytes from the listener identity.
#[doc(hidden)]
pub fn read_tls_identity_file(
    path: &FsPath,
    maximum: usize,
    private: bool,
) -> std::io::Result<zeroize::Zeroizing<Vec<u8>>> {
    read_tls_identity_file_after_open(path, maximum, private, || {})
}

/// Testable form of [`read_tls_identity_file`] with one hook after descriptor
/// admission. The hook exists so identity-replacement races remain covered.
#[doc(hidden)]
pub fn read_tls_identity_file_after_open(
    path: &FsPath,
    maximum: usize,
    private: bool,
    after_open: impl FnOnce(),
) -> std::io::Result<zeroize::Zeroizing<Vec<u8>>> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| std::io::Error::other("TLS identity file cannot be safely opened"))?;
    let held = fstat(&descriptor)
        .map_err(|_| std::io::Error::other("TLS identity file cannot be inspected"))?;
    let permission_mask = if private { 0o077 } else { 0o022 };
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || (held.st_mode as u32 & permission_mask) != 0
        || held.st_uid != getuid().as_raw()
    {
        return Err(std::io::Error::other("TLS identity file is unsafe"));
    }
    after_open();
    let file: fs::File = descriptor.into();
    let read_limit = maximum
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("TLS identity size limit is invalid"))?;
    let mut bytes = zeroize::Zeroizing::new(Vec::with_capacity(read_limit));
    (&file)
        .take(u64::try_from(read_limit).expect("TLS identity cap fits u64"))
        .read_to_end(&mut bytes)
        .map_err(|_| std::io::Error::other("TLS identity file cannot be read"))?;
    if bytes.len() > maximum {
        return Err(std::io::Error::other(
            "TLS identity exceeds the fixed size limit",
        ));
    }
    let after =
        fstat(&file).map_err(|_| std::io::Error::other("TLS identity file cannot be inspected"))?;
    let current = fs::symlink_metadata(path)
        .map_err(|_| std::io::Error::other("TLS identity file changed"))?;
    #[allow(clippy::useless_conversion)]
    let held_nlink = u64::from(held.st_nlink);
    if after.st_dev != held.st_dev
        || after.st_ino != held.st_ino
        || after.st_mode != held.st_mode
        || after.st_nlink != held.st_nlink
        || after.st_uid != held.st_uid
        || after.st_gid != held.st_gid
        || after.st_size != held.st_size
        || after.st_mtime != held.st_mtime
        || after.st_mtime_nsec != held.st_mtime_nsec
        || after.st_ctime != held.st_ctime
        || after.st_ctime_nsec != held.st_ctime_nsec
        || current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.uid() != getuid().as_raw()
        || current.dev() != held.st_dev as u64
        || current.ino() != held.st_ino
        || current.nlink() != held_nlink
        || current.uid() != held.st_uid
        || current.gid() != held.st_gid
        || current.mode() != held.st_mode as u32
        || current.len() != u64::try_from(held.st_size).unwrap_or(u64::MAX)
        || current.mtime() != held.st_mtime
        || current.mtime_nsec() != held.st_mtime_nsec as i64
        || current.ctime() != held.st_ctime
        || current.ctime_nsec() != held.st_ctime_nsec as i64
    {
        return Err(std::io::Error::other("TLS identity file changed"));
    }
    Ok(bytes)
}

/// Build the exact TLS identity used by `Serve` from already admitted bytes.
/// Pairing uses this boundary after its stricter no-follow bounded reads so it
/// cannot pin an identity that the native listener would reject.
#[doc(hidden)]
pub async fn rustls_config_from_pem_identity(
    certificate_pem: zeroize::Zeroizing<Vec<u8>>,
    private_key_pem: zeroize::Zeroizing<Vec<u8>>,
) -> std::io::Result<axum_server::tls_rustls::RustlsConfig> {
    crate::crypto::install_default_provider();
    let server_config = tokio::task::spawn_blocking(move || {
        rustls_server_config_from_pem_identity(&certificate_pem, &private_key_pem)
    })
    .await
    .map_err(|_| std::io::Error::other("TLS identity validation task failed"))??;
    Ok(axum_server::tls_rustls::RustlsConfig::from_config(
        Arc::new(server_config),
    ))
}

fn rustls_server_config_from_pem_identity(
    certificate_pem: &[u8],
    private_key_pem: &[u8],
) -> std::io::Result<rustls::ServerConfig> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

    let certificates = CertificateDer::pem_slice_iter(certificate_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| std::io::Error::other("failed to parse certificate"))?;
    let mut key_result: Result<zeroize::Zeroizing<PrivateKeyDer<'static>>, std::io::Error> = Err(
        std::io::Error::other("The private key file contained no keys"),
    );
    for item in PrivateKeyDer::pem_slice_iter(private_key_pem) {
        let key = item
            .map(zeroize::Zeroizing::new)
            .map_err(|_| std::io::Error::other("failed to parse PEM"));
        match key_result {
            Ok(_) => {
                if key.is_ok() {
                    return Err(std::io::Error::other(
                        "The private key file contains multiple keys",
                    ));
                }
            }
            Err(_) => key_result = key,
        }
    }
    let key = key_result?;
    let signing_key =
        rustls::crypto::ring::sign::any_supported_type(&key).map_err(std::io::Error::other)?;
    let certified_key = rustls::sign::CertifiedKey::new(certificates, signing_key);
    certified_key.keys_match().map_err(std::io::Error::other)?;
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(
            certified_key,
        )));
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

pub const MANIFEST_SIGNATURE_HEADER: &str = "x-teslatlas-manifest-signature";
pub const NATIVE_CONFIG_DIGEST_HEADER: &str = "x-teslatlas-native-config-sha256";
pub const SUPPORTED_SCHEMAS_HEADER: &str = "x-teslatlas-supported-schemas";
pub const SYNC_CAPABILITY_HEADER: &str = "x-teslatlas-sync-capability";
pub const DELTA_V2_CAPABILITY: &str = "delta-v2";

const HUB_PROJECTION_SCHEMAS: [SchemaVersion; 3] = [
    HUB_PROJECTION_SCHEMA_V1,
    HUB_PROJECTION_SCHEMA_V2,
    HUB_PROJECTION_SCHEMA_V3,
];
const MAX_SUPPORTED_SCHEMAS: usize = 16;
const SERVER_GRACE_PERIOD: Duration = Duration::from_secs(10);
// axum-server owns the connection drain timer. Keep a small extra bound here
// so this supervisor-owned task cannot remain attached if that timer ever
// fails to make forward progress.
const SERVER_GRACE_WAIT_LIMIT: Duration = Duration::from_secs(11);

/// Owns a listener task for the lifetime of its server supervisor.
///
/// `JoinHandle` drops detach by default. That is unsafe for a network listener:
/// cancellation of the outer Serve future would otherwise leave an unowned
/// endpoint accepting connections. Both TLS and plaintext use axum-server's
/// Handle so the cancellation contract is identical.
struct OwnedServer {
    handle: axum_server::Handle<std::net::SocketAddr>,
    task: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl OwnedServer {
    fn new(
        handle: axum_server::Handle<std::net::SocketAddr>,
        task: tokio::task::JoinHandle<std::io::Result<()>>,
    ) -> Self {
        Self {
            handle,
            task: Some(task),
        }
    }

    async fn wait(&mut self) -> std::io::Result<()> {
        // Do not take the handle before it resolves: dropping this branch of
        // the outer select must retain ownership for the shutdown branch and
        // for Drop.
        let result = self
            .task
            .as_mut()
            .expect("owned server task is awaited at most once")
            .await;
        let _ = self.task.take();
        join_server_task(result)
    }

    async fn graceful_shutdown_and_wait(&mut self) -> std::io::Result<()> {
        self.handle.graceful_shutdown(Some(SERVER_GRACE_PERIOD));
        let result = tokio::time::timeout(
            SERVER_GRACE_WAIT_LIMIT,
            self.task
                .as_mut()
                .expect("owned server task is awaited at most once"),
        )
        .await;
        match result {
            Ok(result) => {
                let _ = self.task.take();
                join_server_task(result)
            }
            Err(_) => {
                // The normal path always requests a graceful drain first. If
                // axum-server does not return within its hard-stop window,
                // force-release the listener and leave no detached task.
                self.handle.shutdown();
                let task = self
                    .task
                    .take()
                    .expect("owned server task is awaited at most once");
                task.abort();
                let _ = task.await;
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "Hub HTTP graceful shutdown exceeded its bounded deadline",
                ))
            }
        }
    }
}

impl Drop for OwnedServer {
    fn drop(&mut self) {
        // This covers abort/drop of serve_with_cursor_key while the outer
        // Serve supervisor is being cancelled. Signal the accept loop before
        // aborting so any already-observed shutdown path is unambiguous.
        self.handle.shutdown();
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
    }
}

#[cfg(unix)]
type ServerAdmission = Arc<crate::hub_user_process::AdmittedUserHub>;

#[derive(Clone)]
pub struct AppState {
    store: Arc<HubStore>,
    supervised_collector_required: bool,
    require_device_auth: bool,
    pairing_claim_enabled: bool,
    manifest_signing: Option<Arc<ManifestSigning>>,
    cursor_key: Option<Arc<CursorKey>>,
    native_config_digest: Option<Sha256Digest>,
    readiness_cache: Arc<Mutex<Option<CachedReadiness>>>,
    readiness_singleflight: Arc<tokio::sync::Semaphore>,
    pack_stream_slots: Arc<tokio::sync::Semaphore>,
    pack_streams_by_device: Arc<Mutex<HashMap<Uuid, usize>>>,
    fleet_telemetry: Option<Arc<FleetTelemetryIngress>>,
}

#[derive(Clone)]
struct FleetTelemetryIngress {
    token_digest: Sha256Digest,
    accumulators: Arc<tokio::sync::Mutex<HashMap<String, FleetTelemetryAccumulator>>>,
}

impl FleetTelemetryIngress {
    fn from_token_file(path: &FsPath) -> std::io::Result<Self> {
        const MAX_TOKEN_BYTES: usize = 256;
        const MIN_TOKEN_BYTES: usize = 32;
        let token = read_tls_identity_file(path, MAX_TOKEN_BYTES, true)
            .map_err(|_| std::io::Error::other("Fleet Telemetry ingress token is unavailable"))?;
        let token = token
            .strip_suffix(b"\r\n")
            .or_else(|| token.strip_suffix(b"\n"))
            .unwrap_or(&token);
        if token.len() < MIN_TOKEN_BYTES
            || !token
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(std::io::Error::other(
                "Fleet Telemetry ingress token is invalid",
            ));
        }
        Ok(Self {
            token_digest: Sha256Digest::of_bytes(token),
            accumulators: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        })
    }

    fn authorizes(&self, headers: &HeaderMap) -> bool {
        bearer_from_headers(headers)
            .is_some_and(|bearer| self.token_digest.matches(bearer.as_bytes()))
    }
}

#[derive(Clone, Copy)]
struct CachedReadiness {
    checked_at: Instant,
    result: Result<(), ReadinessReasonCode>,
}

impl AppState {
    fn new(
        store: HubStore,
        supervised_collector_required: bool,
        require_device_auth: bool,
        pairing_claim_enabled: bool,
        manifest_signing: Option<ManifestSigning>,
        cursor_key: Option<CursorKey>,
        native_config_digest: Option<Sha256Digest>,
        fleet_telemetry: Option<FleetTelemetryIngress>,
    ) -> Self {
        Self {
            store: Arc::new(store),
            supervised_collector_required,
            require_device_auth,
            pairing_claim_enabled,
            manifest_signing: manifest_signing.map(Arc::new),
            cursor_key: cursor_key.map(Arc::new),
            native_config_digest,
            readiness_cache: Arc::new(Mutex::new(None)),
            readiness_singleflight: Arc::new(tokio::sync::Semaphore::new(1)),
            pack_stream_slots: Arc::new(tokio::sync::Semaphore::new(MAX_ACTIVE_PACK_STREAMS)),
            pack_streams_by_device: Arc::new(Mutex::new(HashMap::new())),
            fleet_telemetry: fleet_telemetry.map(Arc::new),
        }
    }

    fn try_acquire_pack_device_slot(&self, device_id: Uuid) -> Option<PackDeviceSlot> {
        let mut counts = self.pack_streams_by_device.lock().ok()?;
        let active = counts.entry(device_id).or_default();
        if *active >= MAX_ACTIVE_PACK_STREAMS_PER_DEVICE {
            return None;
        }
        *active += 1;
        Some(PackDeviceSlot {
            device_id,
            counts: Arc::clone(&self.pack_streams_by_device),
        })
    }

    fn cached_readiness(&self) -> Option<Result<(), ReadinessReasonCode>> {
        let cache = match self.readiness_cache.lock() {
            Ok(cache) => cache,
            Err(_) => return Some(Err(ReadinessReasonCode::CatalogueUnavailable)),
        };
        cache.as_ref().and_then(|cached| {
            (cached.checked_at.elapsed() <= READINESS_CACHE_TTL).then_some(cached.result)
        })
    }

    async fn service_readiness(&self) -> Result<(), ReadinessReasonCode> {
        if let Some(result) = self.cached_readiness() {
            return result;
        }

        let permit = self
            .readiness_singleflight
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ReadinessReasonCode::CatalogueUnavailable)?;
        if let Some(result) = self.cached_readiness() {
            return result;
        }

        let store = Arc::clone(&self.store);
        let cache = Arc::clone(&self.readiness_cache);
        let supervised_collector_required = self.supervised_collector_required;
        tokio::task::spawn_blocking(move || {
            let result = current_epoch_ms()
                .map_err(|_| ReadinessReasonCode::CatalogueUnavailable)
                .and_then(|now_ms| {
                    store
                        .service_readiness_at(supervised_collector_required, now_ms)
                        .map_err(|failure| failure.code)
                });
            if let Ok(mut slot) = cache.lock() {
                *slot = Some(CachedReadiness {
                    checked_at: Instant::now(),
                    result,
                });
            }
            drop(permit);
            result
        })
        .await
        .unwrap_or(Err(ReadinessReasonCode::CatalogueUnavailable))
    }
}

/// Loopback/development router. It never accepts pairing claims: those carry a
/// bearer credential and must be exposed only by the TLS listener.
pub fn router(store: HubStore) -> Router {
    trusted_local_router(store, false, None, None)
}

fn trusted_local_router(
    store: HubStore,
    supervised_collector_required: bool,
    cursor_key: Option<CursorKey>,
    native_config_digest: Option<Sha256Digest>,
) -> Router {
    let manifest_signing = cursor_key.as_ref().map(ManifestSigning::from_cursor_key);
    router_with_access(
        store,
        supervised_collector_required,
        false,
        false,
        manifest_signing,
        cursor_key,
        native_config_digest,
    )
}

/// TLS-facing router. Every mirror endpoint requires a paired-device bearer
/// token; only the one-time pairing claim itself is unauthenticated. Requiring
/// the protected cursor key here makes an unsigned paired router impossible.
pub fn paired_router(store: HubStore, cursor_key: &CursorKey) -> Router {
    router_with_access(
        store,
        false,
        true,
        true,
        Some(ManifestSigning::from_cursor_key(cursor_key)),
        Some(cursor_key.clone()),
        None,
    )
}

fn router_with_access(
    store: HubStore,
    supervised_collector_required: bool,
    require_device_auth: bool,
    pairing_claim_enabled: bool,
    manifest_signing: Option<ManifestSigning>,
    cursor_key: Option<CursorKey>,
    native_config_digest: Option<Sha256Digest>,
) -> Router {
    router_with_access_and_telemetry(
        store,
        supervised_collector_required,
        require_device_auth,
        pairing_claim_enabled,
        manifest_signing,
        cursor_key,
        native_config_digest,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn router_with_access_and_telemetry(
    store: HubStore,
    supervised_collector_required: bool,
    require_device_auth: bool,
    pairing_claim_enabled: bool,
    manifest_signing: Option<ManifestSigning>,
    cursor_key: Option<CursorKey>,
    native_config_digest: Option<Sha256Digest>,
    fleet_telemetry: Option<FleetTelemetryIngress>,
) -> Router {
    let ordinary = Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/.well-known/teslatlas-hub", get(capabilities))
        .route("/v1/pairings/{pairing_id}/claim", post(claim_pairing))
        .route("/v1/device/rotate", post(rotate_device))
        .route("/v1/vehicles", get(vehicles))
        .route("/v1/vehicles/{vehicle_id}/current", get(current_vehicle))
        .route("/v1/vehicles/{vehicle_id}/sync/manifest", get(manifest))
        .route("/v1/vehicles/{vehicle_id}/sync/noop", get(schema_22_noop))
        .route("/v1/packs/sha256/{object_name}", get(pack))
        .layer(DefaultBodyLimit::max(4 * 1024));
    let telemetry = Router::new()
        .route("/v1/internal/fleet-telemetry", post(ingest_fleet_telemetry))
        .layer(DefaultBodyLimit::max(MAX_FLEET_TELEMETRY_INPUT_BYTES));
    let router = ordinary
        .merge(telemetry)
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(AppState::new(
            store,
            supervised_collector_required,
            require_device_auth,
            pairing_claim_enabled,
            manifest_signing,
            cursor_key,
            native_config_digest,
            fleet_telemetry,
        ));
    apply_http_resource_limits(router, MAX_IN_FLIGHT_HTTP_REQUESTS, HTTP_HANDLER_TIMEOUT)
}

fn apply_http_resource_limits(router: Router, maximum: usize, timeout: Duration) -> Router {
    router
        .layer(GlobalConcurrencyLimitLayer::new(maximum))
        // This is outermost, so time spent waiting for a handler slot is also
        // bounded. Streaming response bodies are deliberately not timed out;
        // their file descriptors are bounded separately by pack_stream_slots.
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            timeout,
        ))
}

/// Serve from the one admitted Hub process. Its durable cursor key is kept
/// under the admitted data directory after the local lock is revalidated.
#[cfg(unix)]
#[doc(hidden)]
pub async fn serve_for_admitted_user<F>(
    store: HubStore,
    config: &HubConfig,
    native_config_digest: Sha256Digest,
    admission: std::sync::Arc<crate::hub_user_process::AdmittedUserHub>,
    cursor_key: Option<CursorKey>,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    admission.assert_sensitive_access().map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("Hub admission is unavailable: {error}"),
        )
    })?;
    let cursor_key = match (cursor_key, config.tls.is_some()) {
        (Some(cursor_key), _) => Some(cursor_key),
        (None, true) => Some(
            crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir).map_err(
                |error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("TLS cursor key is unavailable: {error}"),
                    )
                },
            )?,
        ),
        (None, false) => {
            crate::teslamate_credentials::load_existing_cursor_key_bytes(&config.data_dir)
                .map_err(|error| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("cursor key is unavailable: {error}"),
                    )
                })?
                .map(|bytes| {
                    let mut key = [0_u8; 32];
                    key.copy_from_slice(bytes.as_slice());
                    CursorKey::from_bytes(key)
                })
        }
    };
    serve_with_cursor_key(
        store,
        config,
        native_config_digest,
        cursor_key,
        Some(admission),
        shutdown,
    )
    .await
}

#[cfg(unix)]
async fn serve_with_cursor_key<F>(
    store: HubStore,
    config: &HubConfig,
    native_config_digest: Sha256Digest,
    cursor_key: Option<crate::protocol::CursorKey>,
    admission: Option<ServerAdmission>,
    shutdown: F,
) -> std::io::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    crate::crypto::install_default_provider();
    let supervised_collector_required = config.collector.interval_seconds > 0;
    let fleet_telemetry = config
        .collector
        .fleet_telemetry
        .as_ref()
        .map(|telemetry| FleetTelemetryIngress::from_token_file(&telemetry.ingest_token_path))
        .transpose()?;
    if let Some(tls) = &config.tls {
        let cursor_key = cursor_key.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "TLS manifest signing credential is unavailable",
            )
        })?;
        let tls_config = rustls_config_from_identity(tls).await?;
        revalidate_server_admission(admission.as_ref())?;
        let listener = std::net::TcpListener::bind(config.bind)?;
        listener.set_nonblocking(true)?;
        let handle = axum_server::Handle::new();
        let server = axum_server::from_tcp_rustls(listener, tls_config)?.handle(handle.clone());
        let mut server_task = OwnedServer::new(
            handle,
            tokio::spawn(
                server.serve(
                    router_with_access_and_telemetry(
                        store,
                        supervised_collector_required,
                        true,
                        true,
                        Some(ManifestSigning::from_cursor_key(&cursor_key)),
                        Some(cursor_key.clone()),
                        Some(native_config_digest),
                        fleet_telemetry,
                    )
                    .into_make_service(),
                ),
            ),
        );
        let result = tokio::select! {
            result = server_task.wait() => result,
            () = shutdown => {
                server_task.graceful_shutdown_and_wait().await
            }
        };
        result
    } else {
        revalidate_server_admission(admission.as_ref())?;
        // Keep the same cancellation ownership as the TLS path. A dropped
        // JoinHandle would otherwise leave plaintext loopback service alive.
        let listener = std::net::TcpListener::bind(config.bind)?;
        listener.set_nonblocking(true)?;
        let handle = axum_server::Handle::new();
        let server = axum_server::from_tcp(listener)?.handle(handle.clone());
        let mut server_task = OwnedServer::new(
            handle,
            tokio::spawn(
                server.serve(
                    router_with_access_and_telemetry(
                        store,
                        supervised_collector_required,
                        false,
                        false,
                        cursor_key.as_ref().map(ManifestSigning::from_cursor_key),
                        cursor_key,
                        Some(native_config_digest),
                        fleet_telemetry,
                    )
                    .into_make_service(),
                ),
            ),
        );
        let result = tokio::select! {
            result = server_task.wait() => result,
            () = shutdown => {
                server_task.graceful_shutdown_and_wait().await
            }
        };
        result
    }
}

fn join_server_task(
    result: Result<std::io::Result<()>, tokio::task::JoinError>,
) -> std::io::Result<()> {
    result.map_err(|error| std::io::Error::other(format!("Hub server task failed: {error}")))?
}

#[cfg(unix)]
fn revalidate_server_admission(admission: Option<&ServerAdmission>) -> std::io::Result<()> {
    admission
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "server admission is unavailable",
            )
        })?
        .assert_sensitive_access()
        .map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Hub admission is unavailable: {error}"),
            )
        })
}

async fn health() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(Health {
            status: "ok",
            version: BUILD_VERSION,
        }),
    )
}

async fn ingest_fleet_telemetry(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(ingress) = state.fleet_telemetry.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !ingress.authorizes(&headers) {
        return unauthorized();
    }
    let Some(cursor_key) = state.cursor_key.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let vin = match vin_from_json(&body) {
        Ok(vin) => vin,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    // Serialize apply+commit per process. The receiver acknowledges only a
    // 2xx response, so publish the candidate accumulator after SQLite commits;
    // a transient failure can then be retried without losing the transaction.
    let mut accumulators = ingress.accumulators.lock().await;
    let mut candidate = if let Some(accumulator) = accumulators.get(&vin) {
        accumulator.clone()
    } else {
        match crate::collector::fleet_telemetry_seed_for_vin(&state.store, &vin) {
            Ok(Some(owner_data)) => match FleetTelemetryAccumulator::restore(&vin, &owner_data) {
                Ok(accumulator) => accumulator,
                Err(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
            },
            Ok(None) => match FleetTelemetryAccumulator::empty(&vin) {
                Ok(accumulator) => accumulator,
                Err(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
            },
            Err(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
        }
    };
    let snapshot = match candidate.apply_json(&body) {
        Ok(snapshot) => snapshot,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match crate::collector::persist_fleet_telemetry_snapshot(&state.store, cursor_key, &snapshot)
        .await
    {
        Ok(_) => {
            accumulators.insert(vin, candidate);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            tracing::error!(error = %error, "Fleet Telemetry commit failed");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let readiness = state.service_readiness().await;
    let mut response = match readiness {
        Ok(()) => (
            StatusCode::OK,
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(Ready {
                status: "ready",
                reason: None,
            }),
        )
            .into_response(),
        Err(reason) => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(Ready {
                status: "not_ready",
                reason: Some(reason),
            }),
        )
            .into_response(),
    };
    if let Some(digest) = state.native_config_digest {
        response.headers_mut().insert(
            header::HeaderName::from_static(NATIVE_CONFIG_DIGEST_HEADER),
            HeaderValue::from_str(&digest.to_string()).expect("SHA-256 digest is an HTTP header"),
        );
    }
    response
}

async fn capabilities(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(Capabilities {
            protocol: "teslatlas-sync",
            protocol_major: 1,
            version: BUILD_VERSION,
            source_url: corresponding_source_url(),
            pack_format: "sqlite-zstd",
            manifest_public_key: state
                .manifest_signing
                .as_deref()
                .map(ManifestSigning::verifying_key_hex),
        }),
    )
}

async fn vehicles(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(status) = require_authorized_device(&state, &headers) {
        return device_auth_reject(status);
    }
    match state.store.published_vehicles() {
        Ok(vehicles) => Json(VehicleList { vehicles }).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot load published vehicles");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn current_vehicle(
    State(state): State<AppState>,
    Path(vehicle_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(status) = require_authorized_device(&state, &headers) {
        return device_auth_reject(status);
    }
    let Ok(vehicle_id) = Uuid::parse_str(&vehicle_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let observations = match state.store.current_observations_for_vehicle(vehicle_id) {
        Ok(observations) => observations,
        Err(crate::db::StoreError::UnknownVehicle(_)) => {
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load current vehicle observations");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let car = match state.store.materialised_car_for_vehicle(vehicle_id) {
        Ok(car) => car,
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load current vehicle identity");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let lifecycle = match state.store.load_lifecycle_state(vehicle_id) {
        Ok(lifecycle) => lifecycle,
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load current vehicle lifecycle");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let mut summary = crate::current_state::build_current_vehicle_summary(
        vehicle_id,
        &observations,
        car,
        lifecycle.as_ref(),
        None,
    );
    if let (Some(latitude), Some(longitude)) = (summary.latitude, summary.longitude) {
        summary.geofence = match state
            .store
            .geofence_name_at(vehicle_id, latitude, longitude)
        {
            Ok(geofence) => geofence,
            Err(error) => {
                tracing::error!(%error, %vehicle_id, "cannot match current vehicle geofence");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        };
    }
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(summary),
    )
        .into_response()
}

async fn claim_pairing(
    State(state): State<AppState>,
    Path(pairing_id): Path<String>,
    Json(request): Json<PairingClaimRequest>,
) -> Response {
    if !state.pairing_claim_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(pairing_id) = Uuid::parse_str(&pairing_id) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Ok(claimed_at_ms) = current_epoch_ms() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match state.store.claim_pairing(
        pairing_id,
        request.secret.as_str(),
        &request.device_name,
        claimed_at_ms,
    ) {
        Ok(mut access) => Json(PairingClaimResponse {
            device_id: access.device_id,
            access_token: access.access_token.take_bearer().into(),
            expires_at_ms: access.expires_at_ms,
        })
        .into_response(),
        // Do not distinguish a bad secret from expiry, reuse, or an unknown
        // identifier. All of those are invalid pairing authorization.
        Err(crate::db::StoreError::PairingRejected) => StatusCode::UNAUTHORIZED.into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot claim pairing invitation");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn rotate_device(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !state.require_device_auth {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(bearer) = bearer_from_headers(&headers) else {
        return unauthorized();
    };
    let Ok(now_ms) = current_epoch_ms() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match state.store.rotate_device(bearer, now_ms) {
        Ok(mut access) => Json(PairingClaimResponse {
            device_id: access.device_id,
            access_token: access.access_token.take_bearer().into(),
            expires_at_ms: access.expires_at_ms,
        })
        .into_response(),
        Err(crate::db::StoreError::PairingRejected) => unauthorized(),
        Err(error) => {
            tracing::error!(%error, "cannot rotate paired device bearer");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn manifest(
    State(state): State<AppState>,
    Path(vehicle_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(status) = require_authorized_device(&state, &headers) {
        return device_auth_reject(status);
    }
    let Ok(vehicle_id) = Uuid::parse_str(&vehicle_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let capability = match requested_sync_capability(&headers) {
        Ok(capability) => capability,
        Err(()) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if capability == SyncCapabilityRequest::DeltaV2 {
        if headers.contains_key(SUPPORTED_SCHEMAS_HEADER)
            && negotiate_hub_projection_schema(&headers, HUB_PROJECTION_SCHEMA_V2).is_err()
        {
            return StatusCode::NOT_ACCEPTABLE.into_response();
        }
        return match state.store.lineage_manifest_for_vehicle(vehicle_id) {
            Ok(Some(lineage)) => {
                match no_store_lineage_manifest(lineage, state.manifest_signing.as_deref()) {
                    Ok(response) => response,
                    Err(error) => {
                        tracing::error!(%error, "cannot serialize v2 lineage manifest");
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    }
                }
            }
            Ok(None) => StatusCode::NOT_ACCEPTABLE.into_response(),
            Err(error) => {
                tracing::error!(%error, "cannot load v2 lineage manifest");
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        };
    }
    match state.store.manifest_for_vehicle(vehicle_id) {
        Ok(Some(manifest)) => {
            if let Err(error) = negotiate_hub_projection_schema(&headers, manifest.schema) {
                return match error {
                    SchemaNegotiationError::InvalidHeader => {
                        StatusCode::BAD_REQUEST.into_response()
                    }
                    SchemaNegotiationError::NoCompatibleSchema => {
                        StatusCode::NOT_ACCEPTABLE.into_response()
                    }
                };
            }
            if manifest.schema == HUB_PROJECTION_SCHEMA_V3 {
                let Some(cursor_key) = state.cursor_key.as_deref() else {
                    tracing::error!("schema 2.2 serving requires the active cursor key");
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                };
                return match crate::updates_delivery::schema_22_signed_artifacts(
                    &state.store,
                    vehicle_id,
                    cursor_key,
                ) {
                    Ok((manifest_bytes, _)) => {
                        no_store_json_bytes(manifest_bytes, state.manifest_signing.as_deref())
                    }
                    Err(error) => {
                        tracing::error!(%error, "schema 2.2 manifest/no-op pair is unavailable");
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    }
                };
            }
            match no_store_manifest(manifest, state.manifest_signing.as_deref()) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(%error, "cannot serialize sync manifest");
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                }
            }
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::db::StoreError::SchemaPublicationUnavailable(_)) => {
            StatusCode::NOT_ACCEPTABLE.into_response()
        }
        Err(error) => {
            tracing::error!(%error, "cannot load sync manifest");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncCapabilityRequest {
    Legacy,
    DeltaV2,
}

fn requested_sync_capability(headers: &HeaderMap) -> Result<SyncCapabilityRequest, ()> {
    let Some(values) = headers.get(SYNC_CAPABILITY_HEADER) else {
        return Ok(SyncCapabilityRequest::Legacy);
    };
    let value = values.to_str().map_err(|_| ())?.trim();
    match value {
        DELTA_V2_CAPABILITY => Ok(SyncCapabilityRequest::DeltaV2),
        "" => Err(()),
        _ => Err(()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaNegotiationError {
    InvalidHeader,
    NoCompatibleSchema,
}

fn negotiate_hub_projection_schema(
    headers: &HeaderMap,
    stored_schema: SchemaVersion,
) -> Result<SchemaVersion, SchemaNegotiationError> {
    // Schema 2.2 is full-snapshot-only. A missing client header must never
    // accidentally opt a 2.2 manifest into serving for a 2.0/2.1 client.
    if stored_schema == HUB_PROJECTION_SCHEMA_V3 && !headers.contains_key(SUPPORTED_SCHEMAS_HEADER)
    {
        return Err(SchemaNegotiationError::NoCompatibleSchema);
    }
    if !headers.contains_key(SUPPORTED_SCHEMAS_HEADER) {
        return Ok(stored_schema);
    }
    let mut supported = Vec::new();
    for header_value in headers.get_all(SUPPORTED_SCHEMAS_HEADER) {
        let header_value = header_value
            .to_str()
            .map_err(|_| SchemaNegotiationError::InvalidHeader)?;
        for value in header_value.split(',') {
            if supported.len() >= MAX_SUPPORTED_SCHEMAS {
                return Err(SchemaNegotiationError::InvalidHeader);
            }
            supported.push(parse_schema_version(value.trim())?);
        }
    }
    HUB_PROJECTION_SCHEMAS
        .iter()
        .find(|candidate| **candidate == stored_schema && supported.contains(candidate))
        .copied()
        .ok_or(SchemaNegotiationError::NoCompatibleSchema)
}

fn parse_schema_version(value: &str) -> Result<SchemaVersion, SchemaNegotiationError> {
    let (major, minor) = value
        .split_once('.')
        .ok_or(SchemaNegotiationError::InvalidHeader)?;
    if major.is_empty()
        || minor.is_empty()
        || minor.contains('.')
        || !major.bytes().all(|byte| byte.is_ascii_digit())
        || !minor.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(SchemaNegotiationError::InvalidHeader);
    }
    Ok(SchemaVersion {
        major: major
            .parse()
            .map_err(|_| SchemaNegotiationError::InvalidHeader)?,
        minor: minor
            .parse()
            .map_err(|_| SchemaNegotiationError::InvalidHeader)?,
    })
}

async fn schema_22_noop(
    State(state): State<AppState>,
    Path(vehicle_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Err(status) = require_authorized_device(&state, &headers) {
        return device_auth_reject(status);
    }
    let Ok(vehicle_id) = Uuid::parse_str(&vehicle_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if negotiate_hub_projection_schema(&headers, HUB_PROJECTION_SCHEMA_V3).is_err() {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    }
    let Some(cursor_key) = state.cursor_key.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match crate::updates_delivery::schema_22_signed_artifacts(&state.store, vehicle_id, cursor_key)
    {
        Ok((_, noop_bytes)) => no_store_json_bytes(noop_bytes, state.manifest_signing.as_deref()),
        Err(error) => {
            tracing::error!(%error, "schema 2.2 no-op pair is unavailable");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn pack(
    State(state): State<AppState>,
    Path(object_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let device = match require_authorized_device(&state, &headers) {
        Ok(device) => device,
        Err(status) => return device_auth_reject(status),
    };
    let Some(digest) = object_name.strip_suffix(".sqlite.zst") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(digest) = digest.parse::<Sha256Digest>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let stored = match state.store.pack_for_digest(digest) {
        Ok(Some(pack)) => pack,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(crate::db::StoreError::SchemaPublicationUnavailable(_)) => {
            return StatusCode::NOT_ACCEPTABLE.into_response();
        }
        Err(error) => {
            tracing::error!(%error, "cannot load pack catalog entry");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let Some(device_slot) = state.try_acquire_pack_device_slot(device.device_id) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, HeaderValue::from_static("1"))],
        )
            .into_response();
    };
    let permit = match state.pack_stream_slots.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::RETRY_AFTER, HeaderValue::from_static("1"))],
            )
                .into_response();
        }
    };
    stream_pack(
        stored,
        headers
            .get(header::RANGE)
            .and_then(|header| header.to_str().ok()),
        permit,
        device_slot,
    )
    .await
}

fn authorize_device(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<PairedDeviceRecord>, crate::db::StoreError> {
    if !state.require_device_auth {
        // A public loopback router has no device principal by design. The
        // caller uses `Ok(None)` only after a required-auth lookup fails.
        return Ok(Some(PairedDeviceRecord {
            device_id: Uuid::nil(),
            display_name: String::new(),
            created_at_ms: 0,
            expires_at_ms: i64::MAX,
            revoked_at_ms: None,
            last_authenticated_at_ms: None,
        }));
    }
    let Some(bearer) = bearer_from_headers(headers) else {
        return Ok(None);
    };
    state.store.authenticate_device(bearer)
}

fn require_authorized_device(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<PairedDeviceRecord, StatusCode> {
    device_auth_response(device_auth_from_store(authorize_device(state, headers)))
}

fn device_auth_response(decision: DeviceAuthDecision) -> Result<PairedDeviceRecord, StatusCode> {
    match decision {
        DeviceAuthDecision::Allow(device) => Ok(device),
        DeviceAuthDecision::Unauthorized => Err(StatusCode::UNAUTHORIZED),
        DeviceAuthDecision::Unavailable => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

fn device_auth_reject(status: StatusCode) -> Response {
    if status == StatusCode::UNAUTHORIZED {
        unauthorized()
    } else {
        status.into_response()
    }
}

#[derive(Debug)]
enum DeviceAuthDecision {
    Allow(PairedDeviceRecord),
    Unauthorized,
    Unavailable,
}

fn device_auth_from_store(
    result: Result<Option<PairedDeviceRecord>, crate::db::StoreError>,
) -> DeviceAuthDecision {
    match result {
        Ok(Some(device)) => DeviceAuthDecision::Allow(device),
        Ok(None) => DeviceAuthDecision::Unauthorized,
        Err(error) => {
            tracing::error!(%error, "cannot authenticate paired device");
            DeviceAuthDecision::Unavailable
        }
    }
}

fn bearer_from_headers(headers: &HeaderMap) -> Option<&str> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?
        .strip_prefix("Bearer ")?;
    if bearer.is_empty() || bearer.contains(char::is_whitespace) {
        None
    } else {
        Some(bearer)
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"))],
    )
        .into_response()
}

fn current_epoch_ms() -> Result<i64, std::time::SystemTimeError> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
    Ok(i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

fn no_store_manifest(
    manifest: SyncManifest,
    signing: Option<&ManifestSigning>,
) -> Result<Response, serde_json::Error> {
    let raw_json = serde_json::to_vec(&manifest)?;
    Ok(no_store_json_bytes(raw_json, signing))
}

fn no_store_json_bytes(raw_json: Vec<u8>, signing: Option<&ManifestSigning>) -> Response {
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    if let Some(signing) = signing {
        let signature = HeaderValue::from_str(&signing.sign_base64(&raw_json))
            .expect("base64 Ed25519 signature is an HTTP header value");
        response = response.header(MANIFEST_SIGNATURE_HEADER, signature);
    }
    response
        .body(Body::from(raw_json))
        .expect("static JSON response headers are valid")
}

fn no_store_lineage_manifest(
    manifest: LineageManifestV2,
    signing: Option<&ManifestSigning>,
) -> Result<Response, serde_json::Error> {
    let raw_json = serde_json::to_vec(&manifest)?;
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.teslatlas.sync-lineage+json"),
        )
        .header(
            header::ETAG,
            HeaderValue::from_str(&format!("\"{}\"", manifest.head_digest))
                .expect("digest is a valid HTTP header value"),
        );
    if let Some(signing) = signing {
        let signature = HeaderValue::from_str(&signing.sign_base64(&raw_json))
            .expect("base64 Ed25519 signature is an HTTP header value");
        response = response.header(MANIFEST_SIGNATURE_HEADER, signature);
    }
    Ok(response
        .body(Body::from(raw_json))
        .expect("static lineage response headers are valid"))
}

struct PackDeviceSlot {
    device_id: Uuid,
    counts: Arc<Mutex<HashMap<Uuid, usize>>>,
}

impl Drop for PackDeviceSlot {
    fn drop(&mut self) {
        let Ok(mut counts) = self.counts.lock() else {
            return;
        };
        if let Some(active) = counts.get_mut(&self.device_id) {
            *active = active.saturating_sub(1);
            if *active == 0 {
                counts.remove(&self.device_id);
            }
        }
    }
}

async fn stream_pack(
    stored: StoredPack,
    range_header: Option<&str>,
    permit: tokio::sync::OwnedSemaphorePermit,
    device_slot: PackDeviceSlot,
) -> Response {
    let mut file = match tokio::fs::File::open(&stored.path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::error!(digest = %stored.digest, "published pack file is missing");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(error) => {
            tracing::error!(%error, digest = %stored.digest, "cannot open pack file");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let metadata = match file.metadata().await {
        Ok(metadata) if metadata.is_file() && metadata.len() == stored.compressed_bytes => metadata,
        Ok(_) => {
            tracing::error!(digest = %stored.digest, "published pack file size mismatch");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(error) => {
            tracing::error!(%error, digest = %stored.digest, "cannot stat pack file");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let range = match parse_single_range(range_header, metadata.len()) {
        Ok(range) => range,
        Err(_) => return range_not_satisfiable(metadata.len()),
    };
    if let Err(error) =
        tokio::io::AsyncSeekExt::seek(&mut file, std::io::SeekFrom::Start(range.start)).await
    {
        tracing::error!(%error, digest = %stored.digest, "cannot seek pack file");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let content_length = match HeaderValue::from_str(&range.len().to_string()) {
        Ok(value) => value,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let etag = match HeaderValue::from_str(&format!("\"{}\"", stored.digest)) {
        Ok(value) => value,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let mut response = Response::builder()
        .status(if range.is_partial() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=31536000, immutable"),
        )
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.teslatlas.sync-pack"),
        )
        .header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"))
        .header(header::CONTENT_LENGTH, content_length)
        .header(header::ETAG, etag);
    if range.is_partial() {
        let content_range = match HeaderValue::from_str(&range.content_range()) {
            Ok(value) => value,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        response = response.header(header::CONTENT_RANGE, content_range);
    }
    response
        .body(pack_stream_body(
            tokio::io::AsyncReadExt::take(file, range.len()),
            permit,
            device_slot,
            PACK_STREAM_IDLE_TIMEOUT,
        ))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn pack_stream_body<R>(
    mut reader: R,
    permit: tokio::sync::OwnedSemaphorePermit,
    device_slot: PackDeviceSlot,
    idle_timeout: Duration,
) -> Body
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    // The producer, rather than the HTTP body, owns both permits. A client that
    // stops reading fills this small channel; the bounded send then expires and
    // releases the file, global slot, and per-device slot.
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    tokio::spawn(async move {
        let _permit = permit;
        let _device_slot = device_slot;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = match tokio::time::timeout(
                idle_timeout,
                tokio::io::AsyncReadExt::read(&mut reader, &mut buffer),
            )
            .await
            {
                Ok(Ok(0)) => break,
                Ok(Ok(read)) => read,
                Ok(Err(error)) => {
                    let _ = tokio::time::timeout(idle_timeout, sender.send(Err(error))).await;
                    break;
                }
                Err(_) => break,
            };
            let chunk = Bytes::copy_from_slice(&buffer[..read]);
            if !matches!(
                tokio::time::timeout(idle_timeout, sender.send(Ok(chunk))).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
    });
    Body::from_stream(stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    }))
}

fn range_not_satisfiable(complete_length: u64) -> Response {
    let content_range = HeaderValue::from_str(&unsatisfied_content_range(complete_length))
        .expect("content range made only from digits");
    (
        StatusCode::RANGE_NOT_SATISFIABLE,
        [
            (header::ACCEPT_RANGES, HeaderValue::from_static("bytes")),
            (header::CONTENT_RANGE, content_range),
        ],
    )
        .into_response()
}

#[derive(Serialize)]
struct Health<'a> {
    status: &'a str,
    version: &'a str,
}

#[derive(Serialize)]
struct Ready<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<ReadinessReasonCode>,
}

#[derive(Serialize)]
struct Capabilities<'a> {
    protocol: &'a str,
    protocol_major: u8,
    version: &'a str,
    #[serde(rename = "sourceUrl")]
    source_url: String,
    pack_format: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "manifestPublicKey")]
    manifest_public_key: Option<String>,
}

#[derive(Deserialize)]
struct PairingClaimRequest {
    secret: PairingSecretInput,
    device_name: String,
}

struct PairingClaimResponse {
    device_id: Uuid,
    access_token: PairingBearer,
    expires_at_ms: i64,
}

impl Serialize for PairingClaimResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut response = serializer.serialize_struct("PairingClaimResponse", 3)?;
        response.serialize_field("device_id", &self.device_id)?;
        response.serialize_field("access_token", self.access_token.0.as_str())?;
        response.serialize_field("expires_at_ms", &self.expires_at_ms)?;
        response.end()
    }
}

struct PairingSecretInput(zeroize::Zeroizing<String>);

impl<'de> Deserialize<'de> for PairingSecretInput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer).map(|secret| Self(zeroize::Zeroizing::new(secret)))
    }
}

impl PairingSecretInput {
    fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

struct PairingBearer(zeroize::Zeroizing<String>);

impl From<zeroize::Zeroizing<String>> for PairingBearer {
    fn from(value: zeroize::Zeroizing<String>) -> Self {
        Self(value)
    }
}

impl std::fmt::Debug for PairingBearer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PairingBearer([redacted])")
    }
}

impl std::fmt::Debug for PairingSecretInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PairingSecretInput([redacted])")
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VehicleList {
    vehicles: Vec<PublishedVehicle>,
}

#[cfg(test)]
#[path = "server/tests.rs"]
mod tests;
