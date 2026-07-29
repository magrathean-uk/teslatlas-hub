//! Persistence boundary for an explicit legacy owner-token compatibility read.
//!
//! Networking lives in `owner_api`; this module turns one already-completed
//! manual read into bounded, append-only Hub observations. It deliberately has
//! no scheduler and no mutable token state.

use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    config::{ConfigError, HubConfig},
    credentials::{CredentialDirectory, CredentialError},
    db::{HubStore, ObservationInput, SourceDescriptor, StoreError, VehicleDescriptor},
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionPackError, ProjectionPackRequest,
        ProjectionPackWriter, ProjectionSnapshot,
    },
    owner_api::{ManualCollection, OwnerApi, OwnerApiConfigError, OwnerApiError, VehicleData},
    protocol::{CursorKey, SequenceRange},
};

const OWNER_API_SOURCE_KIND: &str = "owner_api_compat";
const OWNER_API_SOURCE_KEY: &str = "local_installation_v1";
const EARLIEST_PLAUSIBLE_TIMESTAMP_MS: i64 = 946_684_800_000; // 2000-01-01 UTC
const FUTURE_TIMESTAMP_SKEW_MS: i64 = 5 * 60 * 1000;

/// Result safe to print from a one-shot collection service. It contains local
/// UUIDs and numeric vehicle ids, but never a bearer token, URL, or response.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ManualCollectionReport {
    pub source_id: Uuid,
    pub vehicles_seen: usize,
    pub online_vehicles_seen: usize,
    pub snapshots_received: usize,
    pub observations_inserted: usize,
    pub observations_already_present: usize,
    pub snapshots_published: usize,
    pub vehicle_failures: usize,
}

/// Read the decrypted systemd credential only for this explicit operation,
/// then perform one compatibility collection and persist it append-only.
pub async fn collect_once_from_systemd(
    store: &HubStore,
    config: &HubConfig,
) -> Result<ManualCollectionReport, CollectorError> {
    // Refuse a missing or invalid explicit endpoint before opening the
    // credential file. A normal Hub install therefore never touches a token
    // merely because somebody invoked the collection unit too early.
    let client = OwnerApi::new(config.collector.owner_api_options()?)?;
    let credentials = CredentialDirectory::from_systemd_environment()?
        .ok_or(CollectorError::MissingCredentialDirectory)?;
    let token = credentials.owner_token()?;
    let cursor_key = credentials.cursor_key()?;
    let collection = client.collect_once(&token).await?;
    let received_at_ms = current_epoch_millis()?;
    let mut report = persist_collection(store, &collection, received_at_ms)?;
    report.snapshots_published =
        publish_compatibility_snapshots(store, &cursor_key, &collection, received_at_ms)?;
    Ok(report)
}

/// Persist one completed compatibility collection. The supplied receipt time
/// makes storage tests deterministic; production obtains it from the system
/// clock only after the HTTP read succeeds.
pub fn persist_collection(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
) -> Result<ManualCollectionReport, CollectorError> {
    if received_at_ms < 0 {
        return Err(CollectorError::InvalidReceiptTimestamp);
    }

    let source = store.register_source(
        &SourceDescriptor::new(OWNER_API_SOURCE_KIND, OWNER_API_SOURCE_KEY),
        received_at_ms,
    )?;
    let mut vehicles = std::collections::BTreeMap::new();
    let mut online_vehicles_seen = 0;

    for vehicle in &collection.vehicles {
        if vehicle.is_online() {
            online_vehicles_seen += 1;
        }
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string());
        descriptor.vin = Some(vehicle.vin.clone());
        descriptor.display_name = vehicle.display_name.clone();
        let registered = store.register_vehicle(&descriptor, received_at_ms)?;
        vehicles.insert(vehicle.id.get(), registered.vehicle_id);
    }

    let mut observations_inserted = 0;
    let mut observations_already_present = 0;
    for snapshot in &collection.snapshots {
        let source_vehicle_id = snapshot.vehicle_id().get();
        let vehicle_id = vehicles
            .get(&source_vehicle_id)
            .copied()
            .ok_or(CollectorError::SnapshotWithoutListedVehicle)?;
        let append = store.append_observation(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id,
                observed_at_ms: observation_timestamp(snapshot, received_at_ms),
                payload: observation_payload(snapshot),
            },
            received_at_ms,
        )?;
        if append.inserted {
            observations_inserted += 1;
        } else {
            observations_already_present += 1;
        }
    }

    Ok(ManualCollectionReport {
        source_id: source.source_id,
        vehicles_seen: collection.vehicles.len(),
        online_vehicles_seen,
        snapshots_received: collection.snapshots.len(),
        observations_inserted,
        observations_already_present,
        snapshots_published: 0,
        vehicle_failures: collection.failures.len(),
    })
}

