//! Persistence boundary for an explicit legacy owner-token compatibility read.
//!
//! Networking lives in `owner_api`; this module turns completed reads into
//! bounded, append-only Hub observations, materialises durable drive/charge
//! history through the pure lifecycle projector, and optionally runs a
//! supervised no-wake schedule. The owner token is never held in configuration
//! or argv.

use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::{
    config::{ConfigError, HubConfig},
    credentials::{CredentialDirectory, CredentialError},
    db::{
        HubStore, ObservationInput, ObservationQuery, SourceDescriptor, StoreError,
        VehicleDescriptor,
    },
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionPackError, ProjectionPackRequest,
        ProjectionPackWriter, ProjectionSnapshot,
    },
    lifecycle::{LifecycleError, LifecycleSample, OpenSessionState, apply_sample},
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
    pub drives_closed: usize,
    pub charges_closed: usize,
    pub positions_materialised: usize,
    pub charge_samples_materialised: usize,
    pub lifecycle_quarantines: usize,
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
    let lifecycle = materialise_lifecycle_for_collection(store, &collection, received_at_ms)?;
    report.drives_closed = lifecycle.drives_closed;
    report.charges_closed = lifecycle.charges_closed;
    report.positions_materialised = lifecycle.positions_materialised;
    report.charge_samples_materialised = lifecycle.charge_samples_materialised;
    report.lifecycle_quarantines = lifecycle.lifecycle_quarantines;
    report.snapshots_published =
        publish_compatibility_snapshots(store, &cursor_key, &collection, received_at_ms)?;
    Ok(report)
}

/// Supervised, opt-in no-wake collector. Requires an explicit positive interval
/// in configuration. Uses exponential backoff on transport failures and never
/// issues wake or command requests.
pub async fn run_supervised_from_systemd(
    store: &HubStore,
    config: &HubConfig,
) -> Result<(), CollectorError> {
    let interval = config.collector.supervised_interval()?;
    let max_backoff = Duration::from_secs(config.collector.max_backoff_seconds.max(1));
    let mut backoff = interval;
    loop {
        let started = Instant::now();
        match collect_once_from_systemd(store, config).await {
            Ok(report) => {
                tracing::info!(
                    vehicles = report.vehicles_seen,
                    online = report.online_vehicles_seen,
                    inserted = report.observations_inserted,
                    drives = report.drives_closed,
                    charges = report.charges_closed,
                    "compatibility collection completed"
                );
                backoff = interval;
            }
            Err(error) => {
                tracing::warn!(error = %error, "compatibility collection failed; backing off");
                sleep(backoff).await;
                backoff = (backoff.saturating_mul(2)).min(max_backoff);
                continue;
            }
        }
        let elapsed = started.elapsed();
        if elapsed < interval {
            sleep(interval - elapsed).await;
        }
    }
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
        drives_closed: 0,
        charges_closed: 0,
        positions_materialised: 0,
        charge_samples_materialised: 0,
        lifecycle_quarantines: 0,
    })
}

#[derive(Debug, Default)]
pub struct LifecycleMaterialisationReport {
    pub drives_closed: usize,
    pub charges_closed: usize,
    pub positions_materialised: usize,
    pub charge_samples_materialised: usize,
    pub lifecycle_quarantines: usize,
}

