//! TeslaMate full-snapshot migration publisher.
//!
//! The PostgreSQL reader is intentionally separate from this module. Once a
//! reviewed, repeatable-read history exists, this module gives it a stable Hub
//! identity, maps only the selected car, writes one immutable typed pack, and
//! publishes a signed full-snapshot manifest. It does not create fake deltas.

use thiserror::Error;
use uuid::Uuid;

use crate::{
    credentials::TeslaMatePostgresPassword,
    db::{HubStore, SourceDescriptor, VehicleDescriptor},
    hub_pack::{
        BuiltProjectionPack, ProjectionBinding, ProjectionPackError, ProjectionPackRequest,
        ProjectionPackWriter, signed_full_snapshot_manifest,
    },
    protocol::{CursorKey, SequenceRange, SyncManifest},
    teslamate::ReadOnlySource,
    teslamate_direct::write_direct_full_snapshot,
    teslamate_fragments::write_staged_full_snapshot,
    teslamate_projection::{ProjectionReport, TeslaMateCar, TeslaMateHistory, project_vehicle},
    teslamate_reader::{TeslaMateReadLimits, read_selected_car},
    teslamate_stage::{TeslaMateStage, TeslaMateStageTable},
};

/// Non-secret input that identifies one TeslaMate migration source and car.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateImportRequest {
    /// Owner-chosen durable label. It must survive a hostname or port change;
    /// it is the stable Hub source key, never a PostgreSQL URL or password.
    pub source_key: String,
    pub selected_car_id: i64,
    pub imported_at_ms: i64,
}

/// Result of a successful immutable snapshot publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeslaMateImportReport {
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub snapshot_id: Uuid,
    pub sequence: u64,
    pub projection: ProjectionReport,
    pub projected_rows: u64,
}

/// Read, validate, pack, sign, and publish one complete TeslaMate snapshot.
/// The caller supplies secrets as systemd-derived values; no secret is stored
/// in the Hub database or encoded in the generated pack.
pub async fn import_from_postgres(
    store: &HubStore,
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    if request.selected_car_id <= 0 {
        return Err(TeslaMateImportError::InvalidSelectedCarId);
    }
    let car = read_selected_car(source, password, request.selected_car_id, limits).await?;
    let registered_source = store.register_source(
        &SourceDescriptor::new("teslamate", request.source_key.clone()),
        request.imported_at_ms,
    )?;
    let source_vehicle_key = stable_vehicle_key_for_car(&car)?;
    let deterministic_vehicle_id =
        Uuid::new_v5(&registered_source.source_id, source_vehicle_key.as_bytes());
    let vehicle = store.register_vehicle_with_id(
        &VehicleDescriptor {
            source_id: registered_source.source_id,
            source_vehicle_key,
            vin: nonblank(car.vin.as_deref()).map(ToOwned::to_owned),
            display_name: nonblank(car.name.as_deref()).map(ToOwned::to_owned),
        },
        request.imported_at_ms,
        deterministic_vehicle_id,
    )?;
    let sequence = store.next_full_snapshot_sequence(vehicle.vehicle_id)?;
    let snapshot_id = Uuid::new_v4();
    let binding = ProjectionBinding {
        installation_id: store.installation_id()?,
        account_id: registered_source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: registered_source.generation,
        selected_car_id: request.selected_car_id,
    };
    let range = SequenceRange {
        from_exclusive: sequence,
        to_inclusive: sequence,
    };
    let direct = write_direct_full_snapshot(
        source,
        password,
        request.selected_car_id,
        limits,
        &ProjectionPackWriter::new(store.packs_dir()),
        binding.clone(),
        snapshot_id,
        range,
    )
    .await?;
    let logical_rows = direct
        .report
        .logical_row_count()
        .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
    if store.snapshot_fingerprint_is_current(vehicle.vehicle_id, direct.fingerprint)? {
        let current = store
            .manifest_for_vehicle(vehicle.vehicle_id)?
            .ok_or(TeslaMateImportError::CurrentSnapshotMissing)?;
        discard_unpublished_chunks(&direct.chunks, &current)?;
        return Ok(TeslaMateImportReport {
            source_id: registered_source.source_id,
            vehicle_id: vehicle.vehicle_id,
            snapshot_id: current.snapshot_id,
            sequence: current.head_sequence,
            projection: direct.report,
            projected_rows: current.total_rows,
        });
    }
    let manifest = signed_full_snapshot_manifest(
        &binding,
        snapshot_id,
        range,
        &direct.chunks,
        logical_rows,
        cursor_key,
    )?;
    store.publish_manifest(&manifest)?;
    store.record_snapshot_fingerprint(vehicle.vehicle_id, direct.fingerprint)?;
    Ok(TeslaMateImportReport {
        source_id: registered_source.source_id,
        vehicle_id: vehicle.vehicle_id,
        snapshot_id,
        sequence,
        projection: direct.report,
        projected_rows: manifest.total_rows,
    })
}

