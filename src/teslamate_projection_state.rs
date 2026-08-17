//! Bounded, private projection-state capture for TeslaMate imports.
//!
//! A full source history can contain millions of facts.  This module retains
//! only a digest for every current projected row, and retains canonical JSON
//! only for rows which are already known to be new or changed.  That lets the
//! importer build a sparse typed successor without materialising a history or
//! duplicating every payload in the durable Hub catalogue.

use std::{
    collections::HashSet,
    error::Error,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, params, params_from_iter};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fstat, open, openat, statat, unlinkat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{
    hub_pack::{
        ProjectionCar, ProjectionCharge, ProjectionChargeSample, ProjectionDrive,
        ProjectionPosition, ProjectionState, ProjectionTombstone, ProjectionUpdate,
    },
    protocol::Sha256Digest,
};

/// The hard cap used by the production reader unless a caller deliberately
/// supplies a narrower budget.
pub const DEFAULT_MAX_ROWS: u64 = 20_000_000;
pub const DEFAULT_MAX_STATE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const DEFAULT_MINIMUM_FREE_BYTES: u64 = 512 * 1024 * 1024;
/// A source row larger than this needs an explicit, narrower-or-wider caller
/// contract.  The production direct importer uses this exact value for both
/// durable retention and one decoded successor page.
pub const DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_PAGE_SIZE: u32 = 10_000;

const STAGING_DIRECTORY: &str = ".projection-state";
const IMPORT_GENERATION_NAMESPACE: &str = "v1";
const OWNER_FILE_NAME: &str = "owner.json";
const OWNER_SCHEMA: u8 = 1;
const OWNER_KIND: &str = "teslatlas-hub/teslamate-projection-state/v1";
const STATE_FILE_EXTENSION: &str = "sqlite";
const SQLITE_JOURNAL_SUFFIX: &str = ".sqlite-journal";
const SQLITE_WAL_SUFFIX: &str = ".sqlite-wal";
const SQLITE_SHM_SUFFIX: &str = ".sqlite-shm";
const MIN_STATE_BYTES: u64 = 64 * 1024;
const DIGEST_DOMAIN: &[u8] = b"teslatlas-hub/teslamate-projection-state/v1";
const TRANSFER_DIGEST_DOMAIN: &[u8] =
    b"teslatlas-hub/teslamate-projection-state/sealed-transfer/v1";

/// Fixed schema name used only while copying a sealed source-state spool into
/// the Hub catalogue. It is deliberately not caller-controlled.
pub(crate) const TESLAMATE_PROJECTION_STATE_ATTACHMENT_SCHEMA: &str =
    "teslamate_projection_state_spool";

// DELETE/FULL is deliberate: a sealed state file is the source of truth for a
// sparse successor and must survive a host crash.  Committing each source row
// under that durability policy is prohibitively expensive, however.  Keep one
// short, fixed-size transaction open instead.  This caps both recovery work
// and the amount of unwritten state without retaining an unbounded history in
// memory. Changed payloads have a byte cap too, so a dense changed-history
// pass does not turn one row-count batch into a multi-gigabyte commit.
const WRITE_BATCH_ROWS: u32 = 8_192;
const WRITE_BATCH_CHANGED_PAYLOAD_BYTES: u64 = DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES;
// Keep dynamic `VALUES` lookups well below SQLite's conservative 999-bind
// build-time limit.  Each requested changed row consumes two bind values.
const CHANGED_PAGE_PAYLOAD_LOOKUP_ROWS: usize = 250;
const MAX_OWNER_MARKER_BYTES: u64 = 1_024;

/// A run/attempt namespace created only by the Hub's gated production
/// constructor.  It makes an interrupted direct import recoverable without
/// guessing whether an older flat spool belongs to a current generation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TeslaMateProjectionStateImportOwnership {
    namespace_root: PathBuf,
    run_id: Uuid,
    attempt_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TeslaMateProjectionStateOwnership {
    Generic,
    ImportGeneration(TeslaMateProjectionStateImportOwnership),
}

/// The fixed, deliberately boring ownership record written once for each
/// direct-import generation.  `deny_unknown_fields` prevents recovery from
/// treating an unrelated private file as an owned spool marker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TeslaMateProjectionStateOwner {
    schema: u8,
    kind: String,
    run_id: String,
}

/// A fully validated stale run which can be reclaimed using only names that
/// were enumerated relative to its already-open namespace directory.
#[derive(Debug)]
struct ValidatedStaleImportRun {
    run_id: Uuid,
    directory_name: String,
    children: Vec<String>,
}

/// The seven typed facts whose identity is owned by a TeslaMate source import.
/// The `car` row participates in change detection but is never tombstoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum TeslaMateProjectionStateEntity {
    Car = 0,
    Drive = 1,
    Position = 2,
    Charge = 3,
    ChargeSample = 4,
    State = 5,
    Update = 6,
}

impl TeslaMateProjectionStateEntity {
    pub const ALL: [Self; 7] = [
        Self::Car,
        Self::Drive,
        Self::Position,
        Self::Charge,
        Self::ChargeSample,
        Self::State,
        Self::Update,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Car => "car",
            Self::Drive => "drive",
            Self::Position => "position",
            Self::Charge => "charge",
            Self::ChargeSample => "charge_sample",
            Self::State => "state",
            Self::Update => "update",
        }
    }

    pub const fn ordinal(self) -> u8 {
        self as u8
    }

    pub const fn tombstone_allowed(self) -> bool {
        !matches!(self, Self::Car)
    }

    pub fn from_ordinal(value: i64) -> Result<Self, TeslaMateProjectionStateError> {
        match value {
            0 => Ok(Self::Car),
            1 => Ok(Self::Drive),
            2 => Ok(Self::Position),
            3 => Ok(Self::Charge),
            4 => Ok(Self::ChargeSample),
            5 => Ok(Self::State),
            6 => Ok(Self::Update),
            _ => Err(TeslaMateProjectionStateError::InvalidStoredEntity(value)),
        }
    }
}

impl std::str::FromStr for TeslaMateProjectionStateEntity {
    type Err = TeslaMateProjectionStateError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "car" => Ok(Self::Car),
            "drive" => Ok(Self::Drive),
            "position" => Ok(Self::Position),
            "charge" => Ok(Self::Charge),
            "charge_sample" => Ok(Self::ChargeSample),
            "state" => Ok(Self::State),
            "update" => Ok(Self::Update),
            _ => Err(TeslaMateProjectionStateError::InvalidStoredEntityName(
                value.to_owned(),
            )),
        }
    }
}

/// Keyset cursor for the canonical entity/id ordering.  `None` starts before
/// the first car row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeslaMateProjectionStateCursor {
    pub entity: TeslaMateProjectionStateEntity,
    pub id: i64,
}

/// One durable comparison fact.  Its payload deliberately is not retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateProjectionStateDigestRow {
    pub entity: TeslaMateProjectionStateEntity,
    pub id: i64,
    pub car_id: i64,
    pub digest: Sha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateProjectionStateDigestPage {
    pub rows: Vec<TeslaMateProjectionStateDigestRow>,
    pub next_after: Option<TeslaMateProjectionStateCursor>,
}

/// A payload retained only when a verified prior-state lookup says it must be
/// sent to a successor pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateProjectionStateChangedRow {
    pub state: TeslaMateProjectionStateDigestRow,
    pub canonical_payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateProjectionStateChangedPage {
    pub rows: Vec<TeslaMateProjectionStateChangedRow>,
    pub next_after: Option<TeslaMateProjectionStateCursor>,
}

/// Metadata is deliberately separate from the full canonical payload.  A
/// changed-page scan may inspect at most `MAX_PAGE_SIZE + 1` such records to
/// decide a byte-bound page, but it must not pull payload JSON until the
/// cumulative byte cap has already been enforced.
#[derive(Debug)]
struct TeslaMateProjectionStateChangedMetadata {
    state: TeslaMateProjectionStateDigestRow,
    payload_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeslaMateProjectionStateLimits {
    pub max_rows: u64,
    pub max_state_bytes: u64,
    pub max_changed_payload_bytes: u64,
    pub minimum_free_bytes: u64,
}

impl Default for TeslaMateProjectionStateLimits {
    fn default() -> Self {
        Self {
            max_rows: DEFAULT_MAX_ROWS,
            max_state_bytes: DEFAULT_MAX_STATE_BYTES,
            max_changed_payload_bytes: DEFAULT_MAX_STATE_BYTES,
            minimum_free_bytes: DEFAULT_MINIMUM_FREE_BYTES,
        }
    }
}

impl TeslaMateProjectionStateLimits {
    pub fn validate(self) -> Result<(), TeslaMateProjectionStateError> {
        if self.max_rows == 0 {
            return Err(TeslaMateProjectionStateError::InvalidMaximumRows);
        }
        if self.max_state_bytes < MIN_STATE_BYTES {
            return Err(TeslaMateProjectionStateError::InvalidMaximumStateBytes {
                minimum: MIN_STATE_BYTES,
            });
        }
        if self.max_changed_payload_bytes == 0 {
            return Err(TeslaMateProjectionStateError::InvalidMaximumChangedPayloadBytes);
        }
        if self.max_changed_payload_bytes > self.max_state_bytes {
            return Err(TeslaMateProjectionStateError::ChangedPayloadBudgetExceedsStateCapacity);
        }
        let page_count = self.max_state_bytes / 4096;
        if page_count == 0 || page_count > i64::MAX as u64 {
            return Err(TeslaMateProjectionStateError::StateCapacityOverflow);
        }
        self.max_state_bytes
            .checked_add(self.minimum_free_bytes)
            .ok_or(TeslaMateProjectionStateError::StateCapacityOverflow)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeslaMateProjectionStateStats {
    pub row_count: u64,
    pub changed_row_count: u64,
    pub changed_payload_bytes: u64,
    pub sealed: bool,
}

/// A verified, short-lived handle for a sealed state spool.  It exposes only
/// the canonical private path and a read-only SQLite URI; callers must
/// revalidate the attachment before copying from it.  The descriptor contains
/// no mutable state and does not alter spool cleanup ownership.
#[derive(Debug, Clone)]
pub(crate) struct TeslaMateProjectionStateTransfer {
    path: PathBuf,
    stats: TeslaMateProjectionStateStats,
    selected_car_id: i64,
    semantic_digest: Sha256Digest,
    ownership: TeslaMateProjectionStateOwnership,
}

impl TeslaMateProjectionStateTransfer {
    /// The canonical, validated spool path.  It is crate-scoped for the
    /// attachment/recovery tests; production callers must use the run-bound
    /// descriptor constructor below rather than selecting a path themselves.
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn stats(&self) -> TeslaMateProjectionStateStats {
        self.stats
    }

    /// A SQLite URI which can only be opened read-only.  The descriptor is
    /// created from a sealed private file; callers still validate the attached
    /// database before using it because a path can be substituted between
    /// inspection and attachment.
    pub(crate) fn read_only_attachment_uri(&self) -> Result<String, TeslaMateProjectionStateError> {
        self.validate_current_path()?;
        let mut uri = Url::from_file_path(&self.path)
            .map_err(|_| TeslaMateProjectionStateError::InvalidTransferPath(self.path.clone()))?;
        uri.query_pairs_mut()
            .append_pair("mode", "ro")
            .append_pair("cache", "private");
        Ok(uri.into())
    }

    /// Authenticate the already attached read-only source against the sealed
    /// descriptor.  This repeats all shape/accounting checks and compares a
    /// row-order-bound digest, so a malformed or substituted file never feeds
    /// a catalogue transaction.
    pub(crate) fn validate_attached(
        &self,
        connection: &Connection,
    ) -> Result<(), TeslaMateProjectionStateError> {
        let canonical_path = self.validate_current_path()?;
        if canonical_path != self.path {
            return Err(TeslaMateProjectionStateError::TransferPathChanged {
                expected: self.path.clone(),
                actual: canonical_path,
            });
        }
        let attached_path = attached_transfer_path(connection)?;
        if attached_path != self.path {
            return Err(
                TeslaMateProjectionStateError::TransferAttachmentPathChanged {
                    expected: self.path.clone(),
                    actual: attached_path,
                },
            );
        }
        validate_transfer_database(
            connection,
            TESLAMATE_PROJECTION_STATE_ATTACHMENT_SCHEMA,
            self.stats,
            self.selected_car_id,
        )?;
        let actual =
            transfer_semantic_digest(connection, TESLAMATE_PROJECTION_STATE_ATTACHMENT_SCHEMA)?;
        if actual != self.semantic_digest {
            return Err(TeslaMateProjectionStateError::TransferDigestMismatch);
        }
        Ok(())
    }

    fn validate_current_path(&self) -> Result<PathBuf, TeslaMateProjectionStateError> {
        match &self.ownership {
            TeslaMateProjectionStateOwnership::Generic => {
                validate_private_transfer_path(&self.path)
            }
            TeslaMateProjectionStateOwnership::ImportGeneration(ownership) => {
                validate_import_generation_transfer_path(&self.path, ownership)
            }
        }
    }
}

/// Result returned by [`TeslaMateProjectionState::record_if_changed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeslaMateProjectionStateChange {
    /// The row is part of a base snapshot and only its digest was retained.
    CapturedDigestOnly,
    Unchanged,
    NewOrChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeslaMateProjectionStateCaptureMode {
    /// Initial immutable-base capture. Full-pack construction already owns the
    /// payload, so this spool records only comparison digests.
    InitialBase,
    /// A successor capture with verified durable prior state. It retains
    /// payload only for new or digest-changed rows.
    Successor,
}

/// A bounded lookup of the immediately preceding, verified source-owned
/// projection state.  Implementors must bind the lookup to one source,
/// vehicle, selected car, immutable base, and current lineage head before
/// exposing it to a capture.
pub trait PriorProjectionStateLookup {
    fn digest(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    ) -> Result<Option<Sha256Digest>, Box<dyn Error + Send + Sync>>;

    fn page_after(
        &mut self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateDigestPage, Box<dyn Error + Send + Sync>>;
}

/// A private current-run index.  It records every current digest, but retains
/// payload JSON only in `changed_rows`, which is bounded independently.
pub struct TeslaMateProjectionState {
    path: PathBuf,
    connection: Connection,
    ownership: TeslaMateProjectionStateOwnership,
    limits: TeslaMateProjectionStateLimits,
    maximum_changed_row_payload_bytes: u64,
    row_count: u64,
    changed_row_count: u64,
    changed_payload_bytes: u64,
    // Accounting accepted by the current SQLite transaction but not yet
    // committed.  It is deliberately scalar-only: source payloads are passed
    // directly to SQLite and no history-sized write queue is retained here.
    pending_row_count: u64,
    pending_changed_row_count: u64,
    pending_changed_payload_bytes: u64,
    pending_write_rows: u32,
    write_transaction_open: bool,
    write_failed: bool,
    sealed: bool,
    cleanup_on_drop: bool,
}

impl std::fmt::Debug for TeslaMateProjectionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TeslaMateProjectionState")
            .field("path", &self.path)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl TeslaMateProjectionState {
    /// Create a new state file below a private Hub-owned directory.  The file
    /// is automatically removed on drop unless [`Self::discard`] is used
    /// explicitly first.
    pub fn create(
        root: impl AsRef<Path>,
        limits: TeslaMateProjectionStateLimits,
    ) -> Result<Self, TeslaMateProjectionStateError> {
        Self::create_with_changed_payload_row_limit(
            root,
            limits,
            DEFAULT_MAX_CHANGED_ROW_PAYLOAD_BYTES,
        )
    }

    /// Create a state spool with an explicit per-row retained-payload cap.
    /// The cap is enforced before the row is inserted into either durable
    /// table, and it is also the default cap for [`Self::changed_page`].
    pub fn create_with_changed_payload_row_limit(
        root: impl AsRef<Path>,
        limits: TeslaMateProjectionStateLimits,
        maximum_changed_row_payload_bytes: u64,
    ) -> Result<Self, TeslaMateProjectionStateError> {
        limits.validate()?;
        validate_changed_row_payload_limit(maximum_changed_row_payload_bytes)?;
        let root = root.as_ref();
        ensure_existing_directory(root)?;
        let staging = root.join(STAGING_DIRECTORY);
        ensure_private_directory(&staging)?;
        let path = staging.join(format!("{}.{}", Uuid::new_v4(), STATE_FILE_EXTENSION));
        Self::create_at_path(
            path,
            TeslaMateProjectionStateOwnership::Generic,
            limits,
            maximum_changed_row_payload_bytes,
        )
    }

    /// Create the only spool shape accepted by direct-import generation
    /// finalizers. `HubStore` verifies the publication gate and staging row
    /// immediately before calling this constructor; this module then anchors
    /// the resulting file to a fixed owned run namespace.
    pub(crate) fn create_for_import_generation(
        root: impl AsRef<Path>,
        run_id: Uuid,
        limits: TeslaMateProjectionStateLimits,
        maximum_changed_row_payload_bytes: u64,
    ) -> Result<Self, TeslaMateProjectionStateError> {
        limits.validate()?;
        validate_changed_row_payload_limit(maximum_changed_row_payload_bytes)?;
        if run_id.is_nil() {
            return Err(TeslaMateProjectionStateError::InvalidImportGenerationRunId);
        }
        let namespace_root = ensure_import_generation_namespace(root.as_ref(), run_id)?;
        let attempt_id = Uuid::new_v4();
        let ownership = TeslaMateProjectionStateImportOwnership {
            namespace_root,
            run_id,
            attempt_id,
        };
        let path = ownership
            .namespace_root
            .join(run_id.to_string())
            .join(format!("{attempt_id}.{STATE_FILE_EXTENSION}"));
        Self::create_at_path(
            path,
            TeslaMateProjectionStateOwnership::ImportGeneration(ownership),
            limits,
            maximum_changed_row_payload_bytes,
        )
    }

    fn create_at_path(
        path: PathBuf,
        ownership: TeslaMateProjectionStateOwnership,
        limits: TeslaMateProjectionStateLimits,
        maximum_changed_row_payload_bytes: u64,
    ) -> Result<Self, TeslaMateProjectionStateError> {
        let parent = path
            .parent()
            .ok_or_else(|| TeslaMateProjectionStateError::InvalidTransferPath(path.clone()))?;
        let required = limits
            .max_state_bytes
            .checked_add(limits.minimum_free_bytes)
            .ok_or(TeslaMateProjectionStateError::StateCapacityOverflow)?;
        let available = available_bytes(parent)?;
        if available < required {
            return Err(TeslaMateProjectionStateError::InsufficientFreeSpace {
                required,
                available,
            });
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        options
            .open(&path)
            .map_err(|source| TeslaMateProjectionStateError::CreateFile {
                path: path.clone(),
                source,
            })?;
        ensure_private_file(&path)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
        configure(&connection, limits)?;
        initialise_schema(&connection)?;

        Ok(Self {
            path,
            connection,
            ownership,
            limits,
            maximum_changed_row_payload_bytes,
            row_count: 0,
            changed_row_count: 0,
            changed_payload_bytes: 0,
            pending_row_count: 0,
            pending_changed_row_count: 0,
            pending_changed_payload_bytes: 0,
            pending_write_rows: 0,
            write_transaction_open: false,
            write_failed: false,
            sealed: false,
            cleanup_on_drop: true,
        })
    }

    pub fn stats(&self) -> TeslaMateProjectionStateStats {
        TeslaMateProjectionStateStats {
            row_count: self.total_row_count(),
            changed_row_count: self.total_changed_row_count(),
            changed_payload_bytes: self.total_changed_payload_bytes(),
            sealed: self.sealed,
        }
    }

    /// Produce a narrow transfer descriptor for set-based durable catalogue
    /// persistence.  A direct importer may construct this only after capture
    /// has sealed the spool.  Validation deliberately happens before the Hub
    /// connection attaches the path, and the returned descriptor authenticates
    /// the attachment again to close the path-substitution window.
    #[cfg(test)]
    pub(crate) fn sealed_transfer(
        &self,
        selected_car_id: i64,
    ) -> Result<TeslaMateProjectionStateTransfer, TeslaMateProjectionStateError> {
        if !matches!(self.ownership, TeslaMateProjectionStateOwnership::Generic) {
            return Err(TeslaMateProjectionStateError::ImportGenerationTransferRequired);
        }
        self.sealed_transfer_with_ownership(selected_car_id, self.ownership.clone())
    }

    /// Produce a descriptor that can only be consumed by the exact direct
    /// import generation which created the spool. Generic/unit-test spools
    /// intentionally cannot cross this boundary.
    pub(crate) fn sealed_transfer_for_import_generation(
        &self,
        run_id: Uuid,
        selected_car_id: i64,
    ) -> Result<TeslaMateProjectionStateTransfer, TeslaMateProjectionStateError> {
        let TeslaMateProjectionStateOwnership::ImportGeneration(ownership) = &self.ownership else {
            return Err(TeslaMateProjectionStateError::ImportGenerationTransferRequired);
        };
        if run_id.is_nil() || ownership.run_id != run_id {
            return Err(TeslaMateProjectionStateError::ImportGenerationRunMismatch {
                expected: ownership.run_id,
                actual: run_id,
            });
        }
        self.sealed_transfer_with_ownership(
            selected_car_id,
            TeslaMateProjectionStateOwnership::ImportGeneration(ownership.clone()),
        )
    }

    fn sealed_transfer_with_ownership(
        &self,
        selected_car_id: i64,
        ownership: TeslaMateProjectionStateOwnership,
    ) -> Result<TeslaMateProjectionStateTransfer, TeslaMateProjectionStateError> {
        self.require_sealed()?;
        if selected_car_id <= 0 {
            return Err(TeslaMateProjectionStateError::InvalidCarId);
        }
        let path = match &ownership {
            TeslaMateProjectionStateOwnership::Generic => {
                validate_private_transfer_path(&self.path)?
            }
            TeslaMateProjectionStateOwnership::ImportGeneration(ownership) => {
                validate_import_generation_transfer_path(&self.path, ownership)?
            }
        };
        let stats = self.stats();
        validate_transfer_database(&self.connection, "main", stats, selected_car_id)?;
        let semantic_digest = transfer_semantic_digest(&self.connection, "main")?;
        Ok(TeslaMateProjectionStateTransfer {
            path,
            stats,
            selected_car_id,
            semantic_digest,
            ownership,
        })
    }

    #[cfg(test)]
    pub(crate) fn path_for_test(&self) -> &Path {
        &self.path
    }

    /// Simulate process termination after a durable capture has closed its
    /// SQLite handle, without invoking normal spool cleanup. This is test
    /// support for startup recovery only; production interruption naturally
    /// leaves the same owned v1 directory behind.
    #[cfg(test)]
    pub(crate) fn abandon_for_recovery_test(mut self) -> Result<(), TeslaMateProjectionStateError> {
        self.flush_pending_writes()?;
        self.cleanup_on_drop = false;
        Ok(())
    }

    pub fn record_car(
        &mut self,
        row: &ProjectionCar,
    ) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
        self.record(TeslaMateProjectionStateEntity::Car, row.id, row.id, row)
    }

    pub fn record_drive(
        &mut self,
        row: &ProjectionDrive,
    ) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::Drive,
            row.id,
            row.car_id,
            row,
        )
    }

    pub fn record_position(
        &mut self,
        row: &ProjectionPosition,
    ) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::Position,
            row.id,
            row.car_id,
            row,
        )
    }

