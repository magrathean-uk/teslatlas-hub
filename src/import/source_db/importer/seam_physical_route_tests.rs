// SPDX-License-Identifier: AGPL-3.0-only

//! The negotiated 1.4 route carries the seam (M03-T21, DEC-117): the
//! physical target of a vehicle with imported history and Hub-collected data
//! is planned with the seam's decisions and the kept Hub rows before it is
//! published. Proven through the packs actually served to a fresh and to an
//! existing 1.4 client, against the planned lineage the store publishes.

use std::path::Path;

use super::seam_repair_packs::{capture_everything, client, hub_rows, served, window};
use super::*;
use crate::hub_pack::ProjectionFixedNumericV2_2;
use crate::source_db_projection::{
    SourceDbChargePhysicalV2_2, SourceDbChargingProcessPhysicalV2_2, SourceDbDrivePhysicalV2_2,
    SourceDbPositionPhysicalV2_2,
};
use crate::source_db_stage::{OwnedSourceDbStage, SourceDbStage, SourceDbStageLimits, SourceDbStageTable};

const PROFILE_1_4: &str = "hub-sync-v1@1.4.0";
const PG_EPOCH_US: i64 = 946_684_800_000_000;

fn pg_us(ms: i64) -> i64 {
    ms * 1_000 - PG_EPOCH_US
}

fn unix_ms(pg_us: i64) -> i64 {
    (pg_us + PG_EPOCH_US) / 1_000
}

