use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use uuid::Uuid;

use crate::{
    BUILD_VERSION,
    config::HubConfig,
    credentials::CredentialDirectory,
    db::{HubStore, PairedDeviceRecord, PublishedVehicle, StoredPack},
    http_range::{parse_single_range, unsatisfied_content_range},
    manifest_signing::ManifestSigning,
    protocol::{CursorKey, Sha256Digest, SyncManifest},
};

pub const MANIFEST_SIGNATURE_HEADER: &str = "x-teslatlas-manifest-signature";

#[derive(Clone)]
pub struct AppState {
    store: Arc<HubStore>,
    require_device_auth: bool,
    pairing_claim_enabled: bool,
    manifest_signing: Option<Arc<ManifestSigning>>,
}

impl AppState {
    fn new(
        store: HubStore,
        require_device_auth: bool,
        pairing_claim_enabled: bool,
        manifest_signing: Option<ManifestSigning>,
    ) -> Self {
        Self {
            store: Arc::new(store),
            require_device_auth,
            pairing_claim_enabled,
            manifest_signing: manifest_signing.map(Arc::new),
        }
    }
}

/// Loopback/development router. It never accepts pairing claims: those carry a
/// bearer credential and must be exposed only by the TLS listener.
pub fn router(store: HubStore) -> Router {
    router_with_access(store, false, false, None)
}

/// TLS-facing router. Every mirror endpoint requires a paired-device bearer
/// token; only the one-time pairing claim itself is unauthenticated. Requiring
/// the protected cursor key here makes an unsigned paired router impossible.
pub fn paired_router(store: HubStore, cursor_key: &CursorKey) -> Router {
    router_with_access(
        store,
        true,
        true,
        Some(ManifestSigning::from_cursor_key(cursor_key)),
    )
}

fn router_with_access(
    store: HubStore,
    require_device_auth: bool,
    pairing_claim_enabled: bool,
    manifest_signing: Option<ManifestSigning>,
) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/.well-known/teslatlas-hub", get(capabilities))
        .route("/v1/pairings/{pairing_id}/claim", post(claim_pairing))
        .route("/v1/vehicles", get(vehicles))
        .route("/v1/vehicles/{vehicle_id}/sync/manifest", get(manifest))
        .route("/v1/packs/sha256/{object_name}", get(pack))
        .layer(DefaultBodyLimit::max(4 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .with_state(AppState::new(
            store,
            require_device_auth,
            pairing_claim_enabled,
            manifest_signing,
        ))
}

pub async fn serve(store: HubStore, config: &HubConfig) -> std::io::Result<()> {
    crate::crypto::install_default_provider();
    if let Some(tls) = &config.tls {
        let cursor_key = CredentialDirectory::required_from_systemd_environment()
            .and_then(|credentials| credentials.cursor_key())
            .map_err(|error| {
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("TLS manifest signing credential is unavailable: {error}"),
                )
            })?;
        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &tls.certificate_path,
            &tls.private_key_path,
        )
        .await?;
        axum_server::bind_rustls(config.bind, tls_config)
            .serve(paired_router(store, &cursor_key).into_make_service())
            .await
    } else {
        let listener = tokio::net::TcpListener::bind(config.bind).await?;
        axum::serve(listener, router(store)).await
    }
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

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    match state.store.quick_check() {
        Ok(()) => (
            StatusCode::OK,
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(Ready { status: "ready" }),
        )
            .into_response(),
        Err(_error) => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(Ready {
                status: "not_ready",
            }),
        )
            .into_response(),
    }
}

async fn capabilities(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(Capabilities {
            protocol: "teslatlas-sync",
            protocol_major: 1,
            version: BUILD_VERSION,
            pack_format: "sqlite-zstd",
            manifest_public_key: state
                .manifest_signing
                .as_deref()
                .map(ManifestSigning::verifying_key_hex),
        }),
    )
}