    pub fn record_charge(
        &mut self,
        row: &ProjectionCharge,
    ) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::Charge,
            row.id,
            row.car_id,
            row,
        )
    }

    pub fn record_charge_sample(
        &mut self,
        car_id: i64,
        row: &ProjectionChargeSample,
    ) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::ChargeSample,
            row.id,
            car_id,
            row,
        )
    }

    pub fn record_state(
        &mut self,
        row: &ProjectionState,
    ) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::State,
            row.id,
            row.car_id,
            row,
        )
    }

    pub fn record_update(
        &mut self,
        row: &ProjectionUpdate,
    ) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::Update,
            row.id,
            row.car_id,
            row,
        )
    }

    /// Record one current fact and retain its payload only when `prior` proves
    /// it is new or differs from the current durable digest.
    pub fn record_if_changed<T: Serialize, L: PriorProjectionStateLookup + ?Sized>(
        &mut self,
        prior: &mut L,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
        car_id: i64,
        value: &T,
    ) -> Result<TeslaMateProjectionStateChange, TeslaMateProjectionStateError> {
        self.require_open()?;
        let (payload, digest) = canonical_payload_and_digest(entity, id, car_id, value)?;
        if let Some(change) = self.existing_change(entity, id, car_id, digest)? {
            return Ok(change);
        }
        let prior_digest = prior
            .digest(entity, id)
            .map_err(TeslaMateProjectionStateError::PriorLookup)?;
        if prior_digest == Some(digest) {
            self.insert_current(entity, id, car_id, digest)?;
            return Ok(TeslaMateProjectionStateChange::Unchanged);
        }
        self.insert_current_and_changed(entity, id, car_id, digest, &payload)?;
        Ok(TeslaMateProjectionStateChange::NewOrChanged)
    }

    /// Record a digest-only current fact.  This is useful when an integration
    /// chooses a batched lookup and calls [`Self::record_changed`] only for
    /// rows it has already classified as changed.
    pub fn record<T: Serialize>(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
        car_id: i64,
        value: &T,
    ) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
        let (_, digest) = canonical_payload_and_digest(entity, id, car_id, value)?;
        self.insert_current(entity, id, car_id, digest)?;
        Ok(digest)
    }

    /// Record a current fact and retain its canonical payload for output.
    pub fn record_changed<T: Serialize>(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
        car_id: i64,
        value: &T,
    ) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
        let (payload, digest) = canonical_payload_and_digest(entity, id, car_id, value)?;
        self.insert_current_and_changed(entity, id, car_id, digest, &payload)?;
        Ok(digest)
    }

    /// Seal after source capture.  Readers and durable persistence require a
    /// sealed file so a later writer cannot race a tombstone or digest scan.
    pub fn seal(&mut self) -> Result<TeslaMateProjectionStateStats, TeslaMateProjectionStateError> {
        self.require_open()?;
        self.flush_pending_writes()?;
        let (rows, changed_rows, changed_bytes): (i64, i64, i64) = self
            .connection
            .query_row(
                "SELECT \
                    (SELECT COUNT(*) FROM current_rows), \
                    (SELECT COUNT(*) FROM changed_rows), \
                    (SELECT COALESCE(SUM(payload_bytes), 0) FROM changed_rows)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(TeslaMateProjectionStateError::Sqlite)?;
        let rows = u64::try_from(rows)
            .map_err(|_| TeslaMateProjectionStateError::InvalidStoredAccounting)?;
        let changed_rows = u64::try_from(changed_rows)
            .map_err(|_| TeslaMateProjectionStateError::InvalidStoredAccounting)?;
        let changed_bytes = u64::try_from(changed_bytes)
            .map_err(|_| TeslaMateProjectionStateError::InvalidStoredAccounting)?;
        if rows != self.row_count
            || changed_rows != self.changed_row_count
            || changed_bytes != self.changed_payload_bytes
            || rows > self.limits.max_rows
            || changed_bytes > self.limits.max_changed_payload_bytes
        {
            return Err(TeslaMateProjectionStateError::PersistedAccountingMismatch);
        }
        let integrity: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(TeslaMateProjectionStateError::Sqlite)?;
        if integrity != "ok" {
            return Err(TeslaMateProjectionStateError::IntegrityCheckFailed(
                integrity,
            ));
        }
        let foreign_key_violation = self
            .connection
            .prepare("PRAGMA foreign_key_check")
            .map_err(TeslaMateProjectionStateError::Sqlite)?
            .exists([])
            .map_err(TeslaMateProjectionStateError::Sqlite)?;
        if foreign_key_violation {
            return Err(TeslaMateProjectionStateError::ForeignKeyCheckFailed);
        }
        self.sealed = true;
        Ok(self.stats())
    }

    pub fn page(
        &self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateDigestPage, TeslaMateProjectionStateError> {
        self.require_sealed()?;
        page_digest_rows(&self.connection, "current_rows", after, limit)
    }

    pub fn changed_page(
        &self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateChangedPage, TeslaMateProjectionStateError> {
        self.changed_page_with_payload_limit(after, limit, self.maximum_changed_row_payload_bytes)
    }

    /// Read a bounded successor page without materialising arbitrary payload
    /// history.  `maximum_payload_bytes` may narrow, but may never relax, the
    /// cap selected when the spool was created.
    pub fn changed_page_with_payload_limit(
        &self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
        maximum_payload_bytes: u64,
    ) -> Result<TeslaMateProjectionStateChangedPage, TeslaMateProjectionStateError> {
        self.require_sealed()?;
        validate_page_limit(limit)?;
        validate_cursor(after)?;
        self.validate_changed_page_payload_limit(maximum_payload_bytes)?;

        // First read only fixed-size metadata.  This determines exactly which
        // rows may be materialised while retaining a hard byte budget even if
        // the sparse successor is dense.
        let metadata = self.changed_page_metadata(after, limit)?;
        let metadata_count = metadata.len();
        let page_limit = usize::try_from(limit).expect("u32 fits usize");
        let mut selected = Vec::with_capacity(page_limit);
        let mut selected_payload_bytes = 0_u64;
        let mut stopped_at_payload_cap = false;
        for row in metadata {
            if row.payload_bytes > maximum_payload_bytes {
                return Err(
                    TeslaMateProjectionStateError::ChangedPayloadRowLimitExceeded {
                        maximum: maximum_payload_bytes,
                        payload_bytes: row.payload_bytes,
                    },
                );
            }
            let next_payload_bytes = selected_payload_bytes
                .checked_add(row.payload_bytes)
                .ok_or(
                    TeslaMateProjectionStateError::ChangedPagePayloadLimitExceeded {
                        maximum: maximum_payload_bytes,
                    },
                )?;
            if next_payload_bytes > maximum_payload_bytes {
                stopped_at_payload_cap = true;
                break;
            }
            selected_payload_bytes = next_payload_bytes;
            selected.push(row);
        }

        // `limit + 1` metadata rows reveal the ordinary row-count boundary;
        // a byte boundary also leaves the first excluded row for the next
        // cursor.  In either case return the last retained cursor so no row is
        // skipped or duplicated by the caller's next request.
        let has_more = stopped_at_payload_cap
            || selected.len() > page_limit
            || metadata_count > selected.len();
        selected.truncate(page_limit);
        selected_payload_bytes = selected.iter().try_fold(0_u64, |total, row| {
            total.checked_add(row.payload_bytes).ok_or(
                TeslaMateProjectionStateError::ChangedPagePayloadLimitExceeded {
                    maximum: maximum_payload_bytes,
                },
            )
        })?;
        let rows = self.changed_page_payload_rows(&selected, maximum_payload_bytes)?;
        debug_assert_eq!(
            selected_payload_bytes,
            rows.iter().fold(0_u64, |total, row| total
                .checked_add(u64::try_from(row.canonical_payload.len()).expect("usize fits u64"))
                .expect("selected page bytes are bounded"))
        );
        let next_after = has_more.then(|| {
            let row = rows
                .last()
                .expect("a non-empty metadata page always has one row within its byte cap");
            TeslaMateProjectionStateCursor {
                entity: row.state.entity,
                id: row.state.id,
            }
        });
        Ok(TeslaMateProjectionStateChangedPage { rows, next_after })
    }

    fn validate_changed_page_payload_limit(
        &self,
        maximum_payload_bytes: u64,
    ) -> Result<(), TeslaMateProjectionStateError> {
        if maximum_payload_bytes == 0
            || maximum_payload_bytes > self.maximum_changed_row_payload_bytes
        {
            return Err(
                TeslaMateProjectionStateError::InvalidChangedPagePayloadLimit {
                    maximum: self.maximum_changed_row_payload_bytes,
                    requested: maximum_payload_bytes,
                },
            );
        }
        Ok(())
    }

    fn changed_page_metadata(
        &self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<Vec<TeslaMateProjectionStateChangedMetadata>, TeslaMateProjectionStateError> {
        let (after_entity, after_id) = cursor_values(after);
        let query_limit = i64::from(limit) + 1;
        let mut statement = self
            .connection
            .prepare(
                "SELECT entity_ordinal, entity_id, car_id, projection_sha256, payload_bytes \
                 FROM changed_rows \
                 WHERE entity_ordinal > ?1 \
                    OR (entity_ordinal = ?1 AND entity_id > ?2) \
                 ORDER BY entity_ordinal ASC, entity_id ASC \
                 LIMIT ?3",
            )
            .map_err(TeslaMateProjectionStateError::Sqlite)?;
        let mut query = statement
            .query(params![after_entity, after_id, query_limit])
            .map_err(TeslaMateProjectionStateError::Sqlite)?;
        let mut metadata = Vec::with_capacity(usize::try_from(limit).expect("u32 fits usize"));
        while let Some(row) = query
            .next()
            .map_err(TeslaMateProjectionStateError::Sqlite)?
        {
            let entity_ordinal = row.get(0).map_err(TeslaMateProjectionStateError::Sqlite)?;
            let entity = TeslaMateProjectionStateEntity::from_ordinal(entity_ordinal)?;
            let digest =
                digest_from_blob(row.get(3).map_err(TeslaMateProjectionStateError::Sqlite)?)?;
            let payload_bytes = row
                .get::<_, i64>(4)
                .map_err(TeslaMateProjectionStateError::Sqlite)
                .and_then(|bytes| {
                    u64::try_from(bytes)
                        .map_err(|_| TeslaMateProjectionStateError::InvalidStoredAccounting)
                })?;
            metadata.push(TeslaMateProjectionStateChangedMetadata {
                state: TeslaMateProjectionStateDigestRow {
                    entity,
                    id: row.get(1).map_err(TeslaMateProjectionStateError::Sqlite)?,
                    car_id: row.get(2).map_err(TeslaMateProjectionStateError::Sqlite)?,
                    digest,
                },
                payload_bytes,
            });
        }
        Ok(metadata)
    }

    fn changed_page_payload_rows(
        &self,
        metadata: &[TeslaMateProjectionStateChangedMetadata],
        maximum_payload_bytes: u64,
    ) -> Result<Vec<TeslaMateProjectionStateChangedRow>, TeslaMateProjectionStateError> {
        let mut result = Vec::with_capacity(metadata.len());
        let mut total_payload_bytes = 0_u64;
        for requested in metadata.chunks(CHANGED_PAGE_PAYLOAD_LOOKUP_ROWS) {
            let mut query = String::from("WITH requested(entity_ordinal, entity_id) AS (VALUES ");
            for index in 0..requested.len() {
                if index != 0 {
                    query.push_str(", ");
                }
                query.push_str("(?, ?)");
            }
            query.push_str(
                ") \
                 SELECT changed_rows.payload_json \
                 FROM requested \
                 JOIN changed_rows \
                   ON changed_rows.entity_ordinal = requested.entity_ordinal \
                  AND changed_rows.entity_id = requested.entity_id \
                 WHERE changed_rows.payload_bytes = length(CAST(changed_rows.payload_json AS BLOB)) \
                 ORDER BY changed_rows.entity_ordinal ASC, changed_rows.entity_id ASC",
            );
            let mut values = Vec::with_capacity(requested.len() * 2);
            for row in requested {
                values.push(i64::from(row.state.entity.ordinal()));
                values.push(row.state.id);
            }
            let mut statement = self
                .connection
                .prepare(&query)
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            let mut rows = statement
                .query(params_from_iter(values.iter()))
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            let mut returned = 0_usize;
            while let Some(row) = rows.next().map_err(TeslaMateProjectionStateError::Sqlite)? {
                let expected = requested
                    .get(returned)
                    .ok_or(TeslaMateProjectionStateError::StoredChangedPayloadAccountingMismatch)?;
                let canonical_payload = row
                    .get::<_, String>(0)
                    .map_err(TeslaMateProjectionStateError::Sqlite)?
                    .into_bytes();
                let payload_bytes = u64::try_from(canonical_payload.len()).expect("usize fits u64");
                if payload_bytes != expected.payload_bytes {
                    return Err(
                        TeslaMateProjectionStateError::StoredChangedPayloadAccountingMismatch,
                    );
                }
                verify_stored_changed_payload(&expected.state, &canonical_payload)?;
                total_payload_bytes = total_payload_bytes.checked_add(payload_bytes).ok_or(
                    TeslaMateProjectionStateError::ChangedPagePayloadLimitExceeded {
                        maximum: maximum_payload_bytes,
                    },
                )?;
                if total_payload_bytes > maximum_payload_bytes {
                    return Err(
                        TeslaMateProjectionStateError::ChangedPagePayloadLimitExceeded {
                            maximum: maximum_payload_bytes,
                        },
                    );
                }
                result.push(TeslaMateProjectionStateChangedRow {
                    state: expected.state.clone(),
                    canonical_payload,
                });
                returned = returned
                    .checked_add(1)
                    .ok_or(TeslaMateProjectionStateError::StoredChangedPayloadAccountingMismatch)?;
            }
            if returned != requested.len() {
                return Err(TeslaMateProjectionStateError::StoredChangedPayloadAccountingMismatch);
            }
        }
        Ok(result)
    }

    /// Derive one bounded page of source-owned tombstones.  A car row is
    /// intentionally never tombstoned, even if a malformed prior index lists
    /// one; that invariant is enforced here rather than delegated to callers.
    pub fn tombstone_page<L: PriorProjectionStateLookup + ?Sized>(
        &self,
        prior: &mut L,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<
        (
            Vec<ProjectionTombstone>,
            Option<TeslaMateProjectionStateCursor>,
        ),
        TeslaMateProjectionStateError,
    > {
        self.require_sealed()?;
        validate_page_limit(limit)?;
        validate_cursor(after)?;
        let page = prior
            .page_after(after, limit)
            .map_err(TeslaMateProjectionStateError::PriorLookup)?;
        let mut tombstones = Vec::new();
        for row in &page.rows {
            validate_row_identity(row.id, row.car_id)?;
            if !row.entity.tombstone_allowed() {
                continue;
            }
            if !self.contains(row.entity, row.id)? {
                tombstones.push(ProjectionTombstone {
                    entity: projection_delta_entity(row.entity),
                    id: row.id,
                    car_id: row.car_id,
                });
            }
        }
        Ok((tombstones, page.next_after))
    }

    /// Explicitly remove this exact private state file.  Dropping without a
    /// call also removes it; this method is convenient for error cleanup.
    pub fn discard(mut self) -> Result<(), TeslaMateProjectionStateError> {
        // A caller can discard an unsealed capture after a successful source
        // pass. Flush first so SQLite can close a clean DELETE-mode journal;
        // on failure Drop still removes this private file.
        self.flush_pending_writes()?;
        self.cleanup_on_drop = false;
        let path = self.path.clone();
        let ownership = self.ownership.clone();
        drop(self);
        fs::remove_file(&path).map_err(|source| TeslaMateProjectionStateError::RemoveFile {
            path: path.clone(),
            source,
        })?;
        cleanup_empty_import_generation_run(&ownership)?;
        Ok(())
    }

    fn require_open(&self) -> Result<(), TeslaMateProjectionStateError> {
        if self.write_failed {
            return Err(TeslaMateProjectionStateError::WriteBatchFailed);
        }
        if self.sealed {
            return Err(TeslaMateProjectionStateError::StateSealed);
        }
        Ok(())
    }

    fn require_sealed(&self) -> Result<(), TeslaMateProjectionStateError> {
        if !self.sealed {
            return Err(TeslaMateProjectionStateError::StateNotSealed);
        }
        Ok(())
    }

    fn insert_current(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
        car_id: i64,
        digest: Sha256Digest,
    ) -> Result<(), TeslaMateProjectionStateError> {
        self.require_open()?;
        validate_row_identity(id, car_id)?;
        if self.total_row_count() >= self.limits.max_rows {
            if self.existing_change(entity, id, car_id, digest)?.is_some() {
                return Ok(());
            }
            return Err(TeslaMateProjectionStateError::RowLimitExceeded {
                maximum: self.limits.max_rows,
            });
        }
        self.begin_pending_write_transaction()?;
        // The direct base capture has millions of distinct, source-ordered
        // rows. Insert first so the normal case does one SQLite operation;
        // consult the stored row only for a key collision, where the
        // exact-repeat/conflict contract still needs to be enforced.
        //
        // Keep the conflict target narrow. INSERT OR IGNORE would also hide
        // malformed-row CHECK or NOT NULL failures, which must remain errors.
        let result = self
            .connection
            .prepare_cached(
                "INSERT INTO current_rows(entity_ordinal, entity_id, car_id, projection_sha256) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(entity_ordinal, entity_id) DO NOTHING",
            )
            .and_then(|mut statement| {
                statement.execute(params![
                    i64::from(entity.ordinal()),
                    id,
                    car_id,
                    digest.as_bytes().as_slice()
                ])
            });
        match result {
            Ok(1) => self.finish_pending_write(1, 0, 0),
            Ok(0) => {
                let existing = self.existing_change(entity, id, car_id, digest);
                self.close_empty_pending_write_transaction();
                match existing? {
                    Some(_) => Ok(()),
                    None => Err(TeslaMateProjectionStateError::InvalidStoredAccounting),
                }
            }
            Ok(_) => {
                self.close_empty_pending_write_transaction();
                Err(TeslaMateProjectionStateError::InvalidStoredAccounting)
            }
            Err(error) => {
                self.close_empty_pending_write_transaction();
                Err(TeslaMateProjectionStateError::Sqlite(error))
            }
        }
    }

    fn insert_current_and_changed(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
        car_id: i64,
        digest: Sha256Digest,
        payload: &[u8],
    ) -> Result<(), TeslaMateProjectionStateError> {
        self.require_open()?;
        validate_row_identity(id, car_id)?;
        if self.existing_change(entity, id, car_id, digest)?.is_some() {
            return Ok(());
        }
        let payload = std::str::from_utf8(payload)
            .map_err(TeslaMateProjectionStateError::CanonicalPayloadUtf8)?;
        let payload_bytes = u64::try_from(payload.len()).expect("usize fits u64");
        if payload_bytes > self.maximum_changed_row_payload_bytes {
            return Err(
                TeslaMateProjectionStateError::ChangedPayloadRowLimitExceeded {
                    maximum: self.maximum_changed_row_payload_bytes,
                    payload_bytes,
                },
            );
        }
        let next_payload_bytes = self
            .total_changed_payload_bytes()
            .checked_add(payload_bytes)
            .ok_or(TeslaMateProjectionStateError::ChangedPayloadLimitExceeded {
                maximum: self.limits.max_changed_payload_bytes,
            })?;
        if next_payload_bytes > self.limits.max_changed_payload_bytes {
            return Err(TeslaMateProjectionStateError::ChangedPayloadLimitExceeded {
                maximum: self.limits.max_changed_payload_bytes,
            });
        }
        if self.total_row_count() >= self.limits.max_rows {
            return Err(TeslaMateProjectionStateError::RowLimitExceeded {
                maximum: self.limits.max_rows,
            });
        }
        self.flush_before_changed_payload(payload_bytes)?;
        self.begin_pending_write_transaction()?;
        if let Err(error) = self
            .connection
            .execute_batch("SAVEPOINT projection_state_row")
        {
            self.close_empty_pending_write_transaction();
            return Err(TeslaMateProjectionStateError::Sqlite(error));
        }
        let result = (|| {
            let current = self.connection.execute(
                "INSERT INTO current_rows(entity_ordinal, entity_id, car_id, projection_sha256) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    i64::from(entity.ordinal()),
                    id,
                    car_id,
                    digest.as_bytes().as_slice()
                ],
            );
            match current {
                Ok(1) => {}
                Ok(_) => return Err(TeslaMateProjectionStateError::InvalidStoredAccounting),
                Err(error) if is_unique_violation(&error) => {
                    return Err(TeslaMateProjectionStateError::ConcurrentWrite { entity, id });
                }
                Err(error) => return Err(TeslaMateProjectionStateError::Sqlite(error)),
            }
            let changed = self.connection.execute(
                "INSERT INTO changed_rows( \
                    entity_ordinal, entity_id, car_id, projection_sha256, payload_json, payload_bytes \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    i64::from(entity.ordinal()),
                    id,
                    car_id,
                    digest.as_bytes().as_slice(),
                    payload,
                    i64::try_from(payload_bytes).expect("bounded payload fits i64")
                ],
            );
            match changed {
                Ok(1) => {}
                Ok(_) => return Err(TeslaMateProjectionStateError::InvalidStoredAccounting),
                Err(error) if is_unique_violation(&error) => {
                    return Err(TeslaMateProjectionStateError::ConcurrentChangedWrite {
                        entity,
                        id,
                    });
                }
                Err(error) => return Err(TeslaMateProjectionStateError::Sqlite(error)),
            }
            self.connection
                .execute_batch("RELEASE SAVEPOINT projection_state_row")
                .map_err(TeslaMateProjectionStateError::Sqlite)
        })();
        if let Err(error) = result {
            self.rollback_pending_row_savepoint();
            self.close_empty_pending_write_transaction();
            return Err(error);
        }
        debug_assert_eq!(
            next_payload_bytes,
            self.total_changed_payload_bytes() + payload_bytes
        );
        self.finish_pending_write(1, 1, payload_bytes)
    }

    fn total_row_count(&self) -> u64 {
        self.row_count
            .checked_add(self.pending_row_count)
            .expect("projection-state row accounting is bounded")
    }

    fn total_changed_row_count(&self) -> u64 {
        self.changed_row_count
            .checked_add(self.pending_changed_row_count)
            .expect("projection-state changed-row accounting is bounded")
    }

    fn total_changed_payload_bytes(&self) -> u64 {
        self.changed_payload_bytes
            .checked_add(self.pending_changed_payload_bytes)
            .expect("projection-state changed-payload accounting is bounded")
    }

    fn begin_pending_write_transaction(&mut self) -> Result<(), TeslaMateProjectionStateError> {
        if self.write_failed {
            return Err(TeslaMateProjectionStateError::WriteBatchFailed);
        }
        if self.write_transaction_open {
            return Ok(());
        }
        debug_assert_eq!(self.pending_write_rows, 0);
        debug_assert_eq!(self.pending_row_count, 0);
        debug_assert_eq!(self.pending_changed_row_count, 0);
        debug_assert_eq!(self.pending_changed_payload_bytes, 0);
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(TeslaMateProjectionStateError::Sqlite)?;
        self.write_transaction_open = true;
        Ok(())
    }

    /// Commit the fixed-size source batch. This is the only durability
    /// boundary during capture, so DELETE/FULL produces one fsync per bounded
    /// group rather than one per fact.
    fn flush_pending_writes(&mut self) -> Result<(), TeslaMateProjectionStateError> {
        if self.write_failed {
            return Err(TeslaMateProjectionStateError::WriteBatchFailed);
        }
        if self.pending_write_rows == 0 {
            if self.write_transaction_open {
                self.abort_pending_write_transaction();
                self.write_failed = true;
                return Err(TeslaMateProjectionStateError::InvalidStoredAccounting);
            }
            return Ok(());
        }
        if !self.write_transaction_open {
            self.abort_pending_write_transaction();
            self.write_failed = true;
            return Err(TeslaMateProjectionStateError::InvalidStoredAccounting);
        }
        let Some(committed_rows) = self.row_count.checked_add(self.pending_row_count) else {
            return self.fail_pending_write_accounting();
        };
        let Some(committed_changed_rows) = self
            .changed_row_count
            .checked_add(self.pending_changed_row_count)
        else {
            return self.fail_pending_write_accounting();
        };
        let Some(committed_changed_payload_bytes) = self
            .changed_payload_bytes
            .checked_add(self.pending_changed_payload_bytes)
        else {
            return self.fail_pending_write_accounting();
        };
        if let Err(error) = self.connection.execute_batch("COMMIT") {
            self.abort_pending_write_transaction();
            self.write_failed = true;
            return Err(TeslaMateProjectionStateError::Sqlite(error));
        }
        self.row_count = committed_rows;
        self.changed_row_count = committed_changed_rows;
        self.changed_payload_bytes = committed_changed_payload_bytes;
        self.clear_pending_write_accounting();
        Ok(())
    }

    fn finish_pending_write(
        &mut self,
        rows: u64,
        changed_rows: u64,
        changed_payload_bytes: u64,
    ) -> Result<(), TeslaMateProjectionStateError> {
        let Some(pending_rows) = self.pending_row_count.checked_add(rows) else {
            return self.fail_pending_write_accounting();
        };
        let Some(pending_changed_rows) = self.pending_changed_row_count.checked_add(changed_rows)
        else {
            return self.fail_pending_write_accounting();
        };
        let Some(pending_changed_payload_bytes) = self
            .pending_changed_payload_bytes
            .checked_add(changed_payload_bytes)
        else {
            return self.fail_pending_write_accounting();
        };
        let Some(pending_write_rows) = self.pending_write_rows.checked_add(1) else {
            return self.fail_pending_write_accounting();
        };
        self.pending_row_count = pending_rows;
        self.pending_changed_row_count = pending_changed_rows;
        self.pending_changed_payload_bytes = pending_changed_payload_bytes;
        self.pending_write_rows = pending_write_rows;
        if self.pending_write_rows >= WRITE_BATCH_ROWS
            || self.pending_changed_payload_bytes >= WRITE_BATCH_CHANGED_PAYLOAD_BYTES
        {
            self.flush_pending_writes()?;
        }
        Ok(())
    }

    /// Keep the byte-boundary between source rows. If one row alone exceeds
    /// the cap it is necessarily written in its own transaction and committed
    /// immediately by [`Self::finish_pending_write`].
    fn flush_before_changed_payload(
        &mut self,
        incoming_payload_bytes: u64,
    ) -> Result<(), TeslaMateProjectionStateError> {
        let pending_payload_bytes = self
            .pending_changed_payload_bytes
            .checked_add(incoming_payload_bytes)
            .ok_or(TeslaMateProjectionStateError::ChangedPayloadLimitExceeded {
                maximum: self.limits.max_changed_payload_bytes,
            })?;
        if self.pending_write_rows > 0 && pending_payload_bytes > WRITE_BATCH_CHANGED_PAYLOAD_BYTES
        {
            self.flush_pending_writes()?;
        }
        Ok(())
    }

    fn rollback_pending_row_savepoint(&mut self) {
        if self.write_transaction_open
            && self
                .connection
                .execute_batch("ROLLBACK TO SAVEPOINT projection_state_row; RELEASE SAVEPOINT projection_state_row")
                .is_err()
        {
            self.abort_pending_write_transaction();
            self.write_failed = true;
        }
    }

    fn abort_pending_write_transaction(&mut self) {
        if self.write_transaction_open {
            let _ = self.connection.execute_batch("ROLLBACK");
        }
        self.clear_pending_write_accounting();
    }

    fn close_empty_pending_write_transaction(&mut self) {
        if self.write_transaction_open && self.pending_write_rows == 0 {
            self.abort_pending_write_transaction();
        }
    }

    fn fail_pending_write_accounting(&mut self) -> Result<(), TeslaMateProjectionStateError> {
        self.abort_pending_write_transaction();
        self.write_failed = true;
        Err(TeslaMateProjectionStateError::InvalidStoredAccounting)
    }

    fn clear_pending_write_accounting(&mut self) {
        self.pending_row_count = 0;
        self.pending_changed_row_count = 0;
        self.pending_changed_payload_bytes = 0;
        self.pending_write_rows = 0;
        self.write_transaction_open = false;
    }

    fn contains(
        &self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    ) -> Result<bool, TeslaMateProjectionStateError> {
        self.connection
            .query_row(
                "SELECT EXISTS( \
                    SELECT 1 FROM current_rows \
                     WHERE entity_ordinal = ?1 AND entity_id = ?2
                 )",
                params![i64::from(entity.ordinal()), id],
                |row| row.get(0),
            )
            .map_err(TeslaMateProjectionStateError::Sqlite)
    }

    /// Return the stored classification for an exact repeat and reject reuse
    /// of an entity/id with a different selected car or digest. TeslaMate
    /// fragments legitimately repeat parent rows, so exact repeats consume
    /// neither current-row nor changed-payload budget.
    fn existing_change(
        &self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
        car_id: i64,
        digest: Sha256Digest,
    ) -> Result<Option<TeslaMateProjectionStateChange>, TeslaMateProjectionStateError> {
        let existing = {
            let mut statement = self
                .connection
                .prepare_cached(
                    "SELECT car_id, projection_sha256, EXISTS( \
                    SELECT 1 FROM changed_rows \
                     WHERE changed_rows.entity_ordinal = current_rows.entity_ordinal \
                       AND changed_rows.entity_id = current_rows.entity_id \
                 ) \
                 FROM current_rows \
                 WHERE entity_ordinal = ?1 AND entity_id = ?2",
                )
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            statement
                .query_row(params![i64::from(entity.ordinal()), id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, bool>(2)?,
                    ))
                })
                .optional()
                .map_err(TeslaMateProjectionStateError::Sqlite)?
        };
        let Some((stored_car_id, stored_digest, has_changed_payload)) = existing else {
            return Ok(None);
        };
        let stored_digest = digest_from_blob(stored_digest)?;
        if stored_car_id != car_id || stored_digest != digest {
            return Err(TeslaMateProjectionStateError::ConflictingRow { entity, id });
        }
        Ok(Some(if has_changed_payload {
            TeslaMateProjectionStateChange::NewOrChanged
        } else {
            TeslaMateProjectionStateChange::Unchanged
        }))
    }
}