/// Publish a complete, typed first-party mirror for every discovered owner
/// vehicle. Compatibility reads provide present-state data, not a durable
/// closed-drive history, so this first projection publishes only the vehicle
/// row. Raw observations retain the precise source material until the Fleet
/// Telemetry normalizer owns drive and charge lifecycle construction.
///
/// A car-only typed pack is still a real, verifiable Hub mirror: it lets an
/// iPhone pair, select this vehicle, and establish the same durable identity
/// and atomic import path used for later telemetry snapshots. No location,
/// drive, or charge row is fabricated from one point-in-time response.
fn publish_compatibility_snapshots(
    store: &HubStore,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
    published_at_ms: i64,
) -> Result<usize, CollectorError> {
    let source = store.register_source(
        &SourceDescriptor::new(OWNER_API_SOURCE_KIND, OWNER_API_SOURCE_KEY),
        published_at_ms,
    )?;
    let installation_id = store.installation_id()?;
    let snapshots: HashMap<u64, &VehicleData> = collection
        .snapshots
        .iter()
        .map(|snapshot| (snapshot.vehicle_id().get(), snapshot))
        .collect();
    let writer = ProjectionPackWriter::new(store.packs_dir());

    for vehicle in &collection.vehicles {
        let source_vehicle_id = vehicle.id.get();
        let mut descriptor =
            VehicleDescriptor::new(source.source_id, source_vehicle_id.to_string());
        descriptor.vin = clean_optional_text(Some(&vehicle.vin));
        descriptor.display_name = clean_optional_text(vehicle.display_name.as_deref());
        let registered = store.register_vehicle(&descriptor, published_at_ms)?;
        let selected_car_id = compatibility_car_id(source_vehicle_id);
        let snapshot = ProjectionSnapshot {
            cars: vec![compatibility_car(
                vehicle,
                snapshots.get(&source_vehicle_id).copied(),
                selected_car_id,
            )],
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
        };
        let sequence = store.next_full_snapshot_sequence(registered.vehicle_id)?;
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: ProjectionBinding {
                installation_id,
                account_id: source.source_id,
                vehicle_id: registered.vehicle_id,
                generation: source.generation,
                selected_car_id,
            },
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &snapshot,
        };
        let built = writer.write_full_snapshot(&request)?;
        let manifest = request.signed_manifest(&built, cursor_key)?;
        store.publish_manifest(&manifest)?;
    }
    Ok(collection.vehicles.len())
}

fn compatibility_car(
    vehicle: &crate::owner_api::Vehicle,
    snapshot: Option<&VehicleData>,
    selected_car_id: i64,
) -> ProjectionCar {
    ProjectionCar {
        id: selected_car_id,
        name: clean_required_text(vehicle.display_name.as_deref(), "Tesla"),
        model: clean_required_text(
            snapshot.and_then(|snapshot| nested_text(snapshot, "vehicle_config", "car_type")),
            "Tesla",
        ),
        vin: clean_optional_text(Some(&vehicle.vin)),
        firmware_version: clean_optional_text(
            snapshot.and_then(|snapshot| nested_text(snapshot, "vehicle_state", "car_version")),
        ),
        efficiency_wh_per_km: None,
    }
}

fn compatibility_car_id(source_vehicle_id: u64) -> i64 {
    // The pack contract uses a positive signed local car ID. This is only an
    // in-pack foreign key; the durable Hub identity is the registered UUID.
    let maximum = i64::MAX as u64;
    i64::try_from(source_vehicle_id % maximum)
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(1)
}

fn nested_text<'a>(snapshot: &'a VehicleData, group: &str, field: &str) -> Option<&'a str> {
    snapshot
        .fields()
        .get(group)
        .and_then(Value::as_object)
        .and_then(|fields| fields.get(field))
        .and_then(Value::as_str)
}

fn clean_required_text(value: Option<&str>, fallback: &str) -> String {
    clean_optional_text(value).unwrap_or_else(|| fallback.to_owned())
}

fn clean_optional_text(value: Option<&str>) -> Option<String> {
    const MAX_COMPATIBILITY_TEXT_BYTES: usize = 512;
    let value = value?.trim();
    (!value.is_empty()
        && value.len() <= MAX_COMPATIBILITY_TEXT_BYTES
        && !value.chars().any(char::is_control))
    .then(|| value.to_owned())
}

fn observation_payload(snapshot: &VehicleData) -> Value {
    let mut payload = Map::new();
    payload.insert(
        "record_type".to_owned(),
        Value::String("owner_api_vehicle_data_v1".to_owned()),
    );
    payload.insert(
        "source_vehicle_id".to_owned(),
        Value::String(snapshot.vehicle_id().get().to_string()),
    );
    payload.insert(
        "vehicle_data".to_owned(),
        Value::Object(snapshot.fields().clone()),
    );
    Value::Object(payload)
}

