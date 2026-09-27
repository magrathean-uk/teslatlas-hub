// SPDX-License-Identifier: AGPL-3.0-only
//! Synthetic interoperability data. This module is linked only into tests and
//! the explicit fixture example, never the production Hub executable.

use serde::Serialize;
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
#[cfg(feature = "interop-fixture")]
use teslatlas_hub::hub_pack::ProjectionCarSettings;
use teslatlas_hub::{
    credentials::OwnerTokens,
    db::{
        HubStore, ObservationInput, SourceDescriptor, TeslaMateLegacyTokenStore, VehicleDescriptor,
    },
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionCharge, ProjectionDrive, ProjectionPackRequest,
        ProjectionPackRequestV2_2, ProjectionPackWriter, ProjectionSnapshot,
        ProjectionSnapshotV2_2,
    },
    protocol::{HUB_PROJECTION_SCHEMA_V3, SequenceRange, Sha256Digest},
    teslamate_credentials::{load_or_create_cursor_key, replace_key_and_tokens},
    teslamate_token::encrypt_legacy_owner_tokens,
    updates_delivery::{
        publish_updates_schema_22, sign_updates_schema_22_manifest, sign_updates_schema_22_noop,
        updates_snapshot_v2_2,
    },
};
#[cfg(feature = "interop-fixture")]
use teslatlas_hub::{
    hub_pack::{
        ProjectionFixedNumericV2_2, ProjectionFloat64BitsV2_2, ProjectionPreferredRangeV2_2,
        ProjectionUnitOfLengthV2_2, ProjectionUnitOfPressureV2_2, ProjectionUnitOfTemperatureV2_2,
    },
    teslamate_physical_fragments::{
        TeslaMatePhysicalFragmentLimits, write_staged_physical_updates_snapshot_v3_with_limits,
    },
    teslamate_projection::{
        TeslaMateCarPhysicalV2_2, TeslaMateCarSettingsPhysicalV2_2, TeslaMateChargePhysicalV2_2,
        TeslaMateChargingProcessPhysicalV2_2, TeslaMateDrivePhysicalV2_2,
        TeslaMatePositionPhysicalV2_2, TeslaMateSettingsPhysicalV2_2, TeslaMateUpdatePhysicalV2_2,
    },
    teslamate_stage::{TeslaMateStage, TeslaMateStageLimits, TeslaMateStageTable},
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const OBSERVED_AT_MS: i64 = 1_788_566_400_000;

#[derive(Clone, Copy)]
pub enum FixtureScenario {
    B1,
    ViewerR1,
    PhysicalV3Public513,
}

#[derive(Clone, Copy)]
struct FixtureVehicleIdentity<'a> {
    vehicle_id: Uuid,
    vin: &'a str,
    car_id: i64,
    name: &'a str,
}

const DEFAULT_PRIMARY_VEHICLE_ID: Uuid = Uuid::from_u128(0x11111111111141118111111111111111);
const DEFAULT_SECONDARY_VEHICLE_ID: Uuid = Uuid::from_u128(0x22222222222242228222222222222222);
const DYNAMIC_VEHICLE_ID: Uuid = Uuid::from_u128(0x33333333333343338333333333333333);
const DEFAULT_PRIMARY_VIN: &str = "5YJ3E1EA7KF000001";
const DEFAULT_SECONDARY_VIN: &str = "5YJ3E1EA7KF000002";
const DYNAMIC_VIN: &str = "5YJ3E1EA7KF000003";
const DYNAMIC_CAR_ID: i64 = 11;
#[cfg(feature = "interop-fixture")]
const PHYSICAL_V3_PROFILE_ID: &str = "hub-sync-v1@1.3.0";
#[cfg(feature = "interop-fixture")]
const PHYSICAL_V3_SNAPSHOT_ID: Uuid = Uuid::from_u128(0x51351351513541358135513513513513);
#[cfg(feature = "interop-fixture")]
const PHYSICAL_V3_UPDATE_COUNT: i32 = 1_018;