/// Publish a complete sealed capture without materialising its historical
/// vectors. The sealed stage is retained until every immutable fragment has
/// passed its pack verifier and the signed manifest has been stored.
pub fn publish_staged_history(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    stage: &TeslaMateStage,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    if request.selected_car_id <= 0 {
        return Err(TeslaMateImportError::InvalidSelectedCarId);
    }
    let car = stage
        .get::<TeslaMateCar>(TeslaMateStageTable::Cars, request.selected_car_id)?
        .ok_or(TeslaMateImportError::SelectedCarMissing)?;
    let source = store.register_source(
        &SourceDescriptor::new("teslamate", request.source_key.clone()),
        request.imported_at_ms,
    )?;
    let source_vehicle_key = stable_vehicle_key_for_car(&car)?;
    let deterministic_vehicle_id = Uuid::new_v5(&source.source_id, source_vehicle_key.as_bytes());
    let vehicle = store.register_vehicle_with_id(
        &VehicleDescriptor {
            source_id: source.source_id,
            source_vehicle_key,
            vin: nonblank(car.vin.as_deref()).map(ToOwned::to_owned),
            display_name: nonblank(car.name.as_deref()).map(ToOwned::to_owned),
        },
        request.imported_at_ms,
        deterministic_vehicle_id,
    )?;
    let sequence = store.next_full_snapshot_sequence(vehicle.vehicle_id)?;
    let snapshot_id = Uuid::new_v4();
    let binding = ProjectionBinding {
        installation_id: store.installation_id()?,
        account_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: source.generation,
        selected_car_id: request.selected_car_id,
    };
    let range = SequenceRange {
        from_exclusive: sequence,
        to_inclusive: sequence,
    };
    let staged = write_staged_full_snapshot(
        stage,
        &ProjectionPackWriter::new(store.packs_dir()),
        binding.clone(),
        snapshot_id,
        range,
    )?;
    let logical_rows = staged
        .report
        .logical_row_count()
        .ok_or(ProjectionPackError::ManifestTotalsOverflow)?;
    if store.snapshot_fingerprint_is_current(vehicle.vehicle_id, staged.fingerprint)? {
        let current = store
            .manifest_for_vehicle(vehicle.vehicle_id)?
            .ok_or(TeslaMateImportError::CurrentSnapshotMissing)?;
        discard_unpublished_chunks(&staged.chunks, &current)?;
        return Ok(TeslaMateImportReport {
            source_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            snapshot_id: current.snapshot_id,
            sequence: current.head_sequence,
            projection: staged.report,
            projected_rows: current.total_rows,
        });
    }
    let manifest = signed_full_snapshot_manifest(
        &binding,
        snapshot_id,
        range,
        &staged.chunks,
        logical_rows,
        cursor_key,
    )?;
    store.publish_manifest(&manifest)?;
    store.record_snapshot_fingerprint(vehicle.vehicle_id, staged.fingerprint)?;
    Ok(TeslaMateImportReport {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        snapshot_id,
        sequence,
        projection: staged.report,
        projected_rows: manifest.total_rows,
    })
}

