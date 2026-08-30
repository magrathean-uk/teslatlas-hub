// SPDX-License-Identifier: AGPL-3.0-only

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
