// SPDX-License-Identifier: AGPL-3.0-only

#[derive(Debug, Clone)]
pub struct ProjectionPackWriter {
    packs_dir: PathBuf,
    limits: ProtocolLimits,
    minimum_free_bytes: u64,
}

impl ProjectionPackWriter {
    pub fn new(packs_dir: impl Into<PathBuf>) -> Self {
        Self {
            packs_dir: packs_dir.into(),
            limits: ProtocolLimits::default(),
            minimum_free_bytes: 0,
        }
    }

    pub fn with_minimum_free_bytes(mut self, minimum_free_bytes: u64) -> Self {
        self.minimum_free_bytes = minimum_free_bytes;
        self
    }

    pub fn with_limits(packs_dir: impl Into<PathBuf>, limits: ProtocolLimits) -> Self {
        Self {
            packs_dir: packs_dir.into(),
            limits,
            minimum_free_bytes: 0,
        }
    }

    pub fn content_path(&self, digest: Sha256Digest) -> PathBuf {
        self.packs_dir
            .join("sha256")
            .join(format!("{digest}.sqlite.zst"))
    }

    /// Refuse a source capture unless there is room for every permitted final
    /// pack, the active SQLite/compression pair, and the caller's free-space
    /// reserve. The limit is intentionally worst-case: a later full snapshot
    /// must never consume the reserve while replacing an earlier one.
    pub fn ensure_full_snapshot_capacity(
        &self,
        minimum_free_bytes: u64,
    ) -> Result<(), ProjectionPackError> {
        self.ensure_full_snapshot_capacity_for_capture(u64::MAX, minimum_free_bytes)
    }

    /// Refuse a source capture unless its validated capture bound, the active
    /// SQLite/compression pair, and the caller's free-space reserve fit on the
    /// target filesystem. The capture bound is doubled for SQLite and parent
    /// row duplication, then clamped by the negotiated protocol ceiling. This
    /// keeps admission tied to the source's bounded import contract instead of
    /// reserving the entire wire-format safety ceiling for every small source.
    pub fn ensure_full_snapshot_capacity_for_capture(
        &self,
        capture_bound_bytes: u64,
        minimum_free_bytes: u64,
    ) -> Result<(), ProjectionPackError> {
        let protocol_final_bytes = u64::try_from(self.limits.max_chunks)
            .map_err(|_| ProjectionPackError::CapacityOverflow)?
            .checked_mul(self.limits.max_compressed_pack_bytes)
            .ok_or(ProjectionPackError::CapacityOverflow)?;
        let capture_final_bytes = capture_bound_bytes
            .checked_mul(2)
            .ok_or(ProjectionPackError::CapacityOverflow)?;
        let final_bytes = protocol_final_bytes.min(capture_final_bytes);
        let required = final_bytes
            .checked_add(self.transient_write_bytes()?)
            .and_then(|value| value.checked_add(minimum_free_bytes))
            .ok_or(ProjectionPackError::CapacityOverflow)?;
        self.ensure_free_bytes(required)
    }

