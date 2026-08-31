// SPDX-License-Identifier: AGPL-3.0-only

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
    #[cfg(test)]
    tombstone_membership_queries: Cell<u64>,
    #[cfg(test)]
    existing_change_queries: Cell<u64>,
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
