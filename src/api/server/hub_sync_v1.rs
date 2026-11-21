// SPDX-License-Identifier: AGPL-3.0-only

//! Runtime adapter for the source-neutral hub-sync-v1 control plane.

use axum::body::to_bytes;

use super::*;

const MAX_CHANGES_SINCE_REQUEST_BYTES: usize = 8_192;
const MAX_PROFILE_PACK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_I_JSON_INTEGER: u64 = 9_007_199_254_740_991;
pub(super) const HUB_SYNC_PROFILE_14: &str = "hub-sync-v1@1.4.0";
pub(super) const PHYSICAL_DELTA_FORMAT: &str = "teslatlas-physical-v3-delta-v1"; // rename:keep (the App switches in lockstep, DEC-160)

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChangesSinceRequest {
    base_receipt_id: String,
    base_manifest_schema: String,
    from_sequence: u64,
    schema_version_range: SchemaVersionRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PhysicalChangesSinceRequest {
    source: PhysicalSource,
    base_receipt_id: String,
    base_manifest_id: String,
    base_manifest_sha256: String,
    base_manifest_schema: String,
    from_sequence: u64,
    schema_version_range: SchemaVersionRange,
    accepted_changed_set_formats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PhysicalSource {
    installation_id: Uuid,
    account_id: Uuid,
    vehicle_id: Uuid,
    generation: u64,
    selected_car_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SchemaVersionRange {
    minimum: String,
    maximum: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct WireSchemaVersion {
    major: u16,
    minor: u16,
}

impl WireSchemaVersion {
    const MINIMUM_SUPPORTED: Self = Self { major: 2, minor: 1 };
    const MAXIMUM_SUPPORTED: Self = Self { major: 2, minor: 2 };

    fn parse(value: &str) -> Option<Self> {
        let (major, minor) = value.split_once('.')?;
        if major.is_empty()
            || minor.is_empty()
            || major.contains('.')
            || minor.contains('.')
            || (major.len() > 1 && major.starts_with('0'))
            || (minor.len() > 1 && minor.starts_with('0'))
            || major.len() > 3
            || minor.len() > 3
            || !major.bytes().all(|byte| byte.is_ascii_digit())
            || !minor.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestValidationError {
    InvalidJson,
    InvalidRequest,
    InvalidSchemaRange,
    UnsupportedSchemaRange,
}

#[derive(Serialize)]
struct SyncError<'a> {
    code: &'a str,
    message: &'a str,
}

#[derive(Serialize)]
struct ChangedSetPayload {
    receipt_id: String,
    vehicle_id: Uuid,
    base_receipt_id: String,
    base_manifest_schema: &'static str,
    from_sequence: u64,
    to_sequence: u64,
    manifest_schema: &'static str,
    changed_set_sha256: String,
    pack: WirePack,
}

#[derive(Serialize)]
struct NoOpPayload {
    kind: &'static str,
    vehicle_id: Uuid,
    base_receipt_id: String,
    base_manifest_schema: &'static str,
    sequence: u64,
    manifest_schema: &'static str,
}

#[derive(Serialize)]
struct Manifest21Payload {
    manifest_id: String,
    receipt_id: String,
    vehicle_id: Uuid,
    kind: &'static str,
    schema_version: &'static str,
    sequence: u64,
    pack: WirePack,
}

#[derive(Serialize)]
struct Manifest22Payload {
    manifest_id: String,
    receipt_id: String,
    vehicle_id: Uuid,
    kind: &'static str,
    schema_version: &'static str,
    sequence: u64,
    chunks: Vec<WireChunk>,
}

#[derive(Serialize)]
struct WireChunk {
    chunk_index: u32,
    pack: WirePack,
}

#[derive(Serialize)]
struct RebasePayload<R> {
    kind: &'static str,
    vehicle_id: Uuid,
    requested_base_receipt_id: String,
    requested_base_manifest_schema: &'static str,
    requested_from_sequence: u64,
    reason: &'static str,
    replacement: R,
    retry_request: RetryRequest,
}

#[derive(Serialize)]
struct Replacement21 {
    manifest_id: String,
    receipt_id: String,
    sequence: u64,
    manifest_schema: &'static str,
    pack: WirePack,
}

#[derive(Serialize)]
struct Replacement22 {
    manifest_id: String,
    receipt_id: String,
    sequence: u64,
    manifest_schema: &'static str,
    chunks: Vec<WireChunk>,
}

#[derive(Serialize)]
struct RetryRequest {
    base_receipt_id: String,
    base_manifest_schema: &'static str,
    from_sequence: u64,
    schema_version_range: SchemaVersionRange,
}

#[derive(Serialize)]
struct WirePack {
    object_name: String,
    sha256: String,
    compressed_bytes: u64,
}

enum CurrentCheckpoint<'a> {
    Changed(&'a crate::protocol::LineageDelta),
    Head,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BootstrapSelection {
    Legacy,
    Selected,
    Selected14,
    Unsupported,
}

pub(super) fn bootstrap_selection(headers: &HeaderMap) -> BootstrapSelection {
    if !headers.contains_key(SYNC_PROFILE_HEADER) {
        return BootstrapSelection::Legacy;
    }
    if has_exact_single_header(headers, SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_ID.as_bytes())
        && has_exact_single_header(headers, SUPPORTED_SCHEMAS_HEADER, b"2.1,2.2")
    {
        BootstrapSelection::Selected
    } else if has_exact_single_header(headers, SYNC_PROFILE_HEADER, HUB_SYNC_PROFILE_14.as_bytes())
        && has_exact_single_header(headers, SUPPORTED_SCHEMAS_HEADER, b"2.1,2.2")
    {
        BootstrapSelection::Selected14
    } else {
        BootstrapSelection::Unsupported
    }
}

fn has_exact_single_header(headers: &HeaderMap, name: &str, expected: &[u8]) -> bool {
    let mut values = headers.get_all(name).iter();
    matches!(
        (values.next(), values.next()),
        (Some(value), None) if value.as_bytes() == expected
    )
}

pub(super) async fn bootstrap_manifest(
    state: &AppState,
    scope: VehicleScope,
    vehicle_id: Uuid,
    physical_only: bool,
) -> Response {
    let Some(signing) = state.manifest_signing.as_deref() else {
        tracing::error!(%vehicle_id, "manifest signing key is unavailable");
        return unavailable_bootstrap_manifest();
    };
    match state
        .store
        .pending_physical_v3_control_admission_for_vehicle(vehicle_id)
    {
        Ok(Some(admission)) if crate::db::physical_v3_admission_is_public(&admission) => {
            let generation = admission.manifest.generation;
            if let Err(response) =
                record_pack_offers(state, scope, vehicle_id, generation, physical_offers(&admission))
            {
                return response;
            }
            return signed_control_response(
                StatusCode::OK,
                &Manifest22Payload {
                    manifest_id: admission.snapshot_id.to_string(),
                    receipt_id: admission.receipt_id,
                    vehicle_id,
                    kind: "snapshot",
                    schema_version: "2.2",
                    sequence: admission.head_sequence,
                    chunks: admission.manifest.chunks.iter().map(wire_chunk).collect(),
                },
                signing,
            );
        }
        Ok(Some(_)) => return unavailable_bootstrap_manifest(),
        Ok(None) => {}
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load admitted physical bootstrap");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    if physical_only {
        return unavailable_bootstrap_manifest();
    }
    // Only this lineage tail verifies stored packs, so only it takes the
    // sync-control slot and leaves the async runtime; the physical front half
    // above never waits for it.
    state
        .sync_control_response(move |state| bootstrap_lineage_manifest(&state, vehicle_id))
        .await
}

/// The schema-2.1 bootstrap answer for a vehicle with no admitted physical
/// base. Verifies every stored lineage pack, so it must run off the async
/// runtime.
fn bootstrap_lineage_manifest(state: &AppState, vehicle_id: Uuid) -> Response {
    let Some(signing) = state.manifest_signing.as_deref() else {
        tracing::error!(%vehicle_id, "manifest signing key is unavailable");
        return unavailable_bootstrap_manifest();
    };
    let lineage = match state.store.lineage_manifest_for_vehicle(vehicle_id) {
        Ok(Some(lineage)) => lineage,
        Ok(None) => return unavailable_bootstrap_manifest(),
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load hub-sync bootstrap lineage");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let Some(pack) = admitted_schema_21_base(&lineage) else {
        return unavailable_bootstrap_manifest();
    };
    signed_control_response(
        StatusCode::OK,
        &Manifest21Payload {
            manifest_id: lineage.base.snapshot_id.to_string(),
            receipt_id: lineage.base.digest.to_string(),
            vehicle_id,
            kind: "snapshot",
            schema_version: "2.1",
            sequence: lineage.base.sequence,
            pack: wire_pack(pack),
        },
        signing,
    )
}

pub(super) async fn changes_since(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Response {
    let device = match require_authorized_device(&state, request.headers()) {
        Ok(device) => device,
        Err(status) => return device_auth_reject(status),
    };
    // A malformed, non-UTF-8, nil, unknown, retired or ungranted vehicle is
    // one answer: the JSON `vehicle_not_found`.
    let vehicle_not_available = || {
        sync_error_response(
            StatusCode::NOT_FOUND,
            "vehicle_not_found",
            "Vehicle is not available to this pairing.",
            false,
        )
    };
    let Some(vehicle_id) = path_vehicle_id(request.uri()) else {
        return vehicle_not_available();
    };
    let scope = state.vehicle_scope(&device);
    match require_vehicle_access(&state, &device, vehicle_id, VehicleVisibility::Active) {
        Ok(()) => {}
        Err(VehicleAccessDenied::NotFound) => return vehicle_not_available(),
        Err(VehicleAccessDenied::Unavailable) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }

    let raw = match to_bytes(request.into_body(), MAX_CHANGES_SINCE_REQUEST_BYTES + 1).await {
        Ok(raw) if raw.len() <= MAX_CHANGES_SINCE_REQUEST_BYTES => raw,
        Ok(_) | Err(_) => {
            return sync_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                "Request body exceeds 8192 bytes.",
                false,
            );
        }
    };
    if serde_json::from_slice::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| {
            value
                .as_object()
                .map(|object| object.contains_key("accepted_changed_set_formats"))
        })
        .unwrap_or(false)
    {
        return match parse_physical_changes_since_request(&raw) {
            Ok(request) => serve_physical_changes_since_14(&state, scope, vehicle_id, request),
            Err(error) => request_error_response(error),
        };
    }
    let request = match parse_changes_since_request(&raw) {
        Ok(request) => request,
        Err(RequestValidationError::InvalidJson) => {
            return sync_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                "Request body is not valid JSON.",
                false,
            );
        }
        Err(RequestValidationError::InvalidRequest) => {
            return sync_error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
                "Request body does not match the changes-since schema.",
                false,
            );
        }
        Err(RequestValidationError::InvalidSchemaRange) => {
            return sync_error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_schema_range",
                "Schema range is reversed or excludes the base schema.",
                false,
            );
        }
        Err(RequestValidationError::UnsupportedSchemaRange) => {
            return sync_error_response(
                StatusCode::NOT_ACCEPTABLE,
                "schema_range_unsupported",
                "Requested schema range has no supported version.",
                true,
            );
        }
    };

    if request.base_manifest_schema == "2.1" {
        state
            .sync_control_response(move |state| {
                serve_changes_since(&state, scope, vehicle_id, request)
            })
            .await
    } else {
        serve_changes_since(&state, scope, vehicle_id, request)
    }
}

fn request_error_response(error: RequestValidationError) -> Response {
    match error {
        RequestValidationError::InvalidJson => sync_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_json",
            "Request body is not valid JSON.",
            false,
        ),
        RequestValidationError::InvalidRequest => sync_error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            "Request body does not match the changes-since schema.",
            false,
        ),
        RequestValidationError::InvalidSchemaRange => sync_error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_schema_range",
            "Schema range is reversed or excludes the base schema.",
            false,
        ),
        RequestValidationError::UnsupportedSchemaRange => schema_range_unsupported(),
    }
}

