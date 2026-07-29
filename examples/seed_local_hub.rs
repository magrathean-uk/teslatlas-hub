//! Publish one typed local snapshot for Mac Simulator pairing proof.
//! Usage: cargo run --example seed_local_hub -- /path/to/data_dir
//! Never used in production packaging.

use std::env;

use teslatlas_hub::{
    db::{HubStore, SourceDescriptor, VehicleDescriptor},
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionDrive, ProjectionPackRequest,
        ProjectionPackWriter, ProjectionPosition, ProjectionSnapshot,
    },
    protocol::{CursorKey, SequenceRange},
};
use uuid::Uuid;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = env::args()
        .nth(1)
        .ok_or("usage: seed_local_hub <data_dir>")?;
    let cursor_path = env::var("CREDENTIALS_DIRECTORY")
        .map(|dir| std::path::PathBuf::from(dir).join("cursor-key"))
        .map_err(|_| "CREDENTIALS_DIRECTORY must point at the local cursor key dir")?;
    let mut key_bytes = [0_u8; 32];
    let bytes = std::fs::read(&cursor_path)?;
    if bytes.len() != 32 {
        return Err("cursor-key must be exactly 32 bytes".into());
    }
    key_bytes.copy_from_slice(&bytes);
    let cursor_key = CursorKey::from_bytes(key_bytes);

    let store = HubStore::initialize(&data_dir)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;
    let source = store.register_source(
        &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
        now,
    )?;
    let mut descriptor = VehicleDescriptor::new(source.source_id, "9");
    descriptor.display_name = Some("Local Mac Hub".to_owned());
    descriptor.vin = Some("5YJ3E1EA7KF000001".to_owned());
    let vehicle = store.register_vehicle(&descriptor, now)?;
    let installation_id = store.installation_id()?;
    let selected_car_id = 9_i64;
    let snapshot = ProjectionSnapshot {
        cars: vec![ProjectionCar {
            id: selected_car_id,
            name: "Local Mac Hub".to_owned(),
            model: "model3".to_owned(),
            vin: Some("5YJ3E1EA7KF000001".to_owned()),
            firmware_version: Some("2026.20".to_owned()),
            efficiency_wh_per_km: None,
        }],
        drives: vec![ProjectionDrive {
            id: 1,
            car_id: selected_car_id,
            optimized_at_ms: None,
            start_date_ms: now - 600_000,
            end_date_ms: now - 300_000,
            distance_km: Some(4.2),
            duration_min: Some(5),
            efficiency: None,
            outside_temp_avg: Some(18.0),
            speed_max: Some(72),
            start_address: None,
            end_address: None,
            start_geofence: None,
            end_geofence: None,
            start_latitude: Some(47.50),
            start_longitude: Some(19.04),
            end_latitude: Some(47.51),
            end_longitude: Some(19.05),
            start_soc: Some(70),
            end_soc: Some(68),
            start_rated_range_km: Some(320.0),
            end_rated_range_km: Some(315.0),
        }],
        positions: vec![
            ProjectionPosition {
                id: 1,
                drive_id: 1,
                car_id: selected_car_id,
                date_ms: now - 600_000,
                latitude: 47.50,
                longitude: 19.04,
                speed: Some(20),
                power: Some(12),
                battery_level: Some(70),
                usable_battery_level: Some(69),
                elevation: None,
                odometer: Some(12_000.0),
                ideal_battery_range_km: None,
                rated_battery_range_km: Some(320.0),
                is_climate_on: Some(true),
                inside_temp: Some(21.0),
                outside_temp: Some(18.0),
            },
            ProjectionPosition {
                id: 2,
                drive_id: 1,
                car_id: selected_car_id,
                date_ms: now - 300_000,
                latitude: 47.51,
                longitude: 19.05,
                speed: Some(0),
                power: Some(0),
                battery_level: Some(68),
                usable_battery_level: Some(67),
                elevation: None,
                odometer: Some(12_004.2),
                ideal_battery_range_km: None,
                rated_battery_range_km: Some(315.0),
                is_climate_on: Some(true),
                inside_temp: Some(21.0),
                outside_temp: Some(18.0),
            },
        ],
        charges: Vec::new(),
        charge_samples: Vec::new(),
    };
    let sequence = store.next_full_snapshot_sequence(vehicle.vehicle_id)?;
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding: ProjectionBinding {
            installation_id,
            account_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            generation: source.generation,
            selected_car_id,
        },
        sequence: SequenceRange {
            from_exclusive: sequence,
            to_inclusive: sequence,
        },
        snapshot: &snapshot,
    };
    let built = ProjectionPackWriter::new(store.packs_dir()).write_full_snapshot(&request)?;
    let manifest = request.signed_manifest(&built, &cursor_key)?;
    store.publish_manifest(&manifest)?;
    println!(
        "{}",
        serde_json::json!({
            "vehicleId": vehicle.vehicle_id,
            "snapshotId": request.snapshot_id,
            "packSha256": built.metadata.sha256.to_string(),
            "rows": built.metadata.row_count,
        })
    );
    Ok(())
}
