// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleStateRecord {
    pub vehicle_id: Uuid,
    pub car_id: i64,
    pub last_observation_id: i64,
    pub open_session_json: Vec<u8>,
    pub quarantined: bool,
    pub updated_at_ms: i64,
}

/// One transactional lifecycle write: open-session snapshot plus completed rows.
#[derive(Debug, Clone)]
pub struct LifecycleCommit<'a> {
    pub vehicle_id: Uuid,
    pub car_id: i64,
    pub open_session_json: &'a [u8],
    pub last_observation_id: i64,
    pub quarantined: bool,
    pub updated_at_ms: i64,
    pub delta: &'a crate::lifecycle::LifecycleDelta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOutboxClaim {
    pub vehicle_id: Uuid,
    pub dirty_revision: i64,
    pub attempts: i64,
    pub claimed_at_ms: i64,
    pub lease_until_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncMutation {
    pub vehicle_id: Uuid,
    pub revision: i64,
    pub entity: String,
    pub entity_id: i64,
    pub car_id: i64,
    pub operation: String,
    pub payload_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncMutationClaim {
    pub vehicle_id: Uuid,
    pub from_revision: i64,
    pub to_revision: i64,
    pub mutations: Vec<SyncMutation>,
}

/// Exact, read-only input for replacing a contiguous collector-owned delta
/// suffix. Import successors are intentionally never represented here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveDeltaCompactionSpan {
    pub delta: LineageDelta,
    pub from_revision: i64,
    pub to_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveDeltaCompactionPlan {
    pub vehicle_id: Uuid,
    pub base_snapshot_id: Uuid,
    pub anchor_sequence: u64,
    pub anchor_digest: Sha256Digest,
    pub head_sequence: u64,
    pub head_digest: Sha256Digest,
    pub first_ordinal: u32,
    pub from_revision: i64,
    pub to_revision: i64,
    pub mutations: Vec<SyncMutation>,
    pub replaced_spans: Vec<LiveDeltaCompactionSpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenSessionSeedReport {
    pub provisional_rows_inserted: usize,
    pub standalone_positions_inserted: usize,
    pub watermarks_written: usize,
    pub no_op: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MaterialisedHistory {
    pub car: Option<crate::hub_pack::ProjectionCar>,
    pub drives: Vec<crate::hub_pack::ProjectionDrive>,
    pub positions: Vec<crate::hub_pack::ProjectionPosition>,
    pub charges: Vec<crate::hub_pack::ProjectionCharge>,
    pub charge_samples: Vec<crate::hub_pack::ProjectionChargeSample>,
    pub states: Vec<crate::hub_pack::ProjectionState>,
    pub updates: Vec<crate::hub_pack::ProjectionUpdate>,
}

/// Non-secret source identity presented by an independent collector. The Hub
/// persists a generated UUID for this pair so restarts never change identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDescriptor {
    pub kind: String,
    pub key: String,
}

impl SourceDescriptor {
    pub fn new(kind: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            key: key.into(),
        }
    }

    fn validate(&self) -> Result<(), StoreError> {
        validate_identity("source kind", &self.kind, MAX_SOURCE_KIND_BYTES)?;
        if !self.kind.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        }) {
            return Err(StoreError::InvalidSourceKind);
        }
        validate_identity("source key", &self.key, MAX_SOURCE_KEY_BYTES)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub source_id: Uuid,
    pub kind: String,
    pub key: String,
    pub generation: u64,
    pub created_at_ms: i64,
}

/// Source-owned stable vehicle identity and optional mutable display fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleDescriptor {
    pub source_id: Uuid,
    pub source_vehicle_key: String,
    pub vin: Option<String>,
    pub display_name: Option<String>,
    pub tesla_eid: Option<i64>,
    pub tesla_vid: Option<i64>,
}

impl VehicleDescriptor {
    pub fn new(source_id: Uuid, source_vehicle_key: impl Into<String>) -> Self {
        Self {
            source_id,
            source_vehicle_key: source_vehicle_key.into(),
            vin: None,
            display_name: None,
            tesla_eid: None,
            tesla_vid: None,
        }
    }

    pub fn with_tesla_identity(mut self, eid: Option<i64>, vid: Option<i64>) -> Self {
        self.tesla_eid = eid;
        self.tesla_vid = vid;
        self
    }

