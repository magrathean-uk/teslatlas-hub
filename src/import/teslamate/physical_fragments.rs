// SPDX-License-Identifier: AGPL-3.0-only

//! Unpublished schema-2.2 update chunks from a sealed physical TeslaMate stage.
//!
//! This module proves the bounded stage-to-pack boundary only. It does not
//! capture PostgreSQL rows and cannot publish a manifest to the Hub catalogue.
//! Relation-bearing history remains fail-closed until a later slice groups
//! each child with the physical parents required by the V3 validator.

use std::{fs, mem};

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    hub_pack::{
        BuiltProjectionPack, ProjectionBinding, ProjectionPackError, ProjectionPackRequestV2_2,
        ProjectionPackWriter, ProjectionSnapshotV2_2, signed_full_snapshot_manifest,
    },
    protocol::{CursorKey, ProtocolLimits, SequenceRange, SyncManifest},
    teslamate_projection::{
        TeslaMateCarPhysicalV2_2, TeslaMateCarSettingsPhysicalV2_2, TeslaMateSettingsPhysicalV2_2,
        TeslaMateUpdatePhysicalV2_2,
    },
    teslamate_stage::{
        TeslaMateStage, TeslaMateStageError, TeslaMateStageFormat, TeslaMateStageState,
        TeslaMateStageTable,
    },
};

const STAGE_PAGE_ROWS: u32 = 10_000;
const DEFAULT_MAX_ROWS_PER_CHUNK: u64 = 50_000;
const DEFAULT_MAX_PROJECTED_JSON_BYTES: u64 = 8 * 1024 * 1024;
const HUB_SYNC_PROFILE_MAX_PACK_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeslaMatePhysicalFragmentLimits {
    pub max_rows_per_chunk: u64,
    pub max_projected_json_bytes: u64,
}

impl Default for TeslaMatePhysicalFragmentLimits {
    fn default() -> Self {
        Self {
            max_rows_per_chunk: DEFAULT_MAX_ROWS_PER_CHUNK,
            max_projected_json_bytes: DEFAULT_MAX_PROJECTED_JSON_BYTES,
        }
    }
}

impl TeslaMatePhysicalFragmentLimits {
    fn validate(self) -> Result<(), TeslaMatePhysicalFragmentError> {
        let protocol = ProtocolLimits::default();
        if self.max_rows_per_chunk < 3 || self.max_rows_per_chunk > protocol.max_rows_per_pack {
            return Err(TeslaMatePhysicalFragmentError::InvalidRowTarget);
        }
        if self.max_projected_json_bytes == 0
            || self.max_projected_json_bytes > protocol.max_uncompressed_pack_bytes
        {
            return Err(TeslaMatePhysicalFragmentError::InvalidByteTarget);
        }
        Ok(())
    }
}

/// A complete signed candidate whose immutable objects are not catalogued.
/// Dropping it removes only files created by this candidate. A later
/// publication slice may explicitly transfer that ownership after a durable
/// catalogue commit.
#[derive(Debug)]
pub struct StagedPhysicalProjectionV3 {
    pub chunks: Vec<BuiltProjectionPack>,
    pub manifest: SyncManifest,
    /// Unique rows in the sealed source stage. The signed internal manifest
    /// separately counts transport rows, including roots repeated per chunk.
    pub logical_source_rows: u64,
    cleanup_on_drop: bool,
}

impl Drop for StagedPhysicalProjectionV3 {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            cleanup_chunks(&mut self.chunks);
        }
    }
}