async fn vehicles(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if authorize_device(&state, &headers).is_none() {
        return unauthorized();
    }
    match state.store.published_vehicles() {
        Ok(vehicles) => Json(VehicleList { vehicles }).into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot load published vehicles");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
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
        &request.secret,
        &request.device_name,
        claimed_at_ms,
    ) {
        Ok(access) => Json(PairingClaimResponse {
            device_id: access.device_id,
            access_token: access.access_token.as_bearer().to_owned(),
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

async fn manifest(
    State(state): State<AppState>,
    Path(vehicle_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if authorize_device(&state, &headers).is_none() {
        return unauthorized();
    }
    let Ok(vehicle_id) = Uuid::parse_str(&vehicle_id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match state.store.manifest_for_vehicle(vehicle_id) {
        Ok(Some(manifest)) => {
            match no_store_manifest(manifest, state.manifest_signing.as_deref()) {
                Ok(response) => response,
                Err(error) => {
                    tracing::error!(%error, "cannot serialize sync manifest");
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                }
            }
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot load sync manifest");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn pack(
    State(state): State<AppState>,
    Path(object_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    if authorize_device(&state, &headers).is_none() {
        return unauthorized();
    }
    let Some(digest) = object_name.strip_suffix(".sqlite.zst") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(digest) = digest.parse::<Sha256Digest>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let stored = match state.store.pack_for_digest(digest) {
        Ok(Some(pack)) => pack,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(%error, "cannot load pack catalog entry");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    stream_pack(
        stored,
        headers
            .get(header::RANGE)
            .and_then(|header| header.to_str().ok()),
    )
    .await
}

fn authorize_device(state: &AppState, headers: &HeaderMap) -> Option<PairedDeviceRecord> {
    if !state.require_device_auth {
        // A public loopback router has no device principal by design. The
        // caller uses `is_none` only to distinguish this harmless local mode
        // from an invalid TLS-facing request.
        return Some(PairedDeviceRecord {
            device_id: Uuid::nil(),
            display_name: String::new(),
            created_at_ms: 0,
            last_authenticated_at_ms: None,
        });
    }
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?
        .strip_prefix("Bearer ")?;
    if bearer.is_empty() || bearer.contains(char::is_whitespace) {
        return None;
    }
    state.store.authenticate_device(bearer).ok().flatten()
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
    Ok(response
        .body(Body::from(raw_json))
        .expect("static manifest response headers are valid"))
}

async fn stream_pack(stored: StoredPack, range_header: Option<&str>) -> Response {
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
            HeaderValue::from_static("public, max-age=31536000, immutable"),
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
        .body(Body::from_stream(ReaderStream::with_capacity(
            tokio::io::AsyncReadExt::take(file, range.len()),
            64 * 1024,
        )))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
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
}

#[derive(Serialize)]
struct Capabilities<'a> {
    protocol: &'a str,
    protocol_major: u8,
    version: &'a str,
    pack_format: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "manifestPublicKey")]
    manifest_public_key: Option<String>,
}

#[derive(Deserialize)]
struct PairingClaimRequest {
    secret: String,
    device_name: String,
}

#[derive(Serialize)]
struct PairingClaimResponse {
    device_id: Uuid,
    access_token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VehicleList {
    vehicles: Vec<PublishedVehicle>,
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use axum::{body::Body, http::Request};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signature, VerifyingKey};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::*;
    use crate::{
        db::HubStore,
        protocol::{
            CursorClaims, CursorKey, MirrorTable, OpaqueCursor, ProtocolVersion, SequenceRange,
            SyncManifest, TRANSPORT_SCHEMA_V1, TransferMode,
        },
        transport::{
            TransportOperation, TransportPackRequest, TransportPackWriter, TransportRow,
            TransportValue,
        },
    };

    #[tokio::test]
    async fn exposes_health_and_capabilities() {
        let temp = tempfile::tempdir().expect("temp directory");
        let app = router(HubStore::initialize(temp.path()).expect("store"));

        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("health response");
        assert_eq!(health.status(), StatusCode::OK);

        let capabilities = app
            .oneshot(
                Request::builder()
                    .uri("/.well-known/teslatlas-hub")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("capabilities response");
        let bytes = capabilities
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let payload: serde_json::Value = serde_json::from_slice(&bytes).expect("capabilities JSON");
        assert_eq!(payload["pack_format"], "sqlite-zstd");
        assert!(payload.get("manifestPublicKey").is_none());
    }

    #[tokio::test]
    async fn tls_capabilities_publish_lowercase_manifest_verifying_key() {
        let temp = tempfile::tempdir().expect("temp directory");
        let cursor_key = CursorKey::from_bytes([29; 32]);
        let expected = ManifestSigning::from_cursor_key(&cursor_key).verifying_key_hex();
        let app = paired_router(
            HubStore::initialize(temp.path()).expect("store"),
            &cursor_key,
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/.well-known/teslatlas-hub")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("capabilities response");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("capabilities body")
            .to_bytes();
        let payload: serde_json::Value = serde_json::from_slice(&bytes).expect("capabilities JSON");
        let published = payload["manifestPublicKey"]
            .as_str()
            .expect("manifest verifying key");
        assert_eq!(published, expected);
        assert_eq!(published.len(), 64);
        assert!(
            published
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
    }

    #[tokio::test]
    async fn tls_router_requires_a_paired_device_and_claims_once() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        let invitation = store
            .create_pairing("iPhone", 1_000, i64::MAX)
            .expect("pairing invitation");
        let cursor_key = CursorKey::from_bytes([31; 32]);
        let app = paired_router(store, &cursor_key);

        let denied = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/vehicles")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("response");
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

        let claim_body = serde_json::json!({
            "secret": invitation.secret(),
            "device_name": "Bolyki iPhone",
        })
        .to_string();
        let claimed = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/pairings/{}/claim", invitation.pairing_id))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(claim_body))
                    .unwrap(),
            )
            .await
            .expect("claim response");
        assert_eq!(claimed.status(), StatusCode::OK);
        let payload = claimed
            .into_body()
            .collect()
            .await
            .expect("claim payload")
            .to_bytes();
        let access_token = serde_json::from_slice::<serde_json::Value>(&payload)
            .expect("claim JSON")["access_token"]
            .as_str()
            .expect("access token")
            .to_owned();

        let listed = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/vehicles")
                    .header(header::AUTHORIZATION, format!("Bearer {access_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("vehicle response");
        assert_eq!(listed.status(), StatusCode::OK);

        let replay = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/pairings/{}/claim", invitation.pairing_id))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "secret": invitation.secret(),
                            "device_name": "Second phone",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .expect("replay response");
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn serves_catalogued_manifest_and_immutable_pack_stream() {
        let temp = tempfile::tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        let installation_id = Uuid::new_v4();
        let account_id = Uuid::new_v4();
        let vehicle_id = Uuid::new_v4();
        let snapshot_id = Uuid::new_v4();
        let rows = vec![TransportRow {
            table: MirrorTable::Position,
            entity_key: "position:1".to_owned(),
            source_sequence: 5,
            operation: TransportOperation::Upsert,
            values: BTreeMap::from([
                ("latitude".to_owned(), TransportValue::Real(51.5072)),
                ("longitude".to_owned(), TransportValue::Real(-0.1276)),
            ]),
        }];
        let tables = [MirrorTable::Position];
        let built = TransportPackWriter::new(store.packs_dir())
            .write_pack(&TransportPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id,
                ordinal: 0,
                schema: TRANSPORT_SCHEMA_V1,
                mode: TransferMode::FullSnapshot,
                sequence: SequenceRange {
                    from_exclusive: 5,
                    to_inclusive: 5,
                },
                tables: &tables,
                rows: &rows,
            })
            .expect("build transport pack");
        let cursor_key = CursorKey::from_bytes([9; 32]);
        let cursor = OpaqueCursor::issue(
            &cursor_key,
            CursorClaims {
                protocol: ProtocolVersion { major: 1, minor: 0 },
                schema: TRANSPORT_SCHEMA_V1,
                installation_id,
                account_id,
                vehicle_id,
                generation: 1,
                sequence: 5,
            },
        )
        .expect("cursor");
        let manifest = SyncManifest {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: TRANSPORT_SCHEMA_V1,
            installation_id,
            account_id,
            vehicle_id,
            generation: 1,
            snapshot_id,
            mode: TransferMode::FullSnapshot,
            base_sequence: 5,
            head_sequence: 5,
            chunk_count: 1,
            total_compressed_bytes: built.metadata.compressed_bytes,
            total_uncompressed_bytes: built.metadata.uncompressed_bytes,
            total_rows: built.metadata.row_count,
            chunks: vec![built.metadata.clone()],
            terminal_cursor: cursor,
        };
        store.publish_manifest(&manifest).expect("publish manifest");
        let expected_pack = fs::read(&built.path).expect("pack bytes");
        let invitation = store
            .create_pairing("manifest test", 1_000, i64::MAX)
            .expect("pairing invitation");
        let access = store
            .claim_pairing(
                invitation.pairing_id,
                invitation.secret(),
                "test client",
                1_001,
            )
            .expect("paired access");
        let bearer = access.access_token.as_bearer().to_owned();
        let verifying_key_hex = ManifestSigning::from_cursor_key(&cursor_key).verifying_key_hex();
        let app = paired_router(store, &cursor_key);

        let manifest_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/vehicles/{vehicle_id}/sync/manifest"))
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("manifest response");
        assert_eq!(manifest_response.status(), StatusCode::OK);
        assert_eq!(
            manifest_response
                .headers()
                .get(header::CACHE_CONTROL)
                .unwrap(),
            "no-store"
        );
        let signature_header = manifest_response
            .headers()
            .get(MANIFEST_SIGNATURE_HEADER)
            .expect("manifest signature")
            .to_str()
            .expect("ASCII signature")
            .to_owned();
        let raw_manifest = manifest_response
            .into_body()
            .collect()
            .await
            .expect("manifest body")
            .to_bytes();
        assert_eq!(
            serde_json::from_slice::<SyncManifest>(&raw_manifest).expect("manifest JSON"),
            manifest
        );
        let verifying_key_bytes: [u8; 32] = hex::decode(verifying_key_hex)
            .expect("verifying key hex")
            .try_into()
            .expect("32-byte verifying key");
        let verifying_key = VerifyingKey::from_bytes(&verifying_key_bytes).expect("verifying key");
        let signature = Signature::from_slice(
            &STANDARD
                .decode(signature_header)
                .expect("base64 manifest signature"),
        )
        .expect("64-byte manifest signature");
        verifying_key
            .verify_strict(&raw_manifest, &signature)
            .expect("exact raw manifest verifies");
        let mut mutated_manifest = raw_manifest.to_vec();
        mutated_manifest[0] ^= 1;
        assert!(
            verifying_key
                .verify_strict(&mutated_manifest, &signature)
                .is_err()
        );

        let pack_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/packs/sha256/{}.sqlite.zst",
                        built.metadata.sha256
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("pack response");
        assert_eq!(pack_response.status(), StatusCode::OK);
        assert_eq!(
            pack_response.headers().get(header::ETAG).unwrap(),
            &built.metadata.etag()
        );
        assert_eq!(
            pack_response.headers().get(header::CONTENT_LENGTH).unwrap(),
            built.metadata.compressed_bytes.to_string().as_str()
        );
        let delivered = pack_response
            .into_body()
            .collect()
            .await
            .expect("streamed body")
            .to_bytes();
        assert_eq!(delivered.as_ref(), expected_pack.as_slice());

        let partial = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/packs/sha256/{}.sqlite.zst",
                        built.metadata.sha256
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .header(header::RANGE, "bytes=1-8")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("partial pack response");
        assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            partial.headers().get(header::ACCEPT_RANGES).unwrap(),
            "bytes"
        );
        assert_eq!(
            partial.headers().get(header::CONTENT_RANGE).unwrap(),
            format!("bytes 1-8/{}", expected_pack.len()).as_str()
        );
        let partial_bytes = partial
            .into_body()
            .collect()
            .await
            .expect("partial body")
            .to_bytes();
        assert_eq!(partial_bytes.as_ref(), &expected_pack[1..=8]);

        let unsatisfiable = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/packs/sha256/{}.sqlite.zst",
                        built.metadata.sha256
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .header(header::RANGE, "bytes=999999-")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("range response");
        assert_eq!(unsatisfiable.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            unsatisfiable.headers().get(header::CONTENT_RANGE).unwrap(),
            format!("bytes */{}", expected_pack.len()).as_str()
        );
    }
}