    fn validate(&self) -> Result<(), StoreError> {
        if self.source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        validate_identity(
            "source vehicle key",
            &self.source_vehicle_key,
            MAX_VEHICLE_KEY_BYTES,
        )?;
        if let Some(vin) = &self.vin {
            validate_identity("vehicle VIN", vin, MAX_VIN_BYTES)?;
        }
        if let Some(display_name) = &self.display_name {
            validate_identity("vehicle display name", display_name, MAX_DISPLAY_NAME_BYTES)?;
        }
        if self.tesla_eid.is_some_and(|value| value <= 0)
            || self.tesla_vid.is_some_and(|value| value <= 0)
        {
            return Err(StoreError::InvalidVehicleIdentity);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VehicleRecord {
    pub vehicle_id: Uuid,
    pub source_id: Uuid,
    pub source_vehicle_key: String,
    pub vin: Option<String>,
    pub display_name: Option<String>,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
}

/// Exact alias-free rows used until the first exported TeslaMate snapshot
/// proves the pre-read VIN/EID/VID tuple. Ordinary rejection removes unchanged
/// provisional rows; crash residue remains non-published and is reused by the
/// same deterministic source/vehicle registration on retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeslaMateIdentityRegistrationCheckpoint {
    source: SourceRecord,
    source_created: bool,
    vehicle: VehicleRecord,
    vehicle_created: bool,
}

/// One collector-provided raw source response. The Hub accepts JSON objects
/// only; a response batch belongs as independent observations, not an array.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservationInput {
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub observed_at_ms: i64,
    pub payload: Value,
}

impl ObservationInput {
    fn validate(&self) -> Result<(), StoreError> {
        if self.source_id.is_nil() {
            return Err(StoreError::NilSourceId);
        }
        if self.vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        validate_timestamp("observed_at_ms", self.observed_at_ms)?;
        if !self.payload.is_object() {
            return Err(StoreError::ObservationMustBeObject);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObservationRecord {
    pub observation_id: i64,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub observed_at_ms: i64,
    pub received_at_ms: i64,
    pub payload_sha256: Sha256Digest,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ObservationTarget {
    vehicle_id: Uuid,
    source_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationWatermark {
    pub source_car_id: i64,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub observation_id: i64,
    pub observed_at_ms: Option<i64>,
    pub received_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationVerification {
    pub source_car_id: i64,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub after_observation_id: i64,
    pub latest_observation_id: Option<i64>,
    pub latest_observed_at_ms: Option<i64>,
    pub latest_received_at_ms: Option<i64>,
}

impl ObservationVerification {
    pub fn verified(&self) -> bool {
        self.latest_observation_id.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OutboundRequestReceiptId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StreamSessionReceiptId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSessionTerminalOutcome {
    CancelledBeforeSubscription,
    TransportEnded,
    Failed,
}

impl StreamSessionTerminalOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CancelledBeforeSubscription => "cancelled_before_subscription",
            Self::TransportEnded => "transport_ended",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundRequestWatermark {
    pub receipt_id: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StreamAuditSummary {
    pub since_ms: i64,
    pub connect_attempts: u64,
    pub successful_connects: u64,
    pub subscribe_attempts: u64,
    pub successful_subscriptions: u64,
    pub transport_errors: u64,
    pub authentication_rejections: u64,
    pub protocol_errors: u64,
    pub unresolved_attempts: u64,
    pub last_subscription_success_at_ms: Option<i64>,
    pub last_failure_at_ms: Option<i64>,
    pub sessions: u64,
    pub unresolved_sessions: u64,
    pub orderly_shutdowns: u64,
    pub transport_ended_sessions: u64,
    pub failed_sessions: u64,
    pub cancelled_before_subscription_sessions: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestTransport {
    OwnerApi,
    FleetApi,
    Stream,
    LegacyAuth,
}

impl OutboundRequestTransport {
    const fn as_str(self) -> &'static str {
        match self {
            Self::OwnerApi => "owner_api",
            Self::FleetApi => "fleet_api",
            Self::Stream => "stream",
            Self::LegacyAuth => "legacy_auth",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "owner_api" => Some(Self::OwnerApi),
            "fleet_api" => Some(Self::FleetApi),
            "stream" => Some(Self::Stream),
            "legacy_auth" => Some(Self::LegacyAuth),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestOperation {
    Products,
    VehicleProbe,
    VehicleData,
    VehicleWake,
    VehicleCommand,
    TokenRefresh,
    StreamConnect,
    StreamSubscribe,
    StreamUnsubscribe,
}

impl OutboundRequestOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Products => "products",
            Self::VehicleProbe => "vehicle_probe",
            Self::VehicleData => "vehicle_data",
            Self::VehicleWake => "vehicle_wake",
            Self::VehicleCommand => "vehicle_command",
            Self::TokenRefresh => "token_refresh",
            Self::StreamConnect => "stream_connect",
            Self::StreamSubscribe => "stream_subscribe",
            Self::StreamUnsubscribe => "stream_unsubscribe",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "products" => Some(Self::Products),
            "vehicle_probe" => Some(Self::VehicleProbe),
            "vehicle_data" => Some(Self::VehicleData),
            "vehicle_wake" => Some(Self::VehicleWake),
            "vehicle_command" => Some(Self::VehicleCommand),
            "token_refresh" => Some(Self::TokenRefresh),
            "stream_connect" => Some(Self::StreamConnect),
            "stream_subscribe" => Some(Self::StreamSubscribe),
            "stream_unsubscribe" => Some(Self::StreamUnsubscribe),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestSafetyClass {
    NonWakeEndpoint,
    ConditionalRead,
    DirectWakeCommand,
    ExplicitVehicleCommand,
}

impl OutboundRequestSafetyClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NonWakeEndpoint => "non_wake_endpoint",
            Self::ConditionalRead => "conditional_read",
            Self::DirectWakeCommand => "direct_wake_command",
            Self::ExplicitVehicleCommand => "explicit_vehicle_command",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "non_wake_endpoint" => Some(Self::NonWakeEndpoint),
            "conditional_read" => Some(Self::ConditionalRead),
            "direct_wake_command" => Some(Self::DirectWakeCommand),
            "explicit_vehicle_command" => Some(Self::ExplicitVehicleCommand),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestPrecondition {
    NotRequired,
    StreamPowerConfirmed,
}

impl OutboundRequestPrecondition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::StreamPowerConfirmed => "stream_power_confirmed",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "not_required" => Some(Self::NotRequired),
            "stream_power_confirmed" => Some(Self::StreamPowerConfirmed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundRequestOutcome {
    Success,
    HttpError,
    Timeout,
    TransportError,
    AuthenticationRejected,
    ProtocolError,
    ResponseTooLarge,
    Cancelled,
}

impl OutboundRequestOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::HttpError => "http_error",
            Self::Timeout => "timeout",
            Self::TransportError => "transport_error",
            Self::AuthenticationRejected => "authentication_rejected",
            Self::ProtocolError => "protocol_error",
            Self::ResponseTooLarge => "response_too_large",
            Self::Cancelled => "cancelled",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "success" => Some(Self::Success),
            "http_error" => Some(Self::HttpError),
            "timeout" => Some(Self::Timeout),
            "transport_error" => Some(Self::TransportError),
            "authentication_rejected" => Some(Self::AuthenticationRejected),
            "protocol_error" => Some(Self::ProtocolError),
            "response_too_large" => Some(Self::ResponseTooLarge),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Typed metadata committed before network I/O. There is deliberately no URL,
/// header, token, request body, response body, or arbitrary error-text field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequestStart {
    pub correlation_id: Uuid,
    pub vehicle_tesla_id: Option<i64>,
    pub transport: OutboundRequestTransport,
    pub operation: OutboundRequestOperation,
    pub safety_class: OutboundRequestSafetyClass,
    pub precondition: OutboundRequestPrecondition,
}

impl OutboundRequestStart {
    fn validate(&self) -> Result<(), StoreError> {
        if self.correlation_id.is_nil() {
            return Err(StoreError::NilOutboundRequestCorrelationId);
        }
        if self.vehicle_tesla_id.is_some_and(|id| id <= 0) {
            return Err(StoreError::InvalidOutboundRequestVehicleId);
        }
        if self.transport == OutboundRequestTransport::LegacyAuth
            && self.operation == OutboundRequestOperation::TokenRefresh
        {
            return Err(StoreError::ReservedLegacyRefreshReceipt);
        }
        if self.operation == OutboundRequestOperation::VehicleData
            && (self.safety_class != OutboundRequestSafetyClass::ConditionalRead
                || self.precondition != OutboundRequestPrecondition::StreamPowerConfirmed)
        {
            return Err(StoreError::InvalidVehicleDataAuditPrecondition);
        }
        if self.operation == OutboundRequestOperation::VehicleWake
            && (self.vehicle_tesla_id.is_none()
                || self.safety_class != OutboundRequestSafetyClass::DirectWakeCommand
                || self.precondition != OutboundRequestPrecondition::NotRequired)
        {
            return Err(StoreError::InvalidVehicleActionAudit);
        }
        if self.operation == OutboundRequestOperation::VehicleCommand
            && (self.vehicle_tesla_id.is_none()
                || self.safety_class != OutboundRequestSafetyClass::ExplicitVehicleCommand
                || self.precondition != OutboundRequestPrecondition::NotRequired)
        {
            return Err(StoreError::InvalidVehicleActionAudit);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequestCompletion {
    pub outcome: OutboundRequestOutcome,
    pub http_status: Option<u16>,
    pub retry_after_seconds: Option<u64>,
}

impl OutboundRequestCompletion {
    fn validate(&self) -> Result<(), StoreError> {
        if self
            .http_status
            .is_some_and(|status| !(100..=599).contains(&status))
        {
            return Err(StoreError::InvalidOutboundRequestHttpStatus);
        }
        if self
            .retry_after_seconds
            .is_some_and(|seconds| i64::try_from(seconds).is_err())
        {
            return Err(StoreError::InvalidOutboundRequestRetryAfter);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboundRequestReceipt {
    pub id: OutboundRequestReceiptId,
    pub correlation_id: Uuid,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub vehicle_tesla_id: Option<i64>,
    pub transport: OutboundRequestTransport,
    pub operation: OutboundRequestOperation,
    pub safety_class: OutboundRequestSafetyClass,
    pub precondition: OutboundRequestPrecondition,
    pub outcome: Option<OutboundRequestOutcome>,
    pub http_status: Option<u16>,
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoWakeVerification {
    pub after_receipt_id: i64,
    pub correlation_id: Uuid,
    pub matching_receipts: i64,
    pub unresolved_receipts: i64,
    pub unresolved_stream_sessions: i64,
    pub direct_wake_receipts: i64,
    pub conditional_without_power_receipts: i64,
    pub observation: Option<ObservationVerification>,
}

impl NoWakeVerification {
    /// An empty audit window is not proof: absence of integration data fails closed.
    pub fn audit_verified(&self) -> bool {
        self.matching_receipts > 0
            && self.unresolved_receipts == 0
            && self.unresolved_stream_sessions == 0
            && self.direct_wake_receipts == 0
            && self.conditional_without_power_receipts == 0
    }
    pub fn verified(&self) -> bool {
        self.audit_verified()
            && self
                .observation
                .as_ref()
                .is_none_or(ObservationVerification::verified)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppendObservation {
    pub observation: ObservationRecord,
    pub inserted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnerObservationResult {
    pub append: AppendObservation,
    pub drives_closed: usize,
    pub charges_closed: usize,
    pub positions_materialised: usize,
    pub charge_samples_materialised: usize,
    pub lifecycle_quarantined: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservationQuery {
    pub from_observed_at_ms: Option<i64>,
    pub until_observed_at_ms: Option<i64>,
    pub limit: u32,
}

impl ObservationQuery {
    pub const fn from_start(limit: u32) -> Self {
        Self {
            from_observed_at_ms: None,
            until_observed_at_ms: None,
            limit,
        }
    }

    fn validate(self) -> Result<(), StoreError> {
        if self.limit == 0 || self.limit > MAX_OBSERVATION_QUERY_LIMIT {
            return Err(StoreError::InvalidObservationQueryLimit {
                actual: self.limit,
                maximum: MAX_OBSERVATION_QUERY_LIMIT,
            });
        }
        if let Some(timestamp) = self.from_observed_at_ms {
            validate_timestamp("observation query lower bound", timestamp)?;
        }
        if let Some(timestamp) = self.until_observed_at_ms {
            validate_timestamp("observation query upper bound", timestamp)?;
        }
        if let (Some(from), Some(until)) = (self.from_observed_at_ms, self.until_observed_at_ms)
            && from >= until
        {
            return Err(StoreError::InvalidObservationQueryRange);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct StoredPack {
    pub digest: Sha256Digest,
    pub compressed_bytes: u64,
    pub path: PathBuf,
}