/// A sealed physical stage of `history`, as the source capture leaves it:
/// the selected car's roots and its drives, positions, charging processes
/// and charges in their physical form.
pub(super) fn history_stage(root: &Path, name: &str, history: &SourceDbHistory) -> OwnedSourceDbStage {
    let mut stage = SourceDbStage::create_physical_v3(
        root.join(name),
        SourceDbStageLimits {
            max_rows: 4_096,
            max_stage_bytes: 8 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("physical stage");
    crate::sync::physical_fragments::tests::seed_roots(&mut stage);
    for drive in &history.drives {
        let row = SourceDbDrivePhysicalV2_2 {
            id: i32::try_from(drive.id).expect("drive id"),
            car_id: 1,
            start_date_pg_us: pg_us(drive.start_date_ms),
            end_date_pg_us: drive.end_date_ms.map(pg_us),
            start_position_id: None,
            end_position_id: None,
            start_address_id: None,
            end_address_id: None,
            start_geofence_id: None,
            end_geofence_id: None,
            outside_temp_avg_e1: None,
            inside_temp_avg_e1: None,
            speed_max: Some(50),
            power_max: None,
            power_min: None,
            start_ideal_range_km_e2: None,
            end_ideal_range_km_e2: None,
            start_rated_range_km_e2: None,
            end_rated_range_km_e2: None,
            start_km: None,
            end_km: None,
            distance: None,
            duration_min: Some(1),
            ascent: None,
            descent: None,
        };
        stage.insert(SourceDbStageTable::Drives, drive.id, &row).expect("drive");
    }
    for position in &history.positions {
        let row = SourceDbPositionPhysicalV2_2 {
            id: i32::try_from(position.id).expect("position id"),
            car_id: 1,
            drive_id: position.drive_id.map(|id| i32::try_from(id).expect("drive id")),
            date_pg_us: pg_us(position.date_ms),
            latitude_e6: ProjectionFixedNumericV2_2::Finite(47_500_000),
            longitude_e6: ProjectionFixedNumericV2_2::Finite(19_000_000),
            elevation: None,
            speed: None,
            power: None,
            odometer: None,
            ideal_battery_range_km_e2: None,
            est_battery_range_km_e2: None,
            rated_battery_range_km_e2: None,
            battery_level: None,
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
        stage.insert(SourceDbStageTable::Positions, position.id, &row).expect("position");
    }
    for process in &history.charging_processes {
        let row = SourceDbChargingProcessPhysicalV2_2 {
            id: i32::try_from(process.id).expect("process id"),
            car_id: 1,
            position_id: i32::try_from(process.position_id.unwrap_or(1)).expect("position id"),
            address_id: None,
            geofence_id: None,
            start_date_pg_us: pg_us(process.start_date_ms),
            end_date_pg_us: process.end_date_ms.map(pg_us),
            charge_energy_added_e2: Some(ProjectionFixedNumericV2_2::Finite(500)),
            charge_energy_used_e2: None,
            start_ideal_range_km_e2: None,
            end_ideal_range_km_e2: None,
            start_rated_range_km_e2: None,
            end_rated_range_km_e2: None,
            start_battery_level: Some(40),
            end_battery_level: Some(60),
            duration_min: None,
            outside_temp_avg_e1: None,
            cost_e2: None,
        };
        stage
            .insert(SourceDbStageTable::ChargingProcesses, process.id, &row)
            .expect("charging process");
    }
    for sample in &history.charges {
        let row = SourceDbChargePhysicalV2_2 {
            id: i32::try_from(sample.id).expect("sample id"),
            charging_process_id: i32::try_from(sample.charging_process_id).expect("process id"),
            date_pg_us: pg_us(sample.date_ms),
            battery_heater: None,
            battery_heater_on: None,
            battery_heater_no_power: None,
            battery_level: Some(50),
            usable_battery_level: None,
            charge_energy_added_e2: ProjectionFixedNumericV2_2::Finite(100),
            charger_actual_current: None,
            charger_phases: None,
            charger_pilot_current: None,
            charger_power: 11,
            charger_voltage: None,
            conn_charge_cable: None,
            fast_charger_present: None,
            fast_charger_brand: None,
            fast_charger_type: None,
            ideal_battery_range_km_e2: ProjectionFixedNumericV2_2::Finite(30_000),
            rated_battery_range_km_e2: None,
            not_enough_power_to_heat: None,
            outside_temp_e1: None,
        };
        stage.insert(SourceDbStageTable::Charges, sample.id, &row).expect("charge sample");
    }
    stage.seal().expect("seal");
    OwnedSourceDbStage::new(stage)
}

/// The physical half of one import, as the importer publishes it after the
/// planned lineage: `seam` is the pack plan that publication proved.
pub(super) fn publish_physical(
    fixture: &Fixture,
    stage: OwnedSourceDbStage,
    seam: Option<&crate::import::source_db::seam::SeamPackPlan>,
) -> Result<Option<crate::physical_publication::PhysicalV3Publication>, SourceDbImportError> {
    publish_physical_route(
        &fixture.store,
        &fixture.cursor_key,
        fixture.binding.clone(),
        Some(stage),
        &fixture.gate,
        seam,
        None,
    )
}

/// One later import of `history` on both routes: planned, written and
/// published as the importer does, then its physical stage.
fn import_both(fixture: &Fixture, root: &Path, name: &str, history: &SourceDbHistory, at_ms: i64) {
    let projected = project_vehicle(history, CAR).expect("projection");
    let mut planned = plan(fixture, &projected);
    let run_id = begin_run(fixture, at_ms);
    let capture = capture_everything(fixture, run_id, &projected);
    publish_captured(fixture, &projected, run_id, capture, &mut planned).expect("planned lineage");
    publish_physical(fixture, history_stage(root, name, history), Some(&planned.seam.packs))
        .expect("physical publication");
}

/// A paired 1.4 client's connection to the router.
pub(super) struct Paired {
    runtime: tokio::runtime::Runtime,
    app: axum::Router,
    bearer: String,
}

impl Paired {
    pub(super) fn get(&self, uri: &str) -> (axum::http::StatusCode, Vec<u8>) {
        served(&self.runtime, &self.app, &self.bearer, PROFILE_1_4, uri, None)
    }

    fn post(&self, uri: &str, body: serde_json::Value) -> (axum::http::StatusCode, Vec<u8>) {
        served(&self.runtime, &self.app, &self.bearer, PROFILE_1_4, uri, Some(body))
    }
}

pub(super) fn paired(fixture: &Fixture) -> Paired {
    let invitation = fixture
        .store
        .create_pairing("seam physical route", 0, i64::MAX)
        .expect("pairing");
    let now_ms = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("clock");
    let access = fixture
        .store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "reference client", now_ms)
        .expect("claim");
    Paired {
        runtime: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime"),
        app: crate::server::paired_router(fixture.store.clone(), &fixture.cursor_key),
        bearer: access.access_token.as_bearer().to_owned(),
    }
}

/// What a 1.4 client holds of one vehicle, read from the SQLite packs it is
/// served, in milliseconds: drives (id, start, end), positions (id, drive,
/// instant), charging processes (id, start, end) and charges (id, process,
/// instant).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct PhysicalMirror {
    pub(super) drives: BTreeMap<i64, (i64, i64)>,
    pub(super) positions: BTreeMap<i64, (Option<i64>, i64)>,
    pub(super) charges: BTreeMap<i64, (i64, Option<i64>)>,
    pub(super) samples: BTreeMap<i64, (i64, i64)>,
}

impl PhysicalMirror {
    /// Applies one served pack: its rows, then its removals.
    fn apply(&mut self, bytes: &[u8]) {
        let sqlite = zstd::stream::decode_all(bytes).expect("pack bytes");
        let temporary = crate::private_tempdir().expect("inspection");
        let file = temporary.path().join("pack.sqlite");
        std::fs::write(&file, sqlite).expect("inspection copy");
        let connection = Connection::open(&file).expect("pack database");
        let rows = |sql: &str| -> Vec<(i64, Option<i64>, Option<i64>)> {
            let mut statement = connection.prepare(sql).expect("rows");
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .expect("rows")
                .collect::<Result<_, _>>()
                .expect("rows")
        };
        for (id, start, end) in rows("SELECT id, start_date_pg_us, end_date_pg_us FROM drives") {
            let start = unix_ms(start.expect("start"));
            self.drives.insert(id, (start, end.map_or(start, unix_ms)));
        }
        for (id, drive, at) in rows("SELECT id, drive_id, date_pg_us FROM positions") {
            self.positions.insert(id, (drive, unix_ms(at.expect("date"))));
        }
        for (id, start, end) in
            rows("SELECT id, start_date_pg_us, end_date_pg_us FROM charging_processes")
        {
            self.charges.insert(id, (unix_ms(start.expect("start")), end.map(unix_ms)));
        }
        for (id, process, at) in rows("SELECT id, charging_process_id, date_pg_us FROM charges") {
            self.samples
                .insert(id, (process.expect("process"), unix_ms(at.expect("date"))));
        }
        let removals: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'tombstones')",
                [],
                |row| row.get(0),
            )
            .expect("schema");
        if removals {
            let mut statement = connection
                .prepare("SELECT table_name, entity_id FROM tombstones")
                .expect("tombstones");
            let removed: Vec<(String, i64)> = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("tombstones")
                .collect::<Result<_, _>>()
                .expect("tombstones");
            for (table, id) in removed {
                match table.as_str() {
                    "drives" => drop(self.drives.remove(&id)),
                    "positions" => drop(self.positions.remove(&id)),
                    "charging_processes" => drop(self.charges.remove(&id)),
                    "charges" => drop(self.samples.remove(&id)),
                    _ => {}
                }
            }
        }
    }
}