#[derive(Serialize)]
pub struct PreparedFixture {
    pub schema_version: u8,
    pub config_path: PathBuf,
    pub certificate_path: PathBuf,
    pub invitation_path: PathBuf,
    pub hub_id: Uuid,
    pub source_id: Uuid,
    pub endpoint: String,
    pub vehicle_ids: [Uuid; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub physical_v3_admission: Option<PreparedPhysicalV3Admission>,
}

#[derive(Serialize)]
pub struct PreparedPhysicalV3Admission {
    pub profile_id: &'static str,
    pub vehicle_id: Uuid,
    pub snapshot_id: Uuid,
    pub head_sequence: u64,
    pub receipt_id: String,
    pub chunk_count: u32,
    pub logical_source_rows: u64,
    pub drive_ids: [i32; 1],
    pub position_ids: [i32; 2],
    pub charging_process_ids: [i32; 1],
    pub charge_sample_ids: [i32; 2],
    pub address_rows: u8,
    pub geofence_rows: u8,
    pub collector_enabled: bool,
}

#[cfg(feature = "interop-fixture")]
#[derive(Serialize)]
pub struct DynamicVehicleMutation {
    pub status: &'static str,
    pub vehicle_id: Uuid,
}

/// One sealed source/vehicle tuple for an empty, synthetic Edge control
/// fixture. It is compiled only when the explicit interop feature is enabled.
#[cfg(feature = "interop-fixture")]
#[derive(Clone)]
pub struct EmptyEdgeBinding {
    pub installation_id: String,
    pub lineage: String,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub vin: String,
    pub car_id: i64,
}

#[cfg(feature = "interop-fixture")]
#[derive(Serialize)]
pub struct PreparedEmptyEdgeBinding {
    pub schema_version: u8,
    pub data_dir: PathBuf,
    pub installation_id: String,
    pub lineage: String,
    pub hub_id: Uuid,
    pub source_id: Uuid,
    pub vehicle_id: Uuid,
    pub vin: String,
    pub car_id: i64,
    pub base_snapshot_id: Uuid,
    pub base_sequence: u64,
}

/// Create one fresh, empty Edge-bound Hub catalogue. This intentionally uses
/// the normal source, vehicle, and settings APIs; it never rewrites catalogue
/// rows or seeds observations or Edge receipts. It publishes the one empty
/// base required by normal macOS preflight to recognise the configured car.
#[cfg(feature = "interop-fixture")]
pub fn prepare_empty_edge_binding(
    root: &Path,
    binding: &EmptyEdgeBinding,
) -> Result<PreparedEmptyEdgeBinding> {
    if !root.is_absolute()
        || binding.source_id.is_nil()
        || binding.vehicle_id.is_nil()
        || binding.car_id <= 0
    {
        return Err(
            "empty Edge fixture requires absolute root and sealed non-nil identities".into(),
        );
    }

    fs::DirBuilder::new().mode(0o700).create(root)?;
    let data_dir = root.join("hub");
    let store = HubStore::initialize(&data_dir)?;
    let source = store.register_interop_source_with_id(
        &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
        OBSERVED_AT_MS,
        binding.source_id,
    )?;
    let mut vehicle = VehicleDescriptor::new(source.source_id, binding.car_id.to_string())
        .with_tesla_identity(Some(binding.car_id), None);
    vehicle.vin = Some(binding.vin.clone());
    vehicle.display_name = Some("Empty Edge binding".into());
    store.register_vehicle_with_id(&vehicle, OBSERVED_AT_MS, binding.vehicle_id)?;
    store.upsert_car_settings(
        binding.vehicle_id,
        binding.car_id,
        &ProjectionCarSettings::default(),
    )?;
    let hub_id = store.installation_id()?;
    let cursor_key = load_or_create_cursor_key(&data_dir)?;
    let car: ProjectionCar = serde_json::from_value(json!({
        "id": binding.car_id,
        "name": "Empty Edge binding",
        "model": "model3",
        "vin": binding.vin,
        "source_eid": binding.car_id,
        "firmware_version": "synthetic-empty-base"
    }))?;
    let snapshot = ProjectionSnapshot {
        cars: vec![car],
        drives: Vec::new(),
        positions: Vec::new(),
        charges: Vec::new(),
        charge_samples: Vec::new(),
    };
    store.persist_materialised_car_if_absent(binding.vehicle_id, &snapshot.cars[0])?;
    let base_sequence = store.next_full_snapshot_sequence(binding.vehicle_id)?;
    let base_snapshot_id = Uuid::new_v4();
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: base_snapshot_id,
        ordinal: 0,
        binding: ProjectionBinding {
            installation_id: hub_id,
            account_id: source.source_id,
            vehicle_id: binding.vehicle_id,
            generation: source.generation,
            selected_car_id: binding.car_id,
        },
        sequence: SequenceRange {
            from_exclusive: base_sequence,
            to_inclusive: base_sequence,
        },
        snapshot: &snapshot,
    };
    let built = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&request, &[], &[])?;
    let manifest =
        request.signed_manifest_with_states_and_updates(&built, &[], &[], &cursor_key)?;
    store.finalize_import_snapshot_with_binding(
        &manifest,
        Sha256Digest::from_bytes([0xE0; 32]),
        &[],
        &request.binding,
    )?;

    Ok(PreparedEmptyEdgeBinding {
        schema_version: 2,
        data_dir,
        installation_id: binding.installation_id.clone(),
        lineage: binding.lineage.clone(),
        hub_id,
        source_id: source.source_id,
        vehicle_id: binding.vehicle_id,
        vin: binding.vin.clone(),
        car_id: binding.car_id,
        base_snapshot_id,
        base_sequence,
    })
}

pub fn prepare(root: &Path, port: u16) -> Result<PreparedFixture> {
    prepare_with_source_id(root, port, Uuid::new_v4())
}

pub fn prepare_viewer_r1(root: &Path, port: u16) -> Result<PreparedFixture> {
    prepare_with_scenario_and_source_id(root, port, FixtureScenario::ViewerR1, Uuid::new_v4())
}

/// Prepare a fixture with the exact source identity required by an isolated
/// interop lane. The caller must provide a non-nil UUID before any vehicle or
/// projection rows are created.
pub fn prepare_with_source_id(root: &Path, port: u16, source_id: Uuid) -> Result<PreparedFixture> {
    prepare_with_scenario_and_source_id(root, port, FixtureScenario::B1, source_id)
}

pub fn prepare_with_scenario_and_source_id(
    root: &Path,
    port: u16,
    scenario: FixtureScenario,
    source_id: Uuid,
) -> Result<PreparedFixture> {
    prepare_with_vehicle_identities(
        root,
        port,
        scenario,
        source_id,
        [
            FixtureVehicleIdentity {
                vehicle_id: DEFAULT_PRIMARY_VEHICLE_ID,
                vin: DEFAULT_PRIMARY_VIN,
                car_id: 9,
                name: "Interop – Árvíztűrő 🚗",
            },
            FixtureVehicleIdentity {
                vehicle_id: DEFAULT_SECONDARY_VEHICLE_ID,
                vin: DEFAULT_SECONDARY_VIN,
                car_id: 10,
                name: "Interop empty",
            },
        ],
    )
}