/// The PackSink-facing capture owner. It keeps the potentially stateful prior
/// lookup behind a trait object, so a fragment consumer need not be generic
/// over a database lookup implementation. With a prior lookup it spools only
/// new or changed payloads; initial-base mode retains digest-only state because
/// its full snapshot pack already owns every payload.
pub struct TeslaMateProjectionStateCapture {
    state: TeslaMateProjectionState,
    prior: Option<Box<dyn PriorProjectionStateLookup>>,
    mode: TeslaMateProjectionStateCaptureMode,
}

impl std::fmt::Debug for TeslaMateProjectionStateCapture {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TeslaMateProjectionStateCapture")
            .field("mode", &self.mode)
            .field("stats", &self.state.stats())
            .finish_non_exhaustive()
    }
}

impl TeslaMateProjectionStateCapture {
    /// Construct an initial-base capture. This never retains canonical row
    /// payloads; the full snapshot pack is the payload authority.
    pub fn for_initial_base(state: TeslaMateProjectionState) -> Self {
        Self {
            state,
            prior: None,
            mode: TeslaMateProjectionStateCaptureMode::InitialBase,
        }
    }

    /// Construct a changed-history successor capture. A verified durable
    /// prior state is mandatory so it can retain only new/changed payloads.
    pub fn for_successor(
        state: TeslaMateProjectionState,
        prior: Box<dyn PriorProjectionStateLookup>,
    ) -> Self {
        Self {
            state,
            prior: Some(prior),
            mode: TeslaMateProjectionStateCaptureMode::Successor,
        }
    }

