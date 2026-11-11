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
    #[error("credential pair is invalid")]
    CredentialPairInvalid,
    #[error("cannot access credential pair store")]
    CredentialPairStore(#[source] rusqlite::Error),
    #[error("credential commit outcome is unknown")]
    CredentialCommit(#[source] rusqlite::Error),
    #[error("cannot access credential refresh receipt")]
    CredentialRefreshReceipt(#[source] rusqlite::Error),
    #[error("credential refresh outcome is unresolved")]
    RefreshOutcomeUnknown { reason: BeginRefusal },
    #[error("refresh token has already been presented")]
    RefreshTokenSpent,
    #[error("a fresh Hub sign-in is required")]
    FreshSignInRequired,
    #[error("credential slot is occupied")]
    SlotOccupied,
    #[error("credential origin is invalid")]
    InvalidOrigin,
    #[error("sign-in binding is invalid")]
    SignInBindingInvalid,
    #[error("sign-in binding has expired")]
    SignInExpired,
    #[error("credential connection has already been taken")]
    CredentialConnectionTaken,
    #[error("credential envelope is invalid")]
    #[allow(
        private_interfaces,
        reason = "M2P42 requires a typed internal envelope error in public StoreError."
    )]
    CredentialEnvelope(crate::auth::credential_envelope::EnvelopeError),
    #[error("invalid address cache record")]
    InvalidAddressCache,
    #[error("cannot write address cache: {0}")]
    AddressCacheWrite(rusqlite::Error),
    #[error("invalid address enrichment result")]
    InvalidAddressEnrichment,
    #[error("invalid terrain tile digest record")]
    InvalidTerrainTileDigest,
    #[error("cannot write address enrichment job: {0}")]
    AddressEnrichmentWrite(rusqlite::Error),
    #[error("data backup waits for credential cleanup")]
    BackupWaitsForCredentialCleanup,
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
    #[error(
        "An import source already exists, but the requested key matches none; stored keys: {stored_keys}. Use --new-import-source to register another source."
    )]
    UnmatchedImportSource { stored_keys: String },
    #[error(
        "The configured import source key for current positions matches no stored import source; stored keys: {stored_keys}"
    )]
    UnmatchedCurrentImportSource { stored_keys: String },
    /// C6 R57: the one-time identity tidy failed and its transaction rolled back.
    /// The text names the step and holds no VIN, name, vehicle id or stored
    /// value; the storage error that caused it stays in the source chain.
    #[error("cannot run the vehicle identity tidy (step vehicle_identity_tidy_v1)")]
    VehicleIdentityTidy(#[source] Box<StoreError>),
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
    #[error("the caller's write guard refused the store write at {0}")]
    WriteRefused(&'static str),
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
    #[error("stored credential status is invalid")]
    InvalidStoredAuthStatus,
    /// The read-only effective credential status could not be derived: the
    /// stored pair, its key or the store failed. It carries no cause so that no
    /// path or secret can travel with it.
    #[error("effective credential status is unavailable")]
    EffectiveCollectorAuthUnavailable,
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
    #[error("physical schema 2.2 candidate is not publication-ready")]
    PhysicalV3AdmissionInvalid,
    #[error("physical schema 2.2 pack admission failed: {0}")]
    PhysicalV3Pack(ProjectionPackError),
    #[error("vehicle {0} already has an admitted physical schema 2.2 head")]
    PhysicalV3SecondHeadUnsupported(Uuid),
    #[error("physical schema 2.2 admission catalogue is inconsistent")]
    PhysicalV3AdmissionConflict,
    #[error("physical schema 2.2 retention window is invalid")]
    PhysicalV3RetentionInvalid,
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
        "Source database imported-history inventory is missing for {0}; refusing a changed import without exact deletion provenance"
    )]
    SourceDbImportInventoryMissing(Uuid),
    #[error(
        "Source database durable projection digest state is missing for {0}; legacy inventory cannot prove changed-row payloads"
    )]
    SourceDbImportProjectionStateMissing(Uuid),
    #[error(
        "Source database legacy direct-import base for {0} cannot be proved unchanged; rebase_required"
    )]
    SourceDbLegacyDirectRebaseRequired(Uuid),
    #[error("Source database projection-state capture failed: {0}")]
    SourceDbProjectionState(#[from] SourceDbProjectionStateError),
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
    #[error("pairing scope names vehicle {0}, which is unknown or retired")]
    PairingVehicleUnavailable(Uuid),
    #[error("pairing scope names no vehicle")]
    EmptyPairingScope,
    #[error("paired device maximum lifetime must be positive")]
    InvalidDeviceMaxLifetime,
    #[error("cannot seal the rotation successor")]
    SealRotationSuccessor,
    #[error("source id must not be nil")]
    NilSourceId,
    #[error("vehicle id must not be nil")]
    NilVehicleId,
    #[error("raw observation payload must be a JSON object")]
    ObservationMustBeObject,
    #[error("current-only observation must be a provider discovery")]
    InvalidCurrentDiscovery,
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
    #[error("drive query limit {0} is invalid")]
    InvalidDriveQueryLimit(u32),
    #[error("drive query time range is empty or reversed")]
    InvalidDriveQueryRange,
    #[error("drive query cursor is invalid")]
    InvalidDriveQueryCursor,
    #[error("materialised drive identity differs from its catalogue key")]
    InvalidDriveProjectionIdentity,
    #[error("drive position page limit {0} is invalid")]
    InvalidDrivePositionPageLimit(u32),
    #[error("public page query is invalid: {0}")]
    InvalidPublicPageQuery(&'static str),
    #[error("materialised {0} identity differs from its catalogue key")]
    InvalidPublicProjectionIdentity(&'static str),
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
    #[error("{0}")]
    GeofenceRejected(crate::geo::geofence_validation::GeofenceRejection),
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
    #[error("{}", unsupported_schema_message(*.0))]
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
    #[error("cannot read receiver state: {0}")]
    ReceiverState(rusqlite::Error),
    #[error("receiver event record is invalid")]
    InvalidEventRecord,
    #[error("migration clock is unavailable")]
    MigrationClock,
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
    ReservedCredentialRefreshReceipt,
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

#[cfg(test)]
mod n1_src_storage_db_errors_rs_display_tests {
    use super::*;
    use crate::runtime::name_test_support::{ErrorExample, assert_name_free, example};

    impl ErrorExample for StoreError {
        fn error_example() -> Self {
            StoreError::EntropyUnavailable
        }
    }

    impl ErrorExample for ObservationVerificationError {
        fn error_example() -> Self {
            ObservationVerificationError::InvalidSourceCarId
        }
    }

    impl ErrorExample for NoWakeVerificationError {
        fn error_example() -> Self {
            NoWakeVerificationError::InvalidAuditWatermark
        }
    }

    #[test]
    fn every_reachable_error_display_omits_the_source_project_name() {
        fn check_store_error(error: StoreError) {
            match &error {
                StoreError::EntropyUnavailable => (),
                StoreError::CreateDataDir(..) => (),
                StoreError::CreatePacksDir(..) => (),
                StoreError::ProtectDataDir(..) => (),
                StoreError::ProtectPacksDir(..) => (),
                StoreError::UnsafeDataDir(..) => (),
                StoreError::UnsafePacksDir(..) => (),
                StoreError::CreateSharedSqlite(..) => (),
                StoreError::InspectSharedSqlite(..) => (),
                StoreError::ProtectSharedSqlite(..) => (),
                StoreError::UnsafeSharedSqlite(..) => (),
                StoreError::CreateImportSpool(..) => (),
                StoreError::InspectImportSpool(..) => (),
                StoreError::UnsafeImportSpool(..) => (),
                StoreError::Open(..) => (),
                StoreError::InspectCatalogue(..) => (),
                StoreError::ReadCatalogue(..) => (),
                StoreError::ResolveCataloguePath(..) => (),
                StoreError::InvalidCataloguePath => (),
                StoreError::PendingCatalogueWal => (),
                StoreError::CatalogueCheckpoint(..) => (),
                StoreError::CatalogueCheckpointIncomplete => (),
                StoreError::ImmutableSnapshotRequired => (),
                StoreError::CatalogueChangedDuringImmutableCheck => (),
                StoreError::Configure(..) => (),
                StoreError::CredentialPairInvalid => (),
                StoreError::CredentialPairStore(..) => (),
                StoreError::CredentialCommit(..) => (),
                StoreError::CredentialRefreshReceipt(..) => (),
                StoreError::RefreshOutcomeUnknown { .. } => (),
                StoreError::RefreshTokenSpent => (),
                StoreError::FreshSignInRequired => (),
                StoreError::SlotOccupied => (),
                StoreError::InvalidOrigin => (),
                StoreError::SignInBindingInvalid => (),
                StoreError::SignInExpired => (),
                StoreError::CredentialConnectionTaken => (),
                StoreError::CredentialEnvelope(..) => (),
                StoreError::InvalidAddressCache => (),
                StoreError::AddressCacheWrite(..) => (),
                StoreError::InvalidAddressEnrichment => (),
                StoreError::InvalidTerrainTileDigest => (),
                StoreError::AddressEnrichmentWrite(..) => (),
                StoreError::BackupWaitsForCredentialCleanup => (),
                StoreError::Backup(..) => (),
                StoreError::BackupDestinationExists(..) => (),
                StoreError::BackupDestinationIsLiveDatabase => (),
                StoreError::CreateBackupDirectory(..) => (),
                StoreError::BackupCapacityOverflow => (),
                StoreError::CopyBackupPack { .. } => (),
                StoreError::BackupPackSizeMismatch { .. } => (),
                StoreError::OpenBackupPack(..) => (),
                StoreError::ReadBackupPack(..) => (),
                StoreError::BackupPackDigestMismatch { .. } => (),
                StoreError::Migrate(..) => (),
                StoreError::UnmatchedImportSource { .. } => (),
                StoreError::UnmatchedCurrentImportSource { .. } => (),
                StoreError::VehicleIdentityTidy(..) => (),
                StoreError::Query(..) => (),
                StoreError::Begin(..) => (),
                StoreError::SupervisedCollectorLeaseWrite(..) => (),
                StoreError::SupervisedCollectorLeaseHeld => (),
                StoreError::SupervisedCollectorLeaseLost => (),
                StoreError::SupervisedCollectorClockOverflow => (),
                StoreError::OpenPublicationGate(..) => (),
                StoreError::ProtectPublicationGate(..) => (),
                StoreError::UnsafePublicationGate(..) => (),
                StoreError::LockPublicationGate(..) => (),
                StoreError::PublicationGateBusy => (),
                StoreError::PublishManifest(..) => (),
                StoreError::CatalogueDurability(..) => (),
                StoreError::AmbiguousCatalogueCommit { .. } => (),
                StoreError::UnsafeUnpublishedPackPath(..) => (),
                StoreError::CleanupUnpublishedPack { .. } => (),
                StoreError::UnpublishedPackDigestMismatch(..) => (),
                StoreError::FingerprintManifestMissing(..) => (),
                StoreError::ImportDeltaRequiresBaseBinding => (),
                StoreError::InvalidVehicleId => (),
                StoreError::InvalidVehicleIdentity => (),
                StoreError::VehicleIdentityConflict => (),
                StoreError::InvalidSourceId => (),
                StoreError::TerrainCarMissing(..) => (),
                StoreError::WriteRefused(..) => (),
                StoreError::TerrainPack(..) => (),
                StoreError::PackStartupRepair(..) => (),
                StoreError::RegisterSource(..) => (),
                StoreError::RegisterVehicle(..) => (),
                StoreError::CreatePairing(..) => (),
                StoreError::RevokePairing(..) => (),
                StoreError::ClaimPairing(..) => (),
                StoreError::InvalidStoredAuthStatus => (),
                StoreError::EffectiveCollectorAuthUnavailable => (),
                StoreError::RotateDevice(..) => (),
                StoreError::RevokeDevice(..) => (),
                StoreError::AppendObservation(..) => (),
                StoreError::InstallationIdentity(..) => (),
                StoreError::SerializeManifest(..) => (),
                StoreError::DeserializeManifest(..) => (),
                StoreError::SchemaPublicationUnavailable(..) => (),
                StoreError::WriteSchema22NoOp(..) => (),
                StoreError::ReadSchema22NoOp(..) => (),
                StoreError::AccessSchema22NoOp(..) => (),
                StoreError::UnsafeSchema22NoOpPath(..) => (),
                StoreError::Schema22NoOpNotFound => (),
                StoreError::Schema22PairPublicationRequired(..) => (),
                StoreError::InvalidSchema22Pair(..) => (),
                StoreError::Schema22SnapshotConflict { .. } => (),
                StoreError::PhysicalV3AdmissionInvalid => (),
                StoreError::PhysicalV3Pack(..) => (),
                StoreError::PhysicalV3SecondHeadUnsupported(..) => (),
                StoreError::PhysicalV3AdmissionConflict => (),
                StoreError::PhysicalV3RetentionInvalid => (),
                StoreError::ImportGeneration(..) => (),
                StoreError::InvalidImportGeneration => (),
                StoreError::ImportGenerationNotFound => (),
                StoreError::ImportGenerationConflict => (),
                StoreError::LineageCatalog(..) => (),
                StoreError::LineageCatalogConflict => (),
                StoreError::LineageCompactionUnavailable => (),
                StoreError::LineageCapacityExhausted => (),
                StoreError::RetiredLineageClock(..) => (),
                StoreError::RetiredLineageClockOverflow => (),
                StoreError::ImmutableBaseBindingMissing(..) => (),
                StoreError::SourceDbImportInventoryMissing(..) => (),
                StoreError::SourceDbImportProjectionStateMissing(..) => (),
                StoreError::SourceDbLegacyDirectRebaseRequired(..) => (),
                StoreError::SourceDbProjectionState(..) => (),
                StoreError::SyncMutation(..) => (),
                StoreError::LineagePackNotReady => (),
                StoreError::LineagePackDigestMismatch => (),
                StoreError::OpenLineagePack(..) => (),
                StoreError::DecodeLineagePack(..) => (),
                StoreError::CreateLineagePackInspection { .. } => (),
                StoreError::SyncLineagePackInspection(..) => (),
                StoreError::SerializeObservation(..) => (),
                StoreError::Manifest(..) => (),
                StoreError::SequenceTooLarge => (),
                StoreError::SequenceExhausted => (),
                StoreError::InvalidStoredSequence => (),
                StoreError::StaleManifest { .. } => (),
                StoreError::PackSizeTooLarge => (),
                StoreError::PackOrdinalTooLarge => (),
                StoreError::UnsafeStoredPackPath => (),
                StoreError::InspectCatalogPack { .. } => (),
                StoreError::CatalogPackNotRegular { .. } => (),
                StoreError::CatalogPackSizeMismatch { .. } => (),
                StoreError::CatalogPackDigestMismatch { .. } => (),
                StoreError::EmptyIdentity(..) => (),
                StoreError::IdentityTooLong { .. } => (),
                StoreError::IdentityControlCharacter(..) => (),
                StoreError::InvalidSourceKind => (),
                StoreError::NegativeTimestamp(..) => (),
                StoreError::InvalidPairingExpiry => (),
                StoreError::PairingRejected => (),
                StoreError::PairingVehicleUnavailable(..) => (),
                StoreError::EmptyPairingScope => (),
                StoreError::InvalidDeviceMaxLifetime => (),
                StoreError::SealRotationSuccessor => (),
                StoreError::NilSourceId => (),
                StoreError::NilVehicleId => (),
                StoreError::ObservationMustBeObject => (),
                StoreError::InvalidCurrentDiscovery => (),
                StoreError::ObservationTooLarge { .. } => (),
                StoreError::ObservationMissingAfterInsert => (),
                StoreError::InvalidObservationQueryLimit { .. } => (),
                StoreError::InvalidObservationQueryRange => (),
                StoreError::UnknownSource(..) => (),
                StoreError::UnknownVehicle(..) => (),
                StoreError::InvalidCarSettings => (),
                StoreError::InvalidDriveId => (),
                StoreError::InvalidDriveQueryLimit(..) => (),
                StoreError::InvalidDriveQueryRange => (),
                StoreError::InvalidDriveQueryCursor => (),
                StoreError::InvalidDriveProjectionIdentity => (),
                StoreError::InvalidDrivePositionPageLimit(..) => (),
                StoreError::InvalidPublicPageQuery(..) => (),
                StoreError::InvalidPublicProjectionIdentity(..) => (),
                StoreError::InvalidChargeId => (),
                StoreError::InvalidChargeCost => (),
                StoreError::UnknownCharge(..) => (),
                StoreError::ChargeCostBasisUnavailable { .. } => (),
                StoreError::InvalidGeofence => (),
                StoreError::GeofenceRejected(..) => (),
                StoreError::UnknownGeofence(..) => (),
                StoreError::VehicleSourceMismatch { .. } => (),
                StoreError::VehicleIdentityMismatch { .. } => (),
                StoreError::InvalidStoredUuid(..) => (),
                StoreError::InvalidStoredGeneration => (),
                StoreError::InvalidStoredCount => (),
                StoreError::QuarantinedLifecycle(..) => (),
                StoreError::UnsupportedSchema(..) => (),
                StoreError::InvalidApplicationId(..) => (),
                StoreError::Integrity(..) => (),
                StoreError::InvalidLifecycleCarId => (),
                StoreError::InvalidLifecycleCursor => (),
                StoreError::InvalidLifecycleSession => (),
                StoreError::LifecycleWrite(..) => (),
                StoreError::ReceiverState(..) => (),
                StoreError::InvalidEventRecord => (),
                StoreError::MigrationClock => (),
                StoreError::LifecycleProjection(..) => (),
                StoreError::InjectedStreamFault(..) => (),
                #[cfg(test)]
                StoreError::InjectedProjectionStateDetachFault => (),
                StoreError::SerializeLifecycleRow(..) => (),
                StoreError::DeserializeLifecycleRow(..) => (),
                StoreError::OutboundRequestReceipt(..) => (),
                StoreError::InvalidOutboundRequestReceiptId => (),
                StoreError::OutboundRequestReceiptNotStarted => (),
                StoreError::ReservedCredentialRefreshReceipt => (),
                StoreError::NilOutboundRequestCorrelationId => (),
                StoreError::InvalidOutboundRequestVehicleId => (),
                StoreError::InvalidVehicleDataAuditPrecondition => (),
                StoreError::InvalidVehicleActionAudit => (),
                StoreError::OutboundRequestClock(..) => (),
                StoreError::OutboundRequestClockOverflow => (),
                StoreError::InvalidOutboundRequestHttpStatus => (),
                StoreError::InvalidOutboundRequestRetryAfter => (),
                StoreError::InvalidOutboundRequestWatermark => (),
                StoreError::InvalidStreamAuditWindow => (),
                StoreError::InvalidOutboundRequestQueryLimit { .. } => (),
                StoreError::OutboundRequestAuditCapacityExhausted => (),
                StoreError::StreamSessionReceipt(..) => (),
                StoreError::InvalidStreamSessionReceiptId => (),
                StoreError::StreamSessionReceiptNotStarted => (),
                StoreError::StreamSessionUnsubscribeNotCompleted => (),
                StoreError::StreamSessionAuditCapacityExhausted => (),
            }
            assert_name_free(&error);
        }
        check_store_error(StoreError::EntropyUnavailable);
        check_store_error(StoreError::CreateDataDir(example()));
        check_store_error(StoreError::CreatePacksDir(example()));
        check_store_error(StoreError::ProtectDataDir(example()));
        check_store_error(StoreError::ProtectPacksDir(example()));
        check_store_error(StoreError::UnsafeDataDir(example()));
        check_store_error(StoreError::UnsafePacksDir(example()));
        check_store_error(StoreError::CreateSharedSqlite(example()));
        check_store_error(StoreError::InspectSharedSqlite(example()));
        check_store_error(StoreError::ProtectSharedSqlite(example()));
        check_store_error(StoreError::UnsafeSharedSqlite(example()));
        check_store_error(StoreError::CreateImportSpool(example()));
        check_store_error(StoreError::InspectImportSpool(example()));
        check_store_error(StoreError::UnsafeImportSpool(example()));
        check_store_error(StoreError::Open(example()));
        check_store_error(StoreError::InspectCatalogue(example()));
        check_store_error(StoreError::ReadCatalogue(example()));
        check_store_error(StoreError::ResolveCataloguePath(example()));
        check_store_error(StoreError::InvalidCataloguePath);
        check_store_error(StoreError::PendingCatalogueWal);
        check_store_error(StoreError::CatalogueCheckpoint(example()));
        check_store_error(StoreError::CatalogueCheckpointIncomplete);
        check_store_error(StoreError::ImmutableSnapshotRequired);
        check_store_error(StoreError::CatalogueChangedDuringImmutableCheck);
        check_store_error(StoreError::Configure(example()));
        check_store_error(StoreError::CredentialPairInvalid);
        check_store_error(StoreError::CredentialPairStore(example()));
        check_store_error(StoreError::CredentialCommit(example()));
        check_store_error(StoreError::CredentialRefreshReceipt(example()));
        check_store_error(StoreError::RefreshOutcomeUnknown {
            reason: BeginRefusal::RowChanged,
        });
        check_store_error(StoreError::RefreshTokenSpent);
        check_store_error(StoreError::FreshSignInRequired);
        check_store_error(StoreError::SlotOccupied);
        check_store_error(StoreError::InvalidOrigin);
        check_store_error(StoreError::SignInBindingInvalid);
        check_store_error(StoreError::SignInExpired);
        check_store_error(StoreError::CredentialConnectionTaken);
        check_store_error(StoreError::CredentialEnvelope(
            crate::auth::credential_envelope::EnvelopeError::Truncated,
        ));
        check_store_error(StoreError::InvalidAddressCache);
        check_store_error(StoreError::AddressCacheWrite(example()));
        check_store_error(StoreError::InvalidAddressEnrichment);
        check_store_error(StoreError::InvalidTerrainTileDigest);
        check_store_error(StoreError::AddressEnrichmentWrite(example()));
        check_store_error(StoreError::Backup(example()));
        check_store_error(StoreError::BackupDestinationExists(example()));
        check_store_error(StoreError::BackupDestinationIsLiveDatabase);
        check_store_error(StoreError::CreateBackupDirectory(example()));
        check_store_error(StoreError::BackupCapacityOverflow);
        check_store_error(StoreError::CopyBackupPack {
            source_path: example(),
            destination: example(),
            source_error: example(),
        });
        check_store_error(StoreError::BackupPackSizeMismatch {
            path: example(),
            expected: example(),
            actual: example(),
        });
        check_store_error(StoreError::OpenBackupPack(example()));
        check_store_error(StoreError::ReadBackupPack(example()));
        check_store_error(StoreError::BackupPackDigestMismatch { path: example() });
        check_store_error(StoreError::Migrate(example()));
        check_store_error(StoreError::UnmatchedImportSource {
            stored_keys: example(),
        });
        check_store_error(StoreError::UnmatchedCurrentImportSource {
            stored_keys: example(),
        });
        check_store_error(StoreError::VehicleIdentityTidy(example()));
        check_store_error(StoreError::Query(example()));
        check_store_error(StoreError::Begin(example()));
        check_store_error(StoreError::SupervisedCollectorLeaseWrite(example()));
        check_store_error(StoreError::SupervisedCollectorLeaseHeld);
        check_store_error(StoreError::SupervisedCollectorLeaseLost);
        check_store_error(StoreError::SupervisedCollectorClockOverflow);
        check_store_error(StoreError::OpenPublicationGate(example()));
        check_store_error(StoreError::ProtectPublicationGate(example()));
        check_store_error(StoreError::UnsafePublicationGate(example()));
        check_store_error(StoreError::LockPublicationGate(example()));
        check_store_error(StoreError::PublicationGateBusy);
        check_store_error(StoreError::PublishManifest(example()));
        check_store_error(StoreError::CatalogueDurability(example()));
        check_store_error(StoreError::AmbiguousCatalogueCommit {
            vehicle_id: example(),
            snapshot_id: example(),
        });
        check_store_error(StoreError::UnsafeUnpublishedPackPath(example()));
        check_store_error(StoreError::CleanupUnpublishedPack {
            path: example(),
            source: example(),
        });
        check_store_error(StoreError::UnpublishedPackDigestMismatch(example()));
        check_store_error(StoreError::FingerprintManifestMissing(example()));
        check_store_error(StoreError::ImportDeltaRequiresBaseBinding);
        check_store_error(StoreError::InvalidVehicleId);
        check_store_error(StoreError::InvalidVehicleIdentity);
        check_store_error(StoreError::VehicleIdentityConflict);
        check_store_error(StoreError::InvalidSourceId);
        check_store_error(StoreError::TerrainCarMissing(example()));
        check_store_error(StoreError::WriteRefused(example()));
        check_store_error(StoreError::TerrainPack(example()));
        check_store_error(StoreError::PackStartupRepair(example()));
        check_store_error(StoreError::RegisterSource(example()));
        check_store_error(StoreError::RegisterVehicle(example()));
        check_store_error(StoreError::CreatePairing(example()));
        check_store_error(StoreError::RevokePairing(example()));
        check_store_error(StoreError::ClaimPairing(example()));
        check_store_error(StoreError::InvalidStoredAuthStatus);
        check_store_error(StoreError::EffectiveCollectorAuthUnavailable);
        check_store_error(StoreError::RotateDevice(example()));
        check_store_error(StoreError::RevokeDevice(example()));
        check_store_error(StoreError::AppendObservation(example()));
        check_store_error(StoreError::InstallationIdentity(example()));
        check_store_error(StoreError::SerializeManifest(example()));
        check_store_error(StoreError::DeserializeManifest(example()));
        check_store_error(StoreError::SchemaPublicationUnavailable(example()));
        check_store_error(StoreError::WriteSchema22NoOp(example()));
        check_store_error(StoreError::ReadSchema22NoOp(example()));
        check_store_error(StoreError::AccessSchema22NoOp(example()));
        check_store_error(StoreError::UnsafeSchema22NoOpPath(example()));
        check_store_error(StoreError::Schema22NoOpNotFound);
        check_store_error(StoreError::Schema22PairPublicationRequired(example()));
        check_store_error(StoreError::InvalidSchema22Pair(example()));
        check_store_error(StoreError::Schema22SnapshotConflict {
            vehicle_id: example(),
            snapshot_id: example(),
        });
        check_store_error(StoreError::PhysicalV3AdmissionInvalid);
        check_store_error(StoreError::PhysicalV3Pack(example()));
        check_store_error(StoreError::PhysicalV3SecondHeadUnsupported(example()));
        check_store_error(StoreError::PhysicalV3AdmissionConflict);
        check_store_error(StoreError::PhysicalV3RetentionInvalid);
        check_store_error(StoreError::ImportGeneration(example()));
        check_store_error(StoreError::InvalidImportGeneration);
        check_store_error(StoreError::ImportGenerationNotFound);
        check_store_error(StoreError::ImportGenerationConflict);
        check_store_error(StoreError::LineageCatalog(example()));
        check_store_error(StoreError::LineageCatalogConflict);
        check_store_error(StoreError::LineageCompactionUnavailable);
        check_store_error(StoreError::LineageCapacityExhausted);
        check_store_error(StoreError::RetiredLineageClock(example()));
        check_store_error(StoreError::RetiredLineageClockOverflow);
        check_store_error(StoreError::ImmutableBaseBindingMissing(example()));
        check_store_error(StoreError::SourceDbImportInventoryMissing(example()));
        check_store_error(StoreError::SourceDbImportProjectionStateMissing(example()));
        check_store_error(StoreError::SourceDbLegacyDirectRebaseRequired(example()));
        check_store_error(StoreError::SourceDbProjectionState(example()));
        check_store_error(StoreError::SyncMutation(example()));
        check_store_error(StoreError::LineagePackNotReady);
        check_store_error(StoreError::LineagePackDigestMismatch);
        check_store_error(StoreError::OpenLineagePack(example()));
        check_store_error(StoreError::DecodeLineagePack(example()));
        check_store_error(StoreError::CreateLineagePackInspection {
            path: example(),
            source: example(),
        });
        check_store_error(StoreError::SyncLineagePackInspection(example()));
        check_store_error(StoreError::SerializeObservation(example()));
        check_store_error(StoreError::Manifest(example()));
        check_store_error(StoreError::SequenceTooLarge);
        check_store_error(StoreError::SequenceExhausted);
        check_store_error(StoreError::InvalidStoredSequence);
        check_store_error(StoreError::StaleManifest {
            vehicle_id: example(),
            attempted: example(),
            current: example(),
        });
        check_store_error(StoreError::PackSizeTooLarge);
        check_store_error(StoreError::PackOrdinalTooLarge);
        check_store_error(StoreError::UnsafeStoredPackPath);
        check_store_error(StoreError::InspectCatalogPack {
            path: example(),
            source: example(),
        });
        check_store_error(StoreError::CatalogPackNotRegular { path: example() });
        check_store_error(StoreError::CatalogPackSizeMismatch {
            path: example(),
            expected: example(),
            actual: example(),
        });
        check_store_error(StoreError::CatalogPackDigestMismatch { path: example() });
        check_store_error(StoreError::EmptyIdentity(example()));
        check_store_error(StoreError::IdentityTooLong {
            field: example(),
            actual: example(),
            maximum: example(),
        });
        check_store_error(StoreError::IdentityControlCharacter(example()));
        check_store_error(StoreError::InvalidSourceKind);
        check_store_error(StoreError::NegativeTimestamp(example()));
        check_store_error(StoreError::InvalidPairingExpiry);
        check_store_error(StoreError::PairingRejected);
        check_store_error(StoreError::PairingVehicleUnavailable(example()));
        check_store_error(StoreError::EmptyPairingScope);
        check_store_error(StoreError::InvalidDeviceMaxLifetime);
        check_store_error(StoreError::SealRotationSuccessor);
        check_store_error(StoreError::NilSourceId);
        check_store_error(StoreError::NilVehicleId);
        check_store_error(StoreError::ObservationMustBeObject);
        check_store_error(StoreError::InvalidCurrentDiscovery);
        check_store_error(StoreError::ObservationTooLarge {
            actual: example(),
            maximum: example(),
        });
        check_store_error(StoreError::ObservationMissingAfterInsert);
        check_store_error(StoreError::InvalidObservationQueryLimit {
            actual: example(),
            maximum: example(),
        });
        check_store_error(StoreError::InvalidObservationQueryRange);
        check_store_error(StoreError::UnknownSource(example()));
        check_store_error(StoreError::UnknownVehicle(example()));
        check_store_error(StoreError::InvalidCarSettings);
        check_store_error(StoreError::InvalidDriveId);
        check_store_error(StoreError::InvalidDriveQueryLimit(example()));
        check_store_error(StoreError::InvalidDriveQueryRange);
        check_store_error(StoreError::InvalidDriveQueryCursor);
        check_store_error(StoreError::InvalidDriveProjectionIdentity);
        check_store_error(StoreError::InvalidDrivePositionPageLimit(example()));
        check_store_error(StoreError::InvalidPublicPageQuery(example()));
        check_store_error(StoreError::InvalidPublicProjectionIdentity(example()));
        check_store_error(StoreError::InvalidChargeId);
        check_store_error(StoreError::InvalidChargeCost);
        check_store_error(StoreError::UnknownCharge(example()));
        check_store_error(StoreError::ChargeCostBasisUnavailable {
            charge_id: example(),
            mode: example(),
        });
        check_store_error(StoreError::InvalidGeofence);
        check_store_error(StoreError::GeofenceRejected(example()));
        check_store_error(StoreError::UnknownGeofence(example()));
        check_store_error(StoreError::VehicleSourceMismatch {
            vehicle_id: example(),
            source_id: example(),
        });
        check_store_error(StoreError::VehicleIdentityMismatch {
            expected: example(),
            actual: example(),
        });
        check_store_error(StoreError::InvalidStoredUuid(example()));
        check_store_error(StoreError::InvalidStoredGeneration);
        check_store_error(StoreError::InvalidStoredCount);
        check_store_error(StoreError::QuarantinedLifecycle(example()));
        check_store_error(StoreError::UnsupportedSchema(example()));
        check_store_error(StoreError::InvalidApplicationId(example()));
        check_store_error(StoreError::Integrity(example()));
        check_store_error(StoreError::InvalidLifecycleCarId);
        check_store_error(StoreError::InvalidLifecycleCursor);
        check_store_error(StoreError::InvalidLifecycleSession);
        check_store_error(StoreError::LifecycleWrite(example()));
        check_store_error(StoreError::ReceiverState(example()));
        check_store_error(StoreError::InvalidEventRecord);
        check_store_error(StoreError::MigrationClock);
        check_store_error(StoreError::LifecycleProjection(example()));
        check_store_error(StoreError::InjectedStreamFault(example()));
        #[cfg(test)]
        check_store_error(StoreError::InjectedProjectionStateDetachFault);
        check_store_error(StoreError::SerializeLifecycleRow(example()));
        check_store_error(StoreError::DeserializeLifecycleRow(example()));
        check_store_error(StoreError::OutboundRequestReceipt(example()));
        check_store_error(StoreError::InvalidOutboundRequestReceiptId);
        check_store_error(StoreError::OutboundRequestReceiptNotStarted);
        check_store_error(StoreError::ReservedCredentialRefreshReceipt);
        check_store_error(StoreError::NilOutboundRequestCorrelationId);
        check_store_error(StoreError::InvalidOutboundRequestVehicleId);
        check_store_error(StoreError::InvalidVehicleDataAuditPrecondition);
        check_store_error(StoreError::InvalidVehicleActionAudit);
        check_store_error(StoreError::OutboundRequestClock(example()));
        check_store_error(StoreError::OutboundRequestClockOverflow);
        check_store_error(StoreError::InvalidOutboundRequestHttpStatus);
        check_store_error(StoreError::InvalidOutboundRequestRetryAfter);
        check_store_error(StoreError::InvalidOutboundRequestWatermark);
        check_store_error(StoreError::InvalidStreamAuditWindow);
        check_store_error(StoreError::InvalidOutboundRequestQueryLimit {
            actual: example(),
            maximum: example(),
        });
        check_store_error(StoreError::OutboundRequestAuditCapacityExhausted);
        check_store_error(StoreError::StreamSessionReceipt(example()));
        check_store_error(StoreError::InvalidStreamSessionReceiptId);
        check_store_error(StoreError::StreamSessionReceiptNotStarted);
        check_store_error(StoreError::StreamSessionUnsubscribeNotCompleted);
        check_store_error(StoreError::StreamSessionAuditCapacityExhausted);
        fn check_observation_verification_error(error: ObservationVerificationError) {
            match &error {
                ObservationVerificationError::InvalidSourceCarId => (),
                ObservationVerificationError::InvalidWatermark => (),
                ObservationVerificationError::NoVehicleMapping => (),
                ObservationVerificationError::AmbiguousVehicleMapping => (),
                ObservationVerificationError::Store(..) => (),
            }
            assert_name_free(&error);
        }
        check_observation_verification_error(ObservationVerificationError::InvalidSourceCarId);
        check_observation_verification_error(ObservationVerificationError::InvalidWatermark);
        check_observation_verification_error(ObservationVerificationError::NoVehicleMapping);
        check_observation_verification_error(ObservationVerificationError::AmbiguousVehicleMapping);
        check_observation_verification_error(ObservationVerificationError::Store(example()));
        fn check_no_wake_verification_error(error: NoWakeVerificationError) {
            match &error {
                NoWakeVerificationError::InvalidAuditWatermark => (),
                NoWakeVerificationError::Store(..) => (),
                NoWakeVerificationError::Observation(..) => (),
            }
            assert_name_free(&error);
        }
        check_no_wake_verification_error(NoWakeVerificationError::InvalidAuditWatermark);
        check_no_wake_verification_error(NoWakeVerificationError::Store(example()));
        check_no_wake_verification_error(NoWakeVerificationError::Observation(example()));
    }
}