/// Prepare a synthetic fixture whose first vehicle exactly matches a sealed
/// Edge binding. The second, empty fixture vehicle remains distinct so normal
/// public-client discovery still has two vehicles.
pub fn prepare_with_scenario_source_and_primary_vehicle(
    root: &Path,
    port: u16,
    scenario: FixtureScenario,
    source_id: Uuid,
    primary_vehicle_id: Uuid,
    primary_vin: &str,
    primary_car_id: i64,
) -> Result<PreparedFixture> {
    if primary_vehicle_id.is_nil()
        || primary_vehicle_id == DEFAULT_PRIMARY_VEHICLE_ID
        || primary_car_id <= 0
        || primary_car_id == 9
        || primary_vin.len() != 17
        || !primary_vin.is_ascii()
        || !primary_vin.bytes().all(|byte| byte.is_ascii_alphanumeric())
        || primary_vin
            .bytes()
            .any(|byte| matches!(byte, b'I' | b'O' | b'Q'))
        || primary_vin == DEFAULT_PRIMARY_VIN
    {
        return Err(
            "fixture primary vehicle identity is invalid or conflicts with its secondary vehicle"
                .into(),
        );
    }
    prepare_with_vehicle_identities(
        root,
        port,
        scenario,
        source_id,
        [
            FixtureVehicleIdentity {
                vehicle_id: primary_vehicle_id,
                vin: primary_vin,
                car_id: primary_car_id,
                name: "Interop – Árvíztűrő 🚗",
            },
            FixtureVehicleIdentity {
                vehicle_id: DEFAULT_PRIMARY_VEHICLE_ID,
                vin: DEFAULT_PRIMARY_VIN,
                car_id: 9,
                name: "Interop empty",
            },
        ],
    )
}