/// A 1.4 client: the App's flow, bootstrap by manifest and packs, later
/// `changes-since` with its 2.2 base.
pub(super) struct PhysicalClient {
    pub(super) mirror: PhysicalMirror,
    held: crate::db::PendingPhysicalV3Admission,
}

impl PhysicalClient {
    /// A fresh client's bootstrap, or the status the manifest was refused with.
    pub(super) fn bootstrap(
        env: &Paired,
        fixture: &Fixture,
    ) -> Result<Self, axum::http::StatusCode> {
        let vehicle_id = fixture.binding.vehicle_id;
        let (status, raw) = env.get(&format!("/v1/vehicles/{vehicle_id}/sync/manifest"));
        if status != axum::http::StatusCode::OK {
            return Err(status);
        }
        let manifest: serde_json::Value = serde_json::from_slice(&raw).expect("manifest");
        assert_eq!(manifest["schema_version"], "2.2", "the 1.4 client gets the physical head");
        let held = fixture
            .store
            .pending_physical_v3_admission_for_vehicle(vehicle_id)
            .expect("head")
            .expect("served head");
        assert_eq!(manifest["receipt_id"], held.receipt_id);
        let mut mirror = PhysicalMirror::default();
        for chunk in manifest["chunks"].as_array().expect("chunks") {
            mirror.apply(&fetch(env, chunk));
        }
        Ok(Self { mirror, held })
    }

