// SPDX-License-Identifier: AGPL-3.0-only

//! Unpublished schema-2.2 physical TeslaMate relation chunks.
//!
//! This module proves the bounded stage-to-pack boundary only. It does not
//! capture PostgreSQL rows and cannot publish a manifest to the Hub catalogue.
//! Selected-car address and geofence rows are owned by their first deterministic
//! referrer so every emitted relation satisfies the V3 same-pack boundary.

use std::{collections::HashSet, fs, mem};

use serde::{Serialize, de::DeserializeOwned};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    hub_pack::{
        BuiltProjectionPack, ProjectionAddressV2_2, ProjectionBinding, ProjectionChargeV2_2,
        ProjectionChargingProcessV2_2, ProjectionGeofenceV2_2, ProjectionPackError,
        ProjectionPackRequestV2_2, ProjectionPackWriter, ProjectionSnapshotV2_2,
        signed_full_snapshot_manifest_with_limits,
    },
    protocol::{CursorKey, ProtocolLimits, SequenceRange, SyncManifest},
    teslamate_projection::{
        TeslaMateAddressPhysicalV2_2, TeslaMateCarPhysicalV2_2, TeslaMateCarSettingsPhysicalV2_2,
        TeslaMateChargePhysicalV2_2, TeslaMateChargingProcessPhysicalV2_2,
        TeslaMateDrivePhysicalV2_2, TeslaMateGeofencePhysicalV2_2, TeslaMatePositionPhysicalV2_2,
        TeslaMateSettingsPhysicalV2_2, TeslaMateStatePhysicalV2_2, TeslaMateUpdatePhysicalV2_2,
    },
    teslamate_stage::{
        TeslaMateStage, TeslaMateStageError, TeslaMateStageFormat, TeslaMateStageState,
        TeslaMateStageTable,
    },
};

const STAGE_PAGE_ROWS: u32 = 10_000;
const CHARGE_STAGE_PAGE_ROWS: u32 = 512;
const DEFAULT_MAX_ROWS_PER_CHUNK: u64 = 50_000;
const DEFAULT_MAX_PROJECTED_JSON_BYTES: u64 = 8 * 1024 * 1024;

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
    pub binding: ProjectionBinding,
    /// Unique rows in the sealed source stage. The signed internal manifest
    /// separately counts transport rows, including roots repeated per chunk.
    pub logical_source_rows: u64,
    cleanup_on_drop: bool,
}

impl StagedPhysicalProjectionV3 {
    /// Transfer deletion ownership only after the exact manifest, pack rows,
    /// and physical admission marker have committed durably.
    pub(crate) fn retain_catalogued_objects(&mut self) {
        self.cleanup_on_drop = false;
    }
}

impl Drop for StagedPhysicalProjectionV3 {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            cleanup_chunks(&mut self.chunks);
        }
    }
}