fn prepare_with_vehicle_identities(
    root: &Path,
    port: u16,
    scenario: FixtureScenario,
    source_id: Uuid,
    vehicle_identities: [FixtureVehicleIdentity<'_>; 2],
) -> Result<PreparedFixture> {
    if !root.is_absolute() || port == 0 {
        return Err("fixture requires an absolute new directory and nonzero port".into());
    }
    if source_id.is_nil() {
        return Err("fixture source identity must be non-nil".into());
    }
    // Atomic create rejects existing paths, including symlinks. No production
    // directory can be accidentally overwritten by invoking this helper twice.
    fs::DirBuilder::new().mode(0o700).create(root)?;
    let data_dir = root.join("hub");
    let store = HubStore::initialize(&data_dir)?;
    let key = load_or_create_cursor_key(&data_dir)?;
    let mut source = store.register_source(
        &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
        OBSERVED_AT_MS,
    )?;
    if source.source_id != source_id {
        // This test-only seed has no source-owned rows yet. Rebind its one
        // freshly-created identity before any vehicle, projection, or
        // observation data refers to it, so an external synthetic producer
        // and this Hub fixture share one sealed source identity.
        let mut connection = store.open()?;
        let transaction = connection.transaction()?;
        transaction.execute_batch("PRAGMA defer_foreign_keys = ON")?;
        let identities = transaction.execute(
            "UPDATE source_identities SET source_id = ?1 WHERE source_id = ?2",
            rusqlite::params![source_id.to_string(), source.source_id.to_string()],
        )?;
        let sources = transaction.execute(
            "UPDATE sources SET source_id = ?1 WHERE source_id = ?2",
            rusqlite::params![source_id.to_string(), source.source_id.to_string()],
        )?;
        if identities != 1 || sources != 1 {
            return Err("fixture source identity rebind failed".into());
        }
        transaction.commit()?;
        source.source_id = source_id;
    }
    let hub_id = store.installation_id()?;
    let vehicle_ids = vehicle_identities.map(|identity| identity.vehicle_id);
    for (index, identity) in vehicle_identities.iter().enumerate() {
        let vehicle_id = identity.vehicle_id;
        let car_id = identity.car_id;
        let name = identity.name;
        let vin = identity.vin;
        let mut descriptor = VehicleDescriptor::new(source.source_id, car_id.to_string())
            .with_tesla_identity(Some(car_id), None);
        descriptor.display_name = Some(name.into());
        descriptor.vin = Some(vin.into());
        store.register_vehicle_with_id(&descriptor, OBSERVED_AT_MS, vehicle_id)?;
        let car: ProjectionCar = serde_json::from_value(json!({
            "id":car_id,"name":name,"model":"model3","vin":vin,"source_eid":car_id,"firmware_version":"2026.20"
        }))?;
        let mut drives = Vec::new();
        if index == 0 {
            let drive_schedule: Vec<(i64, i64)> = match scenario {
                FixtureScenario::B1 | FixtureScenario::PhysicalV3Public513 => vec![
                    (101, 500_000),
                    (102, 400_000),
                    (103, 300_000),
                    (104, 200_000),
                    (105, 200_000),
                ],
                FixtureScenario::ViewerR1 => (1001_i64..=1051_i64)
                    .map(|id| (id, (1052_i64 - id) * 60_000_i64))
                    .collect(),
            };
            for (id, offset) in drive_schedule {
                let drive: ProjectionDrive = serde_json::from_value(json!({
                    "id":id,"car_id":car_id,"start_date_ms":OBSERVED_AT_MS-offset,
                    "end_date_ms":OBSERVED_AT_MS-offset+60_000,
                    "distance_km":if id == 101 {None} else {Some(4.2)},
                    "duration_min":if id == 101 {None} else {Some(1)},
                    "start_soc":70,"end_soc":68,"inside_temp_avg":21.5,
                    "start_address":"Synthetic start","end_address":"Synthetic end"
                }))?;
                drives.push(drive);
            }
        }
        let charges: Vec<ProjectionCharge> = if index == 0 {
            vec![serde_json::from_value(json!({
                "id":201,"car_id":car_id,"start_date_ms":OBSERVED_AT_MS-3_600_000,
                "end_date_ms":OBSERVED_AT_MS-1_800_000,"charge_energy_added":12.5,
                "duration_min":30,"start_battery_level":40,"end_battery_level":65,
                "cost":3.25,"location_name":"Synthetic charging place","is_dc":false
            }))?]
        } else {
            vec![]
        };
        let snapshot = ProjectionSnapshot {
            cars: vec![car],
            drives,
            positions: vec![],
            charges,
            charge_samples: vec![],
        };
        store.persist_materialised_car_if_absent(vehicle_id, &snapshot.cars[0])?;
        // Publishing a transport pack does not materialise query rows. This
        // harness-only setup seeds the same catalogue tables as lifecycle
        // writes; it is HTTP/query evidence, not a TeslaMate import receipt.
        let mut connection = rusqlite::Connection::open(store.database_path())?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let transaction = connection.transaction()?;
        for drive in &snapshot.drives {
            transaction.execute(
                "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![vehicle_id.to_string(),drive.id,drive.car_id,serde_json::to_string(drive)?],
            )?;
        }
        for charge in &snapshot.charges {
            transaction.execute(
                "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![vehicle_id.to_string(),charge.id,charge.car_id,serde_json::to_string(charge)?],
            )?;
        }
        transaction.commit()?;
        drop(connection);
        // Keep the legacy base binding as the durable configured-vehicle fact
        // used by native serve preflight and Edge delivery. Schema 2.2 is a
        // second, newer publication, matching the production import path; it
        // supplements rather than replaces the binding catalogue.
        let legacy_sequence = store.next_full_snapshot_sequence(vehicle_id)?;
        let legacy_request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: ProjectionBinding {
                installation_id: hub_id,
                account_id: source.source_id,
                vehicle_id,
                generation: source.generation,
                selected_car_id: car_id,
            },
            sequence: SequenceRange {
                from_exclusive: legacy_sequence,
                to_inclusive: legacy_sequence,
            },
            snapshot: &snapshot,
        };
        let legacy_built = ProjectionPackWriter::new(store.packs_dir())
            .write_full_snapshot_with_states_and_updates(&legacy_request, &[], &[])?;
        let legacy_manifest = legacy_request.signed_manifest_with_states_and_updates(
            &legacy_built,
            &[],
            &[],
            &key,
        )?;
        store.finalize_import_snapshot_with_binding(
            &legacy_manifest,
            Sha256Digest::from_bytes([0x5A; 32]),
            &[],
            &legacy_request.binding,
        )?;

        let sequence = store.next_full_snapshot_sequence(vehicle_id)?;
        let schema_22_snapshot = schema_22_snapshot(car_id, name, vin)?;
        let request = ProjectionPackRequestV2_2 {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: ProjectionBinding {
                installation_id: hub_id,
                account_id: source.source_id,
                vehicle_id,
                generation: source.generation,
                selected_car_id: car_id,
            },
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &schema_22_snapshot,
        };
        let built =
            ProjectionPackWriter::new(store.packs_dir()).write_full_snapshot_2_2(&request)?;
        let manifest = sign_updates_schema_22_manifest(&request, &built, &key)?;
        let noop = sign_updates_schema_22_noop(
            &request.binding,
            request.snapshot_id,
            request.sequence.to_inclusive,
            &built.metadata.sha256.to_string(),
            &key,
        )?;
        publish_updates_schema_22(&store, &manifest, &noop)?;
        if index == 0 {
            store.append_observation(&ObservationInput {
                source_id:source.source_id,vehicle_id,observed_at_ms:OBSERVED_AT_MS,
                payload:json!({"record_type":"owner_api_vehicle_data_v1","source_vehicle_state":"online","display_name":name,
                    "vehicle_data":{"drive_state":{"speed":10,"active_route_miles_to_arrival":12.5},
                        "charge_state":{"battery_level":0,"est_battery_range":100.0,"charging_state":"Disconnected","scheduled_charging_start_time":1788570000_i64},
                        "climate_state":{"inside_temp":21.5,"outside_temp":null},
                        "vehicle_state":{"locked":true,"odometer":10000.0}}})
            },OBSERVED_AT_MS)?;
        }
    }
    #[cfg(feature = "interop-fixture")]
    let physical_v3_admission = if matches!(scenario, FixtureScenario::PhysicalV3Public513) {
        Some(prepare_physical_v3_public_admission(
            root,
            &store,
            &key,
            vehicle_ids[0],
        )?)
    } else {
        None
    };
    #[cfg(not(feature = "interop-fixture"))]
    let physical_v3_admission = None;

    // Native macOS preflight requires encrypted account material even with
    // collection disabled. These fixed test strings have no provider authority.
    if !matches!(scenario, FixtureScenario::PhysicalV3Public513) {
        let tokens = OwnerTokens::from_file_bytes(
            zeroize::Zeroizing::new(b"interop-not-a-tesla-access-token".to_vec()),
            zeroize::Zeroizing::new(b"interop-not-a-tesla-refresh-token".to_vec()),
        )?;
        let encryption_key = b"interop-local-synthetic-cloak-key";
        let (access, refresh) = encrypt_legacy_owner_tokens(encryption_key, &tokens)?;
        replace_key_and_tokens(
            &data_dir,
            &store,
            encryption_key,
            &TeslaMateLegacyTokenStore::imported(access, refresh)?,
        )?;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as i64;
    let invitation = store.create_pairing("interop fixture", now, now + 900_000)?;
    let invitation_path = root.join("invitation.json");
    write_private(
        &invitation_path,
        &serde_json::to_vec(&json!({
            "pairing_id":invitation.pairing_id,"secret":invitation.secret(),"expires_at_ms":now+900_000
        }))?,
    )?;
    store.checkpoint_catalogue_for_immutable_read()?;
    drop(store);
    let identity =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
    let certificate_path = root.join("server.pem");
    let private_key = root.join("server-key.pem");
    write_private(&certificate_path, identity.cert.pem().as_bytes())?;
    write_private(
        &private_key,
        identity.signing_key.serialize_pem().as_bytes(),
    )?;
    let endpoint = format!("https://127.0.0.1:{port}");
    let config_path = root.join("config.toml");
    let config = format!(
        "data_dir = {data:?}\nbind = {bind:?}\n[tls]\ncertificate_path = {cert:?}\nprivate_key_path = {key:?}\npublic_url = {endpoint:?}\n[collector]\ninterval_seconds = 0\nowner_api_base_url = \"https://127.0.0.1:1/\"\n[collector.legacy_auth]\nenabled = false\n[terrain]\nenabled = false\n[geocoder]\nenabled = false\n",
        data = data_dir.to_str().ok_or("non-UTF8 fixture path")?,
        bind = format!("127.0.0.1:{port}"),
        cert = certificate_path.to_str().ok_or("non-UTF8 fixture path")?,
        key = private_key.to_str().ok_or("non-UTF8 fixture path")?
    );
    write_private(&config_path, config.as_bytes())?;
    let prepared = PreparedFixture {
        schema_version: 1,
        config_path,
        certificate_path,
        invitation_path,
        hub_id,
        source_id: source.source_id,
        endpoint,
        vehicle_ids,
        physical_v3_admission,
    };
    write_private(
        &root.join("connection.json"),
        &serde_json::to_vec_pretty(&prepared)?,
    )?;
    Ok(prepared)
}

/// Build one deterministic, source-physical schema-2.2 fixture and admit it
/// through the private production marker. Its coordinates are synthetic and
/// deliberately have no address or geofence rows.
#[cfg(feature = "interop-fixture")]
fn prepare_physical_v3_public_admission(
    root: &Path,
    store: &HubStore,
    cursor_key: &teslatlas_hub::protocol::CursorKey,
    vehicle_id: Uuid,
) -> Result<PreparedPhysicalV3Admission> {
    const CAR_ID: i16 = 9;
    const DRIVE_ID: i32 = 301;
    const POSITION_IDS: [i32; 2] = [401, 402];
    const CHARGING_PROCESS_ID: i32 = 501;
    const CHARGE_IDS: [i32; 2] = [601, 602];
    const ROOT_ROWS: u64 = 3;
    const RELATION_ROWS: u64 = 6;

    let binding = store.v2_projection_binding(vehicle_id)?;
    if binding.selected_car_id != i64::from(CAR_ID) {
        return Err("physical fixture selected-car binding changed".into());
    }
    let mut stage = TeslaMateStage::create_physical_v3(
        root.join("physical-v3-source"),
        TeslaMateStageLimits {
            max_rows: ROOT_ROWS + RELATION_ROWS + u64::try_from(PHYSICAL_V3_UPDATE_COUNT)?,
            max_stage_bytes: 8 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )?;
    let settings = TeslaMateSettingsPhysicalV2_2 {
        id: 1,
        unit_of_length: ProjectionUnitOfLengthV2_2::Kilometers,
        unit_of_temperature: ProjectionUnitOfTemperatureV2_2::Celsius,
        unit_of_pressure: ProjectionUnitOfPressureV2_2::Bar,
        preferred_range: ProjectionPreferredRangeV2_2::Rated,
        base_url: None,
        grafana_url: None,
        language: "en".into(),
        theme_mode: "system".into(),
        inserted_at_pg_us: 1_000_000,
        updated_at_pg_us: 2_000_000,
    };
    let car_settings = TeslaMateCarSettingsPhysicalV2_2 {
        id: i64::from(CAR_ID),
        suspend_min: 21,
        suspend_after_idle_min: 15,
        req_not_unlocked: false,
        free_supercharging: false,
        use_streaming_api: true,
        enabled: true,
        lfp_battery: false,
    };
    let car = TeslaMateCarPhysicalV2_2 {
        id: CAR_ID,
        eid: 9,
        vid: 9,
        vin: Some(DEFAULT_PRIMARY_VIN.into()),
        name: Some("Interop physical V3".into()),
        model: Some("3".into()),
        efficiency: Some(0.153),
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        display_priority: 1,
        inserted_at_pg_us: 3_000_000,
        updated_at_pg_us: 4_000_000,
        settings_id: car_settings.id,
    };
    stage.insert(TeslaMateStageTable::GlobalSettings, settings.id, &settings)?;
    stage.insert(
        TeslaMateStageTable::CarSettings,
        car_settings.id,
        &car_settings,
    )?;
    stage.insert(TeslaMateStageTable::Cars, i64::from(car.id), &car)?;

    let drive = TeslaMateDrivePhysicalV2_2 {
        id: DRIVE_ID,
        car_id: CAR_ID,
        start_date_pg_us: 5_000_000,
        end_date_pg_us: Some(6_000_000),
        start_position_id: Some(POSITION_IDS[0]),
        end_position_id: Some(POSITION_IDS[1]),
        start_address_id: None,
        end_address_id: None,
        start_geofence_id: None,
        end_geofence_id: None,
        outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::Finite(125)),
        inside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::Finite(210)),
        speed_max: Some(42),
        power_max: Some(60),
        power_min: Some(-12),
        start_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(25_000)),
        end_ideal_range_km_e2: Some(ProjectionFixedNumericV2_2::Finite(24_500)),
        start_rated_range_km_e2: None,
        end_rated_range_km_e2: None,
        start_km: Some(ProjectionFloat64BitsV2_2(10_000.0_f64.to_bits())),
        end_km: Some(ProjectionFloat64BitsV2_2(10_004.2_f64.to_bits())),
        distance: Some(ProjectionFloat64BitsV2_2(4.2_f64.to_bits())),
        duration_min: Some(1),
        ascent: Some(10),
        descent: Some(8),
    };
    stage.insert(TeslaMateStageTable::Drives, i64::from(drive.id), &drive)?;
    for (index, id) in POSITION_IDS.into_iter().enumerate() {
        let position = TeslaMatePositionPhysicalV2_2 {
            id,
            car_id: CAR_ID,
            drive_id: Some(DRIVE_ID),
            date_pg_us: 5_000_000 + i64::try_from(index)? * 1_000_000,
            latitude_e6: ProjectionFixedNumericV2_2::Finite(i64::try_from(index)? * 1_000),
            longitude_e6: ProjectionFixedNumericV2_2::Finite(0),
            elevation: Some(100 + i16::try_from(index)?),
            speed: Some(20),
            power: Some(10),
            odometer: Some(ProjectionFloat64BitsV2_2(
                (10_000.0 + index as f64).to_bits(),
            )),
            ideal_battery_range_km_e2: None,
            est_battery_range_km_e2: None,
            rated_battery_range_km_e2: None,
            battery_level: Some(70 - i16::try_from(index)?),
            usable_battery_level: None,
            battery_heater: None,
            battery_heater_on: None,
            battery_heater_no_power: None,
            outside_temp_e1: None,
            inside_temp_e1: None,
            fan_status: None,
            driver_temp_setting_e1: None,
            passenger_temp_setting_e1: None,
            is_climate_on: None,
            is_rear_defroster_on: None,
            is_front_defroster_on: None,
            tpms_pressure_fl_e1: None,
            tpms_pressure_fr_e1: None,
            tpms_pressure_rl_e1: None,
            tpms_pressure_rr_e1: None,
        };
        stage.insert(TeslaMateStageTable::Positions, i64::from(id), &position)?;
    }
    let process = TeslaMateChargingProcessPhysicalV2_2 {
        id: CHARGING_PROCESS_ID,
        car_id: CAR_ID,
        position_id: POSITION_IDS[1],
        address_id: None,
        geofence_id: None,
        start_date_pg_us: 7_000_000,
        end_date_pg_us: Some(9_000_000),
        charge_energy_added_e2: Some(ProjectionFixedNumericV2_2::Finite(1_250)),
        charge_energy_used_e2: Some(ProjectionFixedNumericV2_2::Finite(1_300)),
        start_ideal_range_km_e2: None,
        end_ideal_range_km_e2: None,
        start_rated_range_km_e2: None,
        end_rated_range_km_e2: None,
        start_battery_level: Some(40),
        end_battery_level: Some(65),
        duration_min: Some(30),
        outside_temp_avg_e1: Some(ProjectionFixedNumericV2_2::Finite(130)),
        cost_e2: Some(ProjectionFixedNumericV2_2::Finite(325)),
    };
    stage.insert(
        TeslaMateStageTable::ChargingProcesses,
        i64::from(process.id),
        &process,
    )?;
    for (index, id) in CHARGE_IDS.into_iter().enumerate() {
        let charge = TeslaMateChargePhysicalV2_2 {
            id,
            charging_process_id: CHARGING_PROCESS_ID,
            date_pg_us: 7_000_000 + i64::try_from(index)? * 1_000_000,
            battery_heater: None,
            battery_heater_on: Some(false),
            battery_heater_no_power: None,
            battery_level: Some(40 + i16::try_from(index)? * 25),
            usable_battery_level: None,
            charge_energy_added_e2: ProjectionFixedNumericV2_2::Finite(
                100 + i64::try_from(index)? * 1_150,
            ),
            charger_actual_current: Some(16),
            charger_phases: Some(3),
            charger_pilot_current: Some(16),
            charger_power: 11,
            charger_voltage: Some(230),
            conn_charge_cable: Some("IEC".into()),
            fast_charger_present: Some(false),
            fast_charger_brand: None,
            fast_charger_type: None,
            ideal_battery_range_km_e2: ProjectionFixedNumericV2_2::Finite(20_000),
            rated_battery_range_km_e2: None,
            not_enough_power_to_heat: Some(false),
            outside_temp_e1: Some(ProjectionFixedNumericV2_2::Finite(130)),
        };
        stage.insert(TeslaMateStageTable::Charges, i64::from(id), &charge)?;
    }
    for id in 1..=PHYSICAL_V3_UPDATE_COUNT {
        let update = TeslaMateUpdatePhysicalV2_2 {
            id: 10_000 + id,
            car_id: CAR_ID,
            start_date_pg_us: 10_000_000 + i64::from(id) * 1_000_000,
            end_date_pg_us: None,
            version: Some(format!("fixture-{id:04}")),
        };
        stage.insert(TeslaMateStageTable::Updates, i64::from(update.id), &update)?;
    }
    let sealed = stage.seal()?;
    let head_sequence = store.next_full_snapshot_sequence(vehicle_id)?;
    let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        &stage,
        &ProjectionPackWriter::new(store.packs_dir()),
        binding,
        PHYSICAL_V3_SNAPSHOT_ID,
        SequenceRange {
            from_exclusive: head_sequence,
            to_inclusive: head_sequence,
        },
        cursor_key,
        TeslaMatePhysicalFragmentLimits {
            max_rows_per_chunk: 5,
            max_projected_json_bytes: 64 * 1024,
        },
    )?;
    stage.discard()?;
    if candidate.chunks.len() != 513
        || candidate.manifest.chunk_count != 513
        || candidate.logical_source_rows != sealed.row_count
    {
        return Err(format!(
            "physical fixture must produce exactly 513 chunks (actual {})",
            candidate.chunks.len()
        )
        .into());
    }
    let logical_source_rows = candidate.logical_source_rows;
    let admission = store.stage_interop_physical_v3_admission(candidate)?;
    if admission.chunk_count != 513 || admission.snapshot_id != PHYSICAL_V3_SNAPSHOT_ID {
        return Err("physical fixture admission receipt changed".into());
    }
    Ok(PreparedPhysicalV3Admission {
        profile_id: PHYSICAL_V3_PROFILE_ID,
        vehicle_id,
        snapshot_id: admission.snapshot_id,
        head_sequence: admission.head_sequence,
        receipt_id: admission.receipt_id,
        chunk_count: admission.chunk_count,
        logical_source_rows,
        drive_ids: [DRIVE_ID],
        position_ids: POSITION_IDS,
        charging_process_ids: [CHARGING_PROCESS_ID],
        charge_sample_ids: CHARGE_IDS,
        address_rows: 0,
        geofence_rows: 0,
        collector_enabled: false,
    })
}

