// SPDX-License-Identifier: AGPL-3.0-only

/// Sign a full-snapshot manifest from several already-verified typed chunks.
///
/// Large history is intentionally represented by several independently
/// resumable SQLite objects, not one host-memory-sized database. Every chunk
/// repeats its required parent rows (the selected car and any parents of its
/// children), so it remains a valid foreign-key-checked SQLite database by
/// itself. The iOS importer stages all chunks before it atomically activates
/// the complete mirror.
pub fn signed_full_snapshot_manifest(
    binding: &ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    chunks: &[BuiltProjectionPack],
    total_rows: u64,
    cursor_key: &CursorKey,
) -> Result<SyncManifest, ProjectionPackError> {
    if chunks
        .first()
        .is_some_and(|built| built.metadata.schema == HUB_PROJECTION_SCHEMA_V3)
    {
        validate_binding_v2_2(binding)?;
    } else {
        validate_binding(binding)?;
    }
    if snapshot_id.is_nil() {
        return Err(invalid("snapshot ID must not be nil"));
    }
    if !sequence.is_ordered() {
        return Err(invalid("full snapshot sequence is unordered"));
    }
    if chunks.is_empty() {
        return Err(invalid("full snapshot needs at least one chunk"));
    }

    let schema = chunks[0].metadata.schema;
    if schema != HUB_PROJECTION_SCHEMA_V1
        && schema != HUB_PROJECTION_SCHEMA_V2
        && schema != HUB_PROJECTION_SCHEMA_V3
    {
        return Err(invalid("unsupported projection schema"));
    }
    let mut total_compressed_bytes = 0_u64;
    let mut total_uncompressed_bytes = 0_u64;
    let mut transport_rows = 0_u64;
    let mut metadata = Vec::with_capacity(chunks.len());
    for (expected_ordinal, built) in chunks.iter().enumerate() {
        let pack = &built.metadata;
        if pack.snapshot_id != snapshot_id
            || pack.schema != schema
            || pack.format != PackFormat::HubProjectionSqlite
            || pack.sequence != sequence
            || pack.ordinal
                != u32::try_from(expected_ordinal)
                    .map_err(|_| ProjectionPackError::TooManyChunks)?
        {
            return Err(invalid("built chunk does not match its snapshot manifest"));
        }
        total_compressed_bytes = total_compressed_bytes
            .checked_add(pack.compressed_bytes)
            .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
        total_uncompressed_bytes = total_uncompressed_bytes
            .checked_add(pack.uncompressed_bytes)
            .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
        transport_rows = transport_rows
            .checked_add(pack.row_count)
            .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
        metadata.push(pack.clone());
    }

    if total_rows != transport_rows {
        return Err(invalid("manifest row total does not match transport rows"));
    }
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: sequence.to_inclusive,
        },
    )?;
    let manifest = SyncManifest {
        protocol: PROTOCOL_V1,
        schema,
        installation_id: binding.installation_id,
        account_id: binding.account_id,
        vehicle_id: binding.vehicle_id,
        generation: binding.generation,
        snapshot_id,
        mode: crate::protocol::TransferMode::FullSnapshot,
        base_sequence: sequence.from_exclusive,
        head_sequence: sequence.to_inclusive,
        chunk_count: u32::try_from(metadata.len())
            .map_err(|_| ProjectionPackError::TooManyChunks)?,
        total_compressed_bytes,
        total_uncompressed_bytes,
        total_rows,
        chunks: metadata,
        terminal_cursor,
    };
    manifest.validate()?;
    manifest.validate_terminal_cursor(cursor_key)?;
    Ok(manifest)
}

/// Whether this candidate created the immutable content-addressed file it
/// references.  The bit is a deletion right, not a statement about whether
/// the object is valid: both variants have passed the same verification.
///
/// A caller may remove an unpublished pack only when it holds `Created`.
/// A `ReusedExisting` pack may already be referenced by a committed catalog
/// entry owned by another candidate, so removing it would corrupt that entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionPackOwnership {
    Created,
    ReusedExisting,
}

/// Durable cleanup receipt for the private staging name used to install a
/// verified content object. `PendingStartupRepair` is still a successful pack
/// publication: the final content name and its directory were synced first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionPackCleanupState {
    Complete,
    PendingStartupRepair,
}

/// A complete, verified immutable object ready for the existing pack catalog.
#[derive(Debug)]
pub struct BuiltProjectionPack {
    pub metadata: TransportPack,
    pub path: PathBuf,
    pub verified: VerifiedTransportPack,
    ownership: ProjectionPackOwnership,
    cleanup_state: ProjectionPackCleanupState,
}

impl BuiltProjectionPack {
    /// Return whether this value created the on-disk content-addressed object.
    pub fn ownership(&self) -> ProjectionPackOwnership {
        self.ownership
    }

    pub fn cleanup_state(&self) -> ProjectionPackCleanupState {
        self.cleanup_state
    }

    /// Candidate cleanup has a deletion right only for a newly linked pack.
    /// Keep this crate-visible so all cleanup paths share the same ownership
    /// boundary instead of open-coding a path-based guess.
    pub(crate) fn may_remove_unpublished_file(&self) -> bool {
        self.ownership == ProjectionPackOwnership::Created
    }
}

impl Clone for BuiltProjectionPack {
    fn clone(&self) -> Self {
        // A clone is only another descriptor for the immutable file. It did
        // not create the hard link, so it must never receive the one-time
        // cleanup right held by the original candidate.
        Self {
            metadata: self.metadata.clone(),
            path: self.path.clone(),
            verified: self.verified,
            ownership: ProjectionPackOwnership::ReusedExisting,
            cleanup_state: self.cleanup_state,
        }
    }
}
