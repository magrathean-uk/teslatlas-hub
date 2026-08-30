// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("operating-system entropy is unavailable for pairing credentials")]
    EntropyUnavailable,
    #[error("cannot create data directory: {0}")]
    CreateDataDir(std::io::Error),
    #[error("cannot create packs directory: {0}")]
    CreatePacksDir(std::io::Error),
    #[error("cannot protect data directory: {0}")]
    ProtectDataDir(std::io::Error),
    #[error("cannot protect packs directory: {0}")]
    ProtectPacksDir(std::io::Error),
    #[error("Hub data directory has unsafe type, owner, or mode: {0}")]
    UnsafeDataDir(PathBuf),
    #[error("Hub packs directory has unsafe type, owner, or mode: {0}")]
    UnsafePacksDir(PathBuf),
    #[error("cannot create shared Hub SQLite file: {0}")]
    CreateSharedSqlite(std::io::Error),
    #[error("cannot inspect shared Hub SQLite file: {0}")]
    InspectSharedSqlite(std::io::Error),
    #[error("cannot protect shared Hub SQLite file: {0}")]
    ProtectSharedSqlite(std::io::Error),
    #[error("shared Hub SQLite file has unsafe type or mode: {0}")]
    UnsafeSharedSqlite(PathBuf),
    #[error("cannot create Hub-private import spool: {0}")]
    CreateImportSpool(std::io::Error),
    #[error("cannot inspect Hub-private import spool: {0}")]
    InspectImportSpool(std::io::Error),
    #[error("Hub-private import spool has unsafe type or mode: {0}")]
    UnsafeImportSpool(PathBuf),
    #[error("cannot open hub database: {0}")]
    Open(rusqlite::Error),
    #[error("cannot inspect hub catalogue: {0}")]
    InspectCatalogue(std::io::Error),
    #[error("cannot read hub catalogue: {0}")]
    ReadCatalogue(std::io::Error),
    #[error("cannot resolve hub catalogue path: {0}")]
    ResolveCataloguePath(std::io::Error),
    #[error("hub catalogue path cannot be represented as a SQLite file URI")]
    InvalidCataloguePath,
    #[error(
        "hub catalogue is actively changing; retry when idle or stop Hub for immutable diagnostics"
    )]
    PendingCatalogueWal,
    #[error("cannot checkpoint Hub catalogue: {0}")]
    CatalogueCheckpoint(rusqlite::Error),
    #[error("Hub catalogue checkpoint is busy or incomplete")]
    CatalogueCheckpointIncomplete,
    #[error("immutable catalogue snapshot mode is required")]
    ImmutableSnapshotRequired,
    #[error("hub catalogue changed during the immutable diagnostic check")]
    CatalogueChangedDuringImmutableCheck,
    #[error("cannot configure hub database: {0}")]
    Configure(rusqlite::Error),
    #[error("TeslaMate token pair is empty")]
    TeslaMateTokenPairEmpty,
    #[error("TeslaMate token ciphertext exceeds the fixed size limit")]
    TeslaMateTokenCiphertextTooLarge,
    #[error("TeslaMate token refresh schedule is invalid")]
    InvalidTeslaMateTokenSchedule,
    #[error("cannot access TeslaMate token store: {0}")]
    TeslaMateTokenStore(rusqlite::Error),
    #[error("Fleet token store is invalid")]
    InvalidFleetTokenStore,
    #[error("cannot access Fleet token store: {0}")]
    FleetTokenStore(rusqlite::Error),
    #[error("cannot access Fleet refresh receipt: {0}")]
    FleetRefreshReceipt(rusqlite::Error),
    #[error("Fleet refresh outcome is ambiguous; replace credentials before retrying")]
    FleetRefreshOutcomeUnknown,
    #[error("Fleet credential generation is invalid")]
    InvalidFleetRefreshGeneration,
    #[error("Fleet retryable refresh failure receipt is invalid")]
    InvalidFleetRefreshFailure,
    #[error("Fleet credential ciphertext WAL scrub did not complete")]
    FleetCredentialScrubIncomplete,
    #[error("invalid address cache record")]
    InvalidAddressCache,
    #[error("cannot write address cache: {0}")]
    AddressCacheWrite(rusqlite::Error),
    #[error("invalid address enrichment result")]
    InvalidAddressEnrichment,
    #[error("cannot write address enrichment job: {0}")]
    AddressEnrichmentWrite(rusqlite::Error),
    #[error("cannot create Hub SQLite backup: {0}")]
    Backup(rusqlite::Error),
    #[error("Hub SQLite backup destination already exists: {0}")]
    BackupDestinationExists(PathBuf),
    #[error("Hub SQLite backup destination must not be the live catalogue")]
    BackupDestinationIsLiveDatabase,
    #[error("cannot create Hub backup directory: {0}")]
    CreateBackupDirectory(std::io::Error),
    #[error("Hub backup copy-byte calculation overflowed")]
    BackupCapacityOverflow,
    #[error("cannot copy Hub backup pack from {source_path} to {destination}: {source_error}")]
    CopyBackupPack {
        source_path: PathBuf,
        destination: PathBuf,
        source_error: std::io::Error,
    },
    #[error("Hub backup pack {path} is {actual} bytes; expected {expected}")]
    BackupPackSizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("cannot open Hub backup pack: {0}")]
    OpenBackupPack(std::io::Error),
    #[error("cannot read Hub backup pack: {0}")]
    ReadBackupPack(std::io::Error),
    #[error("Hub backup pack digest mismatches its catalogue: {path}")]
    BackupPackDigestMismatch { path: PathBuf },
    #[error("cannot migrate hub database: {0}")]
    Migrate(rusqlite::Error),
    #[error("database query failed: {0}")]
    Query(rusqlite::Error),
    #[error("cannot begin local transaction: {0}")]
    Begin(rusqlite::Error),
    #[error("cannot write supervised collector lease: {0}")]
    SupervisedCollectorLeaseWrite(rusqlite::Error),
    #[error("another supervised collector owns the live lease")]
    SupervisedCollectorLeaseHeld,
    #[error("supervised collector lease was lost or expired")]
    SupervisedCollectorLeaseLost,
    #[error("supervised collector lease clock overflowed")]
    SupervisedCollectorClockOverflow,
    #[error("cannot open Hub publication gate: {0}")]
    OpenPublicationGate(std::io::Error),
    #[error("cannot protect Hub publication gate: {0}")]
    ProtectPublicationGate(std::io::Error),
    #[error("Hub publication gate metadata is unsafe: {0}")]
    UnsafePublicationGate(PathBuf),
    #[error("cannot acquire Hub publication gate: {0}")]
    LockPublicationGate(std::io::Error),
    #[error("Hub publication gate is busy")]
    PublicationGateBusy,
    #[error("cannot publish sync manifest: {0}")]
    PublishManifest(rusqlite::Error),
    #[error("catalogue durability checkpoint failed: {0}")]
    CatalogueDurability(std::io::Error),
    #[error(
        "catalogue commit outcome is conflicting for vehicle {vehicle_id} snapshot {snapshot_id}"
    )]
    AmbiguousCatalogueCommit { vehicle_id: Uuid, snapshot_id: Uuid },
    #[error("unpublished pack path is outside its exact content-addressed namespace: {0}")]
    UnsafeUnpublishedPackPath(PathBuf),
    #[error("cannot clean unpublished pack {path}: {source}")]
    CleanupUnpublishedPack {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("unpublished pack bytes do not match its content digest: {0}")]
    UnpublishedPackDigestMismatch(PathBuf),
    #[error("cannot associate a snapshot fingerprint with uncatalogued manifest {0}")]
    FingerprintManifestMissing(Uuid),
    #[error(
        "changed-history import must publish a typed delta bound to the immutable base snapshot"
    )]
    ImportDeltaRequiresBaseBinding,
    #[error("invalid stored vehicle identity")]
    InvalidVehicleId,
    #[error("invalid vehicle identity value")]
    InvalidVehicleIdentity,
    #[error("vehicle identity conflicts across sources")]
    VehicleIdentityConflict,
    #[error("invalid stored source identity")]
    InvalidSourceId,
    #[error("terrain materialised car is missing for {0}")]
    TerrainCarMissing(Uuid),
    #[error("cannot publish terrain projection pack: {0}")]
    TerrainPack(ProjectionPackError),
    #[error("cannot repair projection pack staging: {0}")]
    PackStartupRepair(ProjectionPackError),
    #[error("cannot register source: {0}")]
    RegisterSource(rusqlite::Error),
    #[error("cannot register vehicle: {0}")]
    RegisterVehicle(rusqlite::Error),
    #[error("cannot create pairing invitation: {0}")]
    CreatePairing(rusqlite::Error),
    #[error("cannot revoke pairing invitation: {0}")]
    RevokePairing(rusqlite::Error),
    #[error("cannot claim pairing invitation: {0}")]
    ClaimPairing(rusqlite::Error),
    #[error("cannot access legacy refresh receipt: {0}")]
    LegacyRefreshReceipt(rusqlite::Error),
    #[error("legacy refresh outcome is unresolved; explicit re-login is required")]
    LegacyRefreshOutcomeUnknown,
    #[error("legacy refresh generation is invalid")]
    InvalidLegacyRefreshGeneration,
    #[error("cannot rotate paired device bearer: {0}")]
    RotateDevice(rusqlite::Error),
    #[error("cannot revoke paired device: {0}")]
    RevokeDevice(rusqlite::Error),
    #[error("cannot append raw observation: {0}")]
    AppendObservation(rusqlite::Error),
    #[error("cannot initialise Hub installation identity: {0}")]
    InstallationIdentity(rusqlite::Error),
    #[error("cannot serialize sync manifest: {0}")]
    SerializeManifest(serde_json::Error),
    #[error("cannot deserialize sync manifest: {0}")]
    DeserializeManifest(serde_json::Error),
    #[error(
        "schema {0:?} is recognized but cannot be catalogued or served until its pack, catalogue, and receiver implementation exists"
    )]
    SchemaPublicationUnavailable(crate::protocol::SchemaVersion),
    #[error("cannot write schema 2.2 no-op: {0}")]
    WriteSchema22NoOp(std::io::Error),
    #[error("cannot read schema 2.2 no-op: {0}")]
    ReadSchema22NoOp(std::io::Error),
    #[error("cannot access schema 2.2 no-op storage: {0}")]
    AccessSchema22NoOp(std::io::Error),
    #[error("schema 2.2 no-op storage has unsafe type, ownership, mode, or name: {0}")]
    UnsafeSchema22NoOpPath(PathBuf),
    #[error("schema 2.2 no-op directory is absent")]
    Schema22NoOpNotFound,
    #[error("schema 2.2 manifest for vehicle {0} requires paired publication")]
    Schema22PairPublicationRequired(Uuid),
    #[error("schema 2.2 manifest/no-op pair is invalid: {0}")]
    InvalidSchema22Pair(String),
    #[error("schema 2.2 snapshot {snapshot_id} for vehicle {vehicle_id} is immutable")]
    Schema22SnapshotConflict { vehicle_id: Uuid, snapshot_id: Uuid },
    #[error("cannot access import generation: {0}")]
    ImportGeneration(rusqlite::Error),
    #[error("import generation is invalid")]
    InvalidImportGeneration,
    #[error("import generation was not found or is not staging")]
    ImportGenerationNotFound,
    #[error("import generation promotion became unsettled by newer live state")]
    ImportGenerationConflict,
    #[error("cannot access lineage catalogue: {0}")]
    LineageCatalog(rusqlite::Error),
    #[error("lineage catalogue conflicts with an existing sequence")]
    LineageCatalogConflict,
    #[error("V2 lineage has no safely compactable collector delta suffix")]
    LineageCompactionUnavailable,
    #[error("V2 lineage cannot accept another pack within the client protocol limits")]
    LineageCapacityExhausted,
    #[error("cannot read the store clock for retired-lineage pack retention: {0}")]
    RetiredLineageClock(std::time::SystemTimeError),
    #[error("retired-lineage pack retention clock does not fit epoch milliseconds")]
    RetiredLineageClockOverflow,
    #[error(
        "immutable V2 base binding is missing for {0}; refusing to reconstruct it from mutable source state"
    )]
    ImmutableBaseBindingMissing(Uuid),
    #[error(
        "TeslaMate imported-history inventory is missing for {0}; refusing a changed import without exact deletion provenance"
    )]
    TeslaMateImportInventoryMissing(Uuid),
    #[error(
        "TeslaMate durable projection digest state is missing for {0}; legacy inventory cannot prove changed-row payloads"
    )]
    TeslaMateImportProjectionStateMissing(Uuid),
    #[error(
        "TeslaMate legacy direct-import base for {0} cannot be proved unchanged; rebase_required"
    )]
    TeslaMateLegacyDirectRebaseRequired(Uuid),
    #[error("TeslaMate projection-state capture failed: {0}")]
    TeslaMateProjectionState(#[from] TeslaMateProjectionStateError),
    #[error("invalid sync mutation: {0}")]
    SyncMutation(String),
    #[error("lineage pack is not verified and ready")]
    LineagePackNotReady,
    #[error("lineage pack digest does not match its content")]
    LineagePackDigestMismatch,
    #[error("cannot open stored lineage pack: {0}")]
    OpenLineagePack(std::io::Error),
    #[error("cannot decode stored lineage pack: {0}")]
    DecodeLineagePack(std::io::Error),
    #[error("cannot create transient lineage pack inspection {path}: {source}")]
    CreateLineagePackInspection {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot flush transient lineage pack inspection: {0}")]
    SyncLineagePackInspection(std::io::Error),
    #[error("cannot serialize raw observation: {0}")]
    SerializeObservation(serde_json::Error),
    #[error("invalid sync manifest: {0}")]
    Manifest(crate::protocol::ProtocolError),
    #[error("sync sequence does not fit SQLite signed integer")]
    SequenceTooLarge,
    #[error("sync sequence is exhausted")]
    SequenceExhausted,
    #[error("stored sync sequence is invalid")]
    InvalidStoredSequence,
    #[error(
        "manifest sequence {attempted} is stale; current sequence is {current} for {vehicle_id}"
    )]
    StaleManifest {
        vehicle_id: Uuid,
        attempted: u64,
        current: u64,
    },
    #[error("pack size does not fit SQLite signed integer")]
    PackSizeTooLarge,
    #[error("lineage pack ordinal does not fit the protocol")]
    PackOrdinalTooLarge,
    #[error("stored pack path is not canonical")]
    UnsafeStoredPackPath,
    #[error("cannot inspect catalogue pack {path}: {source}")]
    InspectCatalogPack {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("catalogue pack is not a regular file: {path}")]
    CatalogPackNotRegular { path: PathBuf },
    #[error("catalogue pack {path} is {actual} bytes; expected {expected}")]
    CatalogPackSizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },
    #[error("catalogue pack digest mismatches its record: {path}")]
    CatalogPackDigestMismatch { path: PathBuf },
    #[error("{0} must not be empty")]
    EmptyIdentity(&'static str),
    #[error("{field} is {actual} bytes; maximum is {maximum}")]
    IdentityTooLong {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("{0} must not contain control characters")]
    IdentityControlCharacter(&'static str),
    #[error("source kind must be lowercase ASCII letters, digits, hyphens, or underscores")]
    InvalidSourceKind,
    #[error("{0} must be an epoch timestamp in milliseconds")]
    NegativeTimestamp(&'static str),
    #[error("pairing invitation expiry must be later than its creation time")]
    InvalidPairingExpiry,
    #[error("pairing invitation was rejected")]
    PairingRejected,
    #[error("source id must not be nil")]
    NilSourceId,
    #[error("vehicle id must not be nil")]
    NilVehicleId,
    #[error("raw observation payload must be a JSON object")]
    ObservationMustBeObject,
    #[error("raw observation is {actual} bytes; maximum is {maximum}")]
    ObservationTooLarge { actual: usize, maximum: usize },
    #[error("raw observation is missing after a successful insert")]
    ObservationMissingAfterInsert,
    #[error("raw observation query limit {actual} must be between 1 and {maximum}")]
    InvalidObservationQueryLimit { actual: u32, maximum: u32 },
    #[error("raw observation query time range is empty or reversed")]
    InvalidObservationQueryRange,
    #[error("unknown source {0}")]
    UnknownSource(Uuid),
    #[error("unknown vehicle {0}")]
    UnknownVehicle(Uuid),
    #[error("car settings durations must be positive")]
    InvalidCarSettings,
    #[error("drive id must be positive")]
    InvalidDriveId,
    #[error("drive position page limit {0} is invalid")]
    InvalidDrivePositionPageLimit(u32),
    #[error("charge id must be positive")]
    InvalidChargeId,
    #[error("charge cost must be a finite nonnegative value")]
    InvalidChargeCost,
    #[error("unknown charge {0}")]
    UnknownCharge(i64),
    #[error("charge {charge_id} has no usable {mode} cost basis")]
    ChargeCostBasisUnavailable { charge_id: i64, mode: &'static str },
    #[error("geofence values are invalid")]
    InvalidGeofence,
    #[error("unknown geofence {0}")]
    UnknownGeofence(i64),
    #[error("vehicle {vehicle_id} does not belong to source {source_id}")]
    VehicleSourceMismatch { vehicle_id: Uuid, source_id: Uuid },
    #[error("stored vehicle identity {actual} differs from expected identity {expected}")]
    VehicleIdentityMismatch { expected: Uuid, actual: Uuid },
    #[error("stored {0} is not a valid UUID")]
    InvalidStoredUuid(&'static str),
    #[error("stored source generation is invalid")]
    InvalidStoredGeneration,
    #[error("stored count is invalid")]
    InvalidStoredCount,
    #[error("{0} lifecycle session(s) require reconstruction")]
    QuarantinedLifecycle(usize),
    #[error("unsupported hub schema version {0}")]
    UnsupportedSchema(i32),
    #[error("unexpected hub SQLite application id {0}")]
    InvalidApplicationId(i32),
    #[error("database integrity check failed: {0}")]
    Integrity(String),
    #[error("lifecycle car id must be positive")]
    InvalidLifecycleCarId,
    #[error("lifecycle observation cursor is invalid")]
    InvalidLifecycleCursor,
    #[error("lifecycle open-session payload is invalid")]
    InvalidLifecycleSession,
    #[error("cannot write lifecycle history: {0}")]
    LifecycleWrite(rusqlite::Error),
    #[error("cannot project lifecycle: {0}")]
    LifecycleProjection(crate::lifecycle::LifecycleError),
    #[error("injected stream fault at {0}")]
    InjectedStreamFault(&'static str),
    #[cfg(test)]
    #[error("injected projection-state detach fault")]
    InjectedProjectionStateDetachFault,
    #[error("cannot serialize lifecycle history row: {0}")]
    SerializeLifecycleRow(serde_json::Error),
    #[error("cannot deserialize lifecycle history row: {0}")]
    DeserializeLifecycleRow(serde_json::Error),
    #[error("cannot access outbound request receipt: {0}")]
    OutboundRequestReceipt(rusqlite::Error),
    #[error("outbound request receipt id must be positive")]
    InvalidOutboundRequestReceiptId,
    #[error("outbound request receipt is missing or already terminal")]
    OutboundRequestReceiptNotStarted,
    #[error("token refresh receipts require the dedicated refresh API")]
    ReservedLegacyRefreshReceipt,
    #[error("outbound request correlation id must not be nil")]
    NilOutboundRequestCorrelationId,
    #[error("outbound request vehicle id must be positive")]
    InvalidOutboundRequestVehicleId,
    #[error("vehicle_data audit records require conditional_read and stream_power_confirmed")]
    InvalidVehicleDataAuditPrecondition,
    #[error("vehicle action audit classification is invalid")]
    InvalidVehicleActionAudit,
    #[error("cannot read the store clock for outbound request auditing: {0}")]
    OutboundRequestClock(std::time::SystemTimeError),
    #[error("outbound request audit clock does not fit epoch milliseconds")]
    OutboundRequestClockOverflow,
    #[error("outbound request HTTP status must be between 100 and 599")]
    InvalidOutboundRequestHttpStatus,
    #[error("outbound request Retry-After does not fit a signed epoch-safe integer")]
    InvalidOutboundRequestRetryAfter,
    #[error("outbound request watermark must be non-negative")]
    InvalidOutboundRequestWatermark,
    #[error("stream audit diagnostic window must be non-negative")]
    InvalidStreamAuditWindow,
    #[error("outbound request query limit {actual} must be between 1 and {maximum}")]
    InvalidOutboundRequestQueryLimit { actual: u32, maximum: u32 },
    #[error("outbound request audit has no room without deleting an unresolved receipt")]
    OutboundRequestAuditCapacityExhausted,
    #[error("cannot access stream session receipt: {0}")]
    StreamSessionReceipt(rusqlite::Error),
    #[error("stream session receipt id must be positive")]
    InvalidStreamSessionReceiptId,
    #[error("stream session receipt is missing or already terminal")]
    StreamSessionReceiptNotStarted,
    #[error("stream session requires a successful matching unsubscribe receipt")]
    StreamSessionUnsubscribeNotCompleted,
    #[error("stream session audit has no room without deleting an unresolved session")]
    StreamSessionAuditCapacityExhausted,
}

#[derive(Debug, Error)]
pub enum ObservationVerificationError {
    #[error("source car id must be positive")]
    InvalidSourceCarId,
    #[error("watermark must be non-negative")]
    InvalidWatermark,
    #[error("no vehicle mapping for source car")]
    NoVehicleMapping,
    #[error("ambiguous vehicle mapping for source car")]
    AmbiguousVehicleMapping,
    #[error("observation query failed: {0}")]
    Store(#[from] StoreError),
}

impl ObservationVerificationError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSourceCarId => "invalid_source_car_id",
            Self::InvalidWatermark => "invalid_watermark",
            Self::NoVehicleMapping => "no_vehicle_mapping",
            Self::AmbiguousVehicleMapping => "ambiguous_vehicle_mapping",
            Self::Store(_) => "database_error",
        }
    }
}

#[derive(Debug, Error)]
pub enum NoWakeVerificationError {
    #[error("audit watermark must be non-negative")]
    InvalidAuditWatermark,
    #[error("no-wake audit query failed: {0}")]
    Store(#[from] StoreError),
    #[error("observation verification failed: {0}")]
    Observation(#[from] ObservationVerificationError),
}