/// Build the minimal physical schema-2.2 snapshot for one synthetic fixture
/// vehicle. Compatibility drive and charge JSON remains in the materialised
/// query tables above; inventing physical TeslaMate relationships for it would
/// make the fixture less honest, not more complete.
fn schema_22_snapshot(car_id: i64, name: &str, vin: &str) -> Result<ProjectionSnapshotV2_2> {
    let physical_car_id = i16::try_from(car_id)?;
    let mut snapshot = updates_snapshot_v2_2(Vec::new());
    let car = snapshot
        .cars
        .first_mut()
        .ok_or("schema 2.2 fixture car template is missing")?;
    car.id = physical_car_id;
    car.eid = car_id;
    car.vid = car_id;
    car.vin = Some(vin.into());
    car.name = Some(name.into());
    car.model = Some("model3".into());
    car.settings_id = car_id;
    snapshot
        .car_settings
        .first_mut()
        .ok_or("schema 2.2 fixture car settings template is missing")?
        .id = car_id;
    Ok(snapshot)
}

/// Add (once) or explicitly reactivate the deterministic third vehicle used by
/// development-only dynamic-entity acceptance. All publication goes through
/// normal HubStore and signed-artifact APIs.
#[cfg(feature = "interop-fixture")]
pub fn expose_dynamic_vehicle(root: &Path) -> Result<DynamicVehicleMutation> {
    let (store, source_id) = open_owned_fixture(root)?;
    let was_known = store.source_vehicle_key(DYNAMIC_VEHICLE_ID)?.is_some();
    if let Some(source_vehicle_key) = store.source_vehicle_key(DYNAMIC_VEHICLE_ID)? {
        if source_vehicle_key != DYNAMIC_CAR_ID.to_string() {
            return Err("dynamic fixture vehicle identity conflict".into());
        }
    }

    let source = store.register_source(
        &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
        OBSERVED_AT_MS,
    )?;
    if source.source_id != source_id {
        return Err("fixture source identity mismatch".into());
    }
    let mut descriptor = VehicleDescriptor::new(source.source_id, DYNAMIC_CAR_ID.to_string())
        .with_tesla_identity(Some(DYNAMIC_CAR_ID), None);
    descriptor.vin = Some(DYNAMIC_VIN.into());
    descriptor.display_name = Some("Interop dynamic third".into());
    store.register_vehicle_with_id(&descriptor, OBSERVED_AT_MS, DYNAMIC_VEHICLE_ID)?;

    let car: ProjectionCar = serde_json::from_value(json!({
        "id": DYNAMIC_CAR_ID,
        "name": "Interop dynamic third",
        "model": "model3",
        "vin": DYNAMIC_VIN,
        "source_eid": DYNAMIC_CAR_ID,
        "firmware_version": "2026.20"
    }))?;
    let snapshot = ProjectionSnapshot {
        cars: vec![car],
        drives: Vec::new(),
        positions: Vec::new(),
        charges: Vec::new(),
        charge_samples: Vec::new(),
    };
    store.persist_materialised_car_if_absent(DYNAMIC_VEHICLE_ID, &snapshot.cars[0])?;
    let key = load_or_create_cursor_key(&root.join("hub"))?;
    if let Some(manifest) = store.manifest_for_vehicle(DYNAMIC_VEHICLE_ID)?
        && manifest.schema == HUB_PROJECTION_SCHEMA_V3
    {
        teslatlas_hub::updates_delivery::schema_22_signed_artifacts(
            &store,
            DYNAMIC_VEHICLE_ID,
            &key,
        )?;
        let changed = store.reactivate_vehicle(DYNAMIC_VEHICLE_ID)?;
        store.checkpoint_catalogue_for_immutable_read()?;
        return Ok(DynamicVehicleMutation {
            status: if changed {
                "restored"
            } else {
                "already-exposed"
            },
            vehicle_id: DYNAMIC_VEHICLE_ID,
        });
    }
    let binding = ProjectionBinding {
        installation_id: store.installation_id()?,
        account_id: source.source_id,
        vehicle_id: DYNAMIC_VEHICLE_ID,
        generation: source.generation,
        selected_car_id: DYNAMIC_CAR_ID,
    };
    if store.manifest_for_vehicle(DYNAMIC_VEHICLE_ID)?.is_none() {
        let legacy_sequence = store.next_full_snapshot_sequence(DYNAMIC_VEHICLE_ID)?;
        let legacy_request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: binding.clone(),
            sequence: SequenceRange {
                from_exclusive: legacy_sequence,
                to_inclusive: legacy_sequence,
            },
            snapshot: &snapshot,
        };
        let legacy_built = ProjectionPackWriter::new(store.packs_dir())
            .write_full_snapshot_with_states_and_updates(&legacy_request, &[], &[])?;
        let legacy_manifest = legacy_request.signed_manifest_with_states_and_updates(
            &legacy_built,
            &[],
            &[],
            &key,
        )?;
        store.finalize_import_snapshot_with_binding(
            &legacy_manifest,
            Sha256Digest::from_bytes([0x33; 32]),
            &[],
            &binding,
        )?;
    }

    let sequence = store.next_full_snapshot_sequence(DYNAMIC_VEHICLE_ID)?;
    let schema_22_snapshot =
        schema_22_snapshot(DYNAMIC_CAR_ID, "Interop dynamic third", DYNAMIC_VIN)?;
    let request = ProjectionPackRequestV2_2 {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding,
        sequence: SequenceRange {
            from_exclusive: sequence,
            to_inclusive: sequence,
        },
        snapshot: &schema_22_snapshot,
    };
    let built = ProjectionPackWriter::new(store.packs_dir()).write_full_snapshot_2_2(&request)?;
    let manifest = sign_updates_schema_22_manifest(&request, &built, &key)?;
    let noop = sign_updates_schema_22_noop(
        &request.binding,
        request.snapshot_id,
        request.sequence.to_inclusive,
        &built.metadata.sha256.to_string(),
        &key,
    )?;
    publish_updates_schema_22(&store, &manifest, &noop)?;
    let changed = store.reactivate_vehicle(DYNAMIC_VEHICLE_ID)?;
    store.checkpoint_catalogue_for_immutable_read()?;
    Ok(DynamicVehicleMutation {
        status: if changed {
            "restored"
        } else if was_known {
            "repaired"
        } else {
            "exposed"
        },
        vehicle_id: DYNAMIC_VEHICLE_ID,
    })
}