/// Project newly stored observations into durable drive/charge history and
/// crash-safe open-session state. Pure projection lives in `lifecycle`; this
/// function only loads the cursor, applies samples, and commits the delta.
pub fn materialise_lifecycle_for_collection(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
) -> Result<LifecycleMaterialisationReport, CollectorError> {
    let source = store.register_source(
        &SourceDescriptor::new(OWNER_API_SOURCE_KIND, OWNER_API_SOURCE_KEY),
        received_at_ms,
    )?;
    let mut report = LifecycleMaterialisationReport::default();
    for vehicle in &collection.vehicles {
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string());
        descriptor.vin = clean_optional_text(Some(&vehicle.vin));
        descriptor.display_name = clean_optional_text(vehicle.display_name.as_deref());
        let registered = store.register_vehicle(&descriptor, received_at_ms)?;
        let car_id = compatibility_car_id(vehicle.id.get());
        let materialised = materialise_vehicle_lifecycle(
            store,
            registered.vehicle_id,
            car_id,
            &vehicle.state,
            received_at_ms,
        )?;
        report.drives_closed += materialised.drives_closed;
        report.charges_closed += materialised.charges_closed;
        report.positions_materialised += materialised.positions_materialised;
        report.charge_samples_materialised += materialised.charge_samples_materialised;
        report.lifecycle_quarantines += materialised.lifecycle_quarantines;
    }
    Ok(report)
}

fn materialise_vehicle_lifecycle(
    store: &HubStore,
    vehicle_id: Uuid,
    car_id: i64,
    vehicle_state: &str,
    received_at_ms: i64,
) -> Result<LifecycleMaterialisationReport, CollectorError> {
    let existing = store.load_lifecycle_state(vehicle_id)?;
    let mut state = match existing.as_ref() {
        Some(record) => match OpenSessionState::decode(&record.open_session_json) {
            Ok(state) => state,
            Err(_) => {
                // Corrupt open state is quarantined and rebuilt from a clean
                // cursor so prior completed history remains untouched.
                let mut clean = OpenSessionState::new();
                clean.last_observation_id = record.last_observation_id;
                clean
            }
        },
        None => OpenSessionState::new(),
    };

    let observations = store.observations_for_vehicle(
        vehicle_id,
        ObservationQuery {
            from_observed_at_ms: None,
            until_observed_at_ms: None,
            limit: crate::db::MAX_OBSERVATION_QUERY_LIMIT,
        },
    )?;

    let mut report = LifecycleMaterialisationReport::default();
    let mut total_delta = crate::lifecycle::LifecycleDelta::default();
    let mut quarantined = existing.as_ref().is_some_and(|record| record.quarantined);

    for observation in observations {
        if observation.observation_id <= state.last_observation_id {
            continue;
        }
        let sample = LifecycleSample {
            observation_id: observation.observation_id,
            observed_at_ms: observation.observed_at_ms,
            vehicle_state: vehicle_state.to_owned(),
            payload: observation.payload,
        };
        let step = apply_sample(state, car_id, &sample)?;
        state = step.state;
        quarantined |= step.quarantined;
        if step.quarantined {
            report.lifecycle_quarantines += 1;
        }
        report.drives_closed += step.delta.drives.len();
        report.charges_closed += step.delta.charges.len();
        report.positions_materialised += step.delta.positions.len();
        report.charge_samples_materialised += step.delta.charge_samples.len();
        total_delta.drives.extend(step.delta.drives);
        total_delta.positions.extend(step.delta.positions);
        total_delta.charges.extend(step.delta.charges);
        total_delta.charge_samples.extend(step.delta.charge_samples);
    }

    let encoded = state.encode().map_err(CollectorError::Lifecycle)?;
    store.commit_lifecycle_delta(&crate::db::LifecycleCommit {
        vehicle_id,
        car_id,
        open_session_json: &encoded,
        last_observation_id: state.last_observation_id,
        quarantined,
        updated_at_ms: received_at_ms,
        delta: &total_delta,
    })?;
    Ok(report)
}