    /// Reserve space for one active fragment build plus the caller's durable
    /// free-space floor. Direct imports recheck this before every immutable
    /// pack write instead of reserving a whole-history cap up front.
    pub fn ensure_incremental_capture_capacity(
        &self,
        minimum_free_bytes: u64,
    ) -> Result<(), ProjectionPackError> {
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(minimum_free_bytes)
                .ok_or(ProjectionPackError::CapacityOverflow)?,
        )
    }

    /// Admit a streamed capture before its first source row is decoded. The
    /// caller supplies a conservative final-output estimate; active SQLite and
    /// compression files plus the caller's scratch/free-space reserve are
    /// accounted separately. Per-pack checks still enforce the same reserve.
    pub fn ensure_incremental_capture_capacity_with_final_estimate(
        &self,
        estimated_final_bytes: u64,
        minimum_free_bytes: u64,
    ) -> Result<(), ProjectionPackError> {
        let required =
            self.incremental_capture_required_bytes(estimated_final_bytes, minimum_free_bytes)?;
        self.ensure_free_bytes(required)
    }

    pub(crate) fn incremental_capture_required_bytes(
        &self,
        estimated_final_bytes: u64,
        minimum_free_bytes: u64,
    ) -> Result<u64, ProjectionPackError> {
        self.transient_write_bytes()?
            .checked_add(estimated_final_bytes)
            .and_then(|value| value.checked_add(minimum_free_bytes))
            .ok_or(ProjectionPackError::CapacityOverflow)
    }

    pub(crate) fn incremental_capture_transient_bytes(&self) -> Result<u64, ProjectionPackError> {
        self.transient_write_bytes()
    }

    /// Write and verify an immutable, complete mirror snapshot. The caller
    /// supplies a bounded projection; the writer never inspects raw telemetry.
    pub fn write_full_snapshot(
        &self,
        request: &ProjectionPackRequest<'_>,
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        validate_request(request, self.limits)?;
        validate_v1_snapshot(request)?;
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let staging_dir = self.packs_dir.join(".staging");
        let content_dir = self.packs_dir.join("sha256");
        ensure_private_staging_directory(&staging_dir)?;
        fs::create_dir_all(&content_dir).map_err(|source| {
            ProjectionPackError::CreateDirectory {
                path: content_dir.clone(),
                source,
            }
        })?;

        let sqlite_temp = StagedFile::create(&staging_dir, "projection.sqlite")?;
        write_projection_sqlite(
            sqlite_temp.path(),
            request,
            self.limits,
            HUB_PROJECTION_SCHEMA_V1,
            &[],
            &[],
            request.snapshot.row_count()?,
        )?;
        let uncompressed_bytes = fs::metadata(sqlite_temp.path())
            .map_err(|source| ProjectionPackError::Metadata {
                path: sqlite_temp.path().to_path_buf(),
                source,
            })?
            .len();

        let mut compressed_temp = StagedFile::create(&staging_dir, "projection.zst")?;
        let (sha256, compressed_bytes) = compress_file(sqlite_temp.path(), compressed_temp.path())?;
        let metadata = TransportPack {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            schema: HUB_PROJECTION_SCHEMA_V1,
            format: PackFormat::HubProjectionSqlite,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(sha256),
            sha256,
            compressed_bytes,
            uncompressed_bytes,
            row_count: request.snapshot.row_count()?,
            sequence: request.sequence,
            tables: tables_for_snapshot(request.snapshot, false),
        };
        metadata.validate(self.limits)?;
        let verified = verify_file(&metadata, compressed_temp.path(), self.limits)?;
        let final_path = self.content_path(sha256);
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let publication =
            publish_immutable(&mut compressed_temp, &final_path, &metadata, self.limits)?;

        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
            ownership: publication.ownership,
            cleanup_state: publication.cleanup_state,
        })
    }

    /// Write the additive state-history projection. The original writer and
    /// its schema remain unchanged; callers must opt into this entry point.
    pub fn write_full_snapshot_with_states(
        &self,
        request: &ProjectionPackRequest<'_>,
        states: &[ProjectionState],
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        self.write_full_snapshot_with_states_and_updates(request, states, &[])
    }

    pub fn write_full_snapshot_with_states_and_updates(
        &self,
        request: &ProjectionPackRequest<'_>,
        states: &[ProjectionState],
        updates: &[ProjectionUpdate],
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        validate_request(request, self.limits)?;
        validate_states(states, request.binding.selected_car_id)?;
        validate_updates(updates, request.binding.selected_car_id)?;
        let row_count = row_count_with_states_and_updates(request.snapshot, states, updates)?;
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let staging_dir = self.packs_dir.join(".staging");
        let content_dir = self.packs_dir.join("sha256");
        ensure_private_staging_directory(&staging_dir)?;
        fs::create_dir_all(&content_dir).map_err(|source| {
            ProjectionPackError::CreateDirectory {
                path: content_dir.clone(),
                source,
            }
        })?;

        let sqlite_temp = StagedFile::create(&staging_dir, "projection.sqlite")?;
        write_projection_sqlite(
            sqlite_temp.path(),
            request,
            self.limits,
            HUB_PROJECTION_SCHEMA_V2,
            states,
            updates,
            row_count,
        )?;
        let uncompressed_bytes = fs::metadata(sqlite_temp.path())
            .map_err(|source| ProjectionPackError::Metadata {
                path: sqlite_temp.path().to_path_buf(),
                source,
            })?
            .len();
        let mut compressed_temp = StagedFile::create(&staging_dir, "projection.zst")?;
        let (sha256, compressed_bytes) = compress_file(sqlite_temp.path(), compressed_temp.path())?;
        let metadata = TransportPack {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            schema: HUB_PROJECTION_SCHEMA_V2,
            format: PackFormat::HubProjectionSqlite,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(sha256),
            sha256,
            compressed_bytes,
            uncompressed_bytes,
            row_count,
            sequence: request.sequence,
            tables: tables_for_snapshot(request.snapshot, true),
        };
        metadata.validate(self.limits)?;
        let verified = verify_file(&metadata, compressed_temp.path(), self.limits)?;
        let final_path = self.content_path(sha256);
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let publication =
            publish_immutable(&mut compressed_temp, &final_path, &metadata, self.limits)?;
        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
            ownership: publication.ownership,
            cleanup_state: publication.cleanup_state,
        })
    }

    /// Write and locally verify one full schema-2.2 snapshot.
    ///
    /// Schema 2.2 is full-snapshot-only. The caller signs the returned object
    /// and catalogues it through `HubStore`.
    pub fn write_full_snapshot_2_2(
        &self,
        request: &ProjectionPackRequestV2_2<'_>,
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        let row_count = validate_request_v2_2(request, self.limits)?;
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let staging_dir = self.packs_dir.join(".staging");
        let content_dir = self.packs_dir.join("sha256");
        ensure_private_staging_directory(&staging_dir)?;
        fs::create_dir_all(&content_dir).map_err(|source| {
            ProjectionPackError::CreateDirectory {
                path: content_dir.clone(),
                source,
            }
        })?;

        let sqlite_temp = StagedFile::create(&staging_dir, "projection-2-2.sqlite")?;
        write_projection_sqlite_2_2(sqlite_temp.path(), request, self.limits, row_count)?;
        verify_projection_sqlite_2_2(sqlite_temp.path(), request, row_count)?;
        let uncompressed_bytes = fs::metadata(sqlite_temp.path())
            .map_err(|source| ProjectionPackError::Metadata {
                path: sqlite_temp.path().to_path_buf(),
                source,
            })?
            .len();

        let mut compressed_temp = StagedFile::create(&staging_dir, "projection-2-2.zst")?;
        let (sha256, compressed_bytes) = compress_file(sqlite_temp.path(), compressed_temp.path())?;
        let metadata = TransportPack {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            schema: HUB_PROJECTION_SCHEMA_V3,
            format: PackFormat::HubProjectionSqlite,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(sha256),
            sha256,
            compressed_bytes,
            uncompressed_bytes,
            row_count,
            sequence: request.sequence,
            tables: tables_for_snapshot_v2_2(request.snapshot),
        };
        metadata.validate(self.limits)?;
        let verified = verify_file(&metadata, compressed_temp.path(), self.limits)?;
        let final_path = self.content_path(sha256);
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let publication =
            publish_immutable(&mut compressed_temp, &final_path, &metadata, self.limits)?;
        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
            ownership: publication.ownership,
            cleanup_state: publication.cleanup_state,
        })
    }

    /// Write one sparse schema-2.1 delta. This path creates only the schema;
    /// it never reads or copies the external base lineage.
    pub fn write_delta(
        &self,
        request: &ProjectionDeltaPackRequest<'_>,
    ) -> Result<BuiltProjectionPack, ProjectionPackError> {
        let row_count = validate_delta(request, self.limits)?;
        self.ensure_free_bytes(
            self.transient_write_bytes()?
                .checked_add(self.minimum_free_bytes)
                .ok_or(ProjectionPackError::TooManyRows)?,
        )?;
        let staging_dir = self.packs_dir.join(".staging");
        let content_dir = self.packs_dir.join("sha256");
        ensure_private_staging_directory(&staging_dir)?;
        fs::create_dir_all(&content_dir).map_err(|source| {
            ProjectionPackError::CreateDirectory {
                path: content_dir.clone(),
                source,
            }
        })?;

        let sqlite_temp = StagedFile::create(&staging_dir, "projection-delta.sqlite")?;
        let empty = ProjectionSnapshot {
            cars: Vec::new(),
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
        };
        let schema_request = ProjectionPackRequest {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            binding: request.delta.binding.clone(),
            sequence: request.delta.sequence,
            snapshot: &empty,
        };
        write_projection_sqlite(
            sqlite_temp.path(),
            &schema_request,
            self.limits,
            HUB_PROJECTION_SCHEMA_V2,
            &[],
            &[],
            0,
        )?;
        write_delta_rows(sqlite_temp.path(), request, self.limits, row_count)?;

        let uncompressed_bytes = fs::metadata(sqlite_temp.path())
            .map_err(|source| ProjectionPackError::Metadata {
                path: sqlite_temp.path().to_path_buf(),
                source,
            })?
            .len();
        let mut compressed_temp = StagedFile::create(&staging_dir, "projection-delta.zst")?;
        let (sha256, compressed_bytes) = compress_file(sqlite_temp.path(), compressed_temp.path())?;
        let metadata = TransportPack {
            pack_id: request.pack_id,
            snapshot_id: request.snapshot_id,
            ordinal: request.ordinal,
            schema: HUB_PROJECTION_SCHEMA_V2,
            format: PackFormat::HubProjectionSqlite,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(sha256),
            sha256,
            compressed_bytes,
            uncompressed_bytes,
            row_count,
            sequence: request.delta.sequence,
            tables: tables_for_delta(request.delta),
        };
        metadata.validate(self.limits)?;
        let verified = verify_file(&metadata, compressed_temp.path(), self.limits)?;
        let final_path = self.content_path(sha256);
        let publication =
            publish_immutable(&mut compressed_temp, &final_path, &metadata, self.limits)?;
        Ok(BuiltProjectionPack {
            metadata,
            path: final_path,
            verified,
            ownership: publication.ownership,
            cleanup_state: publication.cleanup_state,
        })
    }

    fn transient_write_bytes(&self) -> Result<u64, ProjectionPackError> {
        self.limits
            .max_uncompressed_pack_bytes
            .checked_mul(2)
            .ok_or(ProjectionPackError::CapacityOverflow)
    }

    fn ensure_free_bytes(&self, required: u64) -> Result<(), ProjectionPackError> {
        let staging_dir = self.packs_dir.join(".staging");
        ensure_private_staging_directory(&staging_dir)?;
        let available = available_bytes(&staging_dir)?;
        if available < required {
            return Err(ProjectionPackError::InsufficientFreeSpace {
                required,
                available,
            });
        }
        Ok(())
    }
}