fn parse_physical_changes_since_request(
    raw: &[u8],
) -> Result<PhysicalChangesSinceRequest, RequestValidationError> {
    let request: PhysicalChangesSinceRequest = serde_json::from_slice(raw).map_err(|error| {
        if error.is_data() {
            RequestValidationError::InvalidRequest
        } else {
            RequestValidationError::InvalidJson
        }
    })?;
    if request.base_manifest_schema != "2.2"
        || request.schema_version_range.minimum != "2.2"
        || request.schema_version_range.maximum != "2.2"
        || !crate::platform::name_migration::wire::is_only_token(&request.accepted_changed_set_formats, PHYSICAL_DELTA_FORMAT)
        || request.base_receipt_id.is_empty()
        || request.base_receipt_id.len() > 4096
        || !request.base_receipt_id.bytes().all(is_receipt_token_byte)
        || request.base_manifest_id.is_empty()
        || request.base_manifest_id.len() > 4096
        || !request.base_manifest_id.bytes().all(is_receipt_token_byte)
        || request
            .base_manifest_sha256
            .parse::<Sha256Digest>()
            .is_err()
        || request.from_sequence == 0
        || !sequence_is_admitted(request.from_sequence)
        || request.source.generation > MAX_I_JSON_INTEGER
        || !(1..=MAX_I_JSON_INTEGER as i64).contains(&request.source.selected_car_id)
    {
        return Err(RequestValidationError::InvalidRequest);
    }
    Ok(request)
}