    /// One `changes-since` from the head the client holds; returns the kind
    /// of answer (`no_op`, `physical_changed_set`, `rebase_required`) or the
    /// refusal's status.
    pub(super) fn sync(&mut self, env: &Paired, fixture: &Fixture) -> String {
        let held = &self.held;
        let signed = crate::manifest_signing::ManifestSigning::from_cursor_key(&fixture.cursor_key)
            .signed_physical_manifest_document(held)
            .expect("signed head");
        let (status, raw) = env.post(
            &format!("/v1/vehicles/{}/sync/changes-since", fixture.binding.vehicle_id),
            serde_json::json!({
                "base_receipt_id": held.receipt_id, "base_manifest_schema": "2.2",
                "from_sequence": held.head_sequence,
                "schema_version_range": {"minimum": "2.2", "maximum": "2.2"},
                "base_manifest_id": held.snapshot_id.to_string(),
                "base_manifest_sha256": Sha256Digest::of_bytes(&signed).to_string(),
                "source": {
                    "installation_id": held.installation_id, "account_id": held.account_id,
                    "vehicle_id": held.vehicle_id, "generation": held.manifest.generation,
                    "selected_car_id": held.selected_car_id,
                },
                "accepted_changed_set_formats": ["teslatlas-physical-v3-delta-v1"],
            }),
        );
        let answer: serde_json::Value = serde_json::from_slice(&raw).unwrap_or_default();
        let current = || {
            fixture
                .store
                .pending_physical_v3_admission_for_vehicle(fixture.binding.vehicle_id)
                .expect("head")
                .expect("served head")
        };
        match (status, answer["kind"].as_str()) {
            (axum::http::StatusCode::OK, Some("no_op")) => "no_op".to_owned(),
            (axum::http::StatusCode::OK, Some("physical_changed_set")) => {
                let target = current();
                assert_eq!(answer["target"]["receipt_id"], target.receipt_id);
                for chunk in answer["chunks"].as_array().expect("chunks") {
                    self.mirror.apply(&fetch(env, chunk));
                }
                self.held = target;
                "physical_changed_set".to_owned()
            }
            (axum::http::StatusCode::CONFLICT, Some("rebase_required")) => {
                let target = current();
                let mut mirror = PhysicalMirror::default();
                for chunk in answer["replacement"]["chunks"].as_array().expect("chunks") {
                    mirror.apply(&fetch(env, chunk));
                }
                self.mirror = mirror;
                self.held = target;
                "rebase_required".to_owned()
            }
            (status, _) => format!("{} {}", status.as_u16(), answer["code"]),
        }
    }
}

/// One served pack's bytes, checked against its digest.
fn fetch(env: &Paired, chunk: &serde_json::Value) -> Vec<u8> {
    let digest = chunk["pack"]["sha256"].as_str().expect("pack digest");
    let (status, bytes) = env.get(&format!("/v1/packs/sha256/{digest}.sqlite.zst"));
    assert_eq!(status, axum::http::StatusCode::OK, "pack {digest} is served");
    assert_eq!(Sha256Digest::of_bytes(&bytes).to_string(), digest);
    bytes
}

/// The current physical head's receipt.
fn physical_receipt(fixture: &Fixture) -> String {
    fixture
        .store
        .pending_physical_v3_admission_for_vehicle(fixture.binding.vehicle_id)
        .expect("head")
        .expect("physical head")
        .receipt_id
}

