// SPDX-License-Identifier: AGPL-3.0-only

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
        let required = write_batch_required_free_bytes(limits.minimum_free_bytes)?;
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
            #[cfg(test)]
            tombstone_membership_queries: Cell::new(0),
            #[cfg(test)]
            existing_change_queries: Cell::new(0),
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
        for row in &page.rows {
            validate_row_identity(row.id, row.car_id)?;
        }
        let missing = self.missing_current_identities(&page.rows)?;
        let mut tombstones = Vec::new();
        for row in &page.rows {
            if !row.entity.tombstone_allowed() {
                continue;
            }
            if missing.contains(&(row.entity, row.id)) {
                tombstones.push(ProjectionTombstone {
                    entity: projection_delta_entity(row.entity),
                    id: row.id,
                    car_id: row.car_id,
                });
            }
        }
        Ok((tombstones, page.next_after))
    }

    fn missing_current_identities(
        &self,
        rows: &[TeslaMateProjectionStateDigestRow],
    ) -> Result<HashSet<(TeslaMateProjectionStateEntity, i64)>, TeslaMateProjectionStateError> {
        let mut ids_by_entity = TeslaMateProjectionStateEntity::ALL.map(|_| Vec::new());
        for row in rows {
            if row.entity.tombstone_allowed() {
                ids_by_entity[usize::from(row.entity.ordinal())].push(row.id);
            }
        }

        let mut missing = HashSet::with_capacity(rows.len());
        for (entity, ids) in TeslaMateProjectionStateEntity::ALL
            .into_iter()
            .zip(ids_by_entity)
        {
            for requested in ids.chunks(TOMBSTONE_MEMBERSHIP_LOOKUP_ROWS) {
                let mut query = String::from("WITH requested(entity_id) AS (VALUES ");
                for index in 0..requested.len() {
                    if index != 0 {
                        query.push_str(", ");
                    }
                    query.push_str("(?)");
                }
                query.push_str(
                    ") \
                     SELECT requested.entity_id \
                       FROM requested \
                      WHERE NOT EXISTS ( \
                            SELECT 1 FROM current_rows \
                             WHERE current_rows.entity_ordinal = ? \
                               AND current_rows.entity_id = requested.entity_id \
                      )",
                );
                let mut values = Vec::with_capacity(requested.len() + 1);
                values.extend_from_slice(requested);
                values.push(i64::from(entity.ordinal()));
                #[cfg(test)]
                self.tombstone_membership_queries.set(
                    self.tombstone_membership_queries
                        .get()
                        .saturating_add(1),
                );
                let mut statement = self
                    .connection
                    .prepare_cached(&query)
                    .map_err(TeslaMateProjectionStateError::Sqlite)?;
                let missing_ids = statement
                    .query_map(params_from_iter(values.iter()), |row| row.get::<_, i64>(0))
                    .map_err(TeslaMateProjectionStateError::Sqlite)?;
                for missing_id in missing_ids {
                    missing.insert((
                        entity,
                        missing_id.map_err(TeslaMateProjectionStateError::Sqlite)?,
                    ));
                }
            }
        }
        Ok(missing)
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
        let changed_payload_limit_exceeded =
            next_payload_bytes > self.limits.max_changed_payload_bytes;
        let row_limit_exceeded = self.total_row_count() >= self.limits.max_rows;
        if (changed_payload_limit_exceeded || row_limit_exceeded)
            && self.existing_change(entity, id, car_id, digest)?.is_some()
        {
            return Ok(());
        }
        if changed_payload_limit_exceeded {
            return Err(TeslaMateProjectionStateError::ChangedPayloadLimitExceeded {
                maximum: self.limits.max_changed_payload_bytes,
            });
        }
        if row_limit_exceeded {
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
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(entity_ordinal, entity_id) DO NOTHING",
                params![
                    i64::from(entity.ordinal()),
                    id,
                    car_id,
                    digest.as_bytes().as_slice()
                ],
            );
            match current {
                Ok(1) => {}
                Ok(0) => {
                    let existing = self.existing_change(entity, id, car_id, digest)?;
                    self.connection
                        .execute_batch("RELEASE SAVEPOINT projection_state_row")
                        .map_err(TeslaMateProjectionStateError::Sqlite)?;
                    return match existing {
                        Some(_) => Ok(false),
                        None => Err(TeslaMateProjectionStateError::InvalidStoredAccounting),
                    };
                }
                Ok(_) => return Err(TeslaMateProjectionStateError::InvalidStoredAccounting),
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
                .map_err(TeslaMateProjectionStateError::Sqlite)?;
            Ok(true)
        })();
        match result {
            Ok(true) => {}
            Ok(false) => {
                self.close_empty_pending_write_transaction();
                return Ok(());
            }
            Err(error) => {
                self.rollback_pending_row_savepoint();
                self.close_empty_pending_write_transaction();
                return Err(error);
            }
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
        let parent = self
            .path
            .parent()
            .ok_or_else(|| TeslaMateProjectionStateError::InvalidTransferPath(self.path.clone()))?;
        let required = write_batch_required_free_bytes(self.limits.minimum_free_bytes)?;
        let available = available_bytes(parent)?;
        if available < required {
            return Err(TeslaMateProjectionStateError::InsufficientFreeSpace {
                required,
                available,
            });
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
        #[cfg(test)]
        self.existing_change_queries
            .set(self.existing_change_queries.get().saturating_add(1));
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