fn parse_changes_since_request(raw: &[u8]) -> Result<ChangesSinceRequest, RequestValidationError> {
    let request = serde_json::from_slice::<ChangesSinceRequest>(raw).map_err(|error| {
        if error.is_data() {
            RequestValidationError::InvalidRequest
        } else {
            RequestValidationError::InvalidJson
        }
    })?;
    if request.base_receipt_id.is_empty()
        || request.base_receipt_id.len() > 4_096
        || !request.base_receipt_id.bytes().all(is_receipt_token_byte)
        || !sequence_is_admitted(request.from_sequence)
        || !matches!(request.base_manifest_schema.as_str(), "2.1" | "2.2")
    {
        return Err(RequestValidationError::InvalidRequest);
    }
    let Some(minimum) = WireSchemaVersion::parse(&request.schema_version_range.minimum) else {
        return Err(RequestValidationError::InvalidRequest);
    };
    let Some(maximum) = WireSchemaVersion::parse(&request.schema_version_range.maximum) else {
        return Err(RequestValidationError::InvalidRequest);
    };
    let base = WireSchemaVersion::parse(&request.base_manifest_schema)
        .ok_or(RequestValidationError::InvalidRequest)?;
    if minimum > maximum {
        return Err(RequestValidationError::InvalidSchemaRange);
    }
    if maximum < WireSchemaVersion::MINIMUM_SUPPORTED
        || minimum > WireSchemaVersion::MAXIMUM_SUPPORTED
    {
        return Err(RequestValidationError::UnsupportedSchemaRange);
    }
    if !(minimum..=maximum).contains(&base) {
        return Err(RequestValidationError::InvalidSchemaRange);
    }
    Ok(request)
}

fn is_receipt_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'-')
}

fn sync_error_response(
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    no_store: bool,
) -> Response {
    bounded_control_json(status, &SyncError { code, message }, no_store).unwrap_or_else(|error| {
        tracing::error!(%error, %status, %code, "cannot serialize bounded sync error response");
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    })
}

fn bounded_control_json(
    status: StatusCode,
    value: &impl Serialize,
    no_store: bool,
) -> Result<Response, BoundedJsonError> {
    let raw_json = serde_json::to_vec(value).map_err(BoundedJsonError::Serialize)?;
    bounded_control_json_bytes(status, raw_json, no_store)
}