/// Publish a typed first-party mirror for every discovered owner vehicle.
/// Completed drive, position, charge, and charge-sample rows come only from
/// the materialised lifecycle store — never fabricated from a single sample.
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
        let history = store.materialised_history(registered.vehicle_id)?;
        let snapshot = ProjectionSnapshot {
            cars: vec![compatibility_car(
                vehicle,
                snapshots.get(&source_vehicle_id).copied(),
                selected_car_id,
            )],
            drives: history.drives,
            positions: history.positions,
            charges: history.charges,
            charge_samples: history.charge_samples,
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
    Lifecycle(#[from] LifecycleError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        lifecycle::OpenSessionState,
        owner_api::{Vehicle, VehicleData},
    };

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
        materialise_lifecycle_for_collection(&store, &collection, collected_at_ms)
            .expect("lifecycle");
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

    #[test]
    fn synthetic_drive_and_charge_survive_mid_session_restart() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let t0 = 1_800_000_500_000_i64;
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");

        // Open a drive.
        let open_drive = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {
                        "shift_state": "D",
                        "speed": 25,
                        "latitude": 47.0,
                        "longitude": 19.0,
                        "timestamp": t0
                    },
                    "charge_state": {"battery_level": 70, "battery_range": 200.0}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &open_drive, t0).expect("persist open");
        materialise_lifecycle_for_collection(&store, &open_drive, t0).expect("materialise open");

        let vehicle_id = store
            .open()
            .expect("db")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("id")
            .parse::<Uuid>()
            .expect("uuid");
        let open_state = store
            .load_lifecycle_state(vehicle_id)
            .expect("load")
            .expect("open state exists");
        let decoded = OpenSessionState::decode(&open_state.open_session_json).expect("decode");
        assert!(decoded.open_drive.is_some());

        // Simulate process restart: reopen store path and finish the drive.
        let store = HubStore::initialize(temp.path()).expect("reopen store");
        let close_drive = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {
                        "shift_state": "P",
                        "speed": 0,
                        "latitude": 47.01,
                        "longitude": 19.01,
                        "timestamp": t0 + 120_000
                    },
                    "charge_state": {"battery_level": 68, "battery_range": 195.0}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &close_drive, t0 + 120_000).expect("persist close");
        let lifecycle = materialise_lifecycle_for_collection(&store, &close_drive, t0 + 120_000)
            .expect("materialise close");
        assert_eq!(lifecycle.drives_closed, 1);
        assert_eq!(lifecycle.positions_materialised, 1);

        // Charge lifecycle on the same durable vehicle.
        let charge_open = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "charge_state": {
                        "charging_state": "Charging",
                        "battery_level": 40,
                        "charge_energy_added": 1.0,
                        "charger_power": 11.0,
                        "battery_range": 120.0
                    },
                    "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t0 + 200_000}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &charge_open, t0 + 200_000).expect("persist charge open");
        materialise_lifecycle_for_collection(&store, &charge_open, t0 + 200_000)
            .expect("materialise charge open");

        let store = HubStore::initialize(temp.path()).expect("second reopen");
        let charge_close = ManualCollection {
            vehicles: vec![vehicle],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "charge_state": {
                        "charging_state": "Complete",
                        "battery_level": 80,
                        "charge_energy_added": 12.0,
                        "charger_power": 0.0,
                        "battery_range": 220.0
                    },
                    "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t0 + 800_000}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &charge_close, t0 + 800_000).expect("persist charge close");
        let lifecycle = materialise_lifecycle_for_collection(&store, &charge_close, t0 + 800_000)
            .expect("materialise charge close");
        assert_eq!(lifecycle.charges_closed, 1);
        assert!(lifecycle.charge_samples_materialised >= 1);

        let history = store.materialised_history(vehicle_id).expect("history");
        assert_eq!(history.drives.len(), 1);
        assert_eq!(history.charges.len(), 1);
        assert_eq!(history.charges[0].end_battery_level, Some(80));
        assert_eq!(history.charges[0].charge_energy_added, Some(12.0));

        publish_compatibility_snapshots(
            &store,
            &CursorKey::from_bytes([9; 32]),
            &charge_close,
            t0 + 800_000,
        )
        .expect("publish");
        let manifest = store
            .manifest_for_vehicle(vehicle_id)
            .expect("manifest")
            .expect("published");
        assert!(manifest.total_rows > 1);
    }
}
