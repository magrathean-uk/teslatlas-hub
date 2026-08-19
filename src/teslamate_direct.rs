//! Direct, bounded TeslaMate PostgreSQL to immutable typed-pack production.
//!
//! The source stays inside one read-only repeatable-read transaction. Large
//! child tables are decoded one keyset page at a time and immediately folded
//! into bounded pack fragments; no JSON or whole-history stage is created.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    pin::pin,
};

use futures_util::TryStreamExt;
use rustix::fs::statvfs;
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_postgres::Client;
use uuid::Uuid;

use crate::{
    credentials::TeslaMatePostgresPassword,
    db::StoreError,
    hub_pack::{
        ProjectionBinding, ProjectionCharge, ProjectionDrive, ProjectionPackError,
        ProjectionPackWriter, ProjectionSnapshot,
    },
    protocol::{SequenceRange, Sha256Digest},
    teslamate::ReadOnlySource,
    teslamate_fragments::{
        FragmentAccumulator, PackSink, StagedProjectionPacks, TeslaMateFragmentError,
        TeslaMateFragmentLimits, UpdateFragmentAccumulator, next_fragment_limits, serialized_bytes,
    },
    teslamate_parity::{TeslaMateSourceEvidenceError, TeslaMateSourceEvidenceFingerprint},
    teslamate_projection::{
        ChargeProjectionFacts, DriveRelations, ProjectionReport, TeslaMateAddress,
        TeslaMateCarPhysicalV2_2, TeslaMateCarSettingsPhysicalV2_2, TeslaMateChargingProcess,
        TeslaMateDrive, TeslaMateGeofence, TeslaMatePosition, TeslaMateProjectionError,
        TeslaMateSettingsPhysicalV2_2, TeslaMateState, TeslaMateUpdate,
        TeslaMateUpdatePhysicalV2_2, project_car, project_charge, project_charge_sample,
        project_drive, project_position, project_state, project_update,
    },
    teslamate_projection_state::{TeslaMateProjectionStateCapture, TeslaMateProjectionStateError},
    teslamate_reader::{
        TeslaMateReadLimits, TeslaMateReaderError, TeslaMateSchemaInfo, binary_copy_sql,
        charge_copy_types, decode_binary_charge, decode_binary_position,
        open_exported_snapshot_lease, open_snapshot_capture_lane,
        open_snapshot_session_with_schema, position_copy_types, read_addresses,
        read_car_and_car_settings_v2_2, read_cars, read_charging_processes, read_drives,
        read_geofences, read_open_session, read_settings_v2_2, read_updates_v2_2,
        related_positions_binary_copy_sql,
    },
    teslamate_schema::SourceTable,
};

/// Direct captures normally produce candidate packs and use only the
/// fragment-independent logical fingerprint. The legacy bridge is the sole
/// exception: it recreates the retired physical fingerprint with the retired
/// fragment target, captures state, and deliberately writes no new packs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectCaptureMode {
    PublishPacks,
    LegacyBridgeCapture,
}

/// Exact physical source facts retained from the same exported PostgreSQL
/// snapshot used by the production direct import. These are consumed only by
/// the schema-2.2 full-snapshot publisher after the legacy catalogue commit.
#[derive(Debug, Clone)]
pub(crate) struct DirectUpdatesSourceV2_2 {
    /// SHA-256 of PostgreSQL's exported snapshot token. The raw token is
    /// transaction-local and is never retained or exposed.
    pub postgres_snapshot_sha256: String,
    pub schema: TeslaMateSchemaInfo,
    pub global_settings: TeslaMateSettingsPhysicalV2_2,
    pub car: TeslaMateCarPhysicalV2_2,
    pub car_settings: TeslaMateCarSettingsPhysicalV2_2,
    pub updates: Vec<TeslaMateUpdatePhysicalV2_2>,
}

#[derive(Debug)]
pub(crate) struct DirectSnapshotCapture {
    pub packs: StagedProjectionPacks,
    pub updates_v2_2: DirectUpdatesSourceV2_2,
}

#[allow(clippy::too_many_arguments)]
pub async fn write_direct_full_snapshot(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    read_limits: TeslaMateReadLimits,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
) -> Result<StagedProjectionPacks, TeslaMateDirectError> {
    write_direct_full_snapshot_with_capture_factory(
        source,
        password,
        selected_car_id,
        read_limits,
        writer,
        binding,
        snapshot_id,
        sequence,
        || Ok(None),
        0,
        DirectCaptureMode::PublishPacks,
    )
    .await
    .map(|capture| capture.packs)
}

/// As [`write_direct_full_snapshot`], but attaches one fresh state capture to
/// every attempt. A fragment-limit retry opens another source lane over the
/// same exported snapshot, so the capture must be recreated rather than
/// reusing rows from a discarded candidate.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_direct_full_snapshot_with_projection_state<F>(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    read_limits: TeslaMateReadLimits,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    projection_state_maximum_bytes: u64,
    mut capture_factory: F,
) -> Result<DirectSnapshotCapture, TeslaMateDirectError>
where
    F: FnMut() -> Result<TeslaMateProjectionStateCapture, TeslaMateDirectError>,
{
    write_direct_full_snapshot_with_capture_factory(
        source,
        password,
        selected_car_id,
        read_limits,
        writer,
        binding,
        snapshot_id,
        sequence,
        || capture_factory().map(Some),
        projection_state_maximum_bytes,
        DirectCaptureMode::PublishPacks,
    )
    .await
}