    /// Compatibility constructor. Prefer the explicit constructors so call
    /// sites make base versus successor payload retention obvious.
    pub fn new(
        state: TeslaMateProjectionState,
        prior: Option<Box<dyn PriorProjectionStateLookup>>,
    ) -> Self {
        match prior {
            Some(prior) => Self::for_successor(state, prior),
            None => Self::for_initial_base(state),
        }
    }

    pub fn has_prior(&self) -> bool {
        self.prior.is_some()
    }

    pub fn mode(&self) -> TeslaMateProjectionStateCaptureMode {
        self.mode
    }

    pub fn state(&self) -> &TeslaMateProjectionState {
        &self.state
    }

    pub fn into_state(self) -> TeslaMateProjectionState {
        self.state
    }

    pub fn stats(&self) -> TeslaMateProjectionStateStats {
        self.state.stats()
    }

    pub fn record_car(
        &mut self,
        row: &ProjectionCar,
    ) -> Result<TeslaMateProjectionStateChange, TeslaMateProjectionStateError> {
        self.record(TeslaMateProjectionStateEntity::Car, row.id, row.id, row)
    }

    pub fn record_drive(
        &mut self,
        row: &ProjectionDrive,
    ) -> Result<TeslaMateProjectionStateChange, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::Drive,
            row.id,
            row.car_id,
            row,
        )
    }

    pub fn record_position(
        &mut self,
        row: &ProjectionPosition,
    ) -> Result<TeslaMateProjectionStateChange, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::Position,
            row.id,
            row.car_id,
            row,
        )
    }

    pub fn record_charge(
        &mut self,
        row: &ProjectionCharge,
    ) -> Result<TeslaMateProjectionStateChange, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::Charge,
            row.id,
            row.car_id,
            row,
        )
    }

    pub fn record_charge_sample(
        &mut self,
        car_id: i64,
        row: &ProjectionChargeSample,
    ) -> Result<TeslaMateProjectionStateChange, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::ChargeSample,
            row.id,
            car_id,
            row,
        )
    }

    pub fn record_state(
        &mut self,
        row: &ProjectionState,
    ) -> Result<TeslaMateProjectionStateChange, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::State,
            row.id,
            row.car_id,
            row,
        )
    }

    pub fn record_update(
        &mut self,
        row: &ProjectionUpdate,
    ) -> Result<TeslaMateProjectionStateChange, TeslaMateProjectionStateError> {
        self.record(
            TeslaMateProjectionStateEntity::Update,
            row.id,
            row.car_id,
            row,
        )
    }

    pub fn record<T: Serialize>(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
        car_id: i64,
        value: &T,
    ) -> Result<TeslaMateProjectionStateChange, TeslaMateProjectionStateError> {
        if let Some(prior) = self.prior.as_mut() {
            self.state
                .record_if_changed(prior.as_mut(), entity, id, car_id, value)
        } else {
            self.state.record(entity, id, car_id, value)?;
            Ok(TeslaMateProjectionStateChange::CapturedDigestOnly)
        }
    }

    pub fn seal(&mut self) -> Result<TeslaMateProjectionStateStats, TeslaMateProjectionStateError> {
        self.state.seal()
    }

    pub fn page(
        &self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateDigestPage, TeslaMateProjectionStateError> {
        self.state.page(after, limit)
    }

    pub fn changed_page(
        &self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateChangedPage, TeslaMateProjectionStateError> {
        self.state.changed_page(after, limit)
    }

    /// Read a changed page with a caller-specified cap that may only tighten
    /// the durable spool's configured per-row cap.
    pub fn changed_page_with_payload_limit(
        &self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
        maximum_payload_bytes: u64,
    ) -> Result<TeslaMateProjectionStateChangedPage, TeslaMateProjectionStateError> {
        self.state
            .changed_page_with_payload_limit(after, limit, maximum_payload_bytes)
    }

    pub fn tombstone_page(
        &mut self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<
        (
            Vec<ProjectionTombstone>,
            Option<TeslaMateProjectionStateCursor>,
        ),
        TeslaMateProjectionStateError,
    > {
        match self.prior.as_mut() {
            Some(prior) => self.state.tombstone_page(prior.as_mut(), after, limit),
            None => Ok((Vec::new(), None)),
        }
    }
}

impl Drop for TeslaMateProjectionState {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            // Best effort only: this file is private and is being removed,
            // but committing a valid tail first lets DELETE-mode SQLite close
            // its journal cleanly. Never commit a poisoned capture; a failed
            // batch has already been rolled back and is removed below.
            if !self.write_failed {
                let _ = self.flush_pending_writes();
            }
            let _ = fs::remove_file(&self.path);
            let _ = cleanup_empty_import_generation_run(&self.ownership);
        }
    }
}

fn canonical_payload_and_digest<T: Serialize>(
    entity: TeslaMateProjectionStateEntity,
    id: i64,
    car_id: i64,
    value: &T,
) -> Result<(Vec<u8>, Sha256Digest), TeslaMateProjectionStateError> {
    validate_row_identity(id, car_id)?;
    let canonical =
        serde_json::to_value(value).map_err(TeslaMateProjectionStateError::SerializeRow)?;
    if !canonical.is_object() {
        return Err(TeslaMateProjectionStateError::PayloadMustBeJsonObject);
    }
    let payload =
        serde_json::to_vec(&canonical).map_err(TeslaMateProjectionStateError::SerializeRow)?;
    let length = u64::try_from(payload.len()).expect("usize fits u64");
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update([0]);
    hasher.update(entity.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(id.to_be_bytes());
    hasher.update(car_id.to_be_bytes());
    hasher.update(length.to_be_bytes());
    hasher.update(&payload);
    Ok((payload, Sha256Digest::from_bytes(hasher.finalize().into())))
}

/// A changed payload is emitted only after it has been recanonicalized and
/// bound to the identity digest captured with it.  The state spool is private,
/// but this is still the final integrity boundary before a sparse successor
/// decodes and publishes the JSON bytes.
fn verify_stored_changed_payload(
    state: &TeslaMateProjectionStateDigestRow,
    payload: &[u8],
) -> Result<(), TeslaMateProjectionStateError> {
    let value = serde_json::from_slice::<serde_json::Value>(payload)
        .map_err(|_| TeslaMateProjectionStateError::StoredChangedPayloadDigestMismatch)?;
    let (canonical_payload, digest) =
        canonical_payload_and_digest(state.entity, state.id, state.car_id, &value)
            .map_err(|_| TeslaMateProjectionStateError::StoredChangedPayloadDigestMismatch)?;
    if canonical_payload != payload || digest != state.digest {
        return Err(TeslaMateProjectionStateError::StoredChangedPayloadDigestMismatch);
    }
    Ok(())
}

fn page_digest_rows(
    connection: &Connection,
    table: &str,
    after: Option<TeslaMateProjectionStateCursor>,
    limit: u32,
) -> Result<TeslaMateProjectionStateDigestPage, TeslaMateProjectionStateError> {
    validate_page_limit(limit)?;
    validate_cursor(after)?;
    let (after_entity, after_id) = cursor_values(after);
    let query_limit = i64::from(limit) + 1;
    let query = match table {
        "current_rows" => {
            "SELECT entity_ordinal, entity_id, car_id, projection_sha256 \
             FROM current_rows \
             WHERE entity_ordinal > ?1 \
                OR (entity_ordinal = ?1 AND entity_id > ?2) \
             ORDER BY entity_ordinal ASC, entity_id ASC \
             LIMIT ?3"
        }
        _ => return Err(TeslaMateProjectionStateError::InvalidStoredTable),
    };
    let mut statement = connection
        .prepare(query)
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    let mut rows = statement
        .query_map(params![after_entity, after_id, query_limit], |row| {
            let entity = TeslaMateProjectionStateEntity::from_ordinal(row.get(0)?)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let digest = digest_from_blob(row.get(3)?)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            Ok(TeslaMateProjectionStateDigestRow {
                entity,
                id: row.get(1)?,
                car_id: row.get(2)?,
                digest,
            })
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    let next_after = if rows.len() > usize::try_from(limit).expect("u32 fits usize") {
        rows.pop();
        rows.last().map(|row| TeslaMateProjectionStateCursor {
            entity: row.entity,
            id: row.id,
        })
    } else {
        None
    };
    Ok(TeslaMateProjectionStateDigestPage { rows, next_after })
}

fn cursor_values(after: Option<TeslaMateProjectionStateCursor>) -> (i64, i64) {
    after.map_or((-1, 0), |cursor| {
        (i64::from(cursor.entity.ordinal()), cursor.id)
    })
}

fn validate_page_limit(limit: u32) -> Result<(), TeslaMateProjectionStateError> {
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(TeslaMateProjectionStateError::InvalidPageSize);
    }
    Ok(())
}

fn validate_changed_row_payload_limit(
    maximum_payload_bytes: u64,
) -> Result<(), TeslaMateProjectionStateError> {
    if maximum_payload_bytes == 0 || maximum_payload_bytes > i64::MAX as u64 {
        return Err(TeslaMateProjectionStateError::InvalidChangedRowPayloadLimit);
    }
    Ok(())
}

fn validate_cursor(
    after: Option<TeslaMateProjectionStateCursor>,
) -> Result<(), TeslaMateProjectionStateError> {
    if after.is_some_and(|cursor| cursor.id <= 0) {
        return Err(TeslaMateProjectionStateError::InvalidCursor);
    }
    Ok(())
}

fn validate_row_identity(id: i64, car_id: i64) -> Result<(), TeslaMateProjectionStateError> {
    if id <= 0 {
        return Err(TeslaMateProjectionStateError::InvalidRowId);
    }
    if car_id <= 0 {
        return Err(TeslaMateProjectionStateError::InvalidCarId);
    }
    Ok(())
}

fn digest_from_blob(blob: Vec<u8>) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
    let bytes: [u8; 32] = blob
        .try_into()
        .map_err(|_: Vec<u8>| TeslaMateProjectionStateError::InvalidStoredDigest)?;
    Ok(Sha256Digest::from_bytes(bytes))
}

fn projection_delta_entity(
    entity: TeslaMateProjectionStateEntity,
) -> crate::hub_pack::ProjectionDeltaEntity {
    match entity {
        TeslaMateProjectionStateEntity::Car => crate::hub_pack::ProjectionDeltaEntity::Car,
        TeslaMateProjectionStateEntity::Drive => crate::hub_pack::ProjectionDeltaEntity::Drive,
        TeslaMateProjectionStateEntity::Position => {
            crate::hub_pack::ProjectionDeltaEntity::Position
        }
        TeslaMateProjectionStateEntity::Charge => crate::hub_pack::ProjectionDeltaEntity::Charge,
        TeslaMateProjectionStateEntity::ChargeSample => {
            crate::hub_pack::ProjectionDeltaEntity::ChargeSample
        }
        TeslaMateProjectionStateEntity::State => crate::hub_pack::ProjectionDeltaEntity::State,
        TeslaMateProjectionStateEntity::Update => crate::hub_pack::ProjectionDeltaEntity::Update,
    }
}

fn configure(
    connection: &Connection,
    limits: TeslaMateProjectionStateLimits,
) -> Result<(), TeslaMateProjectionStateError> {
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE; \
             PRAGMA synchronous=FULL; \
             PRAGMA foreign_keys=ON; \
             PRAGMA page_size=4096;",
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    connection
        .pragma_update(
            None,
            "max_page_count",
            i64::try_from(limits.max_state_bytes / 4096)
                .map_err(|_| TeslaMateProjectionStateError::StateCapacityOverflow)?,
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)
}

fn initialise_schema(connection: &Connection) -> Result<(), TeslaMateProjectionStateError> {
    connection
        .execute_batch(
            "CREATE TABLE current_rows (
                 entity_ordinal INTEGER NOT NULL CHECK(entity_ordinal BETWEEN 0 AND 6),
                 entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                 car_id INTEGER NOT NULL CHECK(car_id > 0),
                 projection_sha256 BLOB NOT NULL CHECK(length(projection_sha256) = 32),
                 PRIMARY KEY(entity_ordinal, entity_id),
                 UNIQUE(entity_ordinal, entity_id, car_id, projection_sha256)
             ) STRICT, WITHOUT ROWID;
             CREATE TABLE changed_rows (
                 entity_ordinal INTEGER NOT NULL CHECK(entity_ordinal BETWEEN 0 AND 6),
                 entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                 car_id INTEGER NOT NULL CHECK(car_id > 0),
                 projection_sha256 BLOB NOT NULL CHECK(length(projection_sha256) = 32),
                 payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
                 payload_bytes INTEGER NOT NULL CHECK(
                     payload_bytes >= 0
                     AND payload_bytes = length(CAST(payload_json AS BLOB))
                 ),
                 PRIMARY KEY(entity_ordinal, entity_id),
                 FOREIGN KEY(entity_ordinal, entity_id, car_id, projection_sha256)
                    REFERENCES current_rows(entity_ordinal, entity_id, car_id, projection_sha256)
                    ON DELETE CASCADE
             ) STRICT, WITHOUT ROWID;",
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)
}