/// Publish an already-read source history. This seam makes the pack/identity
/// path deterministic and testable without a live PostgreSQL server.
pub fn publish_history(
    store: &HubStore,
    cursor_key: &CursorKey,
    request: &TeslaMateImportRequest,
    history: &TeslaMateHistory,
) -> Result<TeslaMateImportReport, TeslaMateImportError> {
    if request.selected_car_id <= 0 {
        return Err(TeslaMateImportError::InvalidSelectedCarId);
    }
    let projected = project_vehicle(history, request.selected_car_id)?;
    let car = projected
        .snapshot
        .cars
        .first()
        .expect("projection guarantees one selected car");
    let source = store.register_source(
        &SourceDescriptor::new("teslamate", request.source_key.clone()),
        request.imported_at_ms,
    )?;
    let source_vehicle_key = stable_vehicle_key_for_car(
        history
            .cars
            .iter()
            .find(|candidate| candidate.id == request.selected_car_id)
            .ok_or(TeslaMateImportError::SelectedCarMissing)?,
    )?;
    let deterministic_vehicle_id = Uuid::new_v5(&source.source_id, source_vehicle_key.as_bytes());
    let vehicle = store.register_vehicle_with_id(
        &VehicleDescriptor {
            source_id: source.source_id,
            source_vehicle_key,
            vin: nonblank(car.vin.as_deref()).map(ToOwned::to_owned),
            display_name: Some(car.name.clone()),
        },
        request.imported_at_ms,
        deterministic_vehicle_id,
    )?;
    let sequence = store.next_full_snapshot_sequence(vehicle.vehicle_id)?;
    let snapshot_id = Uuid::new_v4();
    let pack_id = Uuid::new_v4();
    let pack = ProjectionPackWriter::new(store.packs_dir()).write_full_snapshot(
        &ProjectionPackRequest {
            pack_id,
            snapshot_id,
            ordinal: 0,
            binding: ProjectionBinding {
                installation_id: store.installation_id()?,
                account_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                generation: source.generation,
                selected_car_id: request.selected_car_id,
            },
            // A full snapshot has no delta base. Its equal base/head marker
            // identifies this complete replacement in the catalog.
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &projected.snapshot,
        },
    )?;
    let manifest = ProjectionPackRequest {
        pack_id,
        snapshot_id,
        ordinal: 0,
        binding: ProjectionBinding {
            installation_id: store.installation_id()?,
            account_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            generation: source.generation,
            selected_car_id: request.selected_car_id,
        },
        sequence: SequenceRange {
            from_exclusive: sequence,
            to_inclusive: sequence,
        },
        snapshot: &projected.snapshot,
    }
    .signed_manifest(&pack, cursor_key)?;
    store.publish_manifest(&manifest)?;

    Ok(TeslaMateImportReport {
        source_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        snapshot_id,
        sequence,
        projection: projected.report,
        projected_rows: manifest.total_rows,
    })
}

fn stable_vehicle_key_for_car(car: &TeslaMateCar) -> Result<String, TeslaMateImportError> {
    if let Some(vin) = nonblank(car.vin.as_deref()) {
        return Ok(format!("vin:{vin}"));
    }
    let eid = car.eid;
    if eid <= 0 {
        return Err(TeslaMateImportError::StableVehicleIdentityMissing);
    }
    Ok(format!("eid:{eid}"))
}