#[cfg(feature = "interop-fixture")]
pub fn retire_dynamic_vehicle(root: &Path) -> Result<DynamicVehicleMutation> {
    let (store, _) = open_owned_fixture(root)?;
    ensure_dynamic_fixture_identity(&store)?;
    let changed = store.retire_vehicle(DYNAMIC_VEHICLE_ID, OBSERVED_AT_MS + 120_000)?;
    store.checkpoint_catalogue_for_immutable_read()?;
    Ok(DynamicVehicleMutation {
        status: if changed {
            "retired"
        } else {
            "already-retired"
        },
        vehicle_id: DYNAMIC_VEHICLE_ID,
    })
}

#[cfg(feature = "interop-fixture")]
pub fn restore_dynamic_vehicle(root: &Path) -> Result<DynamicVehicleMutation> {
    let (store, _) = open_owned_fixture(root)?;
    ensure_dynamic_fixture_identity(&store)?;
    let changed = store.reactivate_vehicle(DYNAMIC_VEHICLE_ID)?;
    store.checkpoint_catalogue_for_immutable_read()?;
    Ok(DynamicVehicleMutation {
        status: if changed {
            "restored"
        } else {
            "already-exposed"
        },
        vehicle_id: DYNAMIC_VEHICLE_ID,
    })
}