fn bounded_control_json_bytes(
    status: StatusCode,
    raw_json: Vec<u8>,
    no_store: bool,
) -> Result<Response, BoundedJsonError> {
    if raw_json.len() > MAX_SYNC_CONTROL_RESPONSE_BYTES {
        return Err(BoundedJsonError::ResponseTooLarge);
    }
    let mut response = Response::builder().status(status).header(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if no_store {
        response = response.header(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    Ok(response
        .body(Body::from(raw_json))
        .expect("static JSON response headers are valid"))
}

fn serve_changes_since(
    state: &AppState,
    scope: VehicleScope,
    vehicle_id: Uuid,
    request: ChangesSinceRequest,
) -> Response {
    let Some(signing) = state.manifest_signing.as_deref() else {
        tracing::error!(%vehicle_id, "manifest signing key is unavailable");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if request.base_manifest_schema == "2.2" {
        return serve_physical_changes_since(state, scope, vehicle_id, request, signing);
    }
    if request.base_manifest_schema != "2.1" {
        return schema_range_unsupported();
    }
    let lineage = match state.store.lineage_manifest_for_vehicle(vehicle_id) {
        Ok(Some(lineage)) => lineage,
        Ok(None) => return schema_range_unsupported(),
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load changes-since lineage");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    // A base too large for one 2.1 pack serves no 2.1 checkpoint of its
    // own, but a retired 2.1 checkpoint still gets its signed rebase.
    let base_pack = admitted_schema_21_base(&lineage);
    if let Some(checkpoint) = base_pack.and_then(|_| resolve_current_checkpoint(&lineage, &request)) {
        return match checkpoint {
            CurrentCheckpoint::Changed(delta) => {
                if !pack_is_admitted(&delta.pack) || !sequence_is_admitted(delta.to_sequence) {
                    return schema_range_unsupported();
                }
                let chain_digest = delta.chain_digest.to_string();
                signed_control_response(
                    StatusCode::OK,
                    &ChangedSetPayload {
                        receipt_id: chain_digest.clone(),
                        vehicle_id,
                        base_receipt_id: request.base_receipt_id,
                        base_manifest_schema: "2.1",
                        from_sequence: request.from_sequence,
                        to_sequence: delta.to_sequence,
                        manifest_schema: "2.1",
                        changed_set_sha256: chain_digest,
                        pack: wire_pack(&delta.pack),
                    },
                    signing,
                )
            }
            CurrentCheckpoint::Head => signed_control_response(
                StatusCode::OK,
                &NoOpPayload {
                    kind: "no_op",
                    vehicle_id,
                    base_receipt_id: request.base_receipt_id,
                    base_manifest_schema: "2.1",
                    sequence: request.from_sequence,
                    manifest_schema: "2.1",
                },
                signing,
            ),
        };
    }

    let receipt = match request.base_receipt_id.parse::<Sha256Digest>() {
        Ok(receipt) => receipt,
        Err(_) if base_pack.is_none() => return schema_range_unsupported(),
        Err(_) => return unknown_base_receipt(),
    };
    match state.store.retired_lineage_contains_checkpoint(
        vehicle_id,
        request.from_sequence,
        receipt,
    ) {
        Ok(true) if base_pack.is_none() => {
            retired_21_checkpoint_physical_rebase(state, scope, vehicle_id, &lineage, request, signing)
        }
        Ok(false) if base_pack.is_none() => schema_range_unsupported(),
        Ok(true) => {
            let Some(base_pack) = base_pack else {
                return schema_range_unsupported();
            };
            let replacement_receipt = lineage.base.digest.to_string();
            signed_control_response(
                StatusCode::CONFLICT,
                &RebasePayload {
                    kind: "rebase_required",
                    vehicle_id,
                    requested_base_receipt_id: request.base_receipt_id,
                    requested_base_manifest_schema: "2.1",
                    requested_from_sequence: request.from_sequence,
                    reason: "compacted",
                    replacement: Replacement21 {
                        manifest_id: lineage.base.snapshot_id.to_string(),
                        receipt_id: replacement_receipt.clone(),
                        sequence: lineage.base.sequence,
                        manifest_schema: "2.1",
                        pack: wire_pack(base_pack),
                    },
                    retry_request: RetryRequest {
                        base_receipt_id: replacement_receipt,
                        base_manifest_schema: "2.1",
                        from_sequence: lineage.base.sequence,
                        schema_version_range: request.schema_version_range,
                    },
                },
                signing,
            )
        }
        Ok(false) => unknown_base_receipt(),
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot resolve retained changes-since checkpoint");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

/// The signed rebase for a retired 2.1 checkpoint whose replacement base
/// has more than one pack (DEC-150: the first import onto a window-only Hub
/// retires the window lineage). The 1.3.0 rebase hint admits a 2.2
/// replacement: the public physical checkpoint of the same vehicle and
/// source, as its chunks, with a 2.2 retry. A requested range that excludes
/// 2.2 keeps 406, as its retry would be refused. Until that checkpoint is
/// public the answer is a retryable 503.
fn retired_21_checkpoint_physical_rebase(
    state: &AppState,
    scope: VehicleScope,
    vehicle_id: Uuid,
    lineage: &LineageManifestV2,
    request: ChangesSinceRequest,
    signing: &ManifestSigning,
) -> Response {
    // The retry names 2.2, so a range that excludes it gets no migration
    // offer (REV2-CX-M03-T23).
    let admits_22 = WireSchemaVersion::parse(&request.schema_version_range.maximum)
        .is_some_and(|maximum| maximum >= WireSchemaVersion::MAXIMUM_SUPPORTED);
    if !admits_22 {
        return schema_range_unsupported();
    }
    let admission = match state
        .store
        .pending_physical_v3_control_admission_for_vehicle(vehicle_id)
    {
        Ok(Some(admission)) if crate::db::physical_v3_admission_is_public(&admission) => admission,
        Ok(Some(_)) | Ok(None) => {
            tracing::warn!(%vehicle_id, "retired 2.1 checkpoint waits for a public physical replacement");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load admitted physical checkpoint");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    if admission.installation_id != lineage.installation_id
        || admission.account_id != lineage.account_id
        || admission.vehicle_id != lineage.vehicle_id
    {
        tracing::error!(%vehicle_id, "physical checkpoint does not bind the active lineage");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let generation = admission.manifest.generation;
    if let Err(response) =
        record_pack_offers(state, scope, vehicle_id, generation, physical_offers(&admission))
    {
        return response;
    }
    let replacement_receipt = admission.receipt_id.clone();
    signed_control_response(
        StatusCode::CONFLICT,
        &RebasePayload {
            kind: "rebase_required",
            vehicle_id,
            requested_base_receipt_id: request.base_receipt_id,
            requested_base_manifest_schema: "2.1",
            requested_from_sequence: request.from_sequence,
            reason: "compacted",
            replacement: Replacement22 {
                manifest_id: admission.snapshot_id.to_string(),
                receipt_id: replacement_receipt.clone(),
                sequence: admission.head_sequence,
                manifest_schema: "2.2",
                chunks: admission.manifest.chunks.iter().map(wire_chunk).collect(),
            },
            retry_request: RetryRequest {
                base_receipt_id: replacement_receipt,
                base_manifest_schema: "2.2",
                from_sequence: admission.head_sequence,
                schema_version_range: request.schema_version_range,
            },
        },
        signing,
    )
}

fn serve_physical_changes_since(
    state: &AppState,
    scope: VehicleScope,
    vehicle_id: Uuid,
    request: ChangesSinceRequest,
    signing: &ManifestSigning,
) -> Response {
    let admission = match state
        .store
        .pending_physical_v3_control_admission_for_vehicle(vehicle_id)
    {
        Ok(Some(admission)) if crate::db::physical_v3_admission_is_public(&admission) => admission,
        Ok(Some(_)) | Ok(None) => return schema_range_unsupported(),
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot load admitted physical checkpoint");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    if request.base_receipt_id == admission.receipt_id
        && request.from_sequence == admission.head_sequence
    {
        return signed_control_response(
            StatusCode::OK,
            &NoOpPayload {
                kind: "no_op",
                vehicle_id,
                base_receipt_id: request.base_receipt_id,
                base_manifest_schema: "2.2",
                sequence: request.from_sequence,
                manifest_schema: "2.2",
            },
            signing,
        );
    }
    let now_ms = match current_epoch_ms() {
        Ok(now_ms) => now_ms,
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot read retained physical checkpoint clock");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let retained = match state.store.retained_physical_v3_admission_for_receipt_at(
        vehicle_id,
        &request.base_receipt_id,
        now_ms,
        false,
    ) {
        Ok(Some(retained)) if retained.admission.head_sequence == request.from_sequence => retained,
        Ok(Some(_)) | Ok(None) => return unknown_base_receipt(),
        Err(error) => {
            tracing::error!(%error, %vehicle_id, "cannot resolve retained physical checkpoint");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    if retained.admission.installation_id != admission.installation_id
        || retained.admission.account_id != admission.account_id
        || retained.admission.vehicle_id != admission.vehicle_id
        || retained.admission.selected_car_id != admission.selected_car_id
        || retained.admission.manifest.generation != admission.manifest.generation
        || retained.admission.head_sequence >= admission.head_sequence
    {
        tracing::error!(%vehicle_id, "retained physical checkpoint does not bind active replacement");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let generation = admission.manifest.generation;
    if let Err(response) =
        record_pack_offers(state, scope, vehicle_id, generation, physical_offers(&admission))
    {
        return response;
    }
    let replacement_receipt = admission.receipt_id.clone();
    signed_control_response(
        StatusCode::CONFLICT,
        &RebasePayload {
            kind: "rebase_required",
            vehicle_id,
            requested_base_receipt_id: request.base_receipt_id,
            requested_base_manifest_schema: "2.2",
            requested_from_sequence: request.from_sequence,
            reason: "compacted",
            replacement: Replacement22 {
                manifest_id: admission.snapshot_id.to_string(),
                receipt_id: replacement_receipt.clone(),
                sequence: admission.head_sequence,
                manifest_schema: "2.2",
                chunks: admission.manifest.chunks.iter().map(wire_chunk).collect(),
            },
            retry_request: RetryRequest {
                base_receipt_id: replacement_receipt,
                base_manifest_schema: "2.2",
                from_sequence: admission.head_sequence,
                schema_version_range: request.schema_version_range,
            },
        },
        signing,
    )
}

fn serve_physical_changes_since_14(
    state: &AppState,
    scope: VehicleScope,
    vehicle_id: Uuid,
    request: PhysicalChangesSinceRequest,
) -> Response {
    let Some(signing) = state.manifest_signing.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let admission = match state
        .store
        .pending_physical_v3_control_admission_for_vehicle(vehicle_id)
    {
        Ok(Some(admission)) if crate::db::physical_v3_admission_is_public(&admission) => admission,
        Ok(_) => return schema_range_unsupported(),
        Err(error) => {
            tracing::error!(%error,%vehicle_id,"cannot load 1.4 physical head");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    if request.source.vehicle_id != vehicle_id
        || request.source.installation_id != admission.installation_id
        || request.source.account_id != admission.account_id
        || request.source.generation != admission.manifest.generation
        || request.source.selected_car_id != admission.selected_car_id
    {
        return unknown_base_receipt();
    }
    let target_manifest_bytes = match signing.signed_physical_manifest_document(&admission) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error,"cannot encode current signed physical manifest");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let target_sha = Sha256Digest::of_bytes(&target_manifest_bytes).to_string();
    let base = if request.base_receipt_id == admission.receipt_id
        && request.from_sequence == admission.head_sequence
    {
        admission.clone()
    } else {
        let now_ms = match current_epoch_ms() {
            Ok(ms) => ms,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        match state.store.retained_physical_v3_admission_for_receipt_at(
            vehicle_id,
            &request.base_receipt_id,
            now_ms,
            false,
        ) {
            Ok(Some(retained)) if retained.admission.head_sequence == request.from_sequence => {
                retained.admission
            }
            Ok(_) => return unknown_base_receipt(),
            Err(error) => {
                tracing::error!(%error,%vehicle_id,"cannot resolve 1.4 retained checkpoint");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
    };
    let base_manifest_bytes = match signing.signed_physical_manifest_document(&base) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error,"cannot encode retained signed physical manifest");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    let base_sha = Sha256Digest::of_bytes(&base_manifest_bytes).to_string();
    if request.base_manifest_id != base.snapshot_id.to_string()
        || request.base_manifest_sha256 != base_sha
        || base.installation_id != admission.installation_id
        || base.account_id != admission.account_id
        || base.selected_car_id != admission.selected_car_id
        || base.manifest.generation != admission.manifest.generation
    {
        return unknown_base_receipt();
    }
    if base.receipt_id == admission.receipt_id {
        development_sync_event(
            crate::runtime::development_event_log::Outcome::NoOp,
            base.head_sequence,
            admission.head_sequence,
        );
        return signed_control_response(
            StatusCode::OK,
            &NoOpPayload {
                kind: "no_op",
                vehicle_id,
                base_receipt_id: request.base_receipt_id,
                base_manifest_schema: "2.2",
                sequence: request.from_sequence,
                manifest_schema: "2.2",
            },
            signing,
        );
    }
    if base.head_sequence.checked_add(1) == Some(admission.head_sequence) {
        let now_ms = match current_epoch_ms() {
            Ok(ms) => ms,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        match state.store.physical_v3_delta_receipt_for_base_at(
            vehicle_id,
            &base.receipt_id,
            &admission.receipt_id,
            now_ms,
        ) {
            Ok(Some((stored_base_sha, stored_target_sha, receipt_json)))
                if stored_base_sha == base_sha && stored_target_sha == target_sha =>
            {
                let Some(offers) = delta_offers(&receipt_json) else {
                    tracing::error!(%vehicle_id, "stored physical delta receipt names no packs");
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                };
                let generation = admission.manifest.generation;
                if let Err(response) =
                    record_pack_offers(state, scope, vehicle_id, generation, offers)
                {
                    return response;
                }
                development_sync_event(
                    crate::runtime::development_event_log::Outcome::ChangedSet,
                    base.head_sequence,
                    admission.head_sequence,
                );
                return bounded_control_json_bytes(StatusCode::OK, receipt_json, false)
                    .unwrap_or_else(|_| StatusCode::SERVICE_UNAVAILABLE.into_response());
            }
            Ok(Some(_)) => {
                tracing::error!(%vehicle_id,"physical delta signed-manifest identity differs");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error,%vehicle_id,"cannot resolve physical delta transition");
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
    }
    let replacement = serde_json::json!({
        "manifest_id":admission.snapshot_id.to_string(),
        "receipt_id":admission.receipt_id,
        "manifest_sha256":target_sha,
        "sequence":admission.head_sequence,
        "schema_version":"2.2",
        "chunks":admission.manifest.chunks.iter().map(wire_chunk).collect::<Vec<_>>(),
    });
    let requested = serde_json::json!({
        "manifest_id":base.snapshot_id.to_string(),
        "receipt_id":base.receipt_id,
        "manifest_sha256":base_sha,
        "sequence":base.head_sequence,
        "schema_version":"2.2",
    });
    let generation = admission.manifest.generation;
    if let Err(response) =
        record_pack_offers(state, scope, vehicle_id, generation, physical_offers(&admission))
    {
        return response;
    }
    let mut retry = request.clone();
    retry.base_manifest_id = admission.snapshot_id.to_string();
    retry.base_receipt_id = admission.receipt_id.clone();
    retry.base_manifest_sha256 = target_sha;
    retry.from_sequence = admission.head_sequence;
    development_sync_event(
        crate::runtime::development_event_log::Outcome::RebaseRequired,
        base.head_sequence,
        admission.head_sequence,
    );
    signed_control_response(
        StatusCode::CONFLICT,
        &serde_json::json!({
            "kind":"rebase_required",
            "vehicle_id":vehicle_id,
            "requested_base":requested,
            "reason":"compacted",
            "replacement":replacement,
            "retry_request":retry,
        }),
        signing,
    )
}

fn admitted_schema_21_base(lineage: &LineageManifestV2) -> Option<&crate::protocol::TransportPack> {
    if lineage.schema != HUB_PROJECTION_SCHEMA_V2
        || lineage.base.sequence == 0
        || !sequence_is_admitted(lineage.base.sequence)
        || lineage.base.packs.len() != 1
    {
        return None;
    }
    let pack = &lineage.base.packs[0];
    pack_is_admitted(pack).then_some(pack)
}

fn pack_is_admitted(pack: &crate::protocol::TransportPack) -> bool {
    pack_size_is_admitted(pack.compressed_bytes)
}

fn pack_size_is_admitted(compressed_bytes: u64) -> bool {
    (1..=MAX_PROFILE_PACK_BYTES).contains(&compressed_bytes)
}

fn sequence_is_admitted(sequence: u64) -> bool {
    sequence <= MAX_I_JSON_INTEGER
}

fn resolve_current_checkpoint<'a>(
    lineage: &'a LineageManifestV2,
    request: &ChangesSinceRequest,
) -> Option<CurrentCheckpoint<'a>> {
    if request.from_sequence == lineage.base.sequence
        && request.base_receipt_id == lineage.base.digest.to_string()
    {
        return Some(
            lineage
                .deltas
                .first()
                .map_or(CurrentCheckpoint::Head, CurrentCheckpoint::Changed),
        );
    }
    for (index, delta) in lineage.deltas.iter().enumerate() {
        if request.from_sequence == delta.to_sequence
            && request.base_receipt_id == delta.chain_digest.to_string()
        {
            return Some(
                lineage
                    .deltas
                    .get(index + 1)
                    .map_or(CurrentCheckpoint::Head, CurrentCheckpoint::Changed),
            );
        }
    }
    None
}

fn wire_pack(pack: &crate::protocol::TransportPack) -> WirePack {
    WirePack {
        object_name: format!("{}.sqlite.zst", pack.sha256),
        sha256: pack.sha256.to_string(),
        compressed_bytes: pack.compressed_bytes,
    }
}

fn wire_chunk(pack: &crate::protocol::TransportPack) -> WireChunk {
    WireChunk {
        chunk_index: pack.ordinal,
        pack: wire_pack(pack),
    }
}

/// Issues #98/#109: commit the device's offer leases for the packs a signed
/// response is about to name, before the body leaves. A refused lease is a
/// retryable 503 with no body; an unrestricted (loopback owner) scope has no
/// device to lease to.
pub(super) fn record_pack_offers(
    state: &AppState,
    scope: VehicleScope,
    vehicle_id: Uuid,
    generation: u64,
    offers: impl IntoIterator<Item = (Sha256Digest, crate::db::PackOfferKind)>,
) -> Result<(), Response> {
    let VehicleScope::Device(device_id) = scope else {
        return Ok(());
    };
    let offers = offers
        .into_iter()
        .map(|(digest, kind)| crate::db::PackOffer { digest, kind })
        .collect::<Vec<_>>();
    let now_ms = current_epoch_ms().map_err(|_| StatusCode::SERVICE_UNAVAILABLE.into_response())?;
    state
        .store
        .record_pack_offers_at(device_id, vehicle_id, generation, &offers, now_ms)
        .map_err(|error| {
            tracing::warn!(%error, %vehicle_id, "cannot lease the offered packs");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        })
}

/// The object a live offer lease of this device names (issues #98/#109), and
/// whether it is a changed-set pack. An unrestricted scope holds no leases.
pub(super) fn offered_pack(
    state: &AppState,
    scope: VehicleScope,
    digest: Sha256Digest,
    now_ms: i64,
) -> Result<Option<(crate::db::StoredPack, bool)>, Response> {
    let VehicleScope::Device(device_id) = scope else {
        return Ok(None);
    };
    match state.store.offered_pack_for_digest_at(device_id, digest, now_ms) {
        Ok(found) => Ok(found.map(|(pack, kind)| (pack, kind == crate::db::PackOfferKind::Delta))),
        Err(error) => {
            tracing::error!(%error, %digest, "cannot load an offered pack lease");
            Err(StatusCode::SERVICE_UNAVAILABLE.into_response())
        }
    }
}

/// Issue #109: lease the pack a current signed prepared receipt names to the
/// device it is about to be returned to, under the current head's generation.
pub(super) fn lease_prepared_receipt(
    state: &AppState,
    device: &PairedDeviceRecord,
    vehicle_id: Uuid,
    receipt: &[u8],
) -> Result<(), Response> {
    let unavailable = || StatusCode::SERVICE_UNAVAILABLE.into_response();
    let scope = state.vehicle_scope(device);
    if !matches!(scope, VehicleScope::Device(_)) {
        return Ok(());
    }
    let digest = serde_json::from_slice::<serde_json::Value>(receipt)
        .ok()
        .and_then(|receipt| receipt.get("pack")?.get("sha256")?.as_str()?.parse().ok())
        .ok_or_else(|| {
            tracing::error!(%vehicle_id, "a served prepared receipt names no pack digest");
            unavailable()
        })?;
    let generation = match state
        .store
        .pending_physical_v3_control_admission_for_vehicle(vehicle_id)
    {
        Ok(Some(head)) => head.manifest.generation,
        Ok(None) | Err(_) => return Err(unavailable()),
    };
    record_pack_offers(
        state,
        scope,
        vehicle_id,
        generation,
        [(digest, crate::db::PackOfferKind::Prepared)],
    )
}

/// The chunks of a full physical head, as offers.
fn physical_offers(
    admission: &crate::db::PendingPhysicalV3Admission,
) -> impl Iterator<Item = (Sha256Digest, crate::db::PackOfferKind)> + '_ {
    admission
        .manifest
        .chunks
        .iter()
        .map(|chunk| (chunk.sha256, crate::db::PackOfferKind::Physical))
}

/// The packs a stored signed changed-set receipt names, as offers. `None`
/// when the receipt does not name them in the 1.4 shape.
fn delta_offers(receipt_json: &[u8]) -> Option<Vec<(Sha256Digest, crate::db::PackOfferKind)>> {
    let receipt: serde_json::Value = serde_json::from_slice(receipt_json).ok()?;
    receipt
        .get("chunks")?
        .as_array()?
        .iter()
        .map(|chunk| {
            let digest = chunk.get("pack")?.get("sha256")?.as_str()?.parse().ok()?;
            Some((digest, crate::db::PackOfferKind::Delta))
        })
        .collect()
}

fn signed_control_response(
    status: StatusCode,
    payload: &impl Serialize,
    signing: &ManifestSigning,
) -> Response {
    let response = (|| {
        let bytes = signing
            .signed_control_document(payload)
            .map_err(BoundedJsonError::Serialize)?;
        bounded_control_json_bytes(status, bytes, false)
    })();
    response.unwrap_or_else(|error| {
        tracing::error!(%error, %status, "cannot serialize bounded signed sync response");
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    })
}

fn schema_range_unsupported() -> Response {
    sync_error_response(
        StatusCode::NOT_ACCEPTABLE,
        "schema_range_unsupported",
        "Requested schema range has no supported version.",
        true,
    )
}

fn unknown_base_receipt() -> Response {
    sync_error_response(
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown_base_receipt",
        "Base receipt is not known for this vehicle.",
        false,
    )
}

pub(super) fn unavailable_bootstrap_manifest() -> Response {
    (
        StatusCode::NOT_ACCEPTABLE,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
    )
        .into_response()
}

fn development_sync_event(
    outcome: crate::runtime::development_event_log::Outcome,
    base: u64,
    target: u64,
) {
    use crate::runtime::development_event_log as dev;
    let mut event = dev::Event::new(dev::Kind::Sync, outcome);
    event.base_sequence = Some(base);
    event.target_sequence = Some(target);
    dev::record(event);
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    fn fixture(name: &str) -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("protocol/profiles/hub-sync-v1/1.3.0/examples")
            .join(name);
        serde_json::from_slice(&std::fs::read(path).expect("read Protocol fixture"))
            .expect("parse Protocol fixture")
    }

    #[test]
    fn bootstrap_selector_is_exact_and_explicit_errors_fail_closed() {
        let selected = fixture("bootstrap-hub-sync-v1-1-3-selected.json");
        assert_eq!(
            selected["request"]["headers"],
            serde_json::json!([
                {"name": SYNC_PROFILE_HEADER, "value": HUB_SYNC_PROFILE_ID},
                {"name": SUPPORTED_SCHEMAS_HEADER, "value": "2.1,2.2"}
            ])
        );
        let legacy = fixture("bootstrap-schema-list-only-legacy.json");
        assert_eq!(
            legacy["request"]["headers"],
            serde_json::json!([
                {"name": SUPPORTED_SCHEMAS_HEADER, "value": "2.1,2.2"}
            ])
        );

        let mut headers = HeaderMap::new();
        headers.insert(
            SUPPORTED_SCHEMAS_HEADER,
            HeaderValue::from_static("2.1,2.2"),
        );
        assert_eq!(bootstrap_selection(&headers), BootstrapSelection::Legacy);

        headers.insert(
            SYNC_PROFILE_HEADER,
            HeaderValue::from_static(HUB_SYNC_PROFILE_ID),
        );
        assert_eq!(bootstrap_selection(&headers), BootstrapSelection::Selected);

        headers.insert(
            SUPPORTED_SCHEMAS_HEADER,
            HeaderValue::from_static("2.1, 2.2"),
        );
        assert_eq!(
            bootstrap_selection(&headers),
            BootstrapSelection::Unsupported
        );

        headers.insert(
            SUPPORTED_SCHEMAS_HEADER,
            HeaderValue::from_static("2.1,2.2"),
        );
        headers.append(
            SUPPORTED_SCHEMAS_HEADER,
            HeaderValue::from_static("2.1,2.2"),
        );
        assert_eq!(
            bootstrap_selection(&headers),
            BootstrapSelection::Unsupported
        );

        headers.remove(SUPPORTED_SCHEMAS_HEADER);
        assert_eq!(
            bootstrap_selection(&headers),
            BootstrapSelection::Unsupported
        );

        headers.insert(
            SUPPORTED_SCHEMAS_HEADER,
            HeaderValue::from_static("2.1,2.2"),
        );
        headers.insert(
            SYNC_PROFILE_HEADER,
            HeaderValue::from_static("hub-sync-v1@1.2.0"),
        );
        assert_eq!(
            bootstrap_selection(&headers),
            BootstrapSelection::Unsupported
        );
    }

    #[test]
    fn changes_since_request_validation_matches_protocol_fixtures() {
        let valid = fixture("changes-since-request.json");
        assert!(
            parse_changes_since_request(
                &serde_json::to_vec(&valid["request"]).expect("serialize request")
            )
            .is_ok()
        );

        for (name, expected) in [
            (
                "changes-since-request-invalid-missing-field.json",
                RequestValidationError::InvalidRequest,
            ),
            (
                "changes-since-request-invalid-extra-field.json",
                RequestValidationError::InvalidRequest,
            ),
            (
                "changes-since-request-invalid-wrong-type.json",
                RequestValidationError::InvalidRequest,
            ),
            (
                "changes-since-request-reversed-range.json",
                RequestValidationError::InvalidSchemaRange,
            ),
            (
                "changes-since-request-base-excluded.json",
                RequestValidationError::InvalidSchemaRange,
            ),
            (
                "changes-since-request-unsupported-range.json",
                RequestValidationError::UnsupportedSchemaRange,
            ),
        ] {
            let value = fixture(name);
            assert_eq!(
                parse_changes_since_request(
                    &serde_json::to_vec(&value["request"]).expect("serialize request")
                ),
                Err(expected),
                "{name}"
            );
        }
        assert_eq!(
            parse_changes_since_request(br#"{"#),
            Err(RequestValidationError::InvalidJson)
        );
    }

    #[test]
    fn changes_since_request_accepts_exact_raw_limit() {
        let at_limit = fixture("changes-since-request-8192-bytes.json");
        let raw = at_limit["body"].as_str().expect("fixture body").as_bytes();
        assert_eq!(raw.len(), MAX_CHANGES_SINCE_REQUEST_BYTES);
        assert!(parse_changes_since_request(raw).is_ok());

        let over_limit = fixture("changes-since-request-8193-bytes.json");
        assert_eq!(
            over_limit["body"].as_str().expect("fixture body").len(),
            MAX_CHANGES_SINCE_REQUEST_BYTES + 1
        );
    }

    #[test]
    fn sequence_admission_matches_the_i_json_exact_integer_boundary() {
        let mut request = fixture("changes-since-request.json")["request"].clone();
        request["from_sequence"] = serde_json::json!(MAX_I_JSON_INTEGER);
        assert!(
            parse_changes_since_request(&serde_json::to_vec(&request).expect("safe request JSON"))
                .is_ok()
        );

        request["from_sequence"] = serde_json::json!(MAX_I_JSON_INTEGER + 1);
        assert_eq!(
            parse_changes_since_request(
                &serde_json::to_vec(&request).expect("unsafe request JSON")
            ),
            Err(RequestValidationError::InvalidRequest)
        );
        assert!(sequence_is_admitted(MAX_I_JSON_INTEGER));
        assert!(!sequence_is_admitted(MAX_I_JSON_INTEGER + 1));
    }

    #[test]
    fn profile_pack_admission_is_inclusive_and_rejects_internal_64_mib_capacity() {
        assert!(!pack_size_is_admitted(0));
        assert!(pack_size_is_admitted(1));
        assert!(pack_size_is_admitted(MAX_PROFILE_PACK_BYTES));
        assert!(!pack_size_is_admitted(MAX_PROFILE_PACK_BYTES + 1));
        assert!(!pack_size_is_admitted(64 * 1024 * 1024));
    }

    #[tokio::test]
    async fn every_hub_sync_json_response_is_raw_byte_bounded() {
        fn materialize(name: &str, document_key: &str) -> Vec<u8> {
            let value = fixture(name);
            let mut raw = serde_json::to_vec(&value[document_key]).expect("fixture document");
            let padding = value["padding_bytes"]
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .expect("fixture padding");
            raw.resize(raw.len() + padding, b' ');
            assert_eq!(
                raw.len(),
                value["body_bytes"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .expect("fixture body bytes")
            );
            assert_eq!(
                Sha256Digest::of_bytes(&raw).to_string(),
                value["body_sha256"].as_str().expect("fixture digest")
            );
            raw
        }

        for (at_limit, over_limit, document_key, status) in [
            (
                "sync-manifest-response-2097152-bytes.json",
                "sync-manifest-response-2097153-bytes.json",
                "document",
                StatusCode::OK,
            ),
            (
                "changes-since-changed-set-response-2097152-bytes.json",
                "changes-since-changed-set-response-2097153-bytes.json",
                "document",
                StatusCode::OK,
            ),
            (
                "changes-since-rebase-response-2097152-bytes.json",
                "changes-since-rebase-response-2097153-bytes.json",
                "document",
                StatusCode::CONFLICT,
            ),
            (
                "changes-since-error-response-2097152-bytes.json",
                "changes-since-error-response-2097153-bytes.json",
                "document",
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
        ] {
            let accepted = materialize(at_limit, document_key);
            let response = bounded_control_json_bytes(status, accepted.clone(), false)
                .expect("at-limit response");
            assert_eq!(response.status(), status);
            assert_eq!(
                response
                    .into_body()
                    .collect()
                    .await
                    .expect("response body")
                    .to_bytes()
                    .as_ref(),
                accepted.as_slice()
            );

            let rejected = materialize(over_limit, document_key);
            assert!(matches!(
                bounded_control_json_bytes(status, rejected, false),
                Err(BoundedJsonError::ResponseTooLarge)
            ));
        }
    }

    #[tokio::test]
    async fn unsupported_range_error_is_the_only_error_that_requires_no_store() {
        let unsupported = sync_error_response(
            StatusCode::NOT_ACCEPTABLE,
            "schema_range_unsupported",
            "Requested schema range has no supported version.",
            true,
        );
        assert_eq!(
            unsupported.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store"
        );
        let invalid = sync_error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            "Request body does not match the changes-since schema.",
            false,
        );
        assert!(invalid.headers().get(header::CACHE_CONTROL).is_none());
    }
}