fn nonblank(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn discard_unpublished_chunks(
    chunks: &[BuiltProjectionPack],
    current: &SyncManifest,
) -> Result<(), TeslaMateImportError> {
    for chunk in chunks {
        if current
            .chunks
            .iter()
            .any(|published| published.sha256 == chunk.metadata.sha256)
        {
            continue;
        }
        match std::fs::remove_file(&chunk.path) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(TeslaMateImportError::DiscardUnpublishedPack {
                    path: chunk.path.clone(),
                    source,
                });
            }
        }
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum TeslaMateImportError {
    #[error("TeslaMate selected car id must be positive")]
    InvalidSelectedCarId,
    #[error("TeslaMate selected car disappeared before publication")]
    SelectedCarMissing,
    #[error("current TeslaMate snapshot fingerprint has no published manifest")]
    CurrentSnapshotMissing,
    #[error("cannot discard unchanged unpublished pack {path}: {source}")]
    DiscardUnpublishedPack {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("TeslaMate selected car has neither a VIN nor a valid EID")]
    StableVehicleIdentityMissing,
    #[error(transparent)]
    Reader(#[from] crate::teslamate_reader::TeslaMateReaderError),
    #[error(transparent)]
    Direct(#[from] crate::teslamate_direct::TeslaMateDirectError),
    #[error(transparent)]
    Stage(#[from] crate::teslamate_stage::TeslaMateStageError),
    #[error(transparent)]
    Projection(#[from] crate::teslamate_projection::TeslaMateProjectionError),
    #[error(transparent)]
    Store(#[from] crate::db::StoreError),
    #[error(transparent)]
    Pack(#[from] crate::hub_pack::ProjectionPackError),
    #[error(transparent)]
    Fragments(#[from] crate::teslamate_fragments::TeslaMateFragmentError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::HubStore,
        protocol::CursorKey,
        teslamate_projection::{TeslaMateCar, TeslaMateHistory},
        teslamate_stage::{TeslaMateStage, TeslaMateStageLimits, TeslaMateStageTable},
    };

    fn history() -> TeslaMateHistory {
        TeslaMateHistory {
            cars: vec![TeslaMateCar {
                id: 1,
                eid: 88,
                vin: Some("5YJTESTVIN1234567".into()),
                name: Some("Road car".into()),
                model: Some("Model 3".into()),
                trim_badging: None,
                marketing_name: None,
                efficiency_wh_per_km: Some(0.145),
            }],
            drives: vec![],
            positions: vec![],
            charging_processes: vec![],
            charges: vec![],
            addresses: vec![],
            geofences: vec![],
            updates: vec![],
        }
    }

    #[test]
    fn publishes_stable_vehicle_full_snapshots_with_rising_markers() {
        let temporary = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(temporary.path()).unwrap();
        let request = TeslaMateImportRequest {
            source_key: "home-teslamate".into(),
            selected_car_id: 1,
            imported_at_ms: 1_700_000_000_000,
        };
        let cursor_key = CursorKey::from_bytes([7; 32]);
        let first = publish_history(&store, &cursor_key, &request, &history()).unwrap();
        let second = publish_history(&store, &cursor_key, &request, &history()).unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(first.vehicle_id, second.vehicle_id);
        assert_eq!(first.projected_rows, 1);
        let manifest = store
            .manifest_for_vehicle(first.vehicle_id)
            .unwrap()
            .expect("latest manifest");
        assert_eq!(manifest.head_sequence, 2);
        assert_eq!(
            manifest.chunks[0].format,
            crate::protocol::PackFormat::HubProjectionSqlite
        );
    }

    #[test]
    fn eid_fallback_never_uses_the_local_car_id() {
        let mut source = history();
        source.cars[0].vin = None;
        source.cars[0].eid = 9001;
        assert_eq!(
            stable_vehicle_key_for_car(&source.cars[0]).unwrap(),
            "eid:9001"
        );
    }

    #[test]
    fn sealed_stage_publication_never_needs_an_in_memory_history() {
        let data = tempfile::tempdir().unwrap();
        let imports = tempfile::tempdir().unwrap();
        let store = HubStore::initialize(data.path()).unwrap();
        let mut stage = TeslaMateStage::create(
            imports.path(),
            TeslaMateStageLimits {
                max_rows: 10,
                max_stage_bytes: 128 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .unwrap();
        let car = history().cars.remove(0);
        stage
            .insert(TeslaMateStageTable::Cars, car.id, &car)
            .unwrap();
        stage.seal().unwrap();

        let report = publish_staged_history(
            &store,
            &CursorKey::from_bytes([9; 32]),
            &TeslaMateImportRequest {
                source_key: "home-teslamate".into(),
                selected_car_id: 1,
                imported_at_ms: 1_700_000_000_000,
            },
            &stage,
        )
        .unwrap();
        let manifest = store
            .manifest_for_vehicle(report.vehicle_id)
            .unwrap()
            .expect("published staged manifest");
        assert_eq!(manifest.chunk_count, 1);
        assert_eq!(manifest.total_rows, 1);
        assert_eq!(report.projection, ProjectionReport::default());

        let unchanged = publish_staged_history(
            &store,
            &CursorKey::from_bytes([9; 32]),
            &TeslaMateImportRequest {
                source_key: "home-teslamate".into(),
                selected_car_id: 1,
                imported_at_ms: 1_700_000_060_000,
            },
            &stage,
        )
        .unwrap();
        assert_eq!(unchanged.snapshot_id, report.snapshot_id);
        assert_eq!(unchanged.sequence, report.sequence);
        assert_eq!(
            store
                .manifest_for_vehicle(report.vehicle_id)
                .unwrap()
                .unwrap(),
            manifest
        );
    }
}