#[cfg(feature = "interop-fixture")]
fn ensure_dynamic_fixture_identity(store: &HubStore) -> Result<()> {
    match store.source_vehicle_key(DYNAMIC_VEHICLE_ID)? {
        Some(key) if key == DYNAMIC_CAR_ID.to_string() => Ok(()),
        _ => Err("dynamic fixture vehicle has not been exposed".into()),
    }
}

#[cfg(feature = "interop-fixture")]
fn open_owned_fixture(root: &Path) -> Result<(HubStore, Uuid)> {
    if !root.is_absolute() {
        return Err("owned private fixture directory required".into());
    }
    let metadata = root.symlink_metadata()?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("owned private fixture directory required".into());
    }
    let connection: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("connection.json"))?)?;
    let expected_hub_id = Uuid::parse_str(
        connection["hub_id"]
            .as_str()
            .ok_or("fixture connection hub identity required")?,
    )?;
    let expected_source_id = Uuid::parse_str(
        connection["source_id"]
            .as_str()
            .ok_or("fixture connection source identity required")?,
    )?;
    let store = HubStore::initialize(root.join("hub"))?;
    if store.installation_id()? != expected_hub_id {
        return Err("fixture installation identity mismatch".into());
    }
    Ok((store, expected_source_id))
}

/// Harness-only single later observation; never exposed through product HTTP.
/// Reopen the catalogue both before and after writing, and verify independent
/// scenario literals for the already-seeded charge before reporting success.
pub fn advance(root: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if !root.is_absolute()
        || root.symlink_metadata()?.file_type().is_symlink()
        || root.metadata()?.permissions().mode() & 0o077 != 0
    {
        return Err("owned private fixture directory required".into());
    }
    // Existing connection descriptor is an explicit fixture creation witness.
    let connection: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("connection.json"))?)?;
    let expected: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("scenario.json"))?)?;
    if !matches!(
        expected["name"].as_str(),
        Some("two-vehicles-five-drives") | Some("viewer-r1-51-drives")
    ) || expected["provenance"] != "synthetic-only"
    {
        return Err("synthetic scenario required".into());
    }
    write_private(&root.join("advance.started"), b"single owned update\n")?;
    let data_dir = root.join("hub");
    let store = HubStore::initialize(&data_dir)?;
    if serde_json::to_value(store.installation_id()?)? != connection["hub_id"] {
        return Err("fixture installation identity mismatch".into());
    }
    let source = store.register_source(
        &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
        OBSERVED_AT_MS,
    )?;
    let vehicle_id = Uuid::parse_str("11111111-1111-4111-8111-111111111111")?;
    store.append_observation(&ObservationInput {
        source_id: source.source_id, vehicle_id, observed_at_ms: 1_788_566_460_000,
        payload: json!({"record_type":"owner_api_vehicle_data_v1","source_vehicle_state":"online",
            "vehicle_data":{"charge_state":{"battery_level":1},
                "climate_state":{"inside_temp":22.5,"outside_temp":null}}})
    }, 1_788_566_460_000)?;
    store.checkpoint_catalogue_for_immutable_read()?;
    drop(store);
    let reopened = HubStore::initialize(&data_dir)?;
    let observations = reopened.current_observations_for_vehicle(vehicle_id)?;
    if observations.iter().map(|item| item.observed_at_ms).max() != Some(1_788_566_460_000) {
        return Err("later observation did not survive reopen".into());
    }
    let db = rusqlite::Connection::open_with_flags(
        reopened.database_path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let charge_json: String = db.query_row(
        "SELECT charge_json FROM materialised_charges WHERE vehicle_id=?1 AND charge_id=201",
        [vehicle_id.to_string()],
        |row| row.get(0),
    )?;
    let charge: serde_json::Value = serde_json::from_str(&charge_json)?;
    let mut preserved = serde_json::Map::new();
    for (field, value) in expected["charge"]
        .as_object()
        .ok_or("charge expectation required")?
    {
        if &charge[field] != value {
            return Err("seeded charge changed after reopen".into());
        }
        preserved.insert(field.clone(), charge[field].clone());
    }
    write_private(
        &root.join("advance.json"),
        &serde_json::to_vec(&json!({
        "status":"passed","observed_at_ms":1788566460000_i64,"charge":preserved,
        "provenance":"synthetic-owned-store-reopen"}))?,
    )?;
    Ok(())
}

fn write_private(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}
