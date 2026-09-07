// SPDX-License-Identifier: AGPL-3.0-only
//! Synthetic interoperability data. This module is linked only into tests and
//! the explicit fixture example, never the production Hub executable.

use serde::Serialize;
use serde_json::json;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use teslatlas_hub::{
    credentials::OwnerTokens,
    db::{
        HubStore, ObservationInput, SourceDescriptor, TeslaMateLegacyTokenStore, VehicleDescriptor,
    },
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionCharge, ProjectionDrive, ProjectionPackRequest,
        ProjectionPackWriter, ProjectionSnapshot,
    },
    protocol::{SequenceRange, Sha256Digest},
    teslamate_credentials::{load_or_create_cursor_key, replace_key_and_tokens},
    teslamate_token::encrypt_legacy_owner_tokens,
};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const OBSERVED_AT_MS: i64 = 1_788_566_400_000;

#[derive(Serialize)]
pub struct PreparedFixture {
    pub schema_version: u8,
    pub config_path: PathBuf,
    pub certificate_path: PathBuf,
    pub invitation_path: PathBuf,
    pub hub_id: Uuid,
    pub endpoint: String,
    pub vehicle_ids: [Uuid; 2],
}

pub fn prepare(root: &Path, port: u16) -> Result<PreparedFixture> {
    if !root.is_absolute() || port == 0 {
        return Err("fixture requires an absolute new directory and nonzero port".into());
    }
    // Atomic create rejects existing paths, including symlinks. No production
    // directory can be accidentally overwritten by invoking this helper twice.
    fs::DirBuilder::new().mode(0o700).create(root)?;
    let data_dir = root.join("hub");
    let store = HubStore::initialize(&data_dir)?;
    let key = load_or_create_cursor_key(&data_dir)?;
    let source = store.register_source(
        &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
        OBSERVED_AT_MS,
    )?;
    let hub_id = store.installation_id()?;
    let vehicle_ids = [
        Uuid::parse_str("11111111-1111-4111-8111-111111111111")?,
        Uuid::parse_str("22222222-2222-4222-8222-222222222222")?,
    ];
    for (index, vehicle_id) in vehicle_ids.iter().enumerate() {
        let car_id = 9 + index as i64;
        let name = if index == 0 {
            "Interop – Árvíztűrő 🚗"
        } else {
            "Interop empty"
        };
        let vin = if index == 0 {
            "5YJ3E1EA7KF000001"
        } else {
            "5YJ3E1EA7KF000002"
        };
        let mut descriptor = VehicleDescriptor::new(source.source_id, &car_id.to_string())
            .with_tesla_identity(Some(car_id), None);
        descriptor.display_name = Some(name.into());
        descriptor.vin = Some(vin.into());
        store.register_vehicle_with_id(&descriptor, OBSERVED_AT_MS, *vehicle_id)?;
        let car: ProjectionCar = serde_json::from_value(json!({
            "id":car_id,"name":name,"model":"model3","vin":vin,"source_eid":car_id,"firmware_version":"2026.20"
        }))?;
        let mut drives = Vec::new();
        if index == 0 {
            for (id, offset) in [
                (101, 500_000),
                (102, 400_000),
                (103, 300_000),
                (104, 200_000),
                (105, 200_000),
            ] {
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
        store.persist_materialised_car_if_absent(*vehicle_id, &snapshot.cars[0])?;
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
        let sequence = store.next_full_snapshot_sequence(*vehicle_id)?;
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: ProjectionBinding {
                installation_id: hub_id,
                account_id: source.source_id,
                vehicle_id: *vehicle_id,
                generation: source.generation,
                selected_car_id: car_id,
            },
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &snapshot,
        };
        let built = ProjectionPackWriter::new(store.packs_dir())
            .write_full_snapshot_with_states_and_updates(&request, &[], &[])?;
        let manifest = request.signed_manifest_with_states_and_updates(&built, &[], &[], &key)?;
        store.finalize_import_snapshot_with_binding(
            &manifest,
            Sha256Digest::from_bytes([0x5A; 32]),
            &[],
            &request.binding,
        )?;
        if index == 0 {
            store.append_observation(&ObservationInput {
                source_id:source.source_id,vehicle_id:*vehicle_id,observed_at_ms:OBSERVED_AT_MS,
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
        endpoint,
        vehicle_ids,
    };
    write_private(
        &root.join("connection.json"),
        &serde_json::to_vec_pretty(&prepared)?,
    )?;
    Ok(prepared)
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
    if expected["name"] != "two-vehicles-five-drives" || expected["provenance"] != "synthetic-only"
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