/// Remove only the v1 spool runs whose ownership can be proved without
/// following symlinks. The caller must already hold the Hub publication gate:
/// a live direct import holds that same gate for its complete capture and
/// publication lifetime, so every validated run observed here is stale.
///
/// Flat pre-v1 files remain deliberately out of scope. They have no durable
/// generation binding and are safer to leave alone than to guess about.
pub(crate) fn recover_stale_import_generation_spools(
    root: &Path,
) -> Result<Vec<Uuid>, TeslaMateProjectionStateError> {
    let root_path = root.to_path_buf();
    let root_fd = open_private_directory_fd(root, &root_path)?;
    let staging_path = root.join(STAGING_DIRECTORY);
    let staging_fd =
        match open_child_private_directory_fd(&root_fd, STAGING_DIRECTORY, &staging_path) {
            Ok(fd) => fd,
            Err(TeslaMateProjectionStateError::ScopedFilesystem { source, .. })
                if source == rustix::io::Errno::NOENT =>
            {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
    let namespace_path = staging_path.join(IMPORT_GENERATION_NAMESPACE);
    let namespace_fd = match open_child_private_directory_fd(
        &staging_fd,
        IMPORT_GENERATION_NAMESPACE,
        &namespace_path,
    ) {
        Ok(fd) => fd,
        Err(TeslaMateProjectionStateError::ScopedFilesystem { source, .. })
            if source == rustix::io::Errno::NOENT =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };

    // Preflight the whole namespace before unlinking anything. A malformed
    // sibling must leave every entry and every staging row intact.
    let mut validated = Vec::new();
    let entries = Dir::read_from(&namespace_fd).map_err(|source| {
        TeslaMateProjectionStateError::ScopedFilesystem {
            path: namespace_path.clone(),
            source,
        }
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: namespace_path.clone(),
            source,
        })?;
        let Ok(name) = entry.file_name().to_str() else {
            return Err(
                TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(
                    namespace_path.clone(),
                ),
            );
        };
        if matches!(name, "." | "..") {
            continue;
        }
        validated.push(validate_stale_import_run(
            &namespace_fd,
            &namespace_path,
            name,
        )?);
    }

    let mut reclaimed = Vec::with_capacity(validated.len());
    for stale in validated {
        // Reopen by the exact, validated child name. `NOFOLLOW` and the
        // checked private directory mode make a replacement fail closed.
        let run_path = namespace_path.join(&stale.directory_name);
        let run_fd =
            open_child_private_directory_fd(&namespace_fd, &stale.directory_name, &run_path)?;
        validate_owned_import_run_fd(&run_fd, &run_path, stale.run_id, Some(&stale.children))?;
        for child in &stale.children {
            unlinkat(&run_fd, child.as_str(), AtFlags::empty()).map_err(|source| {
                TeslaMateProjectionStateError::ScopedFilesystem {
                    path: run_path.join(child),
                    source,
                }
            })?;
        }
        unlinkat(
            &namespace_fd,
            stale.directory_name.as_str(),
            AtFlags::REMOVEDIR,
        )
        .map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: run_path,
            source,
        })?;
        reclaimed.push(stale.run_id);
    }
    Ok(reclaimed)
}

fn ensure_import_generation_namespace(
    root: &Path,
    run_id: Uuid,
) -> Result<PathBuf, TeslaMateProjectionStateError> {
    ensure_exact_private_directory(root, false)?;
    let staging = root.join(STAGING_DIRECTORY);
    ensure_exact_private_directory(&staging, true)?;
    let namespace = staging.join(IMPORT_GENERATION_NAMESPACE);
    ensure_exact_private_directory(&namespace, true)?;
    let canonical_namespace = fs::canonicalize(&namespace).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: namespace.clone(),
            source,
        }
    })?;
    let run_directory = canonical_namespace.join(run_id.to_string());
    match fs::symlink_metadata(&run_directory) {
        Ok(metadata) => {
            validate_exact_private_directory(&run_directory, &metadata)?;
            validate_owner_marker(&run_directory, run_id)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&run_directory).map_err(|source| {
                TeslaMateProjectionStateError::CreateDirectory {
                    path: run_directory.clone(),
                    source,
                }
            })?;
            if let Err(error) = set_exact_private_directory_permissions(&run_directory)
                .and_then(|()| write_owner_marker(&run_directory, run_id))
            {
                // Do not recursively remove a partially-created path. This
                // best-effort cleanup can only remove the exact empty/new
                // directory, leaving an unexpected entry for fail-closed
                // startup recovery.
                let _ = fs::remove_file(run_directory.join(OWNER_FILE_NAME));
                let _ = fs::remove_dir(&run_directory);
                return Err(error);
            }
        }
        Err(source) => {
            return Err(TeslaMateProjectionStateError::InspectPath {
                path: run_directory,
                source,
            });
        }
    }
    Ok(canonical_namespace)
}

fn write_owner_marker(
    run_directory: &Path,
    run_id: Uuid,
) -> Result<(), TeslaMateProjectionStateError> {
    let marker = TeslaMateProjectionStateOwner {
        schema: OWNER_SCHEMA,
        kind: OWNER_KIND.to_owned(),
        run_id: run_id.to_string(),
    };
    let encoded =
        serde_json::to_vec(&marker).map_err(TeslaMateProjectionStateError::SerializeOwner)?;
    let path = run_directory.join(OWNER_FILE_NAME);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file =
        options
            .open(&path)
            .map_err(|source| TeslaMateProjectionStateError::CreateFile {
                path: path.clone(),
                source,
            })?;
    file.write_all(&encoded)
        .map_err(|source| TeslaMateProjectionStateError::WriteOwnerMarker {
            path: path.clone(),
            source,
        })?;
    file.sync_all()
        .map_err(|source| TeslaMateProjectionStateError::WriteOwnerMarker {
            path: path.clone(),
            source,
        })?;
    let metadata = fs::symlink_metadata(&path).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: path.clone(),
            source,
        }
    })?;
    validate_exact_private_file(&path, &metadata)
}

fn validate_import_generation_transfer_path(
    path: &Path,
    ownership: &TeslaMateProjectionStateImportOwnership,
) -> Result<PathBuf, TeslaMateProjectionStateError> {
    if ownership.run_id.is_nil() || ownership.attempt_id.is_nil() {
        return Err(TeslaMateProjectionStateError::InvalidImportGenerationRunId);
    }
    let run_name = ownership.run_id.to_string();
    let expected_file = format!("{}.{STATE_FILE_EXTENSION}", ownership.attempt_id);
    let expected_path = ownership
        .namespace_root
        .join(&run_name)
        .join(&expected_file);
    if path != expected_path {
        return Err(
            TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf()),
        );
    }
    if ownership
        .namespace_root
        .file_name()
        .and_then(|name| name.to_str())
        != Some(IMPORT_GENERATION_NAMESPACE)
    {
        return Err(
            TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf()),
        );
    }
    validate_exact_private_directory(
        &ownership.namespace_root,
        &fs::symlink_metadata(&ownership.namespace_root).map_err(|source| {
            TeslaMateProjectionStateError::InspectPath {
                path: ownership.namespace_root.clone(),
                source,
            }
        })?,
    )?;
    let staging = ownership.namespace_root.parent().ok_or_else(|| {
        TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf())
    })?;
    if staging.file_name().and_then(|name| name.to_str()) != Some(STAGING_DIRECTORY) {
        return Err(
            TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf()),
        );
    }
    validate_exact_private_directory(
        staging,
        &fs::symlink_metadata(staging).map_err(|source| {
            TeslaMateProjectionStateError::InspectPath {
                path: staging.to_path_buf(),
                source,
            }
        })?,
    )?;
    let root = staging.parent().ok_or_else(|| {
        TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf())
    })?;
    validate_exact_private_directory(
        root,
        &fs::symlink_metadata(root).map_err(|source| {
            TeslaMateProjectionStateError::InspectPath {
                path: root.to_path_buf(),
                source,
            }
        })?,
    )?;
    let canonical_namespace = fs::canonicalize(&ownership.namespace_root).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: ownership.namespace_root.clone(),
            source,
        }
    })?;
    if canonical_namespace != ownership.namespace_root {
        return Err(
            TeslaMateProjectionStateError::ImportGenerationNamespaceChanged {
                expected: ownership.namespace_root.clone(),
                actual: canonical_namespace,
            },
        );
    }
    let run_directory = ownership.namespace_root.join(&run_name);
    let run_metadata = fs::symlink_metadata(&run_directory).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: run_directory.clone(),
            source,
        }
    })?;
    validate_exact_private_directory(&run_directory, &run_metadata)?;
    validate_owner_marker(&run_directory, ownership.run_id)?;
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        }
    })?;
    validate_exact_private_file(path, &metadata)?;
    let canonical_file =
        fs::canonicalize(path).map_err(|source| TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        })?;
    let canonical_run = fs::canonicalize(&run_directory).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: run_directory,
            source,
        }
    })?;
    if canonical_file.parent() != Some(canonical_run.as_path())
        || canonical_file.file_name().and_then(|name| name.to_str()) != Some(expected_file.as_str())
    {
        return Err(
            TeslaMateProjectionStateError::InvalidImportGenerationTransferPath(path.to_path_buf()),
        );
    }
    Ok(canonical_file)
}

fn validate_owner_marker(
    run_directory: &Path,
    expected_run_id: Uuid,
) -> Result<(), TeslaMateProjectionStateError> {
    let path = run_directory.join(OWNER_FILE_NAME);
    let fd = open(
        &path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
        path: path.clone(),
        source,
    })?;
    validate_owner_marker_file(std::fs::File::from(fd), &path, expected_run_id)
}

fn validate_owner_marker_file(
    mut file: std::fs::File,
    path: &Path,
    expected_run_id: Uuid,
) -> Result<(), TeslaMateProjectionStateError> {
    let metadata =
        file.metadata()
            .map_err(|source| TeslaMateProjectionStateError::InspectPath {
                path: path.to_path_buf(),
                source,
            })?;
    validate_exact_private_file(path, &metadata)?;
    let mut bytes = Vec::new();
    let mut limited = (&mut file).take(MAX_OWNER_MARKER_BYTES.saturating_add(1));
    limited.read_to_end(&mut bytes).map_err(|source| {
        TeslaMateProjectionStateError::ReadOwnerMarker {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if u64::try_from(bytes.len()).expect("marker length fits u64") > MAX_OWNER_MARKER_BYTES {
        return Err(TeslaMateProjectionStateError::InvalidOwnerMarker(
            path.to_path_buf(),
        ));
    }
    let owner: TeslaMateProjectionStateOwner = serde_json::from_slice(&bytes)
        .map_err(|_| TeslaMateProjectionStateError::InvalidOwnerMarker(path.to_path_buf()))?;
    if owner.schema != OWNER_SCHEMA
        || owner.kind != OWNER_KIND
        || owner.run_id != expected_run_id.to_string()
    {
        return Err(TeslaMateProjectionStateError::InvalidOwnerMarker(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

fn validate_stale_import_run(
    namespace_fd: &impl std::os::fd::AsFd,
    namespace_path: &Path,
    name: &str,
) -> Result<ValidatedStaleImportRun, TeslaMateProjectionStateError> {
    let run_id = parse_canonical_uuid_component(name, namespace_path)?;
    let run_path = namespace_path.join(name);
    let run_fd = open_child_private_directory_fd(namespace_fd, name, &run_path)?;
    let children = validate_owned_import_run_fd(&run_fd, &run_path, run_id, None)?;
    Ok(ValidatedStaleImportRun {
        run_id,
        directory_name: name.to_owned(),
        children,
    })
}

fn validate_owned_import_run_fd(
    run_fd: &impl std::os::fd::AsFd,
    run_path: &Path,
    expected_run_id: Uuid,
    expected_children: Option<&[String]>,
) -> Result<Vec<String>, TeslaMateProjectionStateError> {
    validate_private_directory_stat(
        run_path,
        fstat(run_fd).map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: run_path.to_path_buf(),
            source,
        })?,
    )?;
    let owner_path = run_path.join(OWNER_FILE_NAME);
    let owner_fd = openat(
        run_fd,
        OWNER_FILE_NAME,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
        path: owner_path.clone(),
        source,
    })?;
    validate_owner_marker_file(std::fs::File::from(owner_fd), &owner_path, expected_run_id)?;

    let mut children = Vec::new();
    let mut main_attempts = HashSet::new();
    let mut sidecar_attempts = Vec::new();
    let entries = Dir::read_from(run_fd).map_err(|source| {
        TeslaMateProjectionStateError::ScopedFilesystem {
            path: run_path.to_path_buf(),
            source,
        }
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: run_path.to_path_buf(),
            source,
        })?;
        let Ok(name) = entry.file_name().to_str() else {
            return Err(
                TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(
                    run_path.to_path_buf(),
                ),
            );
        };
        if matches!(name, "." | "..") {
            continue;
        }
        let path = run_path.join(name);
        if name == OWNER_FILE_NAME {
            let stat = statat(run_fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|source| {
                TeslaMateProjectionStateError::ScopedFilesystem {
                    path: path.clone(),
                    source,
                }
            })?;
            validate_private_regular_file_stat(&path, stat)?;
            children.push(name.to_owned());
            continue;
        }
        let (attempt_id, is_main) = parse_import_spool_child(name, run_path)?;
        let stat = statat(run_fd, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|source| {
            TeslaMateProjectionStateError::ScopedFilesystem {
                path: path.clone(),
                source,
            }
        })?;
        validate_private_regular_file_stat(&path, stat)?;
        if is_main {
            main_attempts.insert(attempt_id);
        } else {
            sidecar_attempts.push(attempt_id);
        }
        children.push(name.to_owned());
    }
    if !children.iter().any(|name| name == OWNER_FILE_NAME)
        || sidecar_attempts
            .iter()
            .any(|attempt| !main_attempts.contains(attempt))
    {
        return Err(
            TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(run_path.to_path_buf()),
        );
    }
    children.sort_unstable();
    if let Some(expected) = expected_children
        && children.as_slice() != expected
    {
        return Err(
            TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(run_path.to_path_buf()),
        );
    }
    Ok(children)
}

fn parse_canonical_uuid_component(
    value: &str,
    parent: &Path,
) -> Result<Uuid, TeslaMateProjectionStateError> {
    let id = Uuid::parse_str(value).map_err(|_| {
        TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(parent.join(value))
    })?;
    if id.is_nil() || id.to_string() != value {
        return Err(
            TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(parent.join(value)),
        );
    }
    Ok(id)
}

fn parse_import_spool_child(
    value: &str,
    parent: &Path,
) -> Result<(Uuid, bool), TeslaMateProjectionStateError> {
    let (stem, is_main) =
        if let Some(stem) = value.strip_suffix(&format!(".{STATE_FILE_EXTENSION}")) {
            (stem, true)
        } else if let Some(stem) = value.strip_suffix(SQLITE_JOURNAL_SUFFIX) {
            (stem, false)
        } else if let Some(stem) = value.strip_suffix(SQLITE_WAL_SUFFIX) {
            (stem, false)
        } else if let Some(stem) = value.strip_suffix(SQLITE_SHM_SUFFIX) {
            (stem, false)
        } else {
            return Err(
                TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(parent.join(value)),
            );
        };
    Ok((parse_canonical_uuid_component(stem, parent)?, is_main))
}

fn open_private_directory_fd(
    path: &Path,
    display_path: &Path,
) -> Result<rustix::fd::OwnedFd, TeslaMateProjectionStateError> {
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
        path: display_path.to_path_buf(),
        source,
    })?;
    validate_private_directory_stat(
        display_path,
        fstat(&fd).map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: display_path.to_path_buf(),
            source,
        })?,
    )?;
    Ok(fd)
}

fn open_child_private_directory_fd(
    parent_fd: &impl std::os::fd::AsFd,
    name: &str,
    display_path: &Path,
) -> Result<rustix::fd::OwnedFd, TeslaMateProjectionStateError> {
    let fd = openat(
        parent_fd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
        path: display_path.to_path_buf(),
        source,
    })?;
    validate_private_directory_stat(
        display_path,
        fstat(&fd).map_err(|source| TeslaMateProjectionStateError::ScopedFilesystem {
            path: display_path.to_path_buf(),
            source,
        })?,
    )?;
    Ok(fd)
}

fn cleanup_empty_import_generation_run(
    ownership: &TeslaMateProjectionStateOwnership,
) -> Result<(), TeslaMateProjectionStateError> {
    let TeslaMateProjectionStateOwnership::ImportGeneration(ownership) = ownership else {
        return Ok(());
    };
    let run_directory = ownership.namespace_root.join(ownership.run_id.to_string());
    let metadata = match fs::symlink_metadata(&run_directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(TeslaMateProjectionStateError::InspectPath {
                path: run_directory,
                source,
            });
        }
    };
    validate_exact_private_directory(&run_directory, &metadata)?;
    validate_owner_marker(&run_directory, ownership.run_id)?;
    let mut entries = fs::read_dir(&run_directory).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: run_directory.clone(),
            source,
        }
    })?;
    let Some(entry) = entries.next() else {
        return Err(TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(run_directory));
    };
    let entry = entry.map_err(|source| TeslaMateProjectionStateError::InspectPath {
        path: run_directory.clone(),
        source,
    })?;
    if entry.file_name() != OWNER_FILE_NAME || entries.next().is_some() {
        return Ok(());
    }
    let marker = run_directory.join(OWNER_FILE_NAME);
    fs::remove_file(&marker).map_err(|source| TeslaMateProjectionStateError::RemoveFile {
        path: marker,
        source,
    })?;
    fs::remove_dir(&run_directory).map_err(|source| {
        TeslaMateProjectionStateError::RemoveDirectory {
            path: run_directory,
            source,
        }
    })
}