/// Recreate the retired direct-import physical fingerprint for one exact
/// source snapshot while also producing the current logical fingerprint and
/// a sealed-state candidate. Unlike normal capture this never writes a new
/// immutable pack: callers may use it only to bridge a verified legacy base.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn capture_direct_snapshot_for_legacy_bridge<F>(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    read_limits: TeslaMateReadLimits,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    projection_state_maximum_bytes: u64,
    mut capture_factory: F,
) -> Result<DirectSnapshotCapture, TeslaMateDirectError>
where
    F: FnMut() -> Result<TeslaMateProjectionStateCapture, TeslaMateDirectError>,
{
    write_direct_full_snapshot_with_capture_factory(
        source,
        password,
        selected_car_id,
        read_limits,
        writer,
        binding,
        snapshot_id,
        sequence,
        || capture_factory().map(Some),
        projection_state_maximum_bytes,
        DirectCaptureMode::LegacyBridgeCapture,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn write_direct_full_snapshot_with_capture_factory<F>(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    read_limits: TeslaMateReadLimits,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    mut capture_factory: F,
    projection_state_maximum_bytes: u64,
    capture_mode: DirectCaptureMode,
) -> Result<DirectSnapshotCapture, TeslaMateDirectError>
where
    F: FnMut() -> Result<Option<TeslaMateProjectionStateCapture>, TeslaMateDirectError>,
{
    ensure_direct_capture_capacity(
        writer,
        read_limits.maximum_stage_bytes,
        read_limits.minimum_free_bytes,
        projection_state_maximum_bytes,
    )?;
    let (lease, selected_car_id_i16, schema) =
        open_exported_snapshot_lease(source, password, selected_car_id, read_limits).await?;
    let snapshot_token = lease.snapshot_id().to_owned();
    let result = async {
        // Read the authoritative counters through a lane imported from this
        // lease's snapshot. It is therefore safe both to select an initial
        // encoder target and to reconcile every later retry against it.
        let source_counts = read_direct_source_counts_from_exported_snapshot(
            source,
            password,
            &snapshot_token,
            selected_car_id_i16,
            read_limits,
        )
        .await?;
        // The count lane is imported from this exact exported source
        // snapshot. Admit every history-sized in-memory relation before the
        // metadata lanes can decode or allocate one of them.
        let retention = admit_direct_retention(source_counts, read_limits)?;
        // The old physical fingerprint deliberately depended on the emitted
        // fragment layout. A compatibility bridge must therefore replay the
        // old default/retry policy, never the current dense-corpus hint.
        let mut fragment_limits = match capture_mode {
            DirectCaptureMode::PublishPacks => initial_direct_fragment_limits(source_counts),
            DirectCaptureMode::LegacyBridgeCapture => TeslaMateFragmentLimits::default(),
        };
        loop {
            let projection_state = capture_factory()?;
            match write_direct_full_snapshot_once(
                source,
                password,
                selected_car_id,
                selected_car_id_i16,
                read_limits,
                writer,
                binding.clone(),
                snapshot_id,
                sequence,
                &snapshot_token,
                fragment_limits,
                source_counts,
                retention,
                projection_state,
                capture_mode,
                schema.clone(),
            )
            .await
            {
                Err(TeslaMateDirectError::Fragment(TeslaMateFragmentError::TooManyFragments)) => {
                    let next = next_fragment_limits(fragment_limits).ok_or(
                        TeslaMateDirectError::Fragment(TeslaMateFragmentError::TooManyFragments),
                    )?;
                    tracing::warn!(
                        selected_car_id,
                        previous_max_rows_per_fragment = fragment_limits.max_rows_per_fragment,
                        next_max_rows_per_fragment = next.max_rows_per_fragment,
                        previous_max_projected_json_bytes =
                            fragment_limits.max_projected_json_bytes,
                        next_max_projected_json_bytes = next.max_projected_json_bytes,
                        "restarting TeslaMate history capture with larger bounded fragments"
                    );
                    fragment_limits = next;
                }
                result => break result,
            }
        }
    }
    .await;
    let lease_finish = lease.finish().await;
    match (result, lease_finish) {
        (Ok(packs), Ok(())) => Ok(packs),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_direct_full_snapshot_once(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    read_limits: TeslaMateReadLimits,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    snapshot_token: &str,
    fragment_limits: TeslaMateFragmentLimits,
    source_counts: TeslaMateSourceCounts,
    retention: DirectRetentionAdmission,
    projection_state: Option<TeslaMateProjectionStateCapture>,
    capture_mode: DirectCaptureMode,
    schema: TeslaMateSchemaInfo,
) -> Result<DirectSnapshotCapture, TeslaMateDirectError> {
    tracing::debug!(
        snapshot_id = %snapshot_id,
        direct_retained_row_units = retention.retained_row_units,
        related_position_cache_ids = retention.related_position_cache_ids,
        "admitted bounded direct TeslaMate retention"
    );
    let metadata = if read_limits.parallel_copy_lanes >= 2 {
        tokio::try_join!(
            read_addresses_lane(
                source,
                password,
                snapshot_token,
                selected_car_id_i16,
                read_limits
            ),
            read_geofences_lane(
                source,
                password,
                snapshot_token,
                selected_car_id_i16,
                read_limits
            ),
        )
    } else {
        let addresses = read_addresses_lane(
            source,
            password,
            snapshot_token,
            selected_car_id_i16,
            read_limits,
        )
        .await?;
        let geofences = read_geofences_lane(
            source,
            password,
            snapshot_token,
            selected_car_id_i16,
            read_limits,
        )
        .await?;
        Ok((addresses, geofences))
    };
    let (addresses, geofences) = match metadata {
        Ok(metadata) => metadata,
        Err(error) => {
            return Err(error);
        }
    };
    ensure_direct_retained_table_rows(
        "addresses",
        addresses.len(),
        DIRECT_MAX_RETAINED_ADDRESS_ROWS,
    )?;
    ensure_direct_retained_table_rows(
        "geofences",
        geofences.len(),
        DIRECT_MAX_RETAINED_GEOFENCE_ROWS,
    )?;
    validate_direct_count(
        "addresses",
        source_counts.addresses,
        u64::try_from(addresses.len())
            .map_err(|_| TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
    )?;
    validate_direct_count(
        "geofences",
        source_counts.geofences,
        u64::try_from(geofences.len())
            .map_err(|_| TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
    )?;
    let metadata_rows = addresses.len().checked_add(geofences.len()).ok_or(
        TeslaMateDirectError::MaximumRowsExceeded {
            maximum: read_limits.maximum_rows,
        },
    )?;
    if metadata_rows > read_limits.maximum_rows {
        return Err(TeslaMateDirectError::MaximumRowsExceeded {
            maximum: read_limits.maximum_rows,
        });
    }
    let lane = match open_snapshot_capture_lane(source, password, snapshot_token, read_limits).await
    {
        Ok(lane) => lane,
        Err(error) => {
            return Err(error.into());
        }
    };
    tracing::debug!(
        snapshot_id = %snapshot_id,
        "capturing TeslaMate source snapshot"
    );
    // This marker exists only in test builds and remains inert unless the
    // native 10M test explicitly enables its phase trace. It deliberately
    // covers the direct source/projection path while PackSink records its
    // nested immutable-pack and projection-state work separately.
    #[cfg(test)]
    let mut source_projection_phase = native_ten_million_phase_trace::mark(
        native_ten_million_phase_trace::NativeTenMillionPhase::SourceProjection,
    );
    let result = write_from_session(
        &lane.client,
        selected_car_id,
        selected_car_id_i16,
        read_limits,
        metadata_rows,
        addresses,
        geofences,
        retention,
        writer,
        binding,
        snapshot_id,
        sequence,
        fragment_limits,
        source_counts,
        projection_state,
        capture_mode,
        Sha256Digest::of_bytes(snapshot_token.as_bytes()).to_string(),
        schema,
    )
    .await;
    #[cfg(test)]
    source_projection_phase.complete();
    let lane_finish = lane.finish().await;
    match (result, lane_finish) {
        (Ok(packs), Ok(())) => Ok(packs),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

/// Read one car's active TeslaMate sessions through the same source adapter as
/// direct pack production. No pack is written by this bridge.
pub async fn read_direct_open_session(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    limits: TeslaMateReadLimits,
) -> Result<crate::teslamate_projection::TeslaMateOpenSession, TeslaMateDirectError> {
    read_open_session(source, password, selected_car_id, limits)
        .await
        .map_err(Into::into)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TeslaMateSourceCounts {
    pub cars: u64,
    pub drives: u64,
    pub positions: u64,
    #[serde(rename = "chargingProcesses")]
    pub charging_processes: u64,
    pub charges: u64,
    pub states: u64,
    pub addresses: u64,
    pub geofences: u64,
    pub updates: u64,
}

// The direct producer deliberately keeps the high-volume position and charge
// sample histories out of RAM. A small set of parent/metadata relations still
// has to survive across dependent streams, however: drives are needed while
// positions stream; charging processes and their aggregates are needed while
// charge samples stream; and schema 2.1 places state rows in the first pack.
//
// These are *retained object* ceilings, not a second source-history limit.
// They are intentionally much smaller than `TeslaMateReadLimits::maximum_rows`
// (20M by default), and are admitted from exact counts in the exported source
// snapshot before any of the relevant vectors or maps are allocated. The
// canonical 10.4M-position fixture is comfortably below every retained cap.
const DIRECT_MAX_RETAINED_ADDRESS_ROWS: u64 = 32_768;
const DIRECT_MAX_RETAINED_GEOFENCE_ROWS: u64 = 32_768;
const DIRECT_MAX_RETAINED_STATE_ROWS: u64 = 65_536;
const DIRECT_MAX_RETAINED_DRIVE_ROWS: u64 = 32_768;
const DIRECT_MAX_RETAINED_CHARGING_PROCESS_ROWS: u64 = 32_768;
const DIRECT_MAX_RETAINED_SCHEMA_22_UPDATE_ROWS: u64 = 65_536;
const DIRECT_MAX_RELATED_POSITION_CACHE_IDS: u64 = 65_536;
const DIRECT_MAX_RETAINED_ROW_UNITS: u64 = 600_000;

/// Exact bounded-memory admission derived from one exported PostgreSQL
/// snapshot. `related_position_cache_ids` covers both the final cache and the
/// temporary sorted ID set used to fetch it; fixed-size SQL batches add at
/// most `RELATED_POSITION_BATCH_SIZE` IDs beyond that accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirectRetentionAdmission {
    related_position_cache_ids: usize,
    retained_row_units: u64,
}

/// Direct imports must suppress duplicate source histories even when an
/// adaptive fragment retry lays out the same projected facts differently.
///
/// The domain is deliberately separate from the staged-import fingerprint:
/// direct capture is streamed by projection kind, rather than by immutable
/// pack. Each fact is length-delimited and type-tagged so a change in either
/// data, type, or source order changes the digest, while repeated parents,
/// pack IDs, and fragment boundaries cannot.
const DIRECT_LOGICAL_FINGERPRINT_DOMAIN: &[u8] =
    b"teslatlas-hub/teslamate-direct-logical-projection/v1";

#[derive(Clone, Copy)]
#[repr(u8)]
enum DirectProjectionFingerprintFact {
    Car = 1,
    State = 2,
    Drive = 3,
    Position = 4,
    Charge = 5,
    ChargeSample = 6,
    Update = 7,
}

struct DirectProjectionFingerprint {
    digest: Sha256,
}

impl DirectProjectionFingerprint {
    fn new() -> Self {
        let mut digest = Sha256::new();
        digest.update(DIRECT_LOGICAL_FINGERPRINT_DOMAIN);
        Self { digest }
    }

    fn bind_source_evidence(&mut self, source_evidence: &Sha256Digest) {
        self.digest
            .update(b"teslatlas-hub/teslamate-direct-source-evidence-binding/v1");
        self.digest.update(source_evidence.as_bytes());
    }

    fn record<T: Serialize>(
        &mut self,
        kind: DirectProjectionFingerprintFact,
        value: &T,
    ) -> Result<(), TeslaMateDirectError> {
        let canonical = serde_json::to_vec(value)
            .map_err(TeslaMateDirectError::LogicalFingerprintSerialization)?;
        let length = u64::try_from(canonical.len())
            .map_err(|_| TeslaMateDirectError::LogicalFingerprintFactTooLarge)?;
        self.digest.update([kind as u8]);
        self.digest.update(length.to_be_bytes());
        self.digest.update(canonical);
        Ok(())
    }

    fn finish(self) -> Sha256Digest {
        Sha256Digest::from_bytes(self.digest.finalize().into())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TeslaMatePreflightAdmission {
    pub passed: bool,
    pub reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TeslaMatePreflightReport {
    #[serde(rename = "selectedCarId")]
    pub selected_car_id: i64,
    #[serde(rename = "sourceDatabaseBytes")]
    pub source_database_bytes: u64,
    pub schema: TeslaMateSchemaInfo,
    #[serde(rename = "sourceRowCounts")]
    pub source_row_counts: TeslaMateSourceCounts,
    #[serde(rename = "targetAvailableBytes")]
    pub target_available_bytes: u64,
    #[serde(rename = "configuredMaximumRows")]
    pub configured_maximum_rows: usize,
    #[serde(rename = "configuredStagingLimitBytes")]
    pub configured_staging_limit_bytes: u64,
    #[serde(rename = "configuredStagingReserveBytes")]
    pub configured_staging_reserve_bytes: u64,
    pub admission: TeslaMatePreflightAdmission,
}

pub async fn preflight_teslamate_import(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    selected_car_id: i64,
    read_limits: TeslaMateReadLimits,
    target_packs_dir: &Path,
) -> Result<TeslaMatePreflightReport, TeslaMateDirectError> {
    let (session, selected_car_id_i16, schema) =
        open_snapshot_session_with_schema(source, password, selected_car_id, read_limits).await?;
    let result = async {
        let source_database_bytes = read_source_database_size(&session.client).await?;
        let source_row_counts =
            read_direct_source_counts(&session.client, selected_car_id_i16).await?;
        let retention_reason = match admit_direct_retention(source_row_counts, read_limits) {
            Ok(_) => None,
            Err(error) => Some(direct_retention_preflight_reason(&error).ok_or(error)?),
        };
        let (target_available_bytes, capacity_passed) = preflight_target_capacity(
            target_packs_dir,
            read_limits.maximum_stage_bytes,
            read_limits.minimum_free_bytes,
        )?;
        Ok(TeslaMatePreflightReport {
            selected_car_id,
            source_database_bytes,
            schema,
            source_row_counts,
            target_available_bytes,
            configured_maximum_rows: read_limits.maximum_rows,
            configured_staging_limit_bytes: read_limits.maximum_stage_bytes,
            configured_staging_reserve_bytes: read_limits.minimum_free_bytes,
            admission: TeslaMatePreflightAdmission {
                passed: capacity_passed && retention_reason.is_none(),
                reason: (!capacity_passed)
                    .then_some("insufficient_target_capacity")
                    .or(retention_reason),
            },
        })
    }
    .await;
    let finish = session.finish().await;
    match (result, finish) {
        (Ok(report), Ok(())) => Ok(report),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

async fn read_source_database_size(client: &Client) -> Result<u64, TeslaMateDirectError> {
    let row = client
        .query_one(
            "SELECT pg_database_size(current_database())::bigint AS \"bytes\"",
            &[],
        )
        .await?;
    let bytes: i64 = row.try_get("bytes")?;
    u64::try_from(bytes).map_err(|_| TeslaMateDirectError::InvalidSourceDatabaseSize { bytes })
}

fn preflight_target_capacity(
    target_packs_dir: &Path,
    capture_bound_bytes: u64,
    minimum_free_bytes: u64,
) -> Result<(u64, bool), TeslaMateDirectError> {
    let writer = ProjectionPackWriter::new(target_packs_dir);
    match writer.ensure_full_snapshot_capacity_for_capture(capture_bound_bytes, minimum_free_bytes)
    {
        Ok(()) => Ok((target_available_bytes(target_packs_dir)?, true)),
        Err(ProjectionPackError::InsufficientFreeSpace { available, .. }) => Ok((available, false)),
        Err(error) => Err(error.into()),
    }
}

/// Admit the direct capture as one reservation. The pack writer accounts for
/// immutable output and its temporary SQLite/compression files; the bounded
/// projection-state spool is added to its reserve so the two independently
/// bounded writers cannot overcommit a shared filesystem.
fn ensure_direct_capture_capacity(
    writer: &ProjectionPackWriter,
    capture_bound_bytes: u64,
    minimum_free_bytes: u64,
    projection_state_maximum_bytes: u64,
) -> Result<(), TeslaMateDirectError> {
    let combined_reserve = minimum_free_bytes
        .checked_add(projection_state_maximum_bytes)
        .ok_or(TeslaMateDirectError::CombinedCapacityOverflow)?;
    writer.ensure_full_snapshot_capacity_for_capture(capture_bound_bytes, combined_reserve)?;
    Ok(())
}

fn target_available_bytes(path: &Path) -> Result<u64, TeslaMateDirectError> {
    let stats = statvfs(path).map_err(|source| TeslaMateDirectError::TargetFilesystemSpace {
        path: path.to_path_buf(),
        source,
    })?;
    stats
        .f_bavail
        .checked_mul(stats.f_frsize)
        .ok_or(TeslaMateDirectError::TargetCapacityOverflow)
}

async fn read_addresses_lane(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    snapshot_id: &str,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<Vec<TeslaMateAddress>, TeslaMateDirectError> {
    let lane = open_snapshot_capture_lane(source, password, snapshot_id, limits).await?;
    let mut retained = 0;
    let result = read_addresses(&lane.client, selected_car_id, limits, &mut retained).await;
    let finish = lane.finish().await;
    match (result, finish) {
        (Ok(rows), Ok(())) => Ok(rows),
        (Err(error), _) => Err(error.into()),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

async fn read_geofences_lane(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    snapshot_id: &str,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<Vec<TeslaMateGeofence>, TeslaMateDirectError> {
    let lane = open_snapshot_capture_lane(source, password, snapshot_id, limits).await?;
    let mut retained = 0;
    let result = read_geofences(&lane.client, selected_car_id, limits, &mut retained).await;
    let finish = lane.finish().await;
    match (result, finish) {
        (Ok(rows), Ok(())) => Ok(rows),
        (Err(error), _) => Err(error.into()),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

const DIRECT_UPDATES_SQL: &str = r#"
SELECT
  "id",
  "car_id",
  (EXTRACT(EPOCH FROM "start_date") * 1000)::bigint AS "start_date_ms",
  CASE
    WHEN "end_date" IS NULL THEN NULL
    ELSE (EXTRACT(EPOCH FROM "end_date") * 1000)::bigint
  END AS "end_date_ms",
  "version"::text AS "version"
FROM "public"."updates"
WHERE "car_id" = $1 AND "id" > $2
ORDER BY "id" ASC
LIMIT $3
"#;

/// Summary from one paged update scan. The direct producer retains no update
/// history across the scan: the publication pass re-reads this same exported
/// PostgreSQL snapshot into bounded sidecar fragments after the car's latest
/// firmware version has been selected.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectUpdateSummary {
    latest_firmware: Option<String>,
    observed_rows: u64,
    projected_updates: u64,
    skipped_incomplete_updates: u64,
}

async fn read_direct_update_summary(
    client: &Client,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    limits: TeslaMateReadLimits,
) -> Result<DirectUpdateSummary, TeslaMateDirectError> {
    let mut last_id = 0_i32;
    let page_size = i64::from(limits.page_size);
    let mut latest = None::<((i64, i64, i64), String)>;
    let mut observed_rows = 0_u64;
    let mut projected_updates = 0_u64;
    let mut skipped_incomplete_updates = 0_u64;
    loop {
        let page = client
            .query(
                DIRECT_UPDATES_SQL,
                &[&selected_car_id_i16, &last_id, &page_size],
            )
            .await?;
        let page_len = page.len();
        for row in page {
            let id: i32 = row.try_get("id")?;
            if id <= last_id {
                return Err(TeslaMateDirectError::NonProgressingPage("updates"));
            }
            last_id = id;
            observed_rows = observed_rows
                .checked_add(1)
                .ok_or(TeslaMateDirectError::CountOverflow { table: "updates" })?;
            let update = direct_update_from_row(&row)?;
            if update.car_id != selected_car_id {
                return Err(TeslaMateDirectError::UpdateWrongCar {
                    update_id: update.id,
                    expected_car_id: selected_car_id,
                    found_car_id: update.car_id,
                });
            }
            match project_update(&update, selected_car_id)? {
                Some(projected) => {
                    projected_updates = projected_updates
                        .checked_add(1)
                        .ok_or(TeslaMateDirectError::CountOverflow { table: "updates" })?;
                    let order = (projected.end_date_ms, projected.start_date_ms, projected.id);
                    if latest.as_ref().is_none_or(|(current, _)| order > *current) {
                        latest = Some((order, projected.version));
                    }
                }
                None => {
                    skipped_incomplete_updates = skipped_incomplete_updates
                        .checked_add(1)
                        .ok_or(TeslaMateDirectError::CountOverflow { table: "updates" })?;
                }
            }
        }
        if page_len < limits.page_size as usize {
            return Ok(DirectUpdateSummary {
                latest_firmware: latest.map(|(_, version)| version),
                observed_rows,
                projected_updates,
                skipped_incomplete_updates,
            });
        }
    }
}

fn direct_update_from_row(
    row: &tokio_postgres::Row,
) -> Result<TeslaMateUpdate, TeslaMateDirectError> {
    Ok(TeslaMateUpdate {
        id: i64::from(row.try_get::<_, i32>("id")?),
        car_id: i64::from(row.try_get::<_, i16>("car_id")?),
        start_date_ms: row.try_get("start_date_ms")?,
        end_date_ms: row.try_get("end_date_ms")?,
        version: row.try_get("version")?,
    })
}

#[allow(clippy::too_many_arguments)]
async fn write_direct_update_fragments(
    client: &Client,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    limits: TeslaMateReadLimits,
    fragment_limits: TeslaMateFragmentLimits,
    car: &crate::hub_pack::ProjectionCar,
    sink: &mut PackSink<'_>,
    logical_fingerprint: &mut DirectProjectionFingerprint,
    report: &mut ProjectionReport,
) -> Result<DirectUpdateSummary, TeslaMateDirectError> {
    let mut last_id = 0_i32;
    let page_size = i64::from(limits.page_size);
    let mut summary = DirectUpdateSummary {
        latest_firmware: None,
        observed_rows: 0,
        projected_updates: 0,
        skipped_incomplete_updates: 0,
    };
    let mut latest = None::<((i64, i64, i64), String)>;
    let mut accumulator = UpdateFragmentAccumulator::new(car.clone(), fragment_limits)?;
    loop {
        let page = client
            .query(
                DIRECT_UPDATES_SQL,
                &[&selected_car_id_i16, &last_id, &page_size],
            )
            .await?;
        let page_len = page.len();
        for row in page {
            let id: i32 = row.try_get("id")?;
            if id <= last_id {
                return Err(TeslaMateDirectError::NonProgressingPage("updates"));
            }
            last_id = id;
            summary.observed_rows = summary
                .observed_rows
                .checked_add(1)
                .ok_or(TeslaMateDirectError::CountOverflow { table: "updates" })?;
            let update = direct_update_from_row(&row)?;
            if update.car_id != selected_car_id {
                return Err(TeslaMateDirectError::UpdateWrongCar {
                    update_id: update.id,
                    expected_car_id: selected_car_id,
                    found_car_id: update.car_id,
                });
            }
            match project_update(&update, selected_car_id)? {
                Some(projected) => {
                    logical_fingerprint
                        .record(DirectProjectionFingerprintFact::Update, &projected)?;
                    let order = (projected.end_date_ms, projected.start_date_ms, projected.id);
                    if latest.as_ref().is_none_or(|(current, _)| order > *current) {
                        latest = Some((order, projected.version.clone()));
                    }
                    accumulator.push(sink, projected)?;
                    summary.projected_updates = summary
                        .projected_updates
                        .checked_add(1)
                        .ok_or(TeslaMateDirectError::CountOverflow { table: "updates" })?;
                    report.projected_updates = report
                        .projected_updates
                        .checked_add(1)
                        .ok_or(TeslaMateDirectError::CountOverflow { table: "updates" })?;
                }
                None => {
                    summary.skipped_incomplete_updates = summary
                        .skipped_incomplete_updates
                        .checked_add(1)
                        .ok_or(TeslaMateDirectError::CountOverflow { table: "updates" })?;
                    report.skipped_incomplete_updates = report
                        .skipped_incomplete_updates
                        .checked_add(1)
                        .ok_or(TeslaMateDirectError::CountOverflow { table: "updates" })?;
                }
            }
        }
        if page_len < limits.page_size as usize {
            accumulator.flush(sink)?;
            summary.latest_firmware = latest.map(|(_, version)| version);
            return Ok(summary);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_from_session(
    client: &Client,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    read_limits: TeslaMateReadLimits,
    mut retained_rows: usize,
    addresses: Vec<TeslaMateAddress>,
    geofences: Vec<TeslaMateGeofence>,
    retention: DirectRetentionAdmission,
    writer: &ProjectionPackWriter,
    binding: ProjectionBinding,
    snapshot_id: Uuid,
    sequence: SequenceRange,
    fragment_limits: TeslaMateFragmentLimits,
    source_counts: TeslaMateSourceCounts,
    projection_state: Option<TeslaMateProjectionStateCapture>,
    capture_mode: DirectCaptureMode,
    postgres_snapshot_sha256: String,
    schema: TeslaMateSchemaInfo,
) -> Result<DirectSnapshotCapture, TeslaMateDirectError> {
    let mut schema_22_retained_rows = 0_usize;
    let global_settings =
        read_settings_v2_2(client, read_limits, &mut schema_22_retained_rows).await?;
    let (physical_car, physical_car_settings) = read_car_and_car_settings_v2_2(
        client,
        selected_car_id_i16,
        read_limits,
        &mut schema_22_retained_rows,
    )
    .await?;
    let physical_updates = read_updates_v2_2(
        client,
        selected_car_id_i16,
        read_limits,
        &mut schema_22_retained_rows,
    )
    .await?;
    validate_direct_count(
        "updates",
        source_counts.updates,
        u64::try_from(physical_updates.len())
            .map_err(|_| TeslaMateDirectError::CountOverflow { table: "updates" })?,
    )?;
    let updates_v2_2 = DirectUpdatesSourceV2_2 {
        postgres_snapshot_sha256,
        schema,
        global_settings,
        car: physical_car,
        car_settings: physical_car_settings,
        updates: physical_updates,
    };
    let cars = read_cars(client, selected_car_id_i16, read_limits, &mut retained_rows).await?;
    ensure_direct_retained_table_rows("cars", cars.len(), 1)?;
    let car = cars
        .first()
        .ok_or(TeslaMateDirectError::SelectedCarMissing)?;
    let drives = read_drives(client, selected_car_id_i16, read_limits, &mut retained_rows).await?;
    ensure_direct_retained_table_rows("drives", drives.len(), DIRECT_MAX_RETAINED_DRIVE_ROWS)?;
    let processes =
        read_charging_processes(client, selected_car_id_i16, read_limits, &mut retained_rows)
            .await?;
    ensure_direct_retained_table_rows(
        "charging_processes",
        processes.len(),
        DIRECT_MAX_RETAINED_CHARGING_PROCESS_ROWS,
    )?;
    let states = read_direct_states(
        client,
        selected_car_id_i16,
        read_limits,
        &mut retained_rows,
        DIRECT_MAX_RETAINED_STATE_ROWS,
    )
    .await?;

    // These are the typed PostgreSQL values, before THP1 projection drops the
    // drive odometer endpoints or validates the state text domain. Independent
    // per-kind streams keep the digest stable across capture-lane scheduling.
    let mut source_evidence = TeslaMateSourceEvidenceFingerprint::new();
    for drive in &drives {
        source_evidence.record_drive(drive)?;
    }
    for state in &states {
        source_evidence.record_state(state)?;
    }
    let source_evidence = source_evidence.finish();

    let update_summary =
        read_direct_update_summary(client, selected_car_id, selected_car_id_i16, read_limits)
            .await?;
    validate_direct_count(
        "updates",
        source_counts.updates,
        update_summary.observed_rows,
    )?;
    let projected_car = project_car(car, update_summary.latest_firmware.clone())?;
    let imported_geofences = geofences.clone();
    let address_by_id =
        direct_rows_by_id("addresses", addresses, DIRECT_MAX_RETAINED_ADDRESS_ROWS)?;
    let geofence_by_id =
        direct_rows_by_id("geofences", geofences, DIRECT_MAX_RETAINED_GEOFENCE_ROWS)?;
    let mut projected_states = Vec::with_capacity(states.len());
    for state in states {
        if let Some(projected) = project_state(&state, selected_car_id)? {
            projected_states.push(projected);
        }
    }
    let projected_state_count = u64::try_from(projected_states.len())
        .map_err(|_| TeslaMateDirectError::CountOverflow { table: "states" })?;
    let mut logical_fingerprint = DirectProjectionFingerprint::new();
    logical_fingerprint.bind_source_evidence(&source_evidence);
    logical_fingerprint.record(DirectProjectionFingerprintFact::Car, &projected_car)?;
    for state in &projected_states {
        logical_fingerprint.record(DirectProjectionFingerprintFact::State, state)?;
    }
    let sink = PackSink::new_with_schema_2_1(
        writer,
        binding,
        snapshot_id,
        sequence,
        projected_states,
        true,
    );
    let mut sink = match capture_mode {
        DirectCaptureMode::PublishPacks => sink.without_physical_fingerprint(),
        DirectCaptureMode::LegacyBridgeCapture => sink.capture_only(),
    };
    if let Some(projection_state) = projection_state {
        sink = sink.with_projection_state_capture(projection_state);
    }
    let mut report = ProjectionReport {
        projected_states: projected_state_count,
        ..ProjectionReport::default()
    };
    let mut related_positions = HashMap::new();
    prefetch_related_positions(
        client,
        selected_car_id_i16,
        &drives,
        &processes,
        &mut related_positions,
        retention.related_position_cache_ids,
    )
    .await?;

    let projected_drives = write_drives(
        selected_car_id,
        drives,
        &address_by_id,
        &geofence_by_id,
        &mut related_positions,
        &projected_car,
        fragment_limits,
        &mut sink,
        &mut logical_fingerprint,
        &mut report,
    )
    .await?;
    write_positions(
        client,
        selected_car_id,
        selected_car_id_i16,
        read_limits,
        &mut retained_rows,
        &projected_car,
        &projected_drives,
        fragment_limits,
        &mut sink,
        &mut logical_fingerprint,
        &mut report,
    )
    .await?;
    write_charges(
        client,
        selected_car_id,
        selected_car_id_i16,
        read_limits,
        &mut retained_rows,
        processes,
        &address_by_id,
        &geofence_by_id,
        &mut related_positions,
        &projected_car,
        fragment_limits,
        &mut sink,
        &mut logical_fingerprint,
        &mut report,
        DIRECT_MAX_RETAINED_CHARGING_PROCESS_ROWS,
    )
    .await?;
    let updates_accounted = match capture_mode {
        DirectCaptureMode::PublishPacks => {
            let emitted = write_direct_update_fragments(
                client,
                selected_car_id,
                selected_car_id_i16,
                read_limits,
                fragment_limits,
                &projected_car,
                &mut sink,
                &mut logical_fingerprint,
                &mut report,
            )
            .await?;
            if emitted != update_summary {
                return Err(TeslaMateDirectError::UpdateProjectionReconciliation);
            }
            report
                .projected_updates
                .checked_add(report.skipped_incomplete_updates)
                .ok_or(TeslaMateDirectError::CountOverflow { table: "updates" })?
        }
        // The one-time bridge must reproduce the retired pack/state shape
        // exactly. It validates update source count but leaves newly supported
        // update rows for the first ordinary successor after the bridge.
        DirectCaptureMode::LegacyBridgeCapture => update_summary.observed_rows,
    };
    validate_direct_source_counts(source_counts, report, updates_accounted)?;

    if !sink.has_written_fragments() {
        sink.write(ProjectionSnapshot {
            cars: vec![projected_car.clone()],
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
        })?;
    }
    let fingerprint = logical_fingerprint.finish();
    let legacy_physical_fingerprint = match capture_mode {
        DirectCaptureMode::PublishPacks => None,
        DirectCaptureMode::LegacyBridgeCapture => Some(
            sink.fingerprint()
                .ok_or(TeslaMateDirectError::LegacyPhysicalFingerprintMissing)?,
        ),
    };
    let (chunks, projection_state, selected_car) = sink.into_parts();
    Ok(DirectSnapshotCapture {
        packs: StagedProjectionPacks::new_with_projection_state(
            chunks,
            report,
            fingerprint,
            legacy_physical_fingerprint,
            imported_geofences,
            projection_state,
            selected_car,
        ),
        updates_v2_2,
    })
}

async fn read_direct_states(
    client: &Client,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    maximum_retained_states: u64,
) -> Result<Vec<TeslaMateState>, TeslaMateDirectError> {
    const DIRECT_STATES_SQL: &str = r#"
SELECT
  "id",
  "car_id",
  "state"::text AS "state",
  (EXTRACT(EPOCH FROM "start_date") * 1000)::bigint AS "start_date_ms",
  CASE
    WHEN "end_date" IS NULL THEN NULL
    ELSE (EXTRACT(EPOCH FROM "end_date") * 1000)::bigint
  END AS "end_date_ms"
FROM "public"."states"
WHERE "car_id" = $1 AND "id" > $2
ORDER BY "id" ASC
LIMIT $3
"#;
    let mut last_id = 0_i32;
    let mut states = Vec::new();
    let page_size = i64::from(limits.page_size);
    loop {
        let page = client
            .query(DIRECT_STATES_SQL, &[&selected_car_id, &last_id, &page_size])
            .await?;
        let page_len = page.len();
        for row in page {
            let id: i32 = row.try_get("id")?;
            if id <= last_id {
                return Err(TeslaMateDirectError::NonProgressingPage("states"));
            }
            last_id = id;
            *retained_rows =
                retained_rows
                    .checked_add(1)
                    .ok_or(TeslaMateDirectError::MaximumRowsExceeded {
                        maximum: limits.maximum_rows,
                    })?;
            if *retained_rows > limits.maximum_rows {
                return Err(TeslaMateDirectError::MaximumRowsExceeded {
                    maximum: limits.maximum_rows,
                });
            }
            ensure_direct_retained_table_rows(
                "states",
                states
                    .len()
                    .checked_add(1)
                    .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
                maximum_retained_states,
            )?;
            states.push(TeslaMateState {
                id: i64::from(id),
                car_id: i64::from(row.try_get::<_, i16>("car_id")?),
                state: row.try_get("state")?,
                start_date_ms: row.try_get("start_date_ms")?,
                end_date_ms: row.try_get("end_date_ms")?,
            });
        }
        if page_len < limits.page_size as usize {
            return Ok(states);
        }
    }
}

const DENSE_DIRECT_POSITION_THRESHOLD: u64 = 10_000_000;
const DENSE_DIRECT_FRAGMENT_LIMITS: TeslaMateFragmentLimits = TeslaMateFragmentLimits {
    max_rows_per_fragment: 100_000,
    max_projected_json_bytes: 16 * 1024 * 1024,
};

/// Read the counts used for both dense-fragment selection and final report
/// reconciliation from a lane imported from the live capture lease. The
/// owner lease remains open until all capture attempts are complete, so every
/// retry observes this exact source snapshot.
async fn read_direct_source_counts_from_exported_snapshot(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    snapshot_token: &str,
    selected_car_id: i16,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateSourceCounts, TeslaMateDirectError> {
    let lane = open_snapshot_capture_lane(source, password, snapshot_token, limits).await?;
    let result = read_direct_source_counts(&lane.client, selected_car_id).await;
    let finish = lane.finish().await;
    match (result, finish) {
        (Ok(counts), Ok(())) => Ok(counts),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

/// Prefer fewer bounded direct fragments for the measured high-volume
/// telemetry shape. Source counts are only a performance hint: the pack
/// writer remains the authority for every protocol size and row limit.
fn initial_direct_fragment_limits(source_counts: TeslaMateSourceCounts) -> TeslaMateFragmentLimits {
    if source_counts.positions >= DENSE_DIRECT_POSITION_THRESHOLD {
        DENSE_DIRECT_FRAGMENT_LIMITS
    } else {
        TeslaMateFragmentLimits::default()
    }
}

async fn read_direct_source_counts(
    client: &Client,
    selected_car_id: i16,
) -> Result<TeslaMateSourceCounts, TeslaMateDirectError> {
    const DIRECT_SOURCE_COUNT_SQL: &str = r#"
SELECT
  (SELECT COUNT(*)::bigint FROM "public"."cars" WHERE "id" = $1) AS "cars",
  (SELECT COUNT(*)::bigint FROM "public"."drives" WHERE "car_id" = $1) AS "drives",
  (SELECT COUNT(*)::bigint FROM "public"."positions" WHERE "car_id" = $1) AS "positions",
  (SELECT COUNT(*)::bigint FROM "public"."charging_processes" WHERE "car_id" = $1)
    AS "charging_processes",
  (
    SELECT COUNT(*)::bigint
    FROM "public"."charges" AS "charge"
    JOIN "public"."charging_processes" AS "process"
      ON "process"."id" = "charge"."charging_process_id"
    WHERE "process"."car_id" = $1
  ) AS "charges",
  (SELECT COUNT(*)::bigint FROM "public"."states" WHERE "car_id" = $1) AS "states",
  (
    SELECT COUNT(*)::bigint
    FROM "public"."addresses" AS "source"
    WHERE EXISTS (
      SELECT 1
      FROM "public"."drives" AS "drive"
      WHERE "drive"."car_id" = $1
        AND (
          "drive"."start_address_id" = "source"."id"
          OR "drive"."end_address_id" = "source"."id"
        )
    )
    OR EXISTS (
      SELECT 1
      FROM "public"."charging_processes" AS "process"
      WHERE "process"."car_id" = $1
        AND "process"."address_id" = "source"."id"
    )
  ) AS "addresses",
  (
    SELECT COUNT(*)::bigint
    FROM "public"."geofences" AS "source"
    WHERE EXISTS (
      SELECT 1
      FROM "public"."drives" AS "drive"
      WHERE "drive"."car_id" = $1
        AND (
          "drive"."start_geofence_id" = "source"."id"
          OR "drive"."end_geofence_id" = "source"."id"
        )
    )
    OR EXISTS (
      SELECT 1
      FROM "public"."charging_processes" AS "process"
      WHERE "process"."car_id" = $1
        AND "process"."geofence_id" = "source"."id"
    )
  ) AS "geofences",
  (SELECT COUNT(*)::bigint FROM "public"."updates" WHERE "car_id" = $1) AS "updates"
"#;
    let row = client
        .query_one(DIRECT_SOURCE_COUNT_SQL, &[&selected_car_id])
        .await?;
    Ok(TeslaMateSourceCounts {
        cars: source_count(&row, "cars")?,
        drives: source_count(&row, "drives")?,
        positions: source_count(&row, "positions")?,
        charging_processes: source_count(&row, "charging_processes")?,
        charges: source_count(&row, "charges")?,
        states: source_count(&row, "states")?,
        addresses: source_count(&row, "addresses")?,
        geofences: source_count(&row, "geofences")?,
        updates: source_count(&row, "updates")?,
    })
}

fn admit_direct_retention(
    source: TeslaMateSourceCounts,
    read_limits: TeslaMateReadLimits,
) -> Result<DirectRetentionAdmission, TeslaMateDirectError> {
    let source_rows = direct_source_row_count(source)?;
    let configured_maximum = u64::try_from(read_limits.maximum_rows)
        .map_err(|_| TeslaMateDirectError::DirectRetentionAccountingOverflow)?;
    if source_rows > configured_maximum {
        return Err(TeslaMateDirectError::MaximumRowsExceeded {
            maximum: read_limits.maximum_rows,
        });
    }

    ensure_direct_source_table_cap("cars", source.cars, 1)?;
    ensure_direct_source_table_cap(
        "addresses",
        source.addresses,
        DIRECT_MAX_RETAINED_ADDRESS_ROWS,
    )?;
    ensure_direct_source_table_cap(
        "geofences",
        source.geofences,
        DIRECT_MAX_RETAINED_GEOFENCE_ROWS,
    )?;
    ensure_direct_source_table_cap("states", source.states, DIRECT_MAX_RETAINED_STATE_ROWS)?;
    ensure_direct_source_table_cap(
        "updates_schema_2_2",
        source.updates,
        DIRECT_MAX_RETAINED_SCHEMA_22_UPDATE_ROWS,
    )?;
    ensure_direct_source_table_cap("drives", source.drives, DIRECT_MAX_RETAINED_DRIVE_ROWS)?;
    ensure_direct_source_table_cap(
        "charging_processes",
        source.charging_processes,
        DIRECT_MAX_RETAINED_CHARGING_PROCESS_ROWS,
    )?;

    let related_position_ids = source
        .drives
        .checked_mul(2)
        .and_then(|count| count.checked_add(source.charging_processes))
        .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?;
    ensure_direct_source_table_cap(
        "related_positions",
        related_position_ids,
        DIRECT_MAX_RELATED_POSITION_CACHE_IDS,
    )?;

    // A `Vec` being consumed into a map may coexist with its destination map.
    // Geofences additionally have an outbound clone, states coexist with their
    // projected schema-2.1 values, and a charging process can coexist with its
    // ID set, facts, sample count, and projected-parent map. Count those
    // concrete live copies rather than pretending every source row is one
    // allocation. Positions and charge samples are deliberately absent: their
    // direct readers stream one row at a time into already bounded fragments.
    let retained_row_units = checked_retained_units(&[
        source.cars,
        source
            .addresses
            .checked_mul(2)
            .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
        source
            .geofences
            .checked_mul(3)
            .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
        source
            .states
            .checked_mul(2)
            .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
        source
            .drives
            .checked_mul(2)
            .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
        source
            .charging_processes
            .checked_mul(5)
            .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
        source.updates,
        related_position_ids
            .checked_mul(2)
            .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
    ])?;
    if retained_row_units > DIRECT_MAX_RETAINED_ROW_UNITS {
        return Err(TeslaMateDirectError::DirectRetainedAggregateLimitExceeded {
            requested: retained_row_units,
            maximum: DIRECT_MAX_RETAINED_ROW_UNITS,
        });
    }
    Ok(DirectRetentionAdmission {
        related_position_cache_ids: usize::try_from(related_position_ids)
            .map_err(|_| TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
        retained_row_units,
    })
}

fn direct_source_row_count(source: TeslaMateSourceCounts) -> Result<u64, TeslaMateDirectError> {
    checked_retained_units(&[
        source.cars,
        source.drives,
        source.positions,
        source.charging_processes,
        source.charges,
        source.states,
        source.addresses,
        source.geofences,
        source.updates,
    ])
}

fn direct_retention_preflight_reason(error: &TeslaMateDirectError) -> Option<&'static str> {
    match error {
        TeslaMateDirectError::MaximumRowsExceeded { .. } => Some("source_row_ceiling_exceeded"),
        TeslaMateDirectError::DirectRetainedTableLimitExceeded { .. } => {
            Some("direct_retained_table_limit_exceeded")
        }
        TeslaMateDirectError::DirectRetainedAggregateLimitExceeded { .. } => {
            Some("direct_retained_aggregate_limit_exceeded")
        }
        _ => None,
    }
}

fn checked_retained_units(values: &[u64]) -> Result<u64, TeslaMateDirectError> {
    values.iter().try_fold(0_u64, |total, value| {
        total
            .checked_add(*value)
            .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)
    })
}

fn ensure_direct_source_table_cap(
    table: &'static str,
    requested: u64,
    maximum: u64,
) -> Result<(), TeslaMateDirectError> {
    if requested > maximum {
        return Err(TeslaMateDirectError::DirectRetainedTableLimitExceeded {
            table,
            requested,
            maximum,
        });
    }
    Ok(())
}

fn ensure_direct_retained_table_rows(
    table: &'static str,
    requested: usize,
    maximum: u64,
) -> Result<(), TeslaMateDirectError> {
    ensure_direct_source_table_cap(
        table,
        u64::try_from(requested)
            .map_err(|_| TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
        maximum,
    )
}

fn direct_rows_by_id<T>(
    table: &'static str,
    rows: Vec<T>,
    maximum: u64,
) -> Result<HashMap<i64, T>, TeslaMateDirectError>
where
    T: DirectSourceRowId,
{
    ensure_direct_retained_table_rows(table, rows.len(), maximum)?;
    let mut by_id = HashMap::with_capacity(rows.len());
    for row in rows {
        let id = row.direct_source_id();
        if by_id.insert(id, row).is_some() {
            return Err(TeslaMateDirectError::DuplicateDirectRetainedId { table, id });
        }
    }
    Ok(by_id)
}

trait DirectSourceRowId {
    fn direct_source_id(&self) -> i64;
}

impl DirectSourceRowId for TeslaMateAddress {
    fn direct_source_id(&self) -> i64 {
        self.id
    }
}

impl DirectSourceRowId for TeslaMateGeofence {
    fn direct_source_id(&self) -> i64 {
        self.id
    }
}

fn insert_direct_bounded_map<T>(
    table: &'static str,
    rows: &mut HashMap<i64, T>,
    id: i64,
    value: T,
    maximum: u64,
) -> Result<(), TeslaMateDirectError> {
    if rows.contains_key(&id) {
        return Err(TeslaMateDirectError::DuplicateDirectRetainedId { table, id });
    }
    let requested = rows
        .len()
        .checked_add(1)
        .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?;
    ensure_direct_retained_table_rows(table, requested, maximum)?;
    rows.insert(id, value);
    Ok(())
}

fn source_count(
    row: &tokio_postgres::Row,
    column: &'static str,
) -> Result<u64, TeslaMateDirectError> {
    let count: i64 = row.try_get(column)?;
    u64::try_from(count).map_err(|_| TeslaMateDirectError::InvalidSourceCount { column, count })
}

fn validate_direct_source_counts(
    source: TeslaMateSourceCounts,
    report: ProjectionReport,
    updates_accounted: u64,
) -> Result<(), TeslaMateDirectError> {
    validate_direct_count("cars", source.cars, 1)?;
    validate_direct_count(
        "drives",
        source.drives,
        report
            .completed_drives
            .checked_add(report.skipped_open_drives)
            .ok_or(TeslaMateDirectError::CountOverflow { table: "drives" })?,
    )?;
    validate_direct_count(
        "positions",
        source.positions,
        report
            .projected_positions
            .checked_add(report.skipped_unattached_positions)
            .ok_or(TeslaMateDirectError::CountOverflow { table: "positions" })?,
    )?;
    validate_direct_count(
        "charging_processes",
        source.charging_processes,
        report.projected_charges,
    )?;
    validate_direct_count("charges", source.charges, report.projected_charge_samples)?;
    validate_direct_count("states", source.states, report.projected_states)?;
    validate_direct_count("updates", source.updates, updates_accounted)
}

fn validate_direct_count(
    table: &'static str,
    source: u64,
    accounted: u64,
) -> Result<(), TeslaMateDirectError> {
    if source == accounted {
        Ok(())
    } else {
        Err(TeslaMateDirectError::UnexplainedSourceRows {
            table,
            source_rows: source,
            accounted,
        })
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_drives(
    selected_car_id: i64,
    drives: Vec<TeslaMateDrive>,
    addresses: &HashMap<i64, TeslaMateAddress>,
    geofences: &HashMap<i64, TeslaMateGeofence>,
    related_positions: &mut HashMap<i64, TeslaMatePosition>,
    car: &crate::hub_pack::ProjectionCar,
    limits: TeslaMateFragmentLimits,
    sink: &mut PackSink<'_>,
    logical_fingerprint: &mut DirectProjectionFingerprint,
    report: &mut ProjectionReport,
) -> Result<HashMap<i64, ProjectionDrive>, TeslaMateDirectError> {
    let mut projected_by_id = HashMap::with_capacity(drives.len());
    let mut accumulator = FragmentAccumulator::new(car.clone(), limits)?;
    for drive in drives {
        let start_position = related_position(drive.start_position_id, related_positions)?;
        let end_position = related_position(drive.end_position_id, related_positions)?;
        let projected = project_drive(
            &drive,
            selected_car_id,
            DriveRelations {
                start_position: start_position.as_ref(),
                end_position: end_position.as_ref(),
                start_address: related(addresses, drive.start_address_id),
                end_address: related(addresses, drive.end_address_id),
                start_geofence: related(geofences, drive.start_geofence_id),
                end_geofence: related(geofences, drive.end_geofence_id),
            },
        )?;
        let Some(projected) = projected else {
            report.skipped_open_drives = checked_increment(report.skipped_open_drives)?;
            continue;
        };
        logical_fingerprint.record(DirectProjectionFingerprintFact::Drive, &projected)?;
        accumulator.prepare(sink, |_| Ok((1, serialized_bytes(&projected)?)))?;
        accumulator.drives.push(projected.clone());
        let projected_id = projected.id;
        insert_direct_bounded_map(
            "projected_drives",
            &mut projected_by_id,
            projected_id,
            projected,
            DIRECT_MAX_RETAINED_DRIVE_ROWS,
        )?;
        report.completed_drives = checked_increment(report.completed_drives)?;
    }
    accumulator.flush(sink)?;
    Ok(projected_by_id)
}

#[allow(clippy::too_many_arguments)]
async fn write_positions(
    client: &Client,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    read_limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    car: &crate::hub_pack::ProjectionCar,
    drives: &HashMap<i64, ProjectionDrive>,
    limits: TeslaMateFragmentLimits,
    sink: &mut PackSink<'_>,
    logical_fingerprint: &mut DirectProjectionFingerprint,
    report: &mut ProjectionReport,
) -> Result<(), TeslaMateDirectError> {
    let mut accumulator = FragmentAccumulator::new(car.clone(), limits)?;
    let mut source_position_rows = 0usize;
    tracing::info!(
        selected_car_id,
        "starting TeslaMate position history capture"
    );
    let stream = client
        .copy_out(&binary_copy_sql(
            SourceTable::Positions,
            selected_car_id_i16,
        ))
        .await?;
    tracing::info!(selected_car_id, "TeslaMate position history stream opened");
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        position_copy_types()
    ));
    while let Some(row) = rows.as_mut().try_next().await? {
        source_position_rows = source_position_rows.checked_add(1).ok_or(
            TeslaMateDirectError::MaximumRowsExceeded {
                maximum: read_limits.maximum_rows,
            },
        )?;
        *retained_rows =
            retained_rows
                .checked_add(1)
                .ok_or(TeslaMateDirectError::MaximumRowsExceeded {
                    maximum: read_limits.maximum_rows,
                })?;
        if *retained_rows > read_limits.maximum_rows {
            return Err(TeslaMateDirectError::MaximumRowsExceeded {
                maximum: read_limits.maximum_rows,
            });
        }
        if source_position_rows.is_multiple_of(250_000) {
            tracing::info!(
                selected_car_id,
                source_position_rows,
                projected_positions = report.projected_positions,
                "staging TeslaMate position history"
            );
        }
        let position = decode_binary_position(&row)?;
        let Some(drive_id) = position.drive_id else {
            let projected = crate::lifecycle::imported_position(&position);
            logical_fingerprint.record(DirectProjectionFingerprintFact::Position, &projected)?;
            accumulator.prepare(sink, |_| Ok((1, serialized_bytes(&projected)?)))?;
            accumulator.positions.push(projected);
            report.projected_positions = checked_increment(report.projected_positions)?;
            continue;
        };
        let Some(drive) = drives.get(&drive_id) else {
            report.skipped_unattached_positions =
                checked_increment(report.skipped_unattached_positions)?;
            continue;
        };
        let projected = project_position(&position, selected_car_id, true)?
            .expect("completed drive position projects");
        logical_fingerprint.record(DirectProjectionFingerprintFact::Position, &projected)?;
        accumulator.prepare(sink, |current| {
            let parent_is_new = !current.drive_ids.contains(&drive.id);
            Ok((
                1 + u64::from(parent_is_new),
                serialized_bytes(&projected)?
                    .checked_add(if parent_is_new {
                        serialized_bytes(drive)?
                    } else {
                        0
                    })
                    .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?,
            ))
        })?;
        if accumulator.drive_ids.insert(drive.id) {
            accumulator.drives.push(drive.clone());
        }
        accumulator.positions.push(projected);
        report.projected_positions = checked_increment(report.projected_positions)?;
    }
    accumulator.flush(sink)?;
    tracing::info!(
        selected_car_id,
        source_position_rows,
        projected_positions = report.projected_positions,
        "finished TeslaMate position history capture"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn write_charges(
    client: &Client,
    selected_car_id: i64,
    selected_car_id_i16: i16,
    read_limits: TeslaMateReadLimits,
    retained_rows: &mut usize,
    processes: Vec<TeslaMateChargingProcess>,
    addresses: &HashMap<i64, TeslaMateAddress>,
    geofences: &HashMap<i64, TeslaMateGeofence>,
    related_positions: &mut HashMap<i64, TeslaMatePosition>,
    car: &crate::hub_pack::ProjectionCar,
    limits: TeslaMateFragmentLimits,
    sink: &mut PackSink<'_>,
    logical_fingerprint: &mut DirectProjectionFingerprint,
    report: &mut ProjectionReport,
    maximum_retained_processes: u64,
) -> Result<(), TeslaMateDirectError> {
    ensure_direct_retained_table_rows(
        "charging_processes",
        processes.len(),
        maximum_retained_processes,
    )?;
    let mut process_ids = HashSet::with_capacity(processes.len());
    for process in &processes {
        if !process_ids.insert(process.id) {
            return Err(TeslaMateDirectError::DuplicateDirectRetainedId {
                table: "charging_processes",
                id: process.id,
            });
        }
    }
    let mut facts = HashMap::<i64, ChargeProjectionFacts>::with_capacity(processes.len());
    let mut sample_counts = HashMap::<i64, u64>::with_capacity(processes.len());
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Charges, selected_car_id_i16))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        charge_copy_types()
    ));
    while let Some(row) = rows.as_mut().try_next().await? {
        *retained_rows =
            retained_rows
                .checked_add(1)
                .ok_or(TeslaMateDirectError::MaximumRowsExceeded {
                    maximum: read_limits.maximum_rows,
                })?;
        if *retained_rows > read_limits.maximum_rows {
            return Err(TeslaMateDirectError::MaximumRowsExceeded {
                maximum: read_limits.maximum_rows,
            });
        }
        let sample = decode_binary_charge(&row)?;
        if !process_ids.contains(&sample.charging_process_id) {
            return Err(TeslaMateDirectError::MissingChargingProcess {
                process_id: sample.charging_process_id,
            });
        }
        if !facts.contains_key(&sample.charging_process_id) {
            ensure_direct_retained_table_rows(
                "charge_facts",
                facts
                    .len()
                    .checked_add(1)
                    .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
                maximum_retained_processes,
            )?;
        }
        facts
            .entry(sample.charging_process_id)
            .or_default()
            .observe(&sample);
        if !sample_counts.contains_key(&sample.charging_process_id) {
            ensure_direct_retained_table_rows(
                "charge_sample_counts",
                sample_counts
                    .len()
                    .checked_add(1)
                    .ok_or(TeslaMateDirectError::DirectRetentionAccountingOverflow)?,
                maximum_retained_processes,
            )?;
        }
        let count = sample_counts.entry(sample.charging_process_id).or_default();
        *count = checked_increment(*count)?;
    }

    let mut projected_by_id = HashMap::<i64, ProjectionCharge>::with_capacity(process_ids.len());
    let mut empty = FragmentAccumulator::new(car.clone(), limits)?;
    let empty_charge_facts = ChargeProjectionFacts::default();
    for process in processes {
        let position = related_position(process.position_id, related_positions)?;
        let projected = project_charge(
            &process,
            selected_car_id,
            position.as_ref(),
            related(addresses, process.address_id),
            related(geofences, process.geofence_id),
            facts.get(&process.id).unwrap_or(&empty_charge_facts),
        )?;
        logical_fingerprint.record(DirectProjectionFingerprintFact::Charge, &projected)?;
        report.projected_charges = checked_increment(report.projected_charges)?;
        if sample_counts.get(&process.id).copied().unwrap_or(0) == 0 {
            empty.prepare(sink, |_| Ok((1, serialized_bytes(&projected)?)))?;
            empty.charges.push(projected.clone());
        }
        let process_id = process.id;
        insert_direct_bounded_map(
            "projected_charges",
            &mut projected_by_id,
            process_id,
            projected,
            maximum_retained_processes,
        )?;
    }
    // The second source pass needs only projected parents. Drop every
    // first-pass aggregate and ID set before it starts so their memory cannot
    // overlap the streamed sample fragments.
    drop(facts);
    drop(sample_counts);
    drop(process_ids);
    empty.flush(sink)?;

    let mut samples = FragmentAccumulator::new(car.clone(), limits)?;
    let mut second_pass_rows = 0_usize;
    let stream = client
        .copy_out(&binary_copy_sql(SourceTable::Charges, selected_car_id_i16))
        .await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        charge_copy_types()
    ));
    while let Some(row) = rows.as_mut().try_next().await? {
        second_pass_rows =
            second_pass_rows
                .checked_add(1)
                .ok_or(TeslaMateDirectError::MaximumRowsExceeded {
                    maximum: read_limits.maximum_rows,
                })?;
        if second_pass_rows > read_limits.maximum_rows {
            return Err(TeslaMateDirectError::MaximumRowsExceeded {
                maximum: read_limits.maximum_rows,
            });
        }
        let sample = decode_binary_charge(&row)?;
        let parent = projected_by_id.get(&sample.charging_process_id).ok_or(
            TeslaMateDirectError::MissingChargingProcess {
                process_id: sample.charging_process_id,
            },
        )?;
        let projected = project_charge_sample(&sample);
        logical_fingerprint.record(DirectProjectionFingerprintFact::ChargeSample, &projected)?;
        samples.prepare(sink, |current| {
            let parent_is_new = !current.charge_ids.contains(&parent.id);
            Ok((
                1 + u64::from(parent_is_new),
                serialized_bytes(&projected)?
                    .checked_add(if parent_is_new {
                        serialized_bytes(parent)?
                    } else {
                        0
                    })
                    .ok_or(TeslaMateFragmentError::FragmentSizeOverflow)?,
            ))
        })?;
        if samples.charge_ids.insert(parent.id) {
            samples.charges.push(parent.clone());
        }
        samples.charge_samples.push(projected);
        report.projected_charge_samples = checked_increment(report.projected_charge_samples)?;
    }
    samples.flush(sink)?;
    Ok(())
}

const RELATED_POSITION_BATCH_SIZE: usize = 256;
/// Relation rows carry complete position records. This cap bounds the cache to
/// a modest historical set while the normal position stream handles the full
/// telemetry corpus without retaining it in memory.
async fn prefetch_related_positions(
    client: &Client,
    selected_car_id: i16,
    drives: &[TeslaMateDrive],
    processes: &[TeslaMateChargingProcess],
    cache: &mut HashMap<i64, TeslaMatePosition>,
    maximum: usize,
) -> Result<(), TeslaMateDirectError> {
    if cache.len() > maximum {
        return Err(TeslaMateDirectError::RelatedPositionCacheLimitExceeded { maximum });
    }
    let mut ids = BTreeSet::new();
    for id in drives
        .iter()
        .flat_map(|drive| [drive.start_position_id, drive.end_position_id])
        .chain(processes.iter().map(|process| process.position_id))
        .flatten()
    {
        let id_i32 = checked_related_position_id(id)?;
        if !cache.contains_key(&id) {
            ids.insert(id_i32);
            let requested = cache
                .len()
                .checked_add(ids.len())
                .ok_or(TeslaMateDirectError::RelatedPositionCacheLimitExceeded { maximum })?;
            if requested > maximum {
                return Err(TeslaMateDirectError::RelatedPositionCacheLimitExceeded { maximum });
            }
        }
    }

    // Do not materialise the entire BTreeSet a second time as a `Vec` merely
    // to form SQL batches. The set and cache are both bounded by admission;
    // this scratch vector never grows beyond the reviewed batch size.
    let mut batch = Vec::with_capacity(RELATED_POSITION_BATCH_SIZE);
    for id in ids {
        batch.push(id);
        if batch.len() == RELATED_POSITION_BATCH_SIZE {
            fetch_related_position_batch(client, selected_car_id, &batch, cache, maximum).await?;
            batch.clear();
        }
    }
    if !batch.is_empty() {
        fetch_related_position_batch(client, selected_car_id, &batch, cache, maximum).await?;
    }
    Ok(())
}

async fn fetch_related_position_batch(
    client: &Client,
    selected_car_id: i16,
    ids: &[i32],
    cache: &mut HashMap<i64, TeslaMatePosition>,
    maximum: usize,
) -> Result<(), TeslaMateDirectError> {
    let sql = related_positions_binary_copy_sql(selected_car_id, ids);
    let stream = client.copy_out(&sql).await?;
    let mut rows = pin!(tokio_postgres::binary_copy::BinaryCopyOutStream::new(
        stream,
        position_copy_types()
    ));
    while let Some(row) = rows.as_mut().try_next().await? {
        let position = decode_binary_position(&row)?;
        if cache.contains_key(&position.id) {
            continue;
        }
        let requested = cache
            .len()
            .checked_add(1)
            .ok_or(TeslaMateDirectError::RelatedPositionCacheLimitExceeded { maximum })?;
        if requested > maximum {
            return Err(TeslaMateDirectError::RelatedPositionCacheLimitExceeded { maximum });
        }
        cache.insert(position.id, position);
    }
    Ok(())
}

fn checked_related_position_id(id: i64) -> Result<i32, TeslaMateDirectError> {
    let id = i32::try_from(id).map_err(|_| TeslaMateDirectError::InvalidRelatedPosition(id))?;
    if id == i32::MIN {
        return Err(TeslaMateDirectError::InvalidRelatedPosition(i64::from(id)));
    }
    Ok(id)
}

fn related_position(
    id: Option<i64>,
    cache: &HashMap<i64, TeslaMatePosition>,
) -> Result<Option<TeslaMatePosition>, TeslaMateDirectError> {
    let Some(id) = id else {
        return Ok(None);
    };
    checked_related_position_id(id)?;
    cache
        .get(&id)
        .cloned()
        .map(Some)
        .ok_or(TeslaMateDirectError::MissingRelatedPosition(id))
}

fn related<T>(rows: &HashMap<i64, T>, id: Option<i64>) -> Option<&T> {
    id.and_then(|id| rows.get(&id))
}

fn checked_increment(value: u64) -> Result<u64, TeslaMateDirectError> {
    value
        .checked_add(1)
        .ok_or(TeslaMateFragmentError::ReportOverflow.into())
}

#[derive(Debug, Error)]
pub enum TeslaMateDirectError {
    #[error("TeslaMate selected car disappeared during direct import")]
    SelectedCarMissing,
    #[error("TeslaMate direct import page did not progress for {0}")]
    NonProgressingPage(&'static str),
    #[error("TeslaMate direct import exceeded the {maximum} source-row ceiling")]
    MaximumRowsExceeded { maximum: usize },
    #[error(
        "TeslaMate direct import would retain {requested} {table} rows, above its {maximum}-row direct-memory ceiling"
    )]
    DirectRetainedTableLimitExceeded {
        table: &'static str,
        requested: u64,
        maximum: u64,
    },
    #[error(
        "TeslaMate direct import would retain {requested} bounded-memory row units, above its {maximum}-unit ceiling"
    )]
    DirectRetainedAggregateLimitExceeded { requested: u64, maximum: u64 },
    #[error("TeslaMate direct-memory retention accounting overflowed")]
    DirectRetentionAccountingOverflow,
    #[error("TeslaMate direct retained {table} relation has duplicate source id {id}")]
    DuplicateDirectRetainedId { table: &'static str, id: i64 },
    #[error("TeslaMate related position id {0} is invalid")]
    InvalidRelatedPosition(i64),
    #[error("TeslaMate related position id {0} is missing")]
    MissingRelatedPosition(i64),
    #[error("TeslaMate related-position cache exceeds its {maximum} unique-position limit")]
    RelatedPositionCacheLimitExceeded { maximum: usize },
    #[error("TeslaMate charge samples reference missing process {process_id}")]
    MissingChargingProcess { process_id: i64 },
    #[error("TeslaMate direct source count {column} is invalid: {count}")]
    InvalidSourceCount { column: &'static str, count: i64 },
    #[error("TeslaMate direct source {table} count overflowed while accounting for rows")]
    CountOverflow { table: &'static str },
    #[error(
        "TeslaMate direct source {table} has {source_rows} rows but only {accounted} are accounted for"
    )]
    UnexplainedSourceRows {
        table: &'static str,
        source_rows: u64,
        accounted: u64,
    },
    #[error("cannot serialize a direct logical-fingerprint fact: {0}")]
    LogicalFingerprintSerialization(#[source] serde_json::Error),
    #[error("a direct logical-fingerprint fact is too large to length-delimit")]
    LogicalFingerprintFactTooLarge,
    #[error("legacy direct bridge did not retain its physical snapshot fingerprint")]
    LegacyPhysicalFingerprintMissing,
    #[error(transparent)]
    SourceEvidence(#[from] TeslaMateSourceEvidenceError),
    #[error("direct update projection changed between scans of one exported source snapshot")]
    UpdateProjectionReconciliation,
    #[error("TeslaMate source database size is invalid: {bytes}")]
    InvalidSourceDatabaseSize { bytes: i64 },
    #[error("could not inspect target free space at {path}: {source}")]
    TargetFilesystemSpace {
        path: PathBuf,
        source: rustix::io::Errno,
    },
    #[error("target free-space calculation overflowed")]
    TargetCapacityOverflow,
    #[error("combined immutable-pack and projection-state capacity overflowed")]
    CombinedCapacityOverflow,
    #[error("TeslaMate update {update_id} belongs to car {found_car_id}, not {expected_car_id}")]
    UpdateWrongCar {
        update_id: i64,
        expected_car_id: i64,
        found_car_id: i64,
    },
    #[error(transparent)]
    Reader(#[from] TeslaMateReaderError),
    #[error(transparent)]
    Postgres(#[from] tokio_postgres::Error),
    #[error(transparent)]
    Projection(#[from] TeslaMateProjectionError),
    #[error(transparent)]
    Pack(#[from] crate::hub_pack::ProjectionPackError),
    #[error(transparent)]
    Fragment(#[from] TeslaMateFragmentError),
    #[error(transparent)]
    ProjectionState(#[from] TeslaMateProjectionStateError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Test-only diagnostics for the opt-in native 10M import exercise.
///
/// The direct import and fragment writer compile without this module outside
/// tests. When enabled, its markers are deliberately coarse: the source
/// marker includes nested pack and state work, and the final report subtracts
/// those nested measurements to show the estimated exclusive source time.
#[cfg(test)]
pub(crate) mod native_ten_million_phase_trace {
    use std::{
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicU8, AtomicU64, Ordering},
        },
        time::Instant,
    };

    const NO_PHASE: u8 = 0;

    #[derive(Clone, Copy)]
    #[repr(u8)]
    pub(crate) enum NativeTenMillionPhase {
        SourceProjection = 1,
        PackBuild = 2,
        ProjectionStateCapture = 3,
    }

    impl NativeTenMillionPhase {
        const fn name(self) -> &'static str {
            match self {
                Self::SourceProjection => "source_projection",
                Self::PackBuild => "pack_build",
                Self::ProjectionStateCapture => "projection_state_capture",
            }
        }

        const fn from_raw(raw: u8) -> Option<Self> {
            match raw {
                1 => Some(Self::SourceProjection),
                2 => Some(Self::PackBuild),
                3 => Some(Self::ProjectionStateCapture),
                _ => None,
            }
        }
    }

    struct PhaseTraceState {
        current_phase: AtomicU8,
        last_entered_phase: AtomicU8,
        interrupted_phase: AtomicU8,
        source_projection_millis: AtomicU64,
        pack_build_millis: AtomicU64,
        projection_state_capture_millis: AtomicU64,
        source_projection_windows: AtomicU64,
        pack_builds: AtomicU64,
        projection_state_captures: AtomicU64,
    }

    impl PhaseTraceState {
        fn record(&self, phase: NativeTenMillionPhase, elapsed_millis: u64) {
            match phase {
                NativeTenMillionPhase::SourceProjection => {
                    self.source_projection_millis
                        .fetch_add(elapsed_millis, Ordering::Relaxed);
                    self.source_projection_windows
                        .fetch_add(1, Ordering::Relaxed);
                }
                NativeTenMillionPhase::PackBuild => {
                    self.pack_build_millis
                        .fetch_add(elapsed_millis, Ordering::Relaxed);
                    self.pack_builds.fetch_add(1, Ordering::Relaxed);
                }
                NativeTenMillionPhase::ProjectionStateCapture => {
                    self.projection_state_capture_millis
                        .fetch_add(elapsed_millis, Ordering::Relaxed);
                    self.projection_state_captures
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        fn snapshot(&self) -> PhaseTraceSnapshot {
            let source_projection_inclusive_millis =
                self.source_projection_millis.load(Ordering::Relaxed);
            let pack_build_millis = self.pack_build_millis.load(Ordering::Relaxed);
            let projection_state_capture_millis =
                self.projection_state_capture_millis.load(Ordering::Relaxed);
            let source_projection_exclusive_millis = source_projection_inclusive_millis
                .saturating_sub(pack_build_millis.saturating_add(projection_state_capture_millis));
            let dominant_phase = [
                ("source_projection", source_projection_exclusive_millis),
                ("pack_build", pack_build_millis),
                ("projection_state_capture", projection_state_capture_millis),
            ]
            .into_iter()
            .max_by_key(|(_, elapsed_millis)| *elapsed_millis)
            .filter(|(_, elapsed_millis)| *elapsed_millis > 0)
            .map_or("none", |(phase, _)| phase);
            PhaseTraceSnapshot {
                interrupted_phase: phase_name(self.interrupted_phase.load(Ordering::Relaxed)),
                last_entered_phase: phase_name(self.last_entered_phase.load(Ordering::Relaxed)),
                dominant_phase,
                source_projection_exclusive_millis,
                pack_build_millis,
                projection_state_capture_millis,
                source_projection_windows: self.source_projection_windows.load(Ordering::Relaxed),
                pack_builds: self.pack_builds.load(Ordering::Relaxed),
                projection_state_captures: self.projection_state_captures.load(Ordering::Relaxed),
            }
        }
    }

    fn phase_name(raw: u8) -> &'static str {
        NativeTenMillionPhase::from_raw(raw).map_or("none", NativeTenMillionPhase::name)
    }

    struct PhaseTraceSnapshot {
        interrupted_phase: &'static str,
        last_entered_phase: &'static str,
        dominant_phase: &'static str,
        source_projection_exclusive_millis: u64,
        pack_build_millis: u64,
        projection_state_capture_millis: u64,
        source_projection_windows: u64,
        pack_builds: u64,
        projection_state_captures: u64,
    }

    fn active_trace() -> &'static Mutex<Option<Arc<PhaseTraceState>>> {
        static ACTIVE_TRACE: OnceLock<Mutex<Option<Arc<PhaseTraceState>>>> = OnceLock::new();
        ACTIVE_TRACE.get_or_init(|| Mutex::new(None))
    }

    /// Install the trace only for an explicit 10M diagnostic run. Nothing is
    /// emitted unless `TESLATLAS_HUB_TRACE_10M_PHASES=1` is set.
    pub(crate) fn enabled_from_environment() -> Option<NativeTenMillionPhaseTrace> {
        if std::env::var("TESLATLAS_HUB_TRACE_10M_PHASES").as_deref() != Ok("1") {
            return None;
        }
        let state = Arc::new(PhaseTraceState {
            current_phase: AtomicU8::new(NO_PHASE),
            last_entered_phase: AtomicU8::new(NO_PHASE),
            interrupted_phase: AtomicU8::new(NO_PHASE),
            source_projection_millis: AtomicU64::new(0),
            pack_build_millis: AtomicU64::new(0),
            projection_state_capture_millis: AtomicU64::new(0),
            source_projection_windows: AtomicU64::new(0),
            pack_builds: AtomicU64::new(0),
            projection_state_captures: AtomicU64::new(0),
        });
        let mut active = active_trace().lock().expect("native 10M phase trace mutex");
        assert!(
            active.is_none(),
            "only one native 10M phase trace may be active"
        );
        *active = Some(Arc::clone(&state));
        Some(NativeTenMillionPhaseTrace {
            state,
            emitted: false,
        })
    }

    /// A scoped marker records completed time normally and records the first
    /// unfinished inner phase if timeout cancellation drops the import future.
    pub(crate) struct PhaseMarker {
        state: Option<Arc<PhaseTraceState>>,
        phase: NativeTenMillionPhase,
        previous_phase: u8,
        started: Option<Instant>,
        completed: bool,
    }

    pub(crate) fn mark(phase: NativeTenMillionPhase) -> PhaseMarker {
        let state = active_trace()
            .lock()
            .expect("native 10M phase trace mutex")
            .as_ref()
            .map(Arc::clone);
        let Some(state) = state else {
            return PhaseMarker {
                state: None,
                phase,
                previous_phase: NO_PHASE,
                started: None,
                completed: true,
            };
        };
        let previous_phase = state.current_phase.swap(phase as u8, Ordering::Relaxed);
        state
            .last_entered_phase
            .store(phase as u8, Ordering::Relaxed);
        PhaseMarker {
            state: Some(state),
            phase,
            previous_phase,
            started: Some(Instant::now()),
            completed: false,
        }
    }

    impl PhaseMarker {
        /// Mark a returned success or error as complete. If the future is
        /// cancelled before this point, Drop retains this phase for reporting.
        pub(crate) fn complete(&mut self) {
            if self.completed {
                return;
            }
            self.record_elapsed();
            self.completed = true;
        }

        fn record_elapsed(&mut self) {
            let (Some(state), Some(started)) = (&self.state, self.started.take()) else {
                return;
            };
            let elapsed_millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            state.record(self.phase, elapsed_millis);
        }
    }

    impl Drop for PhaseMarker {
        fn drop(&mut self) {
            let Some(state) = self.state.as_ref().map(Arc::clone) else {
                return;
            };
            if !self.completed {
                self.record_elapsed();
                let _ = state.interrupted_phase.compare_exchange(
                    NO_PHASE,
                    self.phase as u8,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
            }
            let _ = state.current_phase.compare_exchange(
                self.phase as u8,
                self.previous_phase,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }

    /// Owns the active test-only trace and emits a redacted summary when the
    /// timeout returns or unwinding cancels the import future.
    pub(crate) struct NativeTenMillionPhaseTrace {
        state: Arc<PhaseTraceState>,
        emitted: bool,
    }

    impl NativeTenMillionPhaseTrace {
        pub(crate) fn report(&mut self, reason: &'static str) {
            if self.emitted {
                return;
            }
            let snapshot = self.state.snapshot();
            eprintln!(
                "teslatlas-hub 10m phase trace: reason={reason} interrupted_phase={} last_entered_phase={} dominant_phase={} source_projection_exclusive_ms={} pack_build_ms={} projection_state_capture_ms={} source_projection_windows={} pack_builds={} projection_state_captures={}",
                snapshot.interrupted_phase,
                snapshot.last_entered_phase,
                snapshot.dominant_phase,
                snapshot.source_projection_exclusive_millis,
                snapshot.pack_build_millis,
                snapshot.projection_state_capture_millis,
                snapshot.source_projection_windows,
                snapshot.pack_builds,
                snapshot.projection_state_captures,
            );
            self.emitted = true;
        }
    }

    impl Drop for NativeTenMillionPhaseTrace {
        fn drop(&mut self) {
            if !self.emitted {
                self.report("cancelled_or_panicked");
            }
            let mut active = active_trace().lock().expect("native 10M phase trace mutex");
            if active
                .as_ref()
                .is_some_and(|state| Arc::ptr_eq(state, &self.state))
            {
                *active = None;
            }
        }
    }

    #[test]
    fn phase_marker_records_nested_work_and_the_innermost_interruption() {
        let state = Arc::new(PhaseTraceState {
            current_phase: AtomicU8::new(NO_PHASE),
            last_entered_phase: AtomicU8::new(NO_PHASE),
            interrupted_phase: AtomicU8::new(NO_PHASE),
            source_projection_millis: AtomicU64::new(0),
            pack_build_millis: AtomicU64::new(0),
            projection_state_capture_millis: AtomicU64::new(0),
            source_projection_windows: AtomicU64::new(0),
            pack_builds: AtomicU64::new(0),
            projection_state_captures: AtomicU64::new(0),
        });
        {
            state.current_phase.store(
                NativeTenMillionPhase::SourceProjection as u8,
                Ordering::Relaxed,
            );
            let _source = PhaseMarker {
                state: Some(Arc::clone(&state)),
                phase: NativeTenMillionPhase::SourceProjection,
                previous_phase: NO_PHASE,
                started: Some(Instant::now()),
                completed: false,
            };
            state.current_phase.store(
                NativeTenMillionPhase::ProjectionStateCapture as u8,
                Ordering::Relaxed,
            );
            let _capture = PhaseMarker {
                state: Some(Arc::clone(&state)),
                phase: NativeTenMillionPhase::ProjectionStateCapture,
                previous_phase: NativeTenMillionPhase::SourceProjection as u8,
                started: Some(Instant::now()),
                completed: false,
            };
        }
        let snapshot = state.snapshot();
        assert_eq!(snapshot.interrupted_phase, "projection_state_capture");
        assert_eq!(snapshot.source_projection_windows, 1);
        assert_eq!(snapshot.projection_state_captures, 1);
        assert_eq!(snapshot.pack_builds, 0);
        assert_eq!(state.current_phase.load(Ordering::Relaxed), NO_PHASE);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };

    use serde::Deserialize;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::{
        credentials::TeslaMatePostgresPassword,
        teslamate::ReadOnlySource,
        teslamate_projection_state::{TeslaMateProjectionState, TeslaMateProjectionStateLimits},
    };

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NativeFixtureCounts {
        cars: u64,
        drives: u64,
        positions: u64,
        charging_processes: u64,
        charges: u64,
        schema_migrations: u64,
        private_tokens: u64,
    }

    fn configured_native_postgres_source(
        allow_local_fixture: bool,
    ) -> Option<(ReadOnlySource, TeslaMatePostgresPassword)> {
        let url = std::env::var("TESLATLAS_HUB_TEST_POSTGRES_URL")
            .ok()
            .or_else(|| {
                allow_local_fixture
                    .then(|| std::env::var("TESLATLAS_LOCAL_TESLAMATE_URL").ok())
                    .flatten()
            })?;
        let source = ReadOnlySource::parse(&url).expect("credential-free source URL");
        Some((
            source,
            configured_native_postgres_password(allow_local_fixture),
        ))
    }

    /// Read an explicitly supplied password file using the production parser.
    fn configured_native_postgres_password(allow_local_fixture: bool) -> TeslaMatePostgresPassword {
        if let Some(credentials_directory) =
            std::env::var_os("TESLATLAS_HUB_TEST_POSTGRES_CREDENTIALS_DIRECTORY")
        {
            let path = PathBuf::from(credentials_directory).join("teslamate-postgres-password");
            return TeslaMatePostgresPassword::from_bytes(
                &normalize_private_postgres_password_file_bytes(
                    fs::read(path).expect("test PostgreSQL credential"),
                )
                .expect("test PostgreSQL credential line"),
            )
            .expect("test PostgreSQL credential");
        }

        let password_file = std::env::var_os("TESLATLAS_HUB_TEST_POSTGRES_PASSWORD_FILE")
            .or_else(|| {
                allow_local_fixture
                    .then(|| std::env::var_os("TESLATLAS_LOCAL_TESLAMATE_POSTGRES_PASSWORD_FILE"))
                    .flatten()
            })
            .map(PathBuf::from)
            .expect("a private PostgreSQL password-file environment value");
        let metadata = fs::symlink_metadata(&password_file)
            .expect("configured PostgreSQL password file metadata");
        assert!(
            metadata.file_type().is_file(),
            "configured PostgreSQL password must be a regular file"
        );
        assert_eq!(
            metadata.permissions().mode() & 0o077,
            0,
            "configured PostgreSQL password must not be group- or world-readable"
        );

        let password = normalize_private_postgres_password_file_bytes(
            fs::read(&password_file).expect("read private PostgreSQL password"),
        )
        .expect("configured PostgreSQL password must contain at most one terminal LF or CRLF");
        TeslaMatePostgresPassword::from_bytes(&password)
            .expect("test PostgreSQL password credential")
    }

    fn normalize_private_postgres_password_file_bytes(
        mut bytes: Vec<u8>,
    ) -> Result<Vec<u8>, &'static str> {
        if bytes.ends_with(b"\r\n") {
            bytes.truncate(bytes.len() - 2);
        } else if bytes.ends_with(b"\n") {
            bytes.pop();
        }
        if bytes.is_empty()
            || bytes
                .iter()
                .any(|byte| matches!(byte, b'\0' | b'\r' | b'\n'))
        {
            return Err("invalid PostgreSQL password line");
        }
        Ok(bytes)
    }

    #[test]
    fn private_postgres_password_adapter_accepts_one_terminal_line_ending_only() {
        assert_eq!(
            normalize_private_postgres_password_file_bytes(b"private-password\n".to_vec())
                .expect("LF-terminated private password"),
            b"private-password"
        );
        assert_eq!(
            normalize_private_postgres_password_file_bytes(b"private-password\r\n".to_vec())
                .expect("CRLF-terminated private password"),
            b"private-password"
        );
        for invalid in [
            b"private\npassword".as_slice(),
            b"private\rpassword".as_slice(),
            b"private\n\n".as_slice(),
            b"\n".as_slice(),
            b"private\0password".as_slice(),
        ] {
            assert!(
                normalize_private_postgres_password_file_bytes(invalid.to_vec()).is_err(),
                "only one terminal LF or CRLF is accepted"
            );
        }
    }

    fn validated_native_fixture_source_counts(
        path: &Path,
        expected_sha256: Option<String>,
    ) -> NativeFixtureCounts {
        let metadata = fs::symlink_metadata(path).expect("fixture counts metadata");
        assert!(
            metadata.file_type().is_file(),
            "fixture counts metadata must be a regular file"
        );
        let bytes = fs::read(path).expect("read fixture counts metadata");
        if let Some(expected_sha256) = expected_sha256 {
            assert_eq!(expected_sha256.len(), 64, "fixture counts digest length");
            assert!(
                expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "fixture counts digest must be hexadecimal"
            );
            assert_eq!(
                hex::encode(Sha256::digest(&bytes)),
                expected_sha256.to_ascii_lowercase(),
                "fixture counts digest"
            );
        }
        let counts: NativeFixtureCounts =
            serde_json::from_slice(&bytes).expect("valid fixture counts metadata");
        assert!(counts.cars >= 1, "fixture counts must include a car");
        assert!(
            counts.positions > 0,
            "fixture counts must include positions"
        );
        assert_eq!(
            counts.private_tokens, 0,
            "fixture counts must remain token-redacted"
        );
        let audited_rows = counts
            .cars
            .checked_add(counts.drives)
            .and_then(|value| value.checked_add(counts.positions))
            .and_then(|value| value.checked_add(counts.charging_processes))
            .and_then(|value| value.checked_add(counts.charges))
            .and_then(|value| value.checked_add(counts.schema_migrations))
            .expect("fixture counts total must fit u64");
        assert!(audited_rows >= counts.positions);
        counts
    }

    async fn native_ten_million_expected_source_counts(
        source: &ReadOnlySource,
        password: &TeslaMatePostgresPassword,
        limits: TeslaMateReadLimits,
        packs_dir: &Path,
    ) -> TeslaMateSourceCounts {
        let expected_positions = std::env::var("TESLATLAS_HUB_TEST_EXPECTED_POSITIONS")
            .ok()
            .map(|value| {
                let positions = value
                    .parse::<u64>()
                    .expect("TESLATLAS_HUB_TEST_EXPECTED_POSITIONS must be an unsigned integer");
                assert!(positions > 0, "expected source positions must be positive");
                positions
            });

        let counts = std::env::var_os("TESLATLAS_HUB_TEST_COUNTS_FILE")
            .map(|path| {
                (
                    PathBuf::from(path),
                    std::env::var("TESLATLAS_HUB_TEST_COUNTS_SHA256").ok(),
                )
            })
            .or_else(|| {
                std::env::var_os("TESLATLAS_LOCAL_TESLAMATE_COUNTS_FILE").map(|path| {
                    (
                        PathBuf::from(path),
                        std::env::var("TESLATLAS_LOCAL_TESLAMATE_COUNTS_SHA256").ok(),
                    )
                })
            });
        let fixture_counts = counts.map(|(counts_file, expected_sha256)| {
            validated_native_fixture_source_counts(&counts_file, expected_sha256)
        });
        if let Some(fixture_counts) = fixture_counts.as_ref()
            && let Some(expected_positions) = expected_positions
        {
            assert_eq!(
                fixture_counts.positions, expected_positions,
                "position environment override must match validated fixture metadata"
            );
        }

        let source_counts = preflight_teslamate_import(source, password, 1, limits, packs_dir)
            .await
            .expect("source-count preflight")
            .source_row_counts;
        if let Some(expected_positions) = expected_positions {
            assert_eq!(
                source_counts.positions, expected_positions,
                "position environment override must match source preflight"
            );
        }
        if let Some(fixture_counts) = fixture_counts {
            assert_eq!(
                source_counts.cars, fixture_counts.cars,
                "source preflight cars must match validated fixture metadata"
            );
            assert_eq!(
                source_counts.drives, fixture_counts.drives,
                "source preflight drives must match validated fixture metadata"
            );
            assert_eq!(
                source_counts.positions, fixture_counts.positions,
                "source preflight positions must match validated fixture metadata"
            );
            assert_eq!(
                source_counts.charging_processes, fixture_counts.charging_processes,
                "source preflight charging processes must match validated fixture metadata"
            );
            assert_eq!(
                source_counts.charges, fixture_counts.charges,
                "source preflight charges must match validated fixture metadata"
            );
        }
        source_counts
    }

    #[test]
    fn preflight_report_is_bounded_and_redacted() {
        let report = TeslaMatePreflightReport {
            selected_car_id: 17,
            source_database_bytes: 123,
            schema: TeslaMateSchemaInfo {
                observed_migration_version: 20260411070212,
                observed_migration_count: 100,
                minimum_supported_migration_version: 20260411070212,
                maximum_validated_migration_version: 20260411070212,
                pinned_source_revision: "7054517c10475f39f480edeae8f90c6f717985a3",
                pinned_migration_set_sha256: "f03b25b2c12e4a558a481a45a9b3df65518f7733008bc47931eea7e7c78efefb",
                fingerprint: "abc".to_owned(),
            },
            source_row_counts: TeslaMateSourceCounts {
                cars: 1,
                drives: 2,
                positions: 3,
                charging_processes: 4,
                charges: 5,
                states: 6,
                addresses: 7,
                geofences: 8,
                updates: 9,
            },
            target_available_bytes: 456,
            configured_maximum_rows: 20_000_000,
            configured_staging_limit_bytes: 4 * 1024 * 1024 * 1024,
            configured_staging_reserve_bytes: 512 * 1024 * 1024,
            admission: TeslaMatePreflightAdmission {
                passed: true,
                reason: None,
            },
        };
        let value = serde_json::to_value(report).expect("preflight JSON");
        assert_eq!(value["selectedCarId"], 17);
        assert_eq!(value["sourceRowCounts"]["positions"], 3);
        assert_eq!(value["sourceRowCounts"]["addresses"], 7);
        assert_eq!(value["sourceRowCounts"]["geofences"], 8);
        assert_eq!(value["sourceRowCounts"]["updates"], 9);
        assert_eq!(value["admission"]["passed"], true);
        assert!(value.get("password").is_none());
        assert!(value.get("sourceUrl").is_none());
        assert!(value.get("snapshotId").is_none());
    }

    fn direct_retention_test_counts() -> TeslaMateSourceCounts {
        TeslaMateSourceCounts {
            cars: 1,
            drives: 0,
            positions: 0,
            charging_processes: 0,
            charges: 0,
            states: 0,
            addresses: 0,
            geofences: 0,
            updates: 0,
        }
    }

    #[test]
    fn direct_retention_admission_admits_the_canonical_large_position_shape() {
        let admitted = admit_direct_retention(
            TeslaMateSourceCounts {
                cars: 1,
                drives: 3_151,
                positions: 10_402_457,
                charging_processes: 770,
                charges: 292_548,
                states: 5_164,
                addresses: 942,
                geofences: 19_038,
                updates: 39,
            },
            TeslaMateReadLimits::default(),
        )
        .expect("canonical 10M-position source remains direct-memory admissible");

        assert_eq!(admitted.related_position_cache_ids, 7_072);
        assert_eq!(admitted.retained_row_units, 93_662);
        assert!(admitted.retained_row_units < DIRECT_MAX_RETAINED_ROW_UNITS);
    }

    #[test]
    fn direct_retention_admission_rejects_each_history_sized_retained_relation() {
        let cases = [
            (
                "addresses",
                TeslaMateSourceCounts {
                    addresses: DIRECT_MAX_RETAINED_ADDRESS_ROWS + 1,
                    ..direct_retention_test_counts()
                },
            ),
            (
                "geofences",
                TeslaMateSourceCounts {
                    geofences: DIRECT_MAX_RETAINED_GEOFENCE_ROWS + 1,
                    ..direct_retention_test_counts()
                },
            ),
            (
                "states",
                TeslaMateSourceCounts {
                    states: DIRECT_MAX_RETAINED_STATE_ROWS + 1,
                    ..direct_retention_test_counts()
                },
            ),
            (
                "drives",
                TeslaMateSourceCounts {
                    drives: DIRECT_MAX_RETAINED_DRIVE_ROWS + 1,
                    ..direct_retention_test_counts()
                },
            ),
            (
                "charging_processes",
                TeslaMateSourceCounts {
                    charging_processes: DIRECT_MAX_RETAINED_CHARGING_PROCESS_ROWS + 1,
                    ..direct_retention_test_counts()
                },
            ),
            (
                "updates_schema_2_2",
                TeslaMateSourceCounts {
                    updates: DIRECT_MAX_RETAINED_SCHEMA_22_UPDATE_ROWS + 1,
                    ..direct_retention_test_counts()
                },
            ),
            (
                "related_positions",
                TeslaMateSourceCounts {
                    drives: DIRECT_MAX_RETAINED_DRIVE_ROWS,
                    charging_processes: 1,
                    ..direct_retention_test_counts()
                },
            ),
        ];

        for (table, counts) in cases {
            let error = admit_direct_retention(counts, TeslaMateReadLimits::default())
                .expect_err("retained relation must be admitted before collection allocation");
            assert!(matches!(
                error,
                TeslaMateDirectError::DirectRetainedTableLimitExceeded {
                    table: actual_table,
                    ..
                } if actual_table == table
            ));
        }
    }

    #[test]
    fn direct_retention_admission_has_an_aggregate_cap_and_bounds_schema_22_updates() {
        let aggregate = TeslaMateSourceCounts {
            addresses: DIRECT_MAX_RETAINED_ADDRESS_ROWS,
            geofences: DIRECT_MAX_RETAINED_GEOFENCE_ROWS,
            states: DIRECT_MAX_RETAINED_STATE_ROWS,
            drives: 16_384,
            charging_processes: DIRECT_MAX_RETAINED_CHARGING_PROCESS_ROWS,
            ..direct_retention_test_counts()
        };
        let error = admit_direct_retention(aggregate, TeslaMateReadLimits::default())
            .expect_err("separate retained caps must not add up to an unbounded heap");
        assert!(matches!(
            error,
            TeslaMateDirectError::DirectRetainedAggregateLimitExceeded {
                requested: 622_593,
                maximum: DIRECT_MAX_RETAINED_ROW_UNITS,
            }
        ));

        let update_heavy = TeslaMateSourceCounts {
            updates: 100_000,
            ..direct_retention_test_counts()
        };
        assert!(matches!(
            admit_direct_retention(update_heavy, TeslaMateReadLimits::default()),
            Err(TeslaMateDirectError::DirectRetainedTableLimitExceeded {
                table: "updates_schema_2_2",
                requested: 100_000,
                maximum: DIRECT_MAX_RETAINED_SCHEMA_22_UPDATE_ROWS,
            })
        ));

        let source_ceiling = TeslaMateSourceCounts {
            positions: u64::try_from(TeslaMateReadLimits::default().maximum_rows)
                .expect("configured source ceiling fits u64"),
            updates: 1,
            ..direct_retention_test_counts()
        };
        assert!(matches!(
            admit_direct_retention(source_ceiling, TeslaMateReadLimits::default()),
            Err(TeslaMateDirectError::MaximumRowsExceeded {
                maximum: 20_000_000,
            })
        ));
    }

    #[test]
    fn bounded_direct_maps_reject_growth_and_duplicate_ids() {
        let mut rows = HashMap::new();
        insert_direct_bounded_map("projected_drives", &mut rows, 1, "one", 1)
            .expect("first bounded row");
        assert!(matches!(
            insert_direct_bounded_map("projected_drives", &mut rows, 2, "two", 1),
            Err(TeslaMateDirectError::DirectRetainedTableLimitExceeded {
                table: "projected_drives",
                requested: 2,
                maximum: 1,
            })
        ));
        assert!(matches!(
            insert_direct_bounded_map("projected_drives", &mut rows, 1, "duplicate", 1),
            Err(TeslaMateDirectError::DuplicateDirectRetainedId {
                table: "projected_drives",
                id: 1,
            })
        ));
    }

    #[test]
    fn direct_count_gate_accepts_named_projection_and_skip_reasons() {
        let report = ProjectionReport {
            completed_drives: 1,
            skipped_open_drives: 2,
            skipped_unattached_positions: 3,
            projected_positions: 4,
            projected_charges: 5,
            projected_charge_samples: 6,
            projected_states: 7,
            projected_updates: 8,
            skipped_incomplete_updates: 9,
        };
        assert!(
            validate_direct_source_counts(
                TeslaMateSourceCounts {
                    cars: 1,
                    drives: 3,
                    positions: 7,
                    charging_processes: 5,
                    charges: 6,
                    states: 7,
                    addresses: 0,
                    geofences: 0,
                    updates: 17,
                },
                report,
                17,
            )
            .is_ok()
        );
    }

    #[test]
    fn direct_count_gate_rejects_unexplained_loss() {
        let error = validate_direct_source_counts(
            TeslaMateSourceCounts {
                cars: 1,
                drives: 0,
                positions: 1,
                charging_processes: 0,
                charges: 0,
                states: 0,
                addresses: 0,
                geofences: 0,
                updates: 0,
            },
            ProjectionReport::default(),
            0,
        )
        .expect_err("position must be accounted for");
        assert!(matches!(
            error,
            TeslaMateDirectError::UnexplainedSourceRows {
                table: "positions",
                source_rows: 1,
                accounted: 0,
            }
        ));
    }

    #[test]
    fn direct_metadata_and_update_reconciliation_rejects_count_drift() {
        for table in ["addresses", "geofences", "updates"] {
            assert!(matches!(
                validate_direct_count(table, 1, 0),
                Err(TeslaMateDirectError::UnexplainedSourceRows {
                    table: actual_table,
                    source_rows: 1,
                    accounted: 0,
                }) if actual_table == table
            ));
        }
    }

    #[test]
    fn direct_count_gate_rejects_dropped_update_history() {
        let error = validate_direct_source_counts(
            TeslaMateSourceCounts {
                cars: 1,
                drives: 0,
                positions: 0,
                charging_processes: 0,
                charges: 0,
                states: 0,
                addresses: 0,
                geofences: 0,
                updates: 1,
            },
            ProjectionReport::default(),
            0,
        )
        .expect_err("a direct source update cannot disappear before publication");
        assert!(matches!(
            error,
            TeslaMateDirectError::UnexplainedSourceRows {
                table: "updates",
                source_rows: 1,
                accounted: 0,
            }
        ));
    }

    #[test]
    fn direct_count_gate_rejects_missing_or_extra_state_rows() {
        let missing = validate_direct_source_counts(
            TeslaMateSourceCounts {
                cars: 1,
                drives: 0,
                positions: 0,
                charging_processes: 0,
                charges: 0,
                states: 1,
                addresses: 0,
                geofences: 0,
                updates: 0,
            },
            ProjectionReport::default(),
            0,
        )
        .expect_err("a source state cannot disappear from a direct snapshot");
        assert!(matches!(
            missing,
            TeslaMateDirectError::UnexplainedSourceRows {
                table: "states",
                source_rows: 1,
                accounted: 0,
            }
        ));

        let extra = validate_direct_source_counts(
            TeslaMateSourceCounts {
                cars: 1,
                drives: 0,
                positions: 0,
                charging_processes: 0,
                charges: 0,
                states: 0,
                addresses: 0,
                geofences: 0,
                updates: 0,
            },
            ProjectionReport {
                projected_states: 1,
                ..ProjectionReport::default()
            },
            0,
        )
        .expect_err("a direct snapshot cannot invent a state row");
        assert!(matches!(
            extra,
            TeslaMateDirectError::UnexplainedSourceRows {
                table: "states",
                source_rows: 0,
                accounted: 1,
            }
        ));
    }

    fn fingerprint_test_car() -> crate::hub_pack::ProjectionCar {
        crate::hub_pack::ProjectionCar {
            id: 1,
            name: "Fingerprint test car".to_owned(),
            model: "3".to_owned(),
            vin: Some("5YJ3E1EA7KF000001".to_owned()),
            source_eid: Some(1),
            source_vid: Some(2),
            trim_badging: None,
            marketing_name: None,
            exterior_color: None,
            wheel_type: None,
            spoiler_type: None,
            firmware_version: Some("2026.1".to_owned()),
            efficiency_wh_per_km: Some(150.0),
            settings: crate::hub_pack::ProjectionCarSettings::default(),
        }
    }

    fn fingerprint_test_charge(id: i64, text: &str) -> ProjectionCharge {
        ProjectionCharge {
            id,
            car_id: 1,
            start_date_ms: 1_700_000_000_000 + id,
            end_date_ms: Some(1_700_000_000_001 + id),
            charge_energy_added: Some(1.0),
            charge_energy_used_kwh: Some(1.0),
            start_ideal_range_km: Some(100.0),
            end_ideal_range_km: Some(101.0),
            cost: None,
            fast_charger_type: Some(text.to_owned()),
            billing_type: None,
            cost_per_unit: None,
            session_fee: None,
            start_latitude: Some(51.5),
            start_longitude: Some(-0.1),
            start_battery_level: Some(50),
            end_battery_level: Some(51),
            duration_min: Some(1),
            address: Some(text.to_owned()),
            location_name: Some(text.to_owned()),
            geofence: Some(text.to_owned()),
            is_dc: Some(false),
            charge_rate_km_per_hour: Some(1.0),
            max_charger_power_kw: Some(1.0),
            outside_temp_avg: Some(20.0),
            start_rated_range_km: Some(100.0),
            end_rated_range_km: Some(101.0),
        }
    }

    /// Mirrors the empty-charge fragment layout in `write_charges` without
    /// building packs. The returned count proves the exact limits chose a
    /// different physical layout; it is intentionally not an input to the
    /// direct logical digest.
    fn logical_fingerprint_for_empty_charge_fragments(
        limits: TeslaMateFragmentLimits,
    ) -> (Sha256Digest, u64) {
        const CHARGE_COUNT: i64 = 300;

        let car = fingerprint_test_car();
        let state = crate::hub_pack::ProjectionState {
            id: 1,
            car_id: 1,
            state: "online".to_owned(),
            start_date_ms: 1_700_000_000_000,
            end_date_ms: None,
        };
        let mut fingerprint = DirectProjectionFingerprint::new();
        fingerprint
            .record(DirectProjectionFingerprintFact::Car, &car)
            .expect("serialize test car");
        fingerprint
            .record(DirectProjectionFingerprintFact::State, &state)
            .expect("serialize test state");

        let car_bytes = serialized_bytes(&car).expect("size test car");
        let mut fragment_rows = 1_u64;
        let mut fragment_bytes = car_bytes;
        let mut fragments = 0_u64;
        let text = "x".repeat(crate::hub_pack::MAX_TEXT_BYTES);
        for id in 1..=CHARGE_COUNT {
            let charge = fingerprint_test_charge(id, &text);
            let charge_bytes = serialized_bytes(&charge).expect("size test charge");
            let would_exceed = fragment_rows.checked_add(1).expect("test row count")
                > limits.max_rows_per_fragment
                || fragment_bytes
                    .checked_add(charge_bytes)
                    .expect("test byte count")
                    > limits.max_projected_json_bytes;
            if would_exceed && fragment_rows > 1 {
                fragments = fragments.checked_add(1).expect("test fragment count");
                fragment_rows = 1;
                fragment_bytes = car_bytes;
            }
            assert!(
                fragment_rows.checked_add(1).expect("test row count")
                    <= limits.max_rows_per_fragment
                    && fragment_bytes
                        .checked_add(charge_bytes)
                        .expect("test byte count")
                        <= limits.max_projected_json_bytes,
                "one test charge fits after a fragment reset"
            );
            fragment_rows = fragment_rows.checked_add(1).expect("test row count");
            fragment_bytes = fragment_bytes
                .checked_add(charge_bytes)
                .expect("test byte count");
            fingerprint
                .record(DirectProjectionFingerprintFact::Charge, &charge)
                .expect("serialize test charge");
        }
        if fragment_rows > 1 {
            fragments = fragments.checked_add(1).expect("test fragment count");
        }

        (fingerprint.finish(), fragments)
    }

    #[test]
    fn direct_logical_fingerprint_binds_fact_type_and_order() {
        let mut state_fact = DirectProjectionFingerprint::new();
        state_fact
            .record(DirectProjectionFingerprintFact::State, &"same payload")
            .expect("record state fact");
        let mut charge_fact = DirectProjectionFingerprint::new();
        charge_fact
            .record(DirectProjectionFingerprintFact::Charge, &"same payload")
            .expect("record charge fact");
        assert_ne!(
            state_fact.finish(),
            charge_fact.finish(),
            "entity tags keep equal JSON payloads typed"
        );

        let mut source_order = DirectProjectionFingerprint::new();
        source_order
            .record(DirectProjectionFingerprintFact::Charge, &"first")
            .expect("record first fact");
        source_order
            .record(DirectProjectionFingerprintFact::Charge, &"second")
            .expect("record second fact");
        let mut reversed_order = DirectProjectionFingerprint::new();
        reversed_order
            .record(DirectProjectionFingerprintFact::Charge, &"second")
            .expect("record second fact");
        reversed_order
            .record(DirectProjectionFingerprintFact::Charge, &"first")
            .expect("record first fact");
        assert_ne!(
            source_order.finish(),
            reversed_order.finish(),
            "logical source order participates in duplicate suppression"
        );
    }

    #[test]
    fn direct_logical_fingerprint_binds_preprojection_source_evidence() {
        let first_evidence = Sha256Digest::of_bytes(b"drive endpoints first");
        let second_evidence = Sha256Digest::of_bytes(b"drive endpoints second");
        let mut first = DirectProjectionFingerprint::new();
        first.bind_source_evidence(&first_evidence);
        let mut repeated = DirectProjectionFingerprint::new();
        repeated.bind_source_evidence(&first_evidence);
        let mut changed = DirectProjectionFingerprint::new();
        changed.bind_source_evidence(&second_evidence);

        let first = first.finish();
        let repeated = repeated.finish();
        let changed = changed.finish();
        assert_eq!(first, repeated);
        assert_ne!(
            repeated, changed,
            "a source-only fact change must invalidate direct duplicate suppression"
        );
    }

    #[test]
    fn direct_logical_fingerprint_ignores_fragment_limit_retry_layout() {
        let default_limits = TeslaMateFragmentLimits::default();
        let retry_limits = next_fragment_limits(default_limits)
            .expect("the default direct target has the dense retry target");
        assert_eq!(retry_limits, DENSE_DIRECT_FRAGMENT_LIMITS);

        let (default_fingerprint, default_fragments) =
            logical_fingerprint_for_empty_charge_fragments(default_limits);
        let (retry_fingerprint, retry_fragments) =
            logical_fingerprint_for_empty_charge_fragments(retry_limits);

        assert_ne!(
            default_fragments, retry_fragments,
            "the fixture must exercise distinct 50k/8MiB and 100k/16MiB layouts"
        );
        assert_eq!(
            default_fingerprint, retry_fingerprint,
            "one projected history must keep its duplicate fingerprint across a retry"
        );
    }

    #[test]
    fn direct_fragment_target_keeps_the_default_below_the_dense_boundary() {
        let limits = initial_direct_fragment_limits(TeslaMateSourceCounts {
            cars: 1,
            drives: 1,
            positions: DENSE_DIRECT_POSITION_THRESHOLD - 1,
            charging_processes: 1,
            charges: 1,
            states: 1,
            addresses: 0,
            geofences: 0,
            updates: 0,
        });

        assert_eq!(limits, TeslaMateFragmentLimits::default());
    }

    #[test]
    fn direct_fragment_target_selects_dense_boundary_and_retries_upward() {
        let selected = initial_direct_fragment_limits(TeslaMateSourceCounts {
            cars: 1,
            drives: 3_151,
            positions: DENSE_DIRECT_POSITION_THRESHOLD,
            charging_processes: 770,
            charges: 292_548,
            states: 1,
            addresses: 0,
            geofences: 0,
            updates: 0,
        });
        let protocol = crate::protocol::ProtocolLimits::default();

        assert_eq!(selected, DENSE_DIRECT_FRAGMENT_LIMITS);
        assert!(selected.max_rows_per_fragment <= protocol.max_rows_per_pack);
        assert!(selected.max_projected_json_bytes <= protocol.max_uncompressed_pack_bytes);

        let retry = next_fragment_limits(selected).expect("dense target has a bounded retry");
        assert_eq!(
            retry,
            TeslaMateFragmentLimits {
                max_rows_per_fragment: 200_000,
                max_projected_json_bytes: 32 * 1024 * 1024,
            }
        );
        assert!(retry.max_rows_per_fragment <= protocol.max_rows_per_pack);
        assert!(retry.max_projected_json_bytes <= protocol.max_uncompressed_pack_bytes);
    }

    #[test]
    fn combined_capture_capacity_rejects_overflow_before_filesystem_access() {
        let temporary = tempfile::tempdir().expect("pack directory");
        let error = ensure_direct_capture_capacity(
            &ProjectionPackWriter::new(temporary.path()),
            1,
            u64::MAX,
            1,
        )
        .expect_err("reserve addition must not wrap");
        assert!(matches!(
            error,
            TeslaMateDirectError::CombinedCapacityOverflow
        ));
    }

    #[tokio::test]
    async fn native_complete_corpus_direct_import_projects_every_kind_when_configured() {
        let Some((source, password)) = configured_native_postgres_source(false) else {
            return;
        };
        let packs = tempfile::tempdir().expect("pack directory");
        let result = write_direct_full_snapshot(
            &source,
            &password,
            1,
            TeslaMateReadLimits {
                maximum_rows: 32,
                parallel_copy_lanes: 3,
                ..TeslaMateReadLimits::default()
            },
            &ProjectionPackWriter::new(packs.path()),
            ProjectionBinding {
                installation_id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                vehicle_id: Uuid::new_v4(),
                generation: 1,
                selected_car_id: 1,
            },
            Uuid::new_v4(),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
        )
        .await
        .expect("complete direct import");

        assert!(!result.chunks.is_empty());
        assert_eq!(result.report.completed_drives, 1);
        assert_eq!(result.report.projected_positions, 2);
        assert_eq!(result.report.skipped_unattached_positions, 1);
        assert_eq!(result.report.projected_charges, 1);
        assert_eq!(result.report.projected_charge_samples, 1);
        assert_eq!(result.report.projected_states, 1);
    }

    #[tokio::test]
    async fn native_ten_million_corpus_direct_import_meets_target_when_enabled() {
        if std::env::var("TESLATLAS_HUB_RUN_10M").as_deref() != Ok("1") {
            return;
        }
        let (source, password) = configured_native_postgres_source(true)
            .expect("10m test source URL or local TeslaMate fixture URL");
        let packs = tempfile::tempdir().expect("pack directory");
        let limits = TeslaMateReadLimits {
            // The canonical source contains 10.4M positions before drives,
            // charges, state rows, and metadata are counted. Keep the direct
            // capture bounded while leaving a proven ceiling for that corpus.
            maximum_rows: 20_000_000,
            parallel_copy_lanes: 3,
            ..TeslaMateReadLimits::default()
        };
        let expected_source_counts =
            native_ten_million_expected_source_counts(&source, &password, limits, packs.path())
                .await;
        let projection_state_limits = TeslaMateProjectionStateLimits {
            max_rows: u64::try_from(limits.maximum_rows).expect("maximum rows fits u64"),
            max_state_bytes: limits.maximum_stage_bytes,
            max_changed_payload_bytes: limits.maximum_stage_bytes,
            minimum_free_bytes: limits.minimum_free_bytes,
        };
        let writer = ProjectionPackWriter::new(packs.path());
        let mut phase_trace = native_ten_million_phase_trace::enabled_from_environment();
        let started = Instant::now();
        let import = tokio::time::timeout(
            Duration::from_secs(600),
            write_direct_full_snapshot_with_projection_state(
                &source,
                &password,
                1,
                limits,
                &writer,
                ProjectionBinding {
                    installation_id: Uuid::new_v4(),
                    account_id: Uuid::new_v4(),
                    vehicle_id: Uuid::new_v4(),
                    generation: 1,
                    selected_car_id: 1,
                },
                Uuid::new_v4(),
                SequenceRange {
                    from_exclusive: 0,
                    to_inclusive: 1,
                },
                projection_state_limits.max_state_bytes,
                || {
                    TeslaMateProjectionState::create(packs.path(), projection_state_limits)
                        .map(TeslaMateProjectionStateCapture::for_initial_base)
                        .map_err(Into::into)
                },
            ),
        )
        .await;
        match &import {
            Err(_) => {
                if let Some(trace) = phase_trace.as_mut() {
                    trace.report("timeout");
                }
            }
            Ok(Err(_)) => {
                if let Some(trace) = phase_trace.as_mut() {
                    trace.report("import_error");
                }
            }
            Ok(Ok(_)) => {}
        }
        let mut result = import
            .expect("ten-million direct import timed out")
            .expect("ten-million direct import")
            .packs;
        if let Some(trace) = phase_trace.as_mut() {
            trace.report("completed");
        }

        assert!(started.elapsed() < Duration::from_secs(600));
        let updates_accounted = result
            .report
            .projected_updates
            .checked_add(result.report.skipped_incomplete_updates)
            .expect("update counts do not overflow");
        validate_direct_source_counts(expected_source_counts, result.report, updates_accounted)
            .expect("direct report must account for every validated source row");
        let state = result
            .projection_state
            .as_mut()
            .expect("direct stateful capture must retain its projection state");
        let state_stats = state.seal().expect("seal direct projection state");
        assert!(state_stats.sealed);
        assert_eq!(state_stats.changed_row_count, 0);
        assert_eq!(state_stats.changed_payload_bytes, 0);
        assert!(
            state_stats.row_count >= result.report.logical_row_count().expect("row count"),
            "state capture must cover every projected typed row"
        );
    }
}