/// Stream one sealed physical stage into
/// independently verified V3 SQLite chunks, then sign exactly one manifest
/// over all chunks. No catalogue method is reachable from this boundary.
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

    preflight_charges(stage, binding.selected_car_id)?;
    let relation_preflight = preflight_relations(stage, binding.selected_car_id)?;

    let roots = PhysicalRoots {
        global_settings: settings.value.into(),
        car: car.value.into(),
        car_settings: car_settings.value.into(),
    };
    let mut accumulator = PhysicalChunkAccumulator::new(roots, limits)?;
    let mut chunks = Vec::new();
    let mut emitted_address_ids = HashSet::with_capacity(relation_preflight.address_count);
    let mut emitted_geofence_ids = HashSet::with_capacity(relation_preflight.geofence_count);

    let drives = stream_drives(
        stage,
        writer,
        &binding,
        snapshot_id,
        sequence,
        limits,
        &mut accumulator,
        &mut chunks,
        &mut emitted_address_ids,
        &mut emitted_geofence_ids,
        fail_before_ordinal,
    );
    if let Err(error) = drives {
        cleanup_chunks(&mut chunks);
        return Err(error);
    }

    let positions = for_each_page::<TeslaMatePositionPhysicalV2_2, _>(
        stage,
        TeslaMateStageTable::Positions,
        |row| {
            require_source_id(row.source_id, i64::from(row.value.id), "positions")?;
            if i64::from(row.value.car_id) != binding.selected_car_id {
                return Err(TeslaMatePhysicalFragmentError::SelectedCarMismatch);
            }
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
            accumulator.snapshot.positions.push(projected);
            accumulator.add_payload(projected_bytes)?;
            Ok(())
        },
    );
    if let Err(error) = positions {
        cleanup_chunks(&mut chunks);
        return Err(error);
    }

    let charging_processes = stream_charging_processes(
        stage,
        writer,
        &binding,
        snapshot_id,
        sequence,
        limits,
        &mut accumulator,
        &mut chunks,
        &mut emitted_address_ids,
        &mut emitted_geofence_ids,
        fail_before_ordinal,
    );
    if let Err(error) = charging_processes {
        cleanup_chunks(&mut chunks);
        return Err(error);
    }
    if emitted_address_ids.len() != relation_preflight.address_count
        || emitted_geofence_ids.len() != relation_preflight.geofence_count
    {
        cleanup_chunks(&mut chunks);
        return Err(TeslaMatePhysicalFragmentError::UnemittedRelationRows);
    }

    let states =
        for_each_page::<TeslaMateStatePhysicalV2_2, _>(stage, TeslaMateStageTable::States, |row| {
            require_source_id(row.source_id, i64::from(row.value.id), "states")?;
            if i64::from(row.value.car_id) != binding.selected_car_id {
                return Err(TeslaMatePhysicalFragmentError::SelectedCarMismatch);
            }
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
            accumulator.snapshot.states.push(projected);
            accumulator.add_payload(projected_bytes)?;
            Ok(())
        });
    if let Err(error) = states {
        cleanup_chunks(&mut chunks);
        return Err(error);
    }

    let updates = for_each_page::<TeslaMateUpdatePhysicalV2_2, _>(
        stage,
        TeslaMateStageTable::Updates,
        |row| {
            require_source_id(row.source_id, i64::from(row.value.id), "updates")?;
            if i64::from(row.value.car_id) != binding.selected_car_id {
                return Err(TeslaMatePhysicalFragmentError::SelectedCarMismatch);
            }
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
    if let Err(error) = updates {
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
    let manifest = match signed_full_snapshot_manifest_with_limits(
        &binding,
        snapshot_id,
        sequence,
        &chunks,
        total_rows,
        cursor_key,
        ProtocolLimits::hub_sync_v1_1_3_schema_2_2(),
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
        binding,
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
        self.add_payload_group(1, bytes)
    }

    fn group_needs_flush(
        &self,
        group_rows: u64,
        group_bytes: u64,
        limits: TeslaMatePhysicalFragmentLimits,
    ) -> Result<bool, TeslaMatePhysicalFragmentError> {
        let next_rows = self
            .payload_rows
            .checked_add(3)
            .and_then(|value| value.checked_add(group_rows))
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        let next_total_bytes = self
            .projected_json_bytes
            .checked_add(group_bytes)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        if self.payload_rows == 0
            && (next_rows > limits.max_rows_per_chunk
                || next_total_bytes > limits.max_projected_json_bytes)
        {
            return Err(TeslaMatePhysicalFragmentError::ParentRelationsExceedTarget);
        }
        Ok(self.payload_rows != 0
            && (next_rows > limits.max_rows_per_chunk
                || next_total_bytes > limits.max_projected_json_bytes))
    }

    fn add_payload_group(
        &mut self,
        rows: u64,
        bytes: u64,
    ) -> Result<(), TeslaMatePhysicalFragmentError> {
        self.payload_rows = self
            .payload_rows
            .checked_add(rows)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        self.projected_json_bytes = self
            .projected_json_bytes
            .checked_add(bytes)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        Ok(())
    }

    fn ensure_parent_child_fits(
        &self,
        parent_bytes: u64,
        child_bytes: u64,
        limits: TeslaMatePhysicalFragmentLimits,
    ) -> Result<(), TeslaMatePhysicalFragmentError> {
        let total_bytes = self
            .projected_json_bytes
            .checked_add(parent_bytes)
            .and_then(|value| value.checked_add(child_bytes))
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        if self.payload_rows != 0
            || limits.max_rows_per_chunk < 5
            || total_bytes > limits.max_projected_json_bytes
        {
            return Err(TeslaMatePhysicalFragmentError::ParentChildExceedsTarget);
        }
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
    let profile_limits = ProtocolLimits::hub_sync_v1_1_3_schema_2_2();
    if chunks.len() >= profile_limits.max_chunks {
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
    let built = writer.write_physical_snapshot_2_2_for_hub_sync_v1_1_3(&request)?;
    #[cfg(test)]
    eprintln!(
        "physical candidate chunk: ordinal={ordinal}, compressed_bytes={}, profile_limit_bytes={}",
        built.metadata.compressed_bytes, profile_limits.max_compressed_pack_bytes
    );
    if built.metadata.compressed_bytes > profile_limits.max_compressed_pack_bytes {
        let mut rejected = vec![built];
        cleanup_chunks(&mut rejected);
        return Err(TeslaMatePhysicalFragmentError::PackExceedsProfileLimit);
    }
    if let Err(error) = built.verify_with_limits(profile_limits) {
        let mut rejected = vec![built];
        cleanup_chunks(&mut rejected);
        return Err(error.into());
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

#[derive(Debug, Clone, Copy)]
struct RelationPreflight {
    address_count: usize,
    geofence_count: usize,
}

fn preflight_relations(
    stage: &TeslaMateStage,
    selected_car_id: i64,
) -> Result<RelationPreflight, TeslaMatePhysicalFragmentError> {
    let mut referenced_address_ids = HashSet::new();
    let mut referenced_geofence_ids = HashSet::new();
    for_each_page::<TeslaMateDrivePhysicalV2_2, _>(stage, TeslaMateStageTable::Drives, |row| {
        require_source_id(row.source_id, i64::from(row.value.id), "drives")?;
        if i64::from(row.value.car_id) != selected_car_id {
            return Err(TeslaMatePhysicalFragmentError::SelectedCarMismatch);
        }
        referenced_address_ids.extend(
            [row.value.start_address_id, row.value.end_address_id]
                .into_iter()
                .flatten()
                .map(i64::from),
        );
        referenced_geofence_ids.extend(
            [row.value.start_geofence_id, row.value.end_geofence_id]
                .into_iter()
                .flatten()
                .map(i64::from),
        );
        Ok(())
    })?;
    for_each_page::<TeslaMateChargingProcessPhysicalV2_2, _>(
        stage,
        TeslaMateStageTable::ChargingProcesses,
        |row| {
            require_source_id(row.source_id, i64::from(row.value.id), "charging_processes")?;
            if i64::from(row.value.car_id) != selected_car_id {
                return Err(TeslaMatePhysicalFragmentError::SelectedCarMismatch);
            }
            referenced_address_ids.extend(row.value.address_id.map(i64::from));
            referenced_geofence_ids.extend(row.value.geofence_id.map(i64::from));
            Ok(())
        },
    )?;

    let mut address_count = 0_usize;
    for_each_page::<TeslaMateAddressPhysicalV2_2, _>(
        stage,
        TeslaMateStageTable::Addresses,
        |row| {
            require_source_id(row.source_id, i64::from(row.value.id), "addresses")?;
            if !referenced_address_ids.contains(&row.source_id) {
                return Err(TeslaMatePhysicalFragmentError::UnreferencedRelation {
                    table: "addresses",
                    source_id: row.source_id,
                });
            }
            address_count = address_count
                .checked_add(1)
                .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
            Ok(())
        },
    )?;
    let mut geofence_count = 0_usize;
    for_each_page::<TeslaMateGeofencePhysicalV2_2, _>(
        stage,
        TeslaMateStageTable::Geofences,
        |row| {
            require_source_id(row.source_id, i64::from(row.value.id), "geofences")?;
            if !referenced_geofence_ids.contains(&row.source_id) {
                return Err(TeslaMatePhysicalFragmentError::UnreferencedRelation {
                    table: "geofences",
                    source_id: row.source_id,
                });
            }
            geofence_count = geofence_count
                .checked_add(1)
                .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
            Ok(())
        },
    )?;
    Ok(RelationPreflight {
        address_count,
        geofence_count,
    })
}

#[derive(Debug, Default)]
struct OwnedRelations {
    addresses: Vec<ProjectionAddressV2_2>,
    geofences: Vec<ProjectionGeofenceV2_2>,
    serialized_bytes: u64,
}

impl OwnedRelations {
    fn row_count(&self) -> Result<u64, TeslaMatePhysicalFragmentError> {
        self.addresses
            .len()
            .checked_add(self.geofences.len())
            .and_then(|rows| u64::try_from(rows).ok())
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)
    }

    fn is_empty(&self) -> bool {
        self.addresses.is_empty() && self.geofences.is_empty()
    }
}

fn collect_owned_relations(
    stage: &TeslaMateStage,
    address_ids: impl IntoIterator<Item = i32>,
    geofence_ids: impl IntoIterator<Item = i32>,
    emitted_address_ids: &mut HashSet<i32>,
    emitted_geofence_ids: &mut HashSet<i32>,
) -> Result<OwnedRelations, TeslaMatePhysicalFragmentError> {
    let mut owned = OwnedRelations::default();
    for id in address_ids {
        if id <= 0 || emitted_address_ids.contains(&id) {
            continue;
        }
        let Some(value) = stage
            .get::<TeslaMateAddressPhysicalV2_2>(TeslaMateStageTable::Addresses, i64::from(id))?
        else {
            continue;
        };
        require_source_id(i64::from(id), i64::from(value.id), "addresses")?;
        let projected: ProjectionAddressV2_2 = value.into();
        owned.serialized_bytes = owned
            .serialized_bytes
            .checked_add(serialized_bytes(&projected)?)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        emitted_address_ids.insert(id);
        owned.addresses.push(projected);
    }
    for id in geofence_ids {
        if id <= 0 || emitted_geofence_ids.contains(&id) {
            continue;
        }
        let Some(value) = stage
            .get::<TeslaMateGeofencePhysicalV2_2>(TeslaMateStageTable::Geofences, i64::from(id))?
        else {
            continue;
        };
        require_source_id(i64::from(id), i64::from(value.id), "geofences")?;
        let projected: ProjectionGeofenceV2_2 = value.into();
        owned.serialized_bytes = owned
            .serialized_bytes
            .checked_add(serialized_bytes(&projected)?)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        emitted_geofence_ids.insert(id);
        owned.geofences.push(projected);
    }
    Ok(owned)
}

fn append_parent_relations(
    accumulator: &mut PhysicalChunkAccumulator,
    relations: OwnedRelations,
) -> Result<(), TeslaMatePhysicalFragmentError> {
    let rows = relations.row_count()?;
    accumulator.snapshot.addresses.extend(relations.addresses);
    accumulator.snapshot.geofences.extend(relations.geofences);
    accumulator.add_payload_group(rows, relations.serialized_bytes)
}

#[allow(clippy::too_many_arguments)]
fn stream_drives(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: &ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    limits: TeslaMatePhysicalFragmentLimits,
    accumulator: &mut PhysicalChunkAccumulator,
    chunks: &mut Vec<BuiltProjectionPack>,
    emitted_address_ids: &mut HashSet<i32>,
    emitted_geofence_ids: &mut HashSet<i32>,
    fail_before_ordinal: Option<u32>,
) -> Result<(), TeslaMatePhysicalFragmentError> {
    for_each_page::<TeslaMateDrivePhysicalV2_2, _>(stage, TeslaMateStageTable::Drives, |row| {
        require_source_id(row.source_id, i64::from(row.value.id), "drives")?;
        if i64::from(row.value.car_id) != binding.selected_car_id {
            return Err(TeslaMatePhysicalFragmentError::SelectedCarMismatch);
        }
        let address_ids = [row.value.start_address_id, row.value.end_address_id]
            .into_iter()
            .flatten();
        let geofence_ids = [row.value.start_geofence_id, row.value.end_geofence_id]
            .into_iter()
            .flatten();
        let relations = collect_owned_relations(
            stage,
            address_ids,
            geofence_ids,
            emitted_address_ids,
            emitted_geofence_ids,
        )?;
        let projected = row.value.into();
        let projected_bytes = serialized_bytes(&projected)?;
        let group_rows = 1_u64
            .checked_add(relations.row_count()?)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        let group_bytes = projected_bytes
            .checked_add(relations.serialized_bytes)
            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
        let needs_flush = if relations.is_empty() {
            accumulator.needs_flush(projected_bytes, limits)?
        } else {
            accumulator.group_needs_flush(group_rows, group_bytes, limits)?
        };
        if needs_flush {
            flush_chunk(
                writer,
                binding,
                snapshot_id,
                sequence,
                accumulator,
                chunks,
                fail_before_ordinal,
            )?;
            if !relations.is_empty() {
                accumulator.group_needs_flush(group_rows, group_bytes, limits)?;
            }
        }
        accumulator.snapshot.drives.push(projected);
        accumulator.add_payload(projected_bytes)?;
        append_parent_relations(accumulator, relations)
    })
}

fn preflight_charges(
    stage: &TeslaMateStage,
    selected_car_id: i64,
) -> Result<(), TeslaMatePhysicalFragmentError> {
    for_each_page::<TeslaMateChargePhysicalV2_2, _>(stage, TeslaMateStageTable::Charges, |row| {
        require_source_id(row.source_id, i64::from(row.value.id), "charges")?;
        let parent_source_id = i64::from(row.value.charging_process_id);
        let parent = stage
            .get::<TeslaMateChargingProcessPhysicalV2_2>(
                TeslaMateStageTable::ChargingProcesses,
                parent_source_id,
            )?
            .ok_or(TeslaMatePhysicalFragmentError::MissingChargingProcess {
                charge_id: row.value.id,
                charging_process_id: row.value.charging_process_id,
            })?;
        require_source_id(parent_source_id, i64::from(parent.id), "charging_processes")?;
        if i64::from(parent.car_id) != selected_car_id {
            return Err(TeslaMatePhysicalFragmentError::SelectedCarMismatch);
        }
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
fn stream_charging_processes(
    stage: &TeslaMateStage,
    writer: &ProjectionPackWriter,
    binding: &ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    limits: TeslaMatePhysicalFragmentLimits,
    accumulator: &mut PhysicalChunkAccumulator,
    chunks: &mut Vec<BuiltProjectionPack>,
    emitted_address_ids: &mut HashSet<i32>,
    emitted_geofence_ids: &mut HashSet<i32>,
    fail_before_ordinal: Option<u32>,
) -> Result<(), TeslaMatePhysicalFragmentError> {
    for_each_page::<TeslaMateChargingProcessPhysicalV2_2, _>(
        stage,
        TeslaMateStageTable::ChargingProcesses,
        |row| {
            require_source_id(row.source_id, i64::from(row.value.id), "charging_processes")?;
            if i64::from(row.value.car_id) != binding.selected_car_id {
                return Err(TeslaMatePhysicalFragmentError::SelectedCarMismatch);
            }
            let process_id = row.value.id;
            let mut relations = Some(collect_owned_relations(
                stage,
                row.value.address_id,
                row.value.geofence_id,
                emitted_address_ids,
                emitted_geofence_ids,
            )?);
            let projected_process: ProjectionChargingProcessV2_2 = row.value.into();
            let process_bytes = serialized_bytes(&projected_process)?;
            let mut saw_charge = false;
            for_each_charge_for_process(stage, process_id, |charge_row| {
                require_source_id(
                    charge_row.source_id,
                    i64::from(charge_row.value.id),
                    "charges",
                )?;
                if charge_row.value.charging_process_id != process_id {
                    return Err(TeslaMatePhysicalFragmentError::ChargeParentMismatch {
                        charge_id: charge_row.value.id,
                        expected: process_id,
                        actual: charge_row.value.charging_process_id,
                    });
                }
                let projected_charge: ProjectionChargeV2_2 = charge_row.value.into();
                let charge_bytes = serialized_bytes(&projected_charge)?;
                if !saw_charge {
                    if accumulator.payload_rows != 0 {
                        flush_chunk(
                            writer,
                            binding,
                            snapshot_id,
                            sequence,
                            accumulator,
                            chunks,
                            fail_before_ordinal,
                        )?;
                    }
                    let owned = relations
                        .take()
                        .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
                    if owned.is_empty() {
                        accumulator.ensure_parent_child_fits(
                            process_bytes,
                            charge_bytes,
                            limits,
                        )?;
                        accumulator
                            .snapshot
                            .charging_processes
                            .push(projected_process.clone());
                        accumulator.add_payload(process_bytes)?;
                    } else {
                        let group_rows = 1_u64
                            .checked_add(owned.row_count()?)
                            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
                        let group_bytes = process_bytes
                            .checked_add(owned.serialized_bytes)
                            .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
                        accumulator.group_needs_flush(group_rows, group_bytes, limits)?;
                        accumulator
                            .snapshot
                            .charging_processes
                            .push(projected_process.clone());
                        accumulator.add_payload(process_bytes)?;
                        append_parent_relations(accumulator, owned)?;
                        if accumulator.needs_flush(charge_bytes, limits)? {
                            flush_chunk(
                                writer,
                                binding,
                                snapshot_id,
                                sequence,
                                accumulator,
                                chunks,
                                fail_before_ordinal,
                            )?;
                            accumulator.ensure_parent_child_fits(
                                process_bytes,
                                charge_bytes,
                                limits,
                            )?;
                            accumulator
                                .snapshot
                                .charging_processes
                                .push(projected_process.clone());
                            accumulator.add_payload(process_bytes)?;
                        }
                    }
                } else if accumulator.needs_flush(charge_bytes, limits)? {
                    flush_chunk(
                        writer,
                        binding,
                        snapshot_id,
                        sequence,
                        accumulator,
                        chunks,
                        fail_before_ordinal,
                    )?;
                    accumulator.ensure_parent_child_fits(process_bytes, charge_bytes, limits)?;
                    accumulator
                        .snapshot
                        .charging_processes
                        .push(projected_process.clone());
                    accumulator.add_payload(process_bytes)?;
                }
                accumulator.snapshot.charges.push(projected_charge);
                accumulator.add_payload(charge_bytes)?;
                saw_charge = true;
                Ok(())
            })?;
            if saw_charge {
                flush_chunk(
                    writer,
                    binding,
                    snapshot_id,
                    sequence,
                    accumulator,
                    chunks,
                    fail_before_ordinal,
                )?;
            } else {
                let owned = relations
                    .take()
                    .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
                let group_rows = 1_u64
                    .checked_add(owned.row_count()?)
                    .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
                let group_bytes = process_bytes
                    .checked_add(owned.serialized_bytes)
                    .ok_or(TeslaMatePhysicalFragmentError::AccountingOverflow)?;
                let needs_flush = if owned.is_empty() {
                    accumulator.needs_flush(process_bytes, limits)?
                } else {
                    accumulator.group_needs_flush(group_rows, group_bytes, limits)?
                };
                if needs_flush {
                    flush_chunk(
                        writer,
                        binding,
                        snapshot_id,
                        sequence,
                        accumulator,
                        chunks,
                        fail_before_ordinal,
                    )?;
                    if !owned.is_empty() {
                        accumulator.group_needs_flush(group_rows, group_bytes, limits)?;
                    }
                }
                accumulator
                    .snapshot
                    .charging_processes
                    .push(projected_process);
                accumulator.add_payload(process_bytes)?;
                append_parent_relations(accumulator, owned)?;
            }
            Ok(())
        },
    )
}

fn for_each_charge_for_process<F>(
    stage: &TeslaMateStage,
    charging_process_id: i32,
    mut visit: F,
) -> Result<(), TeslaMatePhysicalFragmentError>
where
    F: FnMut(
        crate::teslamate_stage::TeslaMateStageRow<TeslaMateChargePhysicalV2_2>,
    ) -> Result<(), TeslaMatePhysicalFragmentError>,
{
    let mut after_id = 0_i64;
    loop {
        let page = stage.charge_samples_for_process::<TeslaMateChargePhysicalV2_2>(
            i64::from(charging_process_id),
            after_id,
            CHARGE_STAGE_PAGE_ROWS,
        )?;
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
    #[error("one physical V3 charging parent and child exceed the configured chunk target")]
    ParentChildExceedsTarget,
    #[error("one physical V3 parent and its owned relations exceed the configured chunk target")]
    ParentRelationsExceedTarget,
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
    #[error("physical V3 {table} row {source_id} is not referenced by the selected car")]
    UnreferencedRelation { table: &'static str, source_id: i64 },
    #[error("physical V3 relation preflight did not emit every staged relation row")]
    UnemittedRelationRows,
    #[error("physical V3 charge {charge_id} is missing charging process {charging_process_id}")]
    MissingChargingProcess {
        charge_id: i32,
        charging_process_id: i32,
    },
    #[error("physical V3 charge {charge_id} expected process {expected}, decoded process {actual}")]
    ChargeParentMismatch {
        charge_id: i32,
        expected: i32,
        actual: i32,
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
pub(crate) mod tests;