fn validate_exact_private_directory(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), TeslaMateProjectionStateError> {
    if metadata.file_type().is_symlink() {
        return Err(TeslaMateProjectionStateError::SymlinkPath(
            path.to_path_buf(),
        ));
    }
    if !metadata.is_dir() {
        return Err(TeslaMateProjectionStateError::ExpectedDirectory(
            path.to_path_buf(),
        ));
    }
    require_exact_private_permissions(path, metadata, 0o700)
}

fn validate_exact_private_file(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), TeslaMateProjectionStateError> {
    if metadata.file_type().is_symlink() {
        return Err(TeslaMateProjectionStateError::SymlinkPath(
            path.to_path_buf(),
        ));
    }
    if !metadata.is_file() {
        return Err(TeslaMateProjectionStateError::ExpectedFile(
            path.to_path_buf(),
        ));
    }
    require_exact_private_permissions(path, metadata, 0o600)
}

fn ensure_exact_private_directory(
    path: &Path,
    create_if_missing: bool,
) -> Result<(), TeslaMateProjectionStateError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_exact_private_directory(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create_if_missing => {
            fs::create_dir(path).map_err(|source| {
                TeslaMateProjectionStateError::CreateDirectory {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
            set_exact_private_directory_permissions(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(
            TeslaMateProjectionStateError::ExpectedDirectory(path.to_path_buf()),
        ),
        Err(source) => Err(TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[cfg(unix)]
fn require_exact_private_permissions(
    path: &Path,
    metadata: &fs::Metadata,
    expected: u32,
) -> Result<(), TeslaMateProjectionStateError> {
    if metadata.permissions().mode() & 0o777 != expected {
        return Err(
            TeslaMateProjectionStateError::ImportGenerationPathNotPrivate(path.to_path_buf()),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_exact_private_permissions(
    _path: &Path,
    _metadata: &fs::Metadata,
    _expected: u32,
) -> Result<(), TeslaMateProjectionStateError> {
    Ok(())
}

#[cfg(unix)]
fn set_exact_private_directory_permissions(
    path: &Path,
) -> Result<(), TeslaMateProjectionStateError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        TeslaMateProjectionStateError::SetPermissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_exact_private_directory_permissions(
    _path: &Path,
) -> Result<(), TeslaMateProjectionStateError> {
    Ok(())
}

fn validate_private_directory_stat(
    path: &Path,
    stat: rustix::fs::Stat,
) -> Result<(), TeslaMateProjectionStateError> {
    if !FileType::from_raw_mode(stat.st_mode).is_dir()
        || (Mode::from_raw_mode(stat.st_mode).as_raw_mode() as u32 & 0o777) != 0o700
    {
        return Err(
            TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(path.to_path_buf()),
        );
    }
    Ok(())
}

fn validate_private_regular_file_stat(
    path: &Path,
    stat: rustix::fs::Stat,
) -> Result<(), TeslaMateProjectionStateError> {
    if !FileType::from_raw_mode(stat.st_mode).is_file()
        || (Mode::from_raw_mode(stat.st_mode).as_raw_mode() as u32 & 0o777) != 0o600
    {
        return Err(
            TeslaMateProjectionStateError::UnsafeImportGenerationNamespace(path.to_path_buf()),
        );
    }
    Ok(())
}

fn attached_transfer_path(
    connection: &Connection,
) -> Result<PathBuf, TeslaMateProjectionStateError> {
    let rows = connection
        .prepare("PRAGMA database_list")
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    let path = rows
        .into_iter()
        .find_map(|(schema, path)| {
            (schema == TESLAMATE_PROJECTION_STATE_ATTACHMENT_SCHEMA).then_some(PathBuf::from(path))
        })
        .ok_or(TeslaMateProjectionStateError::TransferAttachmentMissing)?;
    fs::canonicalize(&path)
        .map_err(|source| TeslaMateProjectionStateError::InspectPath { path, source })
}

fn validate_private_transfer_path(path: &Path) -> Result<PathBuf, TeslaMateProjectionStateError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(TeslaMateProjectionStateError::SymlinkPath(
            path.to_path_buf(),
        ));
    }
    if !metadata.is_file() {
        return Err(TeslaMateProjectionStateError::ExpectedFile(
            path.to_path_buf(),
        ));
    }
    if path.extension().and_then(|extension| extension.to_str()) != Some(STATE_FILE_EXTENSION) {
        return Err(TeslaMateProjectionStateError::InvalidTransferPath(
            path.to_path_buf(),
        ));
    }
    require_private_transfer_permissions(path, &metadata)?;

    let parent = path
        .parent()
        .ok_or_else(|| TeslaMateProjectionStateError::InvalidTransferPath(path.to_path_buf()))?;
    if parent.file_name().and_then(|name| name.to_str()) != Some(STAGING_DIRECTORY) {
        return Err(TeslaMateProjectionStateError::InvalidTransferPath(
            path.to_path_buf(),
        ));
    }
    let parent_metadata = fs::symlink_metadata(parent).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: parent.to_path_buf(),
            source,
        }
    })?;
    if parent_metadata.file_type().is_symlink() {
        return Err(TeslaMateProjectionStateError::SymlinkPath(
            parent.to_path_buf(),
        ));
    }
    if !parent_metadata.is_dir() {
        return Err(TeslaMateProjectionStateError::ExpectedDirectory(
            parent.to_path_buf(),
        ));
    }
    require_private_transfer_permissions(parent, &parent_metadata)?;

    let canonical_parent =
        fs::canonicalize(parent).map_err(|source| TeslaMateProjectionStateError::InspectPath {
            path: parent.to_path_buf(),
            source,
        })?;
    let canonical_path =
        fs::canonicalize(path).map_err(|source| TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        })?;
    if canonical_path.parent() != Some(canonical_parent.as_path())
        || canonical_path.file_name() != path.file_name()
    {
        return Err(TeslaMateProjectionStateError::InvalidTransferPath(
            path.to_path_buf(),
        ));
    }
    Ok(canonical_path)
}

#[cfg(unix)]
fn require_private_transfer_permissions(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), TeslaMateProjectionStateError> {
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(TeslaMateProjectionStateError::TransferPathNotPrivate(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn require_private_transfer_permissions(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), TeslaMateProjectionStateError> {
    Ok(())
}

fn validate_transfer_database(
    connection: &Connection,
    schema: &str,
    expected: TeslaMateProjectionStateStats,
    selected_car_id: i64,
) -> Result<(), TeslaMateProjectionStateError> {
    debug_assert!(matches!(
        schema,
        "main" | TESLAMATE_PROJECTION_STATE_ATTACHMENT_SCHEMA
    ));
    validate_transfer_schema(connection, schema)?;

    let integrity: String = connection
        .query_row(&format!("PRAGMA {schema}.integrity_check"), [], |row| {
            row.get(0)
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if integrity != "ok" {
        return Err(TeslaMateProjectionStateError::IntegrityCheckFailed(
            integrity,
        ));
    }
    let foreign_key_violation = connection
        .prepare(&format!("PRAGMA {schema}.foreign_key_check"))
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .exists([])
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if foreign_key_violation {
        return Err(TeslaMateProjectionStateError::ForeignKeyCheckFailed);
    }

    let current_rows = qualified_transfer_table(schema, "current_rows");
    let changed_rows = qualified_transfer_table(schema, "changed_rows");
    let (row_count, changed_row_count, changed_payload_bytes): (i64, i64, i64) = connection
        .query_row(
            &format!(
                "SELECT \
                    (SELECT COUNT(*) FROM {current_rows}), \
                    (SELECT COUNT(*) FROM {changed_rows}), \
                    (SELECT COALESCE(SUM(payload_bytes), 0) FROM {changed_rows})"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    let actual = TeslaMateProjectionStateStats {
        row_count: u64::try_from(row_count)
            .map_err(|_| TeslaMateProjectionStateError::InvalidStoredAccounting)?,
        changed_row_count: u64::try_from(changed_row_count)
            .map_err(|_| TeslaMateProjectionStateError::InvalidStoredAccounting)?,
        changed_payload_bytes: u64::try_from(changed_payload_bytes)
            .map_err(|_| TeslaMateProjectionStateError::InvalidStoredAccounting)?,
        sealed: true,
    };
    if actual != expected {
        return Err(TeslaMateProjectionStateError::TransferAccountingMismatch);
    }

    let invalid_current: bool = connection
        .query_row(
            &format!(
                "SELECT EXISTS(
                    SELECT 1 FROM {current_rows}
                     WHERE typeof(entity_ordinal) <> 'integer'
                        OR entity_ordinal NOT BETWEEN 0 AND 6
                        OR typeof(entity_id) <> 'integer'
                        OR entity_id <= 0
                        OR typeof(car_id) <> 'integer'
                        OR car_id <> ?1
                        OR typeof(projection_sha256) <> 'blob'
                        OR length(projection_sha256) <> 32
                        OR (entity_ordinal = 0 AND entity_id <> ?1)
                )"
            ),
            params![selected_car_id],
            |row| row.get(0),
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if invalid_current {
        return Err(TeslaMateProjectionStateError::TransferRowContractMismatch);
    }
    let cars: i64 = connection
        .query_row(
            &format!(
                "SELECT COUNT(*) FROM {current_rows}
                  WHERE entity_ordinal = 0 AND entity_id = ?1 AND car_id = ?1"
            ),
            params![selected_car_id],
            |row| row.get(0),
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if cars != 1 {
        return Err(TeslaMateProjectionStateError::TransferCarContractMismatch);
    }

    let invalid_changed: bool = connection
        .query_row(
            &format!(
                "SELECT EXISTS(
                    SELECT 1
                      FROM {changed_rows} AS changed
                 LEFT JOIN {current_rows} AS current
                        ON current.entity_ordinal = changed.entity_ordinal
                       AND current.entity_id = changed.entity_id
                     WHERE typeof(changed.entity_ordinal) <> 'integer'
                        OR changed.entity_ordinal NOT BETWEEN 0 AND 6
                        OR typeof(changed.entity_id) <> 'integer'
                        OR changed.entity_id <= 0
                        OR typeof(changed.car_id) <> 'integer'
                        OR changed.car_id <> ?1
                        OR typeof(changed.projection_sha256) <> 'blob'
                        OR length(changed.projection_sha256) <> 32
                        OR typeof(changed.payload_json) <> 'text'
                        OR json_valid(changed.payload_json) <> 1
                        OR typeof(changed.payload_bytes) <> 'integer'
                        OR changed.payload_bytes < 0
                        OR changed.payload_bytes <> length(CAST(changed.payload_json AS BLOB))
                        OR current.entity_id IS NULL
                        OR current.car_id <> changed.car_id
                        OR current.projection_sha256 <> changed.projection_sha256
                )"
            ),
            params![selected_car_id],
            |row| row.get(0),
        )
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if invalid_changed {
        return Err(TeslaMateProjectionStateError::TransferRowContractMismatch);
    }
    Ok(())
}

fn validate_transfer_schema(
    connection: &Connection,
    schema: &str,
) -> Result<(), TeslaMateProjectionStateError> {
    let objects = connection
        .prepare(&format!(
            "SELECT type, name, tbl_name, sql
               FROM {schema}.sqlite_schema
              WHERE name NOT LIKE 'sqlite_%'
              ORDER BY type ASC, name ASC"
        ))
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if objects.len() != 2
        || objects.iter().any(|(kind, name, table, sql)| {
            kind != "table"
                || table != name
                || !matches!(name.as_str(), "changed_rows" | "current_rows")
                || !sql
                    .as_deref()
                    .is_some_and(|sql| sql.contains("STRICT") && sql.contains("WITHOUT ROWID"))
        })
    {
        return Err(TeslaMateProjectionStateError::TransferSchemaMismatch);
    }
    validate_transfer_table_columns(
        connection,
        schema,
        "current_rows",
        &[
            ("entity_ordinal", "INTEGER", 1_i64),
            ("entity_id", "INTEGER", 2_i64),
            ("car_id", "INTEGER", 0_i64),
            ("projection_sha256", "BLOB", 0_i64),
        ],
    )?;
    validate_transfer_table_columns(
        connection,
        schema,
        "changed_rows",
        &[
            ("entity_ordinal", "INTEGER", 1_i64),
            ("entity_id", "INTEGER", 2_i64),
            ("car_id", "INTEGER", 0_i64),
            ("projection_sha256", "BLOB", 0_i64),
            ("payload_json", "TEXT", 0_i64),
            ("payload_bytes", "INTEGER", 0_i64),
        ],
    )?;
    let foreign_keys = connection
        .prepare(&format!("PRAGMA {schema}.foreign_key_list(changed_rows)"))
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    let expected_foreign_keys = HashSet::from([
        (
            "current_rows".to_owned(),
            "entity_ordinal".to_owned(),
            "entity_ordinal".to_owned(),
            "CASCADE".to_owned(),
        ),
        (
            "current_rows".to_owned(),
            "entity_id".to_owned(),
            "entity_id".to_owned(),
            "CASCADE".to_owned(),
        ),
        (
            "current_rows".to_owned(),
            "car_id".to_owned(),
            "car_id".to_owned(),
            "CASCADE".to_owned(),
        ),
        (
            "current_rows".to_owned(),
            "projection_sha256".to_owned(),
            "projection_sha256".to_owned(),
            "CASCADE".to_owned(),
        ),
    ]);
    if foreign_keys.len() != expected_foreign_keys.len()
        || foreign_keys.into_iter().collect::<HashSet<_>>() != expected_foreign_keys
    {
        return Err(TeslaMateProjectionStateError::TransferSchemaMismatch);
    }
    Ok(())
}

fn validate_transfer_table_columns(
    connection: &Connection,
    schema: &str,
    table: &str,
    expected: &[(&str, &str, i64)],
) -> Result<(), TeslaMateProjectionStateError> {
    let columns = connection
        .prepare(&format!("PRAGMA {schema}.table_info({table})"))
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(TeslaMateProjectionStateError::Sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(TeslaMateProjectionStateError::Sqlite)?;
    if columns.len() != expected.len()
        || columns.iter().zip(expected).enumerate().any(
            |(
                index,
                (
                    (cid, name, ty, not_null, default, primary_key),
                    (expected_name, expected_ty, expected_primary_key),
                ),
            )| {
                *cid != i64::try_from(index).expect("column index fits i64")
                    || name != expected_name
                    || ty != expected_ty
                    || *not_null != 1
                    || default.is_some()
                    || primary_key != expected_primary_key
            },
        )
    {
        return Err(TeslaMateProjectionStateError::TransferSchemaMismatch);
    }
    Ok(())
}

fn transfer_semantic_digest(
    connection: &Connection,
    schema: &str,
) -> Result<Sha256Digest, TeslaMateProjectionStateError> {
    let mut digest = Sha256::new();
    digest.update(TRANSFER_DIGEST_DOMAIN);
    digest.update([0]);
    for (tag, table, columns) in [
        (
            b'c',
            "current_rows",
            "entity_ordinal, entity_id, car_id, projection_sha256",
        ),
        (
            b'h',
            "changed_rows",
            "entity_ordinal, entity_id, car_id, projection_sha256, payload_json, payload_bytes",
        ),
    ] {
        digest.update([tag]);
        let table = qualified_transfer_table(schema, table);
        let mut statement = connection
            .prepare(&format!(
                "SELECT {columns} FROM {table} ORDER BY entity_ordinal, entity_id"
            ))
            .map_err(TeslaMateProjectionStateError::Sqlite)?;
        let mut rows = statement
            .query([])
            .map_err(TeslaMateProjectionStateError::Sqlite)?;
        while let Some(row) = rows.next().map_err(TeslaMateProjectionStateError::Sqlite)? {
            let entity_ordinal = row
                .get::<_, i64>(0)
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            let entity_id = row
                .get::<_, i64>(1)
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            let car_id = row
                .get::<_, i64>(2)
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            let row_digest = row
                .get::<_, Vec<u8>>(3)
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            digest.update(entity_ordinal.to_be_bytes());
            digest.update(entity_id.to_be_bytes());
            digest.update(car_id.to_be_bytes());
            digest.update(
                u64::try_from(row_digest.len())
                    .expect("digest length fits u64")
                    .to_be_bytes(),
            );
            digest.update(row_digest);
            if tag == b'h' {
                let payload = row
                    .get::<_, String>(4)
                    .map_err(TeslaMateProjectionStateError::Sqlite)?;
                let payload_bytes = row
                    .get::<_, i64>(5)
                    .map_err(TeslaMateProjectionStateError::Sqlite)?;
                digest.update(payload_bytes.to_be_bytes());
                digest.update(
                    u64::try_from(payload.len())
                        .expect("payload length fits u64")
                        .to_be_bytes(),
                );
                digest.update(payload.as_bytes());
            }
        }
    }
    Ok(Sha256Digest::from_bytes(digest.finalize().into()))
}

fn qualified_transfer_table(schema: &str, table: &str) -> String {
    debug_assert!(matches!(
        schema,
        "main" | TESLAMATE_PROJECTION_STATE_ATTACHMENT_SCHEMA
    ));
    debug_assert!(matches!(table, "current_rows" | "changed_rows"));
    format!("{schema}.{table}")
}

fn is_unique_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == ErrorCode::ConstraintViolation
    )
}

fn ensure_existing_directory(path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(TeslaMateProjectionStateError::SymlinkPath(
            path.to_path_buf(),
        ));
    }
    if !metadata.is_dir() {
        return Err(TeslaMateProjectionStateError::ExpectedDirectory(
            path.to_path_buf(),
        ));
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(TeslaMateProjectionStateError::SymlinkPath(
                    path.to_path_buf(),
                ));
            }
            if !metadata.is_dir() {
                return Err(TeslaMateProjectionStateError::ExpectedDirectory(
                    path.to_path_buf(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|source| {
                TeslaMateProjectionStateError::CreateDirectory {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
        }
        Err(source) => {
            return Err(TeslaMateProjectionStateError::InspectPath {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    set_private_directory_permissions(path)
}

fn ensure_private_file(path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        TeslaMateProjectionStateError::InspectPath {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if metadata.file_type().is_symlink() {
        return Err(TeslaMateProjectionStateError::SymlinkPath(
            path.to_path_buf(),
        ));
    }
    if !metadata.is_file() {
        return Err(TeslaMateProjectionStateError::ExpectedFile(
            path.to_path_buf(),
        ));
    }
    set_private_file_permissions(path)
}

fn available_bytes(path: &Path) -> Result<u64, TeslaMateProjectionStateError> {
    let stats = rustix::fs::statvfs(path).map_err(|source| {
        TeslaMateProjectionStateError::FilesystemSpace {
            path: path.to_path_buf(),
            source,
        }
    })?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or(TeslaMateProjectionStateError::StateCapacityOverflow)
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        TeslaMateProjectionStateError::SetPermissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        TeslaMateProjectionStateError::SetPermissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<(), TeslaMateProjectionStateError> {
    Ok(())
}

#[derive(Debug, Error)]
pub enum TeslaMateProjectionStateError {
    #[error("projection-state maximum rows must be positive")]
    InvalidMaximumRows,
    #[error("projection-state maximum database bytes must be at least {minimum}")]
    InvalidMaximumStateBytes { minimum: u64 },
    #[error("projection-state changed payload budget must be positive")]
    InvalidMaximumChangedPayloadBytes,
    #[error("projection-state changed-row payload cap must be positive and fit SQLite accounting")]
    InvalidChangedRowPayloadLimit,
    #[error("projection-state changed payload budget exceeds total state capacity")]
    ChangedPayloadBudgetExceedsStateCapacity,
    #[error("projection-state capacity calculation overflowed")]
    StateCapacityOverflow,
    #[error("could not create private projection-state directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not create private projection-state file {path}: {source}")]
    CreateFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not inspect projection-state path {path}: {source}")]
    InspectPath {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not set private permissions on {path}: {source}")]
    SetPermissions {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("projection-state path may not be a symlink: {0}")]
    SymlinkPath(PathBuf),
    #[error("expected projection-state directory at {0}")]
    ExpectedDirectory(PathBuf),
    #[error("expected projection-state file at {0}")]
    ExpectedFile(PathBuf),
    #[error("projection-state transfer path is not a canonical private spool: {0}")]
    InvalidTransferPath(PathBuf),
    #[error("projection-state transfer path is not private to the current Hub user: {0}")]
    TransferPathNotPrivate(PathBuf),
    #[error("direct-import projection-state run id must be a non-nil UUID")]
    InvalidImportGenerationRunId,
    #[error("direct-import finalization requires a run-bound projection-state spool")]
    ImportGenerationTransferRequired,
    #[error("direct-import projection-state spool belongs to run {expected}, not {actual}")]
    ImportGenerationRunMismatch { expected: Uuid, actual: Uuid },
    #[error("direct-import projection-state transfer path is outside its owned v1 namespace: {0}")]
    InvalidImportGenerationTransferPath(PathBuf),
    #[error(
        "direct-import projection-state namespace changed after creation (expected {expected}, got {actual})"
    )]
    ImportGenerationNamespaceChanged { expected: PathBuf, actual: PathBuf },
    #[error("direct-import projection-state path is not exactly private: {0}")]
    ImportGenerationPathNotPrivate(PathBuf),
    #[error("direct-import projection-state namespace is malformed or unsafe: {0}")]
    UnsafeImportGenerationNamespace(PathBuf),
    #[error("direct-import projection-state owner marker is invalid: {0}")]
    InvalidOwnerMarker(PathBuf),
    #[error("could not serialize direct-import projection-state owner marker: {0}")]
    SerializeOwner(serde_json::Error),
    #[error("could not write direct-import projection-state owner marker {path}: {source}")]
    WriteOwnerMarker {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not read direct-import projection-state owner marker {path}: {source}")]
    ReadOwnerMarker {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not safely access direct-import projection-state path {path}: {source}")]
    ScopedFilesystem {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("projection-state transfer attachment is missing")]
    TransferAttachmentMissing,
    #[error(
        "projection-state transfer attachment path changed (expected {expected}, got {actual})"
    )]
    TransferAttachmentPathChanged { expected: PathBuf, actual: PathBuf },
    #[error(
        "projection-state transfer path changed after validation (expected {expected}, got {actual})"
    )]
    TransferPathChanged { expected: PathBuf, actual: PathBuf },
    #[error("projection-state transfer schema does not match the sealed spool contract")]
    TransferSchemaMismatch,
    #[error("projection-state transfer accounting no longer matches its sealed descriptor")]
    TransferAccountingMismatch,
    #[error("projection-state transfer rows violate their source/car/digest contract")]
    TransferRowContractMismatch,
    #[error("projection-state transfer must contain exactly one selected-car row")]
    TransferCarContractMismatch,
    #[error("projection-state transfer attachment does not match the sealed spool descriptor")]
    TransferDigestMismatch,
    #[error("could not inspect free space for {path}: {source}")]
    FilesystemSpace {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("projection-state needs {required} free bytes but only {available} are available")]
    InsufficientFreeSpace { required: u64, available: u64 },
    #[error("projection-state SQLite error: {0}")]
    Sqlite(#[source] rusqlite::Error),
    #[error("projection-state row id must be positive")]
    InvalidRowId,
    #[error("projection-state car id must be positive")]
    InvalidCarId,
    #[error("projection-state received conflicting values for {entity:?} row {id}")]
    ConflictingRow {
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    },
    #[error("projection-state was modified concurrently at {entity:?} row {id}")]
    ConcurrentWrite {
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    },
    #[error("projection-state changed payload was modified concurrently at {entity:?} row {id}")]
    ConcurrentChangedWrite {
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    },
    #[error("projection-state row limit exceeded ({maximum})")]
    RowLimitExceeded { maximum: u64 },
    #[error("projection-state changed payload limit exceeded ({maximum} bytes)")]
    ChangedPayloadLimitExceeded { maximum: u64 },
    #[error(
        "projection-state changed payload row is too large ({payload_bytes} bytes; maximum {maximum})"
    )]
    ChangedPayloadRowLimitExceeded { maximum: u64, payload_bytes: u64 },
    #[error(
        "projection-state changed-page payload cap must be between 1 and {maximum} bytes (requested {requested})"
    )]
    InvalidChangedPagePayloadLimit { maximum: u64, requested: u64 },
    #[error("projection-state changed page exceeded its payload cap ({maximum} bytes)")]
    ChangedPagePayloadLimitExceeded { maximum: u64 },
    #[error("projection-state canonical payload must be UTF-8: {0}")]
    CanonicalPayloadUtf8(std::str::Utf8Error),
    #[error("could not serialize projection-state row: {0}")]
    SerializeRow(serde_json::Error),
    #[error("projection-state payload must serialize to a JSON object")]
    PayloadMustBeJsonObject,
    #[error("projection-state is sealed and cannot accept rows")]
    StateSealed,
    #[error("projection-state write batch failed and must be discarded")]
    WriteBatchFailed,
    #[error("projection-state must be sealed before it can be read")]
    StateNotSealed,
    #[error("projection-state page size must be between 1 and {MAX_PAGE_SIZE}")]
    InvalidPageSize,
    #[error("projection-state cursor id must be positive")]
    InvalidCursor,
    #[error("projection-state stored entity ordinal is invalid: {0}")]
    InvalidStoredEntity(i64),
    #[error("projection-state stored entity name is invalid: {0}")]
    InvalidStoredEntityName(String),
    #[error("projection-state stored digest is invalid")]
    InvalidStoredDigest,
    #[error("projection-state internal table selection is invalid")]
    InvalidStoredTable,
    #[error("projection-state accounting does not match its persisted rows")]
    PersistedAccountingMismatch,
    #[error("projection-state stored accounting is invalid")]
    InvalidStoredAccounting,
    #[error("projection-state stored changed payload byte accounting does not match its payload")]
    StoredChangedPayloadAccountingMismatch,
    #[error("projection-state stored changed payload does not match its canonical identity digest")]
    StoredChangedPayloadDigestMismatch,
    #[error("projection-state integrity check failed: {0}")]
    IntegrityCheckFailed(String),
    #[error("projection-state foreign-key check failed")]
    ForeignKeyCheckFailed,
    #[error("prior projection-state lookup failed: {0}")]
    PriorLookup(#[source] Box<dyn Error + Send + Sync>),
    #[error("could not remove private projection-state file {path}: {source}")]
    RemoveFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not remove private projection-state directory {path}: {source}")]
    RemoveDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::hub_pack::{ProjectionCarSettings, ProjectionDrive};

    #[derive(Default)]
    struct MemoryPrior {
        rows: BTreeMap<(u8, i64), TeslaMateProjectionStateDigestRow>,
    }

    impl PriorProjectionStateLookup for MemoryPrior {
        fn digest(
            &mut self,
            entity: TeslaMateProjectionStateEntity,
            id: i64,
        ) -> Result<Option<Sha256Digest>, Box<dyn Error + Send + Sync>> {
            Ok(self.rows.get(&(entity.ordinal(), id)).map(|row| row.digest))
        }

        fn page_after(
            &mut self,
            after: Option<TeslaMateProjectionStateCursor>,
            limit: u32,
        ) -> Result<TeslaMateProjectionStateDigestPage, Box<dyn Error + Send + Sync>> {
            let (entity, id) = cursor_values(after);
            let mut rows = self
                .rows
                .values()
                .filter(|row| {
                    i64::from(row.entity.ordinal()) > entity
                        || (i64::from(row.entity.ordinal()) == entity && row.id > id)
                })
                .cloned()
                .collect::<Vec<_>>();
            let limit = usize::try_from(limit).expect("u32 fits usize");
            let next_after = if rows.len() > limit {
                rows.truncate(limit);
                rows.last().map(|row| TeslaMateProjectionStateCursor {
                    entity: row.entity,
                    id: row.id,
                })
            } else {
                None
            };
            Ok(TeslaMateProjectionStateDigestPage { rows, next_after })
        }
    }

    fn drive(id: i64, distance_km: Option<f64>) -> ProjectionDrive {
        ProjectionDrive {
            id,
            car_id: 1,
            optimized_at_ms: None,
            start_date_ms: 1,
            end_date_ms: 2,
            distance_km,
            duration_min: None,
            efficiency: None,
            outside_temp_avg: None,
            inside_temp_avg: None,
            speed_max: None,
            power_max: None,
            power_min: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_address: None,
            end_address: None,
            start_geofence: None,
            end_geofence: None,
            start_latitude: None,
            start_longitude: None,
            end_latitude: None,
            end_longitude: None,
            start_soc: None,
            end_soc: None,
            start_rated_range_km: None,
            end_rated_range_km: None,
            ascent: None,
            descent: None,
        }
    }

    #[test]
    fn retains_only_changed_payloads_and_pages_digests() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        let original = drive(7, Some(10.0));
        let original_digest = state.record_drive(&original).expect("record original");
        let mut prior = MemoryPrior::default();
        prior.rows.insert(
            (TeslaMateProjectionStateEntity::Drive.ordinal(), 7),
            TeslaMateProjectionStateDigestRow {
                entity: TeslaMateProjectionStateEntity::Drive,
                id: 7,
                car_id: 1,
                digest: original_digest,
            },
        );
        let unchanged = state
            .record_if_changed(
                &mut prior,
                TeslaMateProjectionStateEntity::Position,
                8,
                1,
                &serde_json::json!({"id": 8}),
            )
            .expect("new row");
        assert_eq!(unchanged, TeslaMateProjectionStateChange::NewOrChanged);
        state.seal().expect("seal");
        assert_eq!(state.stats().row_count, 2);
        assert_eq!(state.stats().changed_row_count, 1);
        let current = state.page(None, 10).expect("current page");
        assert_eq!(current.rows.len(), 2);
        let changed = state.changed_page(None, 10).expect("changed page");
        assert_eq!(changed.rows.len(), 1);
        assert_eq!(changed.rows[0].state.id, 8);
    }

    #[test]
    fn lookup_unchanged_omits_payload_and_missing_old_rows_become_tombstones() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut seed = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("seed state");
        let row = drive(7, Some(10.0));
        let digest = seed.record_drive(&row).expect("record seed");
        seed.seal().expect("seal seed");
        let mut prior = MemoryPrior::default();
        prior.rows.insert(
            (TeslaMateProjectionStateEntity::Drive.ordinal(), 7),
            TeslaMateProjectionStateDigestRow {
                entity: TeslaMateProjectionStateEntity::Drive,
                id: 7,
                car_id: 1,
                digest,
            },
        );
        prior.rows.insert(
            (TeslaMateProjectionStateEntity::Position.ordinal(), 8),
            TeslaMateProjectionStateDigestRow {
                entity: TeslaMateProjectionStateEntity::Position,
                id: 8,
                car_id: 1,
                digest: Sha256Digest::of_bytes(b"removed"),
            },
        );
        prior.rows.insert(
            (TeslaMateProjectionStateEntity::Car.ordinal(), 1),
            TeslaMateProjectionStateDigestRow {
                entity: TeslaMateProjectionStateEntity::Car,
                id: 1,
                car_id: 1,
                digest: Sha256Digest::of_bytes(b"car"),
            },
        );

        let mut state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        assert_eq!(
            state
                .record_if_changed(
                    &mut prior,
                    TeslaMateProjectionStateEntity::Drive,
                    row.id,
                    row.car_id,
                    &row,
                )
                .expect("unchanged drive"),
            TeslaMateProjectionStateChange::Unchanged
        );
        state.seal().expect("seal");
        assert!(
            state
                .changed_page(None, 10)
                .expect("changed")
                .rows
                .is_empty()
        );
        let (tombstones, next) = state
            .tombstone_page(&mut prior, None, 10)
            .expect("tombstones");
        assert!(next.is_none());
        assert_eq!(tombstones.len(), 1);
        assert_eq!(tombstones[0].id, 8);
        assert_eq!(
            tombstones[0].entity,
            crate::hub_pack::ProjectionDeltaEntity::Position
        );
    }

    #[test]
    fn initial_base_capture_keeps_only_digests_even_for_repeated_fragment_rows() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 1,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        let row = drive(7, Some(10.0));
        let mut capture = TeslaMateProjectionStateCapture::for_initial_base(state);
        assert_eq!(
            capture.record_drive(&row).expect("capture row"),
            TeslaMateProjectionStateChange::CapturedDigestOnly
        );
        assert_eq!(
            capture
                .record_drive(&row)
                .expect("deduplicate fragment repeat"),
            TeslaMateProjectionStateChange::CapturedDigestOnly
        );
        capture.seal().expect("seal");
        assert_eq!(
            capture.mode(),
            TeslaMateProjectionStateCaptureMode::InitialBase
        );
        assert_eq!(capture.stats().row_count, 1);
        assert_eq!(capture.stats().changed_row_count, 0);
        assert_eq!(capture.stats().changed_payload_bytes, 0);
    }

    #[test]
    fn canonicalizes_nested_object_keys_before_digesting_or_spooling_payload() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let limits = TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        };
        let mut nested_one = serde_json::Map::new();
        nested_one.insert("z".into(), serde_json::json!(2));
        nested_one.insert("a".into(), serde_json::json!(1));
        let mut object_one = serde_json::Map::new();
        object_one.insert("z".into(), serde_json::json!(3));
        object_one.insert("a".into(), serde_json::Value::Object(nested_one));

        let mut nested_two = serde_json::Map::new();
        nested_two.insert("a".into(), serde_json::json!(1));
        nested_two.insert("z".into(), serde_json::json!(2));
        let mut object_two = serde_json::Map::new();
        object_two.insert("a".into(), serde_json::Value::Object(nested_two));
        object_two.insert("z".into(), serde_json::json!(3));

        let mut first = TeslaMateProjectionState::create(temporary.path(), limits).expect("first");
        let digest = first
            .record_changed(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::Value::Object(object_one),
            )
            .expect("record changed");
        first.seal().expect("seal first");
        assert_eq!(
            first.changed_page(None, 10).expect("changed page").rows[0].canonical_payload,
            br#"{"a":{"a":1,"z":2},"z":3}"#
        );

        let mut second =
            TeslaMateProjectionState::create(temporary.path(), limits).expect("second");
        let equivalent_digest = second
            .record(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::Value::Object(object_two),
            )
            .expect("record equivalent");
        assert_eq!(digest, equivalent_digest);
    }

    #[test]
    fn changed_page_rejects_a_same_length_payload_tampered_after_capture() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        let original = br#"{"payload":"a"}"#;
        let tampered = br#"{"payload":"b"}"#;
        assert_eq!(
            original.len(),
            tampered.len(),
            "regression requires same-size tampering"
        );
        state
            .record_changed(
                TeslaMateProjectionStateEntity::Position,
                7,
                1,
                &serde_json::json!({"payload": "a"}),
            )
            .expect("record changed payload");
        state.seal().expect("seal");

        // The API cannot create this condition. Mutate the private spool only
        // in the regression to prove byte accounting alone is insufficient.
        state
            .connection
            .execute(
                "UPDATE changed_rows SET payload_json = ?1 \
                 WHERE entity_ordinal = ?2 AND entity_id = ?3",
                params![
                    std::str::from_utf8(tampered).expect("test JSON is UTF-8"),
                    i64::from(TeslaMateProjectionStateEntity::Position.ordinal()),
                    7_i64,
                ],
            )
            .expect("same-length tamper");

        assert!(matches!(
            state.changed_page(None, 10),
            Err(TeslaMateProjectionStateError::StoredChangedPayloadDigestMismatch)
        ));
    }

    #[test]
    fn deduplicates_exact_rows_rejects_conflicts_and_cleans_up_private_file() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 2,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        let path = state.path.clone();
        assert_eq!(
            fs::metadata(path.parent().expect("state parent"))
                .expect("state parent permissions")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let car = ProjectionCar {
            id: 1,
            name: "Road car".into(),
            model: "3".into(),
            vin: None,
            source_eid: None,
            source_vid: None,
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            firmware_version: None,
            efficiency_wh_per_km: None,
            settings: ProjectionCarSettings::default(),
        };
        state.record_car(&car).expect("car");
        state.record_car(&car).expect("exact repeat is a no-op");
        assert_eq!(state.stats().row_count, 1);
        let conflicting = ProjectionCar {
            name: "Other car".into(),
            ..car.clone()
        };
        assert!(matches!(
            state.record_car(&conflicting),
            Err(TeslaMateProjectionStateError::ConflictingRow { .. })
        ));
        let changed = serde_json::json!({"id": 2, "value": "new"});
        state
            .record_changed(TeslaMateProjectionStateEntity::Position, 2, 1, &changed)
            .expect("changed row");
        state
            .record_car(&car)
            .expect("exact current repeat is permitted at row capacity");
        let accounting = state.stats();
        state
            .record_changed(TeslaMateProjectionStateEntity::Position, 2, 1, &changed)
            .expect("exact changed repeat is a no-op");
        assert_eq!(state.stats(), accounting);
        assert!(matches!(
            state.page(None, 1),
            Err(TeslaMateProjectionStateError::StateNotSealed)
        ));
        state.discard().expect("discard");
        assert!(
            matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn targeted_current_upsert_does_not_ignore_non_uniqueness_errors() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 2,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        state
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_current_rows \
                 BEFORE INSERT ON current_rows \
                 BEGIN SELECT RAISE(FAIL, 'injected current-row failure'); END;",
            )
            .expect("install failure trigger");

        assert!(matches!(
            state.record(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"id": 1}),
            ),
            Err(TeslaMateProjectionStateError::Sqlite(_))
        ));
        assert_eq!(state.stats().row_count, 0);
        assert!(state.connection.is_autocommit());

        state
            .connection
            .execute_batch("DROP TRIGGER reject_current_rows")
            .expect("remove failure trigger");
        state
            .record(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"id": 1}),
            )
            .expect("state remains usable after a non-uniqueness error");
        state.seal().expect("seal state");
    }

    #[test]
    fn targeted_current_upsert_error_mid_batch_preserves_prior_pending_rows() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        state
            .record(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"id": 1}),
            )
            .expect("first pending row");
        assert_eq!(state.pending_write_rows, 1);
        assert!(!state.connection.is_autocommit());

        state
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_second_current_row \
                 BEFORE INSERT ON current_rows \
                 WHEN NEW.entity_id = 2 \
                 BEGIN SELECT RAISE(ABORT, 'injected current-row failure'); END;",
            )
            .expect("install failure trigger");
        assert!(matches!(
            state.record(
                TeslaMateProjectionStateEntity::Position,
                2,
                1,
                &serde_json::json!({"id": 2}),
            ),
            Err(TeslaMateProjectionStateError::Sqlite(_))
        ));
        assert_eq!(state.stats().row_count, 1);
        assert_eq!(state.pending_write_rows, 1);
        assert!(!state.connection.is_autocommit());

        state
            .connection
            .execute_batch("DROP TRIGGER reject_second_current_row")
            .expect("remove failure trigger");
        state
            .record(
                TeslaMateProjectionStateEntity::Position,
                3,
                1,
                &serde_json::json!({"id": 3}),
            )
            .expect("state remains usable after a mid-batch fast-path error");
        state.seal().expect("seal recovered state");
        assert_eq!(
            state
                .page(None, 10)
                .expect("sealed rows")
                .rows
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn fixed_write_batch_commits_at_the_boundary_and_seal_flushes_the_tail() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: u64::from(WRITE_BATCH_ROWS) + 1,
                max_state_bytes: 4 * 1024 * 1024,
                max_changed_payload_bytes: 4 * 1024 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        state
            .record_changed(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"id": 1}),
            )
            .expect("first pending changed row");
        for id in 2..i64::from(WRITE_BATCH_ROWS) {
            state
                .record(
                    TeslaMateProjectionStateEntity::Position,
                    id,
                    1,
                    &serde_json::json!({"id": id}),
                )
                .expect("record pending row");
        }
        assert_eq!(state.pending_write_rows, WRITE_BATCH_ROWS - 1);
        assert!(!state.connection.is_autocommit());
        assert_eq!(state.stats().changed_row_count, 1);
        state
            .record(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"id": 1}),
            )
            .expect("deduplicate within unflushed batch");
        assert!(matches!(
            state.record(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"id": 1, "different": true}),
            ),
            Err(TeslaMateProjectionStateError::ConflictingRow { .. })
        ));
        assert_eq!(state.pending_write_rows, WRITE_BATCH_ROWS - 1);
        assert!(!state.connection.is_autocommit());
        assert_eq!(
            state.stats().row_count,
            u64::from(WRITE_BATCH_ROWS - 1),
            "accepted rows remain visible in the state accounting before commit"
        );

        state
            .record(
                TeslaMateProjectionStateEntity::Position,
                i64::from(WRITE_BATCH_ROWS),
                1,
                &serde_json::json!({"id": WRITE_BATCH_ROWS}),
            )
            .expect("record boundary row");
        assert_eq!(state.pending_write_rows, 0);
        assert!(state.connection.is_autocommit());

        state
            .record(
                TeslaMateProjectionStateEntity::Position,
                i64::from(WRITE_BATCH_ROWS) + 1,
                1,
                &serde_json::json!({"id": WRITE_BATCH_ROWS + 1}),
            )
            .expect("record tail row");
        assert_eq!(state.pending_write_rows, 1);
        assert!(!state.connection.is_autocommit());

        let sealed = state.seal().expect("seal flushes tail row");
        assert_eq!(sealed.row_count, u64::from(WRITE_BATCH_ROWS) + 1);
        assert_eq!(sealed.changed_row_count, 1);
        assert!(state.connection.is_autocommit());
        assert_eq!(
            state
                .page(None, WRITE_BATCH_ROWS + 1)
                .expect("read after sealed flush")
                .rows
                .len(),
            usize::try_from(WRITE_BATCH_ROWS + 1).expect("batch size fits usize")
        );
        assert_eq!(
            state
                .changed_page(None, 10)
                .expect("changed row survives shared batch")
                .rows
                .len(),
            1
        );
    }

    #[test]
    fn changed_pair_failure_rolls_back_only_the_unpublished_row() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        state
            .record(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"id": 1}),
            )
            .expect("first pending row");
        state
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_changed_rows \
                 BEFORE INSERT ON changed_rows \
                 BEGIN SELECT RAISE(FAIL, 'injected changed-row failure'); END;",
            )
            .expect("install failure trigger");

        assert!(
            state
                .record_changed(
                    TeslaMateProjectionStateEntity::Position,
                    2,
                    1,
                    &serde_json::json!({"id": 2}),
                )
                .is_err()
        );
        assert_eq!(state.stats().row_count, 1);
        assert_eq!(state.stats().changed_row_count, 0);
        assert_eq!(
            state
                .connection
                .query_row(
                    "SELECT COUNT(*) FROM current_rows WHERE entity_ordinal = ?1 AND entity_id = ?2",
                    params![i64::from(TeslaMateProjectionStateEntity::Position.ordinal()), 2_i64],
                    |row| row.get::<_, i64>(0),
                )
                .expect("inspect current row"),
            0
        );
        assert!(!state.connection.is_autocommit());

        state.seal().expect("remaining batch stays usable");
        let rows = state.page(None, 10).expect("sealed page").rows;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, 1);
    }

    #[test]
    fn changed_payload_byte_cap_flushes_before_the_row_cap() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = TeslaMateProjectionState::create_with_changed_payload_row_limit(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 32 * 1024 * 1024,
                max_changed_payload_bytes: 16 * 1024 * 1024,
                minimum_free_bytes: 0,
            },
            16 * 1024 * 1024,
        )
        .expect("state");
        let payload = "x".repeat(
            usize::try_from(WRITE_BATCH_CHANGED_PAYLOAD_BYTES)
                .expect("payload batch limit fits usize"),
        );
        state
            .record_changed(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"payload": payload}),
            )
            .expect("changed row reaches byte cap");

        assert_eq!(state.pending_write_rows, 0);
        assert!(state.connection.is_autocommit());
        assert_eq!(state.stats().changed_row_count, 1);
        assert!(
            state.stats().changed_payload_bytes >= WRITE_BATCH_CHANGED_PAYLOAD_BYTES,
            "the commit was driven by payload bytes, not the 1,024-row cap"
        );
        state.seal().expect("already-flushed state seals cleanly");
    }

    #[test]
    fn changed_payload_row_cap_rejects_before_any_durable_retention() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let maximum_payload_bytes = 1024_u64;
        let mut state = TeslaMateProjectionState::create_with_changed_payload_row_limit(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
            maximum_payload_bytes,
        )
        .expect("state");

        let error = state
            .record_changed(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"payload": "x".repeat(1024)}),
            )
            .expect_err("canonical JSON overhead makes this source row exceed the configured cap");
        assert!(matches!(
            error,
            TeslaMateProjectionStateError::ChangedPayloadRowLimitExceeded {
                maximum,
                payload_bytes,
            } if maximum == maximum_payload_bytes && payload_bytes > maximum_payload_bytes
        ));
        assert_eq!(state.stats().row_count, 0);
        assert_eq!(state.stats().changed_row_count, 0);
        assert_eq!(state.pending_write_rows, 0);
        assert_eq!(
            state
                .connection
                .query_row("SELECT COUNT(*) FROM current_rows", [], |row| row
                    .get::<_, i64>(0))
                .expect("inspect current rows"),
            0,
            "a rejected payload must not leave a current-row-only orphan"
        );
        assert_eq!(
            state
                .connection
                .query_row("SELECT COUNT(*) FROM changed_rows", [], |row| row
                    .get::<_, i64>(0))
                .expect("inspect changed rows"),
            0
        );
        state
            .seal()
            .expect("empty state remains sealable after rejection");
    }

    #[test]
    fn changed_page_payload_cap_preserves_order_and_cursor_without_loading_an_over_cap_page() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let page_cap = 8 * 1024_u64;
        let mut state = TeslaMateProjectionState::create_with_changed_payload_row_limit(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 64 * 1024,
                minimum_free_bytes: 0,
            },
            16 * 1024,
        )
        .expect("state");
        for id in 1..=3_i64 {
            state
                .record_changed(
                    TeslaMateProjectionStateEntity::Position,
                    id,
                    1,
                    &serde_json::json!({"id": id, "payload": "x".repeat(3 * 1024)}),
                )
                .expect("bounded changed row");
        }
        state.seal().expect("seal");

        let first = state
            .changed_page_with_payload_limit(None, 10, page_cap)
            .expect("first byte-bounded page");
        assert_eq!(
            first
                .rows
                .iter()
                .map(|row| row.state.id)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(
            first
                .rows
                .iter()
                .map(|row| u64::try_from(row.canonical_payload.len()).expect("usize fits u64"))
                .sum::<u64>()
                <= page_cap
        );
        assert_eq!(
            first.next_after,
            Some(TeslaMateProjectionStateCursor {
                entity: TeslaMateProjectionStateEntity::Position,
                id: 2,
            })
        );

        let second = state
            .changed_page_with_payload_limit(first.next_after, 10, page_cap)
            .expect("second byte-bounded page");
        assert_eq!(
            second
                .rows
                .iter()
                .map(|row| row.state.id)
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert!(second.next_after.is_none());
    }

    #[test]
    fn changed_page_rejects_an_individual_row_before_decoding_it() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let configured_row_cap = 16 * 1024_u64;
        let requested_page_cap = 8 * 1024_u64;
        let mut state = TeslaMateProjectionState::create_with_changed_payload_row_limit(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 64 * 1024,
                minimum_free_bytes: 0,
            },
            configured_row_cap,
        )
        .expect("state");
        state
            .record_changed(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"payload": "x".repeat(9 * 1024)}),
            )
            .expect("row fits the durable cap");
        state.seal().expect("seal");

        let error = state
            .changed_page_with_payload_limit(None, 10, requested_page_cap)
            .expect_err("metadata must reject an oversized row before its JSON is fetched");
        assert!(matches!(
            error,
            TeslaMateProjectionStateError::ChangedPayloadRowLimitExceeded {
                maximum,
                payload_bytes,
            } if maximum == requested_page_cap && payload_bytes > requested_page_cap
        ));
    }

    #[test]
    fn changed_payload_boundary_commits_the_prior_batch_before_the_next_row() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 32 * 1024 * 1024,
                max_changed_payload_bytes: 16 * 1024 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        let first_payload = "x".repeat(
            usize::try_from(WRITE_BATCH_CHANGED_PAYLOAD_BYTES - 1024 * 1024)
                .expect("payload batch limit fits usize"),
        );
        let second_payload = "y".repeat(2 * 1024 * 1024);
        state
            .record_changed(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"payload": first_payload}),
            )
            .expect("first changed row stays pending");
        assert_eq!(state.pending_write_rows, 1);
        assert!(!state.connection.is_autocommit());

        state
            .record_changed(
                TeslaMateProjectionStateEntity::Position,
                2,
                1,
                &serde_json::json!({"payload": second_payload}),
            )
            .expect("second changed row crosses byte boundary");
        assert_eq!(state.pending_write_rows, 1);
        assert!(!state.connection.is_autocommit());
        assert_eq!(state.stats().changed_row_count, 2);

        state.seal().expect("flush second bounded batch");
        assert_eq!(state.stats().changed_row_count, 2);
    }

    #[test]
    fn failed_batch_flush_rolls_back_pending_rows_and_poisons_the_capture() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = TeslaMateProjectionState::create(
            temporary.path(),
            TeslaMateProjectionStateLimits {
                max_rows: 10,
                max_state_bytes: 128 * 1024,
                max_changed_payload_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("state");
        state
            .record(
                TeslaMateProjectionStateEntity::Position,
                1,
                1,
                &serde_json::json!({"id": 1}),
            )
            .expect("pending row");
        // The API never writes an invalid changed row. Inject one through the
        // private connection solely to make COMMIT fail after a valid pending
        // write, then verify accounting and visibility reset together.
        state
            .connection
            .execute_batch("PRAGMA defer_foreign_keys = ON")
            .expect("defer foreign key enforcement until commit");
        state
            .connection
            .execute(
                "INSERT INTO changed_rows( \
                    entity_ordinal, entity_id, car_id, projection_sha256, payload_json, payload_bytes \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![0_i64, 99_i64, 1_i64, vec![0_u8; 32], "{}", 2_i64],
            )
            .expect("inject deferred foreign-key violation");

        assert!(matches!(
            state.seal(),
            Err(TeslaMateProjectionStateError::Sqlite(_))
        ));
        assert!(state.write_failed);
        assert!(state.connection.is_autocommit());
        assert_eq!(state.stats().row_count, 0);
        assert_eq!(state.stats().changed_row_count, 0);
        assert_eq!(
            state
                .connection
                .query_row("SELECT COUNT(*) FROM current_rows", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("pending rows rolled back"),
            0
        );
        assert!(matches!(
            state.record(
                TeslaMateProjectionStateEntity::Position,
                2,
                1,
                &serde_json::json!({"id": 2}),
            ),
            Err(TeslaMateProjectionStateError::WriteBatchFailed)
        ));
        let mut prior = MemoryPrior::default();
        assert!(matches!(
            state.record_if_changed(
                &mut prior,
                TeslaMateProjectionStateEntity::Position,
                2,
                1,
                &serde_json::json!({"id": 2}),
            ),
            Err(TeslaMateProjectionStateError::WriteBatchFailed)
        ));
    }
}