fn observation_timestamp(snapshot: &VehicleData, received_at_ms: i64) -> i64 {
    let fields = snapshot.fields();
    let candidates = [
        fields
            .get("drive_state")
            .and_then(Value::as_object)
            .and_then(|drive_state| drive_state.get("timestamp")),
        fields.get("timestamp"),
    ];
    let maximum = received_at_ms.saturating_add(FUTURE_TIMESTAMP_SKEW_MS);
    candidates
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .find(|timestamp| {
            (*timestamp >= EARLIEST_PLAUSIBLE_TIMESTAMP_MS) && (*timestamp <= maximum)
        })
        .unwrap_or(received_at_ms)
}

fn current_epoch_millis() -> Result<i64, CollectorError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CollectorError::SystemClockBeforeEpoch)?;
    i64::try_from(duration.as_millis()).map_err(|_| CollectorError::SystemClockOutOfRange)
}

#[derive(Debug, Error)]
pub enum CollectorError {
    #[error("manual collection requires a systemd credential directory")]
    MissingCredentialDirectory,
    #[error("manual collection receipt timestamp is invalid")]
    InvalidReceiptTimestamp,
    #[error("manual collection received data for a vehicle absent from discovery")]
    SnapshotWithoutListedVehicle,
    #[error("system clock is before the Unix epoch")]
    SystemClockBeforeEpoch,
    #[error("system clock is outside the supported timestamp range")]
    SystemClockOutOfRange,
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Credential(#[from] CredentialError),
    #[error(transparent)]
    OwnerApiConfig(#[from] OwnerApiConfigError),
    #[error(transparent)]
    OwnerApi(#[from] OwnerApiError),
    #[error(transparent)]
    Projection(#[from] ProjectionPackError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::owner_api::{Vehicle, VehicleData};

    #[test]
    fn persists_a_collected_snapshot_and_retries_without_duplication() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let received_at_ms = 1_800_000_000_000;
        let collection = ManualCollection {
            vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({"drive_state": {"timestamp": received_at_ms - 1}}),
            )],
            failures: vec![],
        };

        let first =
            persist_collection(&store, &collection, received_at_ms).expect("first collection");
        let second =
            persist_collection(&store, &collection, received_at_ms).expect("retry collection");

        assert_eq!(first.observations_inserted, 1);
        assert_eq!(second.observations_inserted, 0);
        assert_eq!(second.observations_already_present, 1);
        let vehicle_id = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("stored UUID");
        let observations = store
            .observations_for_vehicle(vehicle_id, crate::db::ObservationQuery::from_start(1))
            .expect("stored observation");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].observed_at_ms, received_at_ms - 1);
        assert_eq!(
            observations[0].payload["record_type"],
            "owner_api_vehicle_data_v1"
        );
    }

    #[test]
    fn invalid_or_future_source_times_fall_back_to_receipt_time() {
        let received_at_ms = 1_800_000_000_000;
        for timestamp in [1_i64, received_at_ms + FUTURE_TIMESTAMP_SKEW_MS + 1] {
            let snapshot =
                VehicleData::for_test(9, json!({"drive_state": {"timestamp": timestamp}}));
            assert_eq!(
                observation_timestamp(&snapshot, received_at_ms),
                received_at_ms
            );
        }
    }

    #[test]
    fn compatibility_collection_publishes_a_real_car_only_phone_snapshot() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let collected_at_ms = 1_800_000_000_000;
        let collection = ManualCollection {
            vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {"timestamp": collected_at_ms - 1},
                    "vehicle_config": {"car_type": "model3"},
                    "vehicle_state": {"car_version": "2026.20"}
                }),
            )],
            failures: vec![],
        };

        persist_collection(&store, &collection, collected_at_ms).expect("raw observation");
        let published = publish_compatibility_snapshots(
            &store,
            &CursorKey::from_bytes([7; 32]),
            &collection,
            collected_at_ms,
        )
        .expect("typed projection");

        assert_eq!(published, 1);
        let vehicle_id = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("stored UUID");
        let manifest = store
            .manifest_for_vehicle(vehicle_id)
            .expect("manifest query")
            .expect("published manifest");
        assert_eq!(manifest.chunk_count, 1);
        assert_eq!(manifest.total_rows, 1);
        assert_eq!(
            manifest.chunks[0].tables,
            vec![crate::protocol::MirrorTable::Car]
        );
        assert_eq!(store.published_vehicles().expect("published cars").len(), 1);
    }
}