/// A vehicle with imported history and a window of Hub-collected drives and
/// positions: a fresh 1.4 client bootstraps and an existing one (on the
/// head it took before the window) takes `changes-since`, and both end with
/// exactly the drives and positions the planned lineage gives: the Hub's
/// drives H1 and H2, source drive 11 gone, source positions inside the
/// window gone and those outside under their winners, the Hub's positions
/// kept. An unchanged second import publishes nothing (`no_op`); a second
/// window then an import gives the existing client the next changed set.
#[test]
fn seam_physical_route_drives_and_positions_reach_fresh_and_existing_clients() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
        .expect("base head");
    let env = paired(&fixture);
    let mut existing = PhysicalClient::bootstrap(&env, &fixture).expect("served before the window");
    assert_eq!(existing.mirror.drives.keys().copied().collect::<Vec<_>>(), vec![10]);

    let (h1, _) = collect(&fixture);
    assert_eq!(
        PhysicalClient::bootstrap(&env, &fixture).err(),
        Some(axum::http::StatusCode::NOT_ACCEPTABLE),
        "before the next import the vehicle's head is not planned"
    );
    let history = successor_history();
    import_both(&fixture, root.path(), "after-window", &history, T + 3 * HOUR);

    let lineage = client(&fixture);
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("the planned head is served");
    assert!(fresh.mirror.drives.contains_key(&h1), "H1 won inside the window");
    assert!(!fresh.mirror.drives.contains_key(&11), "drive 11 lost to H1");
    assert!(fresh.mirror.drives.contains_key(&13), "drive 13 crosses the window's end and wins");
    assert!(!fresh.mirror.positions.contains_key(&1101), "inside coverage the Hub's rows stand");
    assert_eq!(fresh.mirror.drives, lineage.drives, "fresh client: the store's drives");
    assert_eq!(fresh.mirror.positions, lineage.positions, "fresh client: their positions");
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    assert_eq!(existing.mirror, fresh.mirror, "the existing client ends where a fresh one does");

    // A second import of the same source changes nothing.
    let receipt = physical_receipt(&fixture);
    import_both(&fixture, root.path(), "unchanged", &history, T + 4 * HOUR);
    assert_eq!(physical_receipt(&fixture), receipt, "an unchanged replay publishes nothing");
    assert_eq!(existing.sync(&env, &fixture), "no_op");

    // A second window: the Hub's drive H3 wins over source drive 12.
    let h3 = HUB_ID_BASE + 30;
    window(&fixture, 2, T + 2 * HOUR - 5 * MINUTE, T + 3 * HOUR);
    hub_rows(
        &fixture,
        &[(h3, T + 2 * HOUR, T + 2 * HOUR + 30 * MINUTE)],
        &[(HUB_ID_BASE + 30, h3, T + 2 * HOUR + 15 * MINUTE)],
    );
    import_both(&fixture, root.path(), "second-window", &history, T + 5 * HOUR);
    let lineage = client(&fixture);
    assert!(lineage.drives.contains_key(&h3) && !lineage.drives.contains_key(&12));
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("served after the second import");
    assert_eq!(fresh.mirror.drives, lineage.drives);
    assert_eq!(fresh.mirror.positions, lineage.positions);
    assert_eq!(existing.mirror, fresh.mirror);
}

/// The review's three-session case on the 1.4 route: the planned target is
/// worked out from the whole source each time, so source drive B's packed
/// position follows H, and B leaves the client, in the first changed set.
#[test]
fn seam_physical_route_transitive_overlap_reaches_the_existing_client_at_once() {
    let fixture = fixture();
    let root = crate::private_tempdir().expect("stage root");
    let (b, a, h) = (15, 16, HUB_ID_BASE + 15);
    let mut drives = base_history().drives;
    drives.push(source_drive(b, T - 15 * MINUTE, T + 5 * MINUTE));
    publish_physical(&fixture, history_stage(root.path(), "base", &base_history()), None)
        .expect("base head");
    let first = history(drives.clone(), vec![source_position(1501, Some(b), T - 10 * MINUTE)]);
    import_both(&fixture, root.path(), "first", &first, T - 30 * MINUTE);
    let env = paired(&fixture);
    let mut existing = PhysicalClient::bootstrap(&env, &fixture).expect("served before the window");
    assert_eq!(existing.mirror.positions.get(&1501), Some(&(Some(b), T - 10 * MINUTE)));

    window(&fixture, 1, T + 10 * MINUTE, T + HOUR);
    hub_rows(
        &fixture,
        &[(h, T + 10 * MINUTE, T + HOUR)],
        &[(HUB_ID_BASE + 1501, h, T + 30 * MINUTE)],
    );
    drives.push(source_drive(a, T, T + 20 * MINUTE));
    let second = history(
        drives,
        vec![
            source_position(1501, Some(b), T - 10 * MINUTE),
            source_position(1601, Some(a), T + 5 * MINUTE),
        ],
    );
    import_both(&fixture, root.path(), "second", &second, T + 3 * HOUR);
    assert_eq!(existing.sync(&env, &fixture), "physical_changed_set");
    assert!(!existing.mirror.drives.contains_key(&b) && !existing.mirror.drives.contains_key(&a));
    assert_eq!(existing.mirror.positions.get(&1501), Some(&(Some(h), T - 10 * MINUTE)));
    assert_eq!(existing.mirror.positions.get(&1601), Some(&(Some(h), T + 5 * MINUTE)));
    let fresh = PhysicalClient::bootstrap(&env, &fixture).expect("planned head");
    assert_eq!(existing.mirror, fresh.mirror);
    let lineage = client(&fixture);
    assert_eq!(fresh.mirror.drives, lineage.drives);
    assert_eq!(fresh.mirror.positions, lineage.positions);
}