/// Stream the update rows from one complete, sealed physical source stage into
/// independently verified V3 SQLite chunks, then sign exactly one manifest
/// over all chunks. Any relation-bearing history rejects before the first pack
/// write. No catalogue method is reachable from this boundary.
pub fn write_staged_physical_updates_snapshot_v3(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    cursor_key: &CursorKey,
) -> Result<StagedPhysicalProjectionV3, TeslaMatePhysicalFragmentError> {
    write_staged_physical_updates_snapshot_v3_with_limits(
        stage,
        writer,
        binding,
        snapshot_id,
        sequence,
        cursor_key,
        TeslaMatePhysicalFragmentLimits::default(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn write_staged_physical_updates_snapshot_v3_with_limits(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    cursor_key: &CursorKey,
    limits: TeslaMatePhysicalFragmentLimits,
) -> Result<StagedPhysicalProjectionV3, TeslaMatePhysicalFragmentError> {
    write_staged_physical_updates_snapshot_v3_inner(
        stage,
        writer,
        binding,
        snapshot_id,
        sequence,
        cursor_key,
        limits,
        None,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn write_staged_physical_updates_snapshot_v3_with_test_failure(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    cursor_key: &CursorKey,
    limits: TeslaMatePhysicalFragmentLimits,
    fail_before_ordinal: u32,
) -> Result<StagedPhysicalProjectionV3, TeslaMatePhysicalFragmentError> {
    write_staged_physical_updates_snapshot_v3_inner(
        stage,
        writer,
        binding,
        snapshot_id,
        sequence,
        cursor_key,
        limits,
        Some(fail_before_ordinal),
    )
}

#[allow(clippy::too_many_arguments)]
fn write_staged_physical_updates_snapshot_v3_inner(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    cursor_key: &CursorKey,
    limits: TeslaMatePhysicalFragmentLimits,
    fail_before_ordinal: Option<u32>,
) -> Result<StagedPhysicalProjectionV3, TeslaMatePhysicalFragmentError> {
    limits.validate()?;
    if snapshot_id.is_nil() {
        return Err(TeslaMatePhysicalFragmentError::NilSnapshotId);
    }
    if !sequence.is_ordered() {
        return Err(TeslaMatePhysicalFragmentError::UnorderedSequence);
    }
    let stage_stats = stage.stats()?;
    if stage_stats.state != TeslaMateStageState::Sealed {
        return Err(TeslaMatePhysicalFragmentError::StageNotSealed);
    }
    if stage.format()? != TeslaMateStageFormat::PhysicalV3 {
        return Err(TeslaMatePhysicalFragmentError::WrongStageFormat);
    }
    for table in [
        TeslaMateStageTable::Addresses,
        TeslaMateStageTable::Geofences,
        TeslaMateStageTable::Drives,
        TeslaMateStageTable::Positions,
        TeslaMateStageTable::ChargingProcesses,
        TeslaMateStageTable::Charges,
        TeslaMateStageTable::States,
    ] {
        if !stage
            .page::<serde_json::Value>(table, 0, 1)?
            .rows
            .is_empty()
        {
            return Err(TeslaMatePhysicalFragmentError::UnsupportedTableRows {
                table: table.as_str(),
            });
        }
    }

    let settings =
        exactly_one::<TeslaMateSettingsPhysicalV2_2>(stage, TeslaMateStageTable::GlobalSettings)?;
    require_source_id(settings.source_id, settings.value.id, "global_settings")?;
    let car = exactly_one::<TeslaMateCarPhysicalV2_2>(stage, TeslaMateStageTable::Cars)?;
    require_source_id(car.source_id, i64::from(car.value.id), "cars")?;
    if i64::from(car.value.id) != binding.selected_car_id {
        return Err(TeslaMatePhysicalFragmentError::SelectedCarMismatch);
    }
    let car_settings =
        exactly_one::<TeslaMateCarSettingsPhysicalV2_2>(stage, TeslaMateStageTable::CarSettings)?;
    require_source_id(
        car_settings.source_id,
        car_settings.value.id,
        "car_settings",
    )?;
    if car.value.settings_id != car_settings.value.id {
        return Err(TeslaMatePhysicalFragmentError::CarSettingsMismatch);
    }

    let roots = PhysicalRoots {
        global_settings: settings.value.into(),
        car: car.value.into(),
        car_settings: car_settings.value.into(),
    };
    let mut accumulator = PhysicalChunkAccumulator::new(roots, limits)?;
    let mut chunks = Vec::new();

    let result = for_each_page::<TeslaMateUpdatePhysicalV2_2, _>(
        stage,
        TeslaMateStageTable::Updates,
        |row| {
            require_source_id(row.source_id, i64::from(row.value.id), "updates")?;
            let projected = row.value.into();
            let projected_bytes = serialized_bytes(&projected)?;
            if accumulator.needs_flush(projected_bytes, limits)? {
                flush_chunk(
                    writer,
                    &binding,
                    snapshot_id,
                    sequence,
                    &mut accumulator,
                    &mut chunks,
                    fail_before_ordinal,
                )?;
            }
            accumulator.snapshot.updates.push(projected);
            accumulator.add_payload(projected_bytes)?;
            Ok(())
        },
    );
    if let Err(error) = result {
        cleanup_chunks(&mut chunks);
        return Err(error);
    }

    if accumulator.payload_rows != 0 || chunks.is_empty() {
        if let Err(error) = flush_chunk(
            writer,
            &binding,
            snapshot_id,
            sequence,
            &mut accumulator,
            &mut chunks,
            fail_before_ordinal,
        ) {
            cleanup_chunks(&mut chunks);
            return Err(error);
        }
    }
    let total_rows = chunks.iter().try_fold(0_u64, |total, chunk| {
        total
            .checked_add(chunk.metadata.row_count)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)
    })?;
    let manifest = match signed_full_snapshot_manifest(
        &binding,
        snapshot_id,
        sequence,
        &chunks,
        total_rows,
        cursor_key,
    ) {
        Ok(manifest) => manifest,
        Err(error) => {
            cleanup_chunks(&mut chunks);
            return Err(error.into());
        }
    };
    Ok(StagedPhysicalProjectionV3 {
        chunks,
        manifest,
        logical_source_rows: stage_stats.row_count,
        cleanup_on_drop: true,
    })
}

#[derive(Clone)]
struct PhysicalRoots {
    global_settings: crate::hub_pack::ProjectionGlobalSettingsV2_2,
    car: crate::hub_pack::ProjectionCarV2_2,
    car_settings: crate::hub_pack::ProjectionCarSettingsV2_2,
}

struct PhysicalChunkAccumulator {
    roots: PhysicalRoots,
    snapshot: ProjectionSnapshotV2_2,
    payload_rows: u64,
    projected_json_bytes: u64,
}

impl PhysicalChunkAccumulator {
    fn new(
        roots: PhysicalRoots,
        limits: TeslaMatePhysicalFragmentLimits,
    ) -> Result<Self, TeslaMatePhysicalFragmentError> {
        let projected_json_bytes = root_serialized_bytes(&roots)?;
        if projected_json_bytes > limits.max_projected_json_bytes {
            return Err(TeslaMatePhysicalFragmentError::RootRowsExceedTarget);
        }
        Ok(Self {
            snapshot: snapshot_with_roots(&roots),
            roots,
            payload_rows: 0,
            projected_json_bytes,
        })
    }

    fn needs_flush(
        &self,
        next_bytes: u64,
        limits: TeslaMatePhysicalFragmentLimits,
    ) -> Result<bool, TeslaMatePhysicalFragmentError> {
        let next_rows = self
            .payload_rows
            .checked_add(4)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        let next_total_bytes = self
            .projected_json_bytes
            .checked_add(next_bytes)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        if self.payload_rows == 0
            && (next_rows > limits.max_rows_per_chunk
                || next_total_bytes > limits.max_projected_json_bytes)
        {
            return Err(TeslaMatePhysicalFragmentError::SingleRowExceedsTarget);
        }
        Ok(self.payload_rows != 0
            && (next_rows > limits.max_rows_per_chunk
                || next_total_bytes > limits.max_projected_json_bytes))
    }

    fn add_payload(&mut self, bytes: u64) -> Result<(), TeslaMatePhysicalFragmentError> {
        self.payload_rows = self
            .payload_rows
            .checked_add(1)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        self.projected_json_bytes = self
            .projected_json_bytes
            .checked_add(bytes)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        Ok(())
    }

    fn take_snapshot(&mut self) -> Result<ProjectionSnapshotV2_2, TeslaMatePhysicalFragmentError> {
        self.payload_rows = 0;
        self.projected_json_bytes = root_serialized_bytes(&self.roots)?;
        Ok(mem::replace(
            &mut self.snapshot,
            snapshot_with_roots(&self.roots),
        ))
    }
}

fn snapshot_with_roots(roots: &PhysicalRoots) -> ProjectionSnapshotV2_2 {
    ProjectionSnapshotV2_2 {
        global_settings: vec![roots.global_settings.clone()],
        cars: vec![roots.car.clone()],
        car_settings: vec![roots.car_settings.clone()],
        addresses: Vec::new(),
        geofences: Vec::new(),
        drives: Vec::new(),
        positions: Vec::new(),
        charging_processes: Vec::new(),
        charges: Vec::new(),
        states: Vec::new(),
        updates: Vec::new(),
    }
}

fn flush_chunk(
    writer: &ProjectionPackWriter,
    binding: &ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    accumulator: &mut PhysicalChunkAccumulator,
    chunks: &mut Vec<BuiltProjectionPack>,
    fail_before_ordinal: Option<u32>,
) -> Result<(), TeslaMatePhysicalFragmentError> {
    if chunks.len() >= ProtocolLimits::default().max_chunks {
        return Err(TeslaMatePhysicalFragmentError::TooManyChunks);
    }
    let ordinal =
        u32::try_from(chunks.len()).map_err(|_| TeslaMatePhysicalFragmentError::TooManyChunks)?;
    #[cfg(test)]
    if fail_before_ordinal == Some(ordinal) {
        return Err(TeslaMatePhysicalFragmentError::InjectedTestFailure(ordinal));
    }
    #[cfg(not(test))]
    let _ = fail_before_ordinal;
    let snapshot = accumulator.take_snapshot()?;
    let pack_id = Uuid::new_v5(
        &snapshot_id,
        format!("teslatlas-hub/schema-2.2/chunk/{ordinal}").as_bytes(),
    );
    let request = ProjectionPackRequestV2_2 {
        pack_id,
        snapshot_id,
        ordinal,
        binding: binding.clone(),
        sequence,
        snapshot: &snapshot,
    };
    let built = writer.write_full_snapshot_2_2(&request)?;
    if built.metadata.compressed_bytes > HUB_SYNC_PROFILE_MAX_PACK_BYTES {
        let mut rejected = vec![built];
        cleanup_chunks(&mut rejected);
        return Err(TeslaMatePhysicalFragmentError::PackExceedsProfileLimit);
    }
    chunks.push(built);
    Ok(())
}

fn exactly_one<T: DeserializeOwned>(
    stage: &TeslaMateStage,
    table: TeslaMateStageTable,
) -> Result<crate::teslamate_stage::TeslaMateStageRow<T>, TeslaMatePhysicalFragmentError> {
    let mut page = stage.page::<T>(table, 0, 2)?;
    if page.rows.len() != 1 || page.next_after_id.is_some() {
        return Err(TeslaMatePhysicalFragmentError::RootCardinality {
            table: table.as_str(),
        });
    }
    Ok(page.rows.remove(0))
}

fn for_each_page<T, F>(
    stage: &TeslaMateStage,
    table: TeslaMateStageTable,
    mut visit: F,
) -> Result<(), TeslaMatePhysicalFragmentError>
where
    T: DeserializeOwned,
    F: FnMut(
        crate::teslamate_stage::TeslaMateStageRow<T>,
    ) -> Result<(), TeslaMatePhysicalFragmentError>,
{
    let mut after_id = 0_i64;
    loop {
        let page = stage.page::<T>(table, after_id, STAGE_PAGE_ROWS)?;
        for row in page.rows {
            visit(row)?;
        }
        match page.next_after_id {
            Some(next) => after_id = next,
            None => return Ok(()),
        }
    }
}

fn require_source_id(
    stored: i64,
    decoded: i64,
    table: &'static str,
) -> Result<(), TeslaMatePhysicalFragmentError> {
    if stored != decoded {
        return Err(TeslaMatePhysicalFragmentError::SourceIdMismatch {
            table,
            stored,
            decoded,
        });
    }
    Ok(())
}

fn serialized_bytes<T: Serialize>(value: &T) -> Result<u64, TeslaMatePhysicalFragmentError> {
    u64::try_from(serde_json::to_vec(value)?.len())
        .map_err(|_| TeslaMatePhysicalFragmentError::AccountingOverflow)
}

fn root_serialized_bytes(roots: &PhysicalRoots) -> Result<u64, TeslaMatePhysicalFragmentError> {
    let settings = serialized_bytes(&roots.global_settings)?;
    let car = serialized_bytes(&roots.car)?;
    let car_settings = serialized_bytes(&roots.car_settings)?;
    settings
        .checked_add(car)
        .and_then(|value| value.checked_add(car_settings))
        .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)
}

fn cleanup_chunks(chunks: &mut Vec<BuiltProjectionPack>) {
    for chunk in chunks.drain(..) {
        if chunk.may_remove_unpublished_file() {
            let _ = fs::remove_file(chunk.path);
        }
    }
}

#[derive(Debug, Error)]
pub enum TeslaMatePhysicalFragmentError {
    #[error("physical V3 snapshot ID must not be nil")]
    NilSnapshotId,
    #[error("physical V3 snapshot sequence is unordered")]
    UnorderedSequence,
    #[error("physical V3 row target is invalid")]
    InvalidRowTarget,
    #[error("physical V3 projected-byte target is invalid")]
    InvalidByteTarget,
    #[error("physical V3 writer requires a sealed stage")]
    StageNotSealed,
    #[error("physical V3 writer requires a physical-v3 stage")]
    WrongStageFormat,
    #[error("physical V3 updates writer does not yet support nonempty {table} rows")]
    UnsupportedTableRows { table: &'static str },
    #[error("physical V3 stage must contain exactly one {table} root row")]
    RootCardinality { table: &'static str },
    #[error("physical V3 selected car does not match the binding")]
    SelectedCarMismatch,
    #[error("physical V3 car settings do not match the selected car")]
    CarSettingsMismatch,
    #[error("physical V3 root rows exceed the configured chunk target")]
    RootRowsExceedTarget,
    #[error("one physical V3 source row exceeds the configured chunk target")]
    SingleRowExceedsTarget,
    #[error("physical V3 candidate exceeds the Hub chunk ceiling")]
    TooManyChunks,
    #[error("physical V3 pack exceeds the 16 MiB Hub sync profile bound")]
    PackExceedsProfileLimit,
    #[error("physical V3 row or byte accounting overflowed")]
    AccountingOverflow,
    #[cfg(test)]
    #[error("injected physical V3 failure before chunk {0}")]
    InjectedTestFailure(u32),
    #[error("physical V3 {table} staged source id {stored} does not match decoded id {decoded}")]
    SourceIdMismatch {
        table: &'static str,
        stored: i64,
        decoded: i64,
    },
    #[error("cannot encode physical V3 row: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error(transparent)]
    Stage(#[from] TeslaMateStageError),
    #[error(transparent)]
    Pack(#[from] ProjectionPackError),
}

#[cfg(test)]
#[path = "physical_fragments/tests.rs"]
mod tests;
