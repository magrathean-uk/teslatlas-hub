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
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const OBSERVED_AT_MS: i64 = 1_788_566_400_000;

#[derive(Clone, Copy)]
pub enum FixtureScenario {
    B1,
    ViewerR1,
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
                FixtureScenario::B1 => vec![
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
    // Native macOS preflight requires encrypted account material even with
    // collection disabled. These fixed test strings have no provider authority.
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
    };
    write_private(
        &root.join("connection.json"),
        &serde_json::to_vec_pretty(&prepared)?,
    )?;
    Ok(prepared)
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
