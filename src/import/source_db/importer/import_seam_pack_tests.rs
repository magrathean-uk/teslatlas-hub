// SPDX-License-Identifier: AGPL-3.0-only

//! The import seam ahead of the packs (design M03-SEAM-R3, section 6,
//! M03-T16): the importer applies the plan before it writes packs, the
//! packs carry the planned source rows only, the Hub's own rows and the
//! seam's tombstones travel as sync, and every head a client can read holds
//! each session once.

use std::collections::BTreeMap;

use rusqlite::{Connection, params};

use super::*;
use crate::db::hub_ids::HUB_ID_BASE;
use crate::import::source_db::seam::SEAM_SLACK_MS;
use crate::source_db_projection::{SourceDbDrive, SourceDbPosition};

const T: i64 = 1_800_000_000_000;
const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const CAR: i64 = 1;

struct Fixture {
    _temporary: tempfile::TempDir,
    store: HubStore,
    binding: ProjectionBinding,
    car: ProjectionCar,
    gate: PublicationGate,
    cursor_key: CursorKey,
}

fn source_car() -> SourceDbCar {
    SourceDbCar {
        id: CAR,
        eid: 88,
        vid: Some(99),
        vin: Some("5YJTESTVIN1234567".into()),
        name: Some("Road car".into()),
        model: Some("Model 3".into()),
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        efficiency_wh_per_km: Some(0.145),
        settings: Default::default(),
    }
}

fn source_drive(id: i64, start_ms: i64, end_ms: i64) -> SourceDbDrive {
    SourceDbDrive {
        id,
        car_id: CAR,
        start_date_ms: start_ms,
        end_date_ms: Some(end_ms),
        outside_temp_avg: None,
        speed_max: Some(50),
        power_max: None,
        power_min: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_rated_range_km: Some(300.0),
        end_rated_range_km: Some(280.0),
        start_km: Some(10.0),
        end_km: Some(20.0),
        distance_km: Some(10.0),
        duration_min: Some(1),
        start_address_id: None,
        end_address_id: None,
        start_geofence_id: None,
        end_geofence_id: None,
        start_position_id: None,
        end_position_id: None,
        ascent: None,
        descent: None,
        inside_temp_avg: None,
    }
}

fn source_position(id: i64, drive_id: Option<i64>, date_ms: i64) -> SourceDbPosition {
    serde_json::from_value(serde_json::json!({
        "id": id, "car_id": CAR, "drive_id": drive_id, "date_ms": date_ms,
        "latitude": 47.5, "longitude": 19.0
    }))
    .expect("source position")
}

fn history(drives: Vec<SourceDbDrive>, positions: Vec<SourceDbPosition>) -> SourceDbHistory {
    SourceDbHistory {
        cars: vec![source_car()],
        drives,
        positions,
        charging_processes: vec![],
        charges: vec![],
        addresses: vec![],
        geofences: vec![],
        states: vec![],
        updates: vec![],
    }
}

/// The base: one drive long before any window.
fn base_history() -> SourceDbHistory {
    history(vec![source_drive(10, T - 5 * HOUR, T - 5 * HOUR + 10 * MINUTE)], vec![])
}

/// The source after the Hub's window: drive 11 is the Hub's drive inside
/// the window, drive 12 lies after it, drive 13 starts inside the window
/// and crosses its end.
fn successor_history() -> SourceDbHistory {
    let mut drives = base_history().drives;
    drives.push(source_drive(11, T + 10 * MINUTE, T + 20 * MINUTE));
    drives.push(source_drive(12, T + 2 * HOUR, T + 2 * HOUR + 10 * MINUTE));
    drives.push(source_drive(13, T + 50 * MINUTE, T + 75 * MINUTE));
    history(
        drives,
        vec![
            source_position(1101, Some(11), T + 12 * MINUTE),
            source_position(1201, Some(12), T + 2 * HOUR + 5 * MINUTE),
            source_position(1301, Some(13), T + 52 * MINUTE),
            source_position(1302, Some(13), T + 70 * MINUTE),
        ],
    )
}

fn state_limits() -> SourceDbProjectionStateLimits {
    SourceDbProjectionStateLimits {
        max_rows: 64,
        max_state_bytes: 256 * 1024,
        max_changed_payload_bytes: 256 * 1024,
        minimum_free_bytes: 0,
    }
}

fn begin_run(fixture: &Fixture, at_ms: i64) -> Uuid {
    let binding = &fixture.binding;
    let run_id = fixture
        .store
        .begin_import_generation(binding.account_id, binding.vehicle_id, CAR, at_ms)
        .expect("generation");
    fixture
        .store
        .stage_import_generation_session(
            run_id,
            &SourceDbOpenSession {
                car_id: CAR,
                ..Default::default()
            },
        )
        .expect("session");
    run_id
}

fn capture(
    fixture: &Fixture,
    run_id: Uuid,
    successor: bool,
    projected: &SourceDbProjection,
) -> SourceDbProjectionStateCapture {
    let binding = &fixture.binding;
    let mut capture = direct_projection_state_capture(
        &fixture.store,
        &fixture.gate,
        run_id,
        binding.vehicle_id,
        binding.account_id,
        CAR,
        successor,
        state_limits(),
    )
    .expect("capture");
    for car in &projected.snapshot.cars {
        capture.record_car(car).expect("car");
    }
    for drive in &projected.snapshot.drives {
        capture.record_drive(drive).expect("drive");
    }
    for position in &projected.snapshot.positions {
        capture.record_position(position).expect("position");
    }
    capture.seal().expect("seal");
    capture
}

/// A store with a published direct base of [`base_history`].
fn fixture() -> Fixture {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new(crate::db::IMPORT_SOURCE_KIND, "import-seam-packs"),
            T,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "import-seam-car"), T)
        .expect("vehicle");
    let binding = ProjectionBinding {
        installation_id: store.installation_id().expect("installation"),
        account_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: source.generation,
        selected_car_id: CAR,
    };
    let gate = store.try_acquire_publication_gate().expect("gate");
    let projected = project_vehicle(&base_history(), CAR).expect("base projection");
    let car = projected.snapshot.cars[0].clone();
    let fixture = Fixture {
        _temporary: temporary,
        store,
        binding,
        car,
        gate,
        cursor_key: CursorKey::from_bytes([41; 32]),
    };
    let run_id = begin_run(&fixture, T - HOUR);
    let state = capture(&fixture, run_id, false, &projected).into_state();
    let sequence = fixture
        .store
        .reserve_next_full_snapshot_sequence(&fixture.gate, fixture.binding.vehicle_id)
        .expect("base sequence");
    let request = ProjectionPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        binding: fixture.binding.clone(),
        sequence: SequenceRange {
            from_exclusive: sequence,
            to_inclusive: sequence,
        },
        snapshot: &projected.snapshot,
    };
    let pack = ProjectionPackWriter::new(fixture.store.packs_dir())
        .write_full_snapshot_with_states_and_updates(&request, &[], &[])
        .expect("base pack");
    let manifest = request
        .signed_manifest_with_states_and_updates(&pack, &[], &[], &fixture.cursor_key)
        .expect("base manifest");
    fixture
        .store
        .finalize_import_generation_with_projection_state_and_materialisation(
            run_id,
            fixture.binding.account_id,
            fixture.binding.vehicle_id,
            CAR,
            T - HOUR,
            &manifest,
            Sha256Digest::of_bytes(b"import-seam-base"),
            &[],
            &fixture.binding,
            &state,
            &fixture.car,
            &projected.snapshot.drives,
            false,
        )
        .expect("base");
    fixture
}

/// The Hub's window `[T, T + 1 h)`, handed back, with its drives and
/// positions as the collector stores and journals them, published to
/// clients as live sync: drive H1 is the source's drive 11, drive H2 the
/// part of the source's drive 13 inside the window.
fn collect(fixture: &Fixture) -> (i64, i64) {
    let hub_drives = collect_unpublished(fixture);
    publish_sync_mutations_ahead_of_packs(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        fixture.binding.vehicle_id,
    )
    .expect("live sync of the Hub's collection");
    hub_drives
}

/// The Hub's window of [`collect`], stored and journalled but not yet
/// published to clients: what the collector writes before its publication.
fn collect_unpublished(fixture: &Fixture) -> (i64, i64) {
    let (h1, h2) = (HUB_ID_BASE + 1, HUB_ID_BASE + 2);
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let template = projected.snapshot.drives[0].clone();
    let hub_drive = |id: i64, start_ms: i64, end_ms: i64| {
        let mut drive = template.clone();
        drive.id = id;
        drive.start_date_ms = start_ms;
        drive.end_date_ms = end_ms;
        drive
    };
    let hub_position = |id: i64, drive_id: i64, date_ms: i64| {
        let mut position =
            crate::lifecycle::imported_position(&source_position(id, Some(drive_id), date_ms));
        position.drive_id = Some(drive_id);
        position
    };
    let vehicle = fixture.binding.vehicle_id.to_string();
    let mut connection = fixture.store.open().expect("catalogue");
    let transaction = connection.transaction().expect("transaction");
    transaction
        .execute(
            "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
             VALUES (1, 'grant-1', ?1, ?2, '[]', 'handback')",
            params![T, T + HOUR],
        )
        .expect("coverage");
    let journal = |entity: &str, id: i64, json: &str| {
        let revision: i64 = transaction
            .query_row(
                "INSERT INTO sync_mutation_sequences(vehicle_id, next_revision) VALUES (?1, 2)
                 ON CONFLICT(vehicle_id) DO UPDATE SET next_revision = next_revision + 1
                 RETURNING next_revision - 1",
                params![vehicle.as_str()],
                |row| row.get(0),
            )
            .expect("revision");
        transaction
            .execute(
                "INSERT INTO sync_mutations(vehicle_id, revision, entity, entity_id, car_id,
                    operation, payload_json, published, claimed_until_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'upsert', ?6, 0, 0)",
                params![vehicle.as_str(), revision, entity, id, CAR, json],
            )
            .expect("mutation");
    };
    for drive in [
        hub_drive(h1, T + 10 * MINUTE, T + 20 * MINUTE),
        hub_drive(h2, T + 50 * MINUTE, T + 60 * MINUTE),
    ] {
        let json = serde_json::to_string(&drive).expect("drive json");
        transaction
            .execute(
                "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![vehicle.as_str(), drive.id, CAR, json],
            )
            .expect("Hub drive");
        journal("drive", drive.id, &json);
    }
    for position in [
        hub_position(HUB_ID_BASE + 1, h1, T + 15 * MINUTE),
        hub_position(HUB_ID_BASE + 2, h2, T + 55 * MINUTE),
    ] {
        let json = serde_json::to_string(&position).expect("position json");
        transaction
            .execute(
                "INSERT INTO materialised_positions(vehicle_id, position_id, drive_id, car_id, position_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![vehicle.as_str(), position.id, position.drive_id, CAR, json],
            )
            .expect("Hub position");
        journal("position", position.id, &json);
    }
    transaction.commit().expect("commit");
    (h1, h2)
}

/// The planned successor of `projected`, as far as its plan.
fn plan(fixture: &Fixture, projected: &SourceDbProjection) -> PlannedDirectSuccessor {
    plan_direct_successor(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        fixture.binding.vehicle_id,
        CAR,
        &projected.snapshot.drives,
        &projected.snapshot.charges,
    )
    .expect("plan ahead of the packs")
}

/// Writes and publishes the planned successor of `projected`.
fn publish(
    fixture: &Fixture,
    projected: &SourceDbProjection,
    run_id: Uuid,
    planned: &mut PlannedDirectSuccessor,
) -> Result<PublishedDirectSuccessor, SourceDbImportError> {
    let capture = capture(fixture, run_id, true, projected);
    publish_captured(fixture, projected, run_id, capture, planned)
}

/// Writes and publishes the planned successor of `projected` from its
/// sealed `capture`.
fn publish_captured(
    fixture: &Fixture,
    projected: &SourceDbProjection,
    run_id: Uuid,
    capture: SourceDbProjectionStateCapture,
    planned: &mut PlannedDirectSuccessor,
) -> Result<PublishedDirectSuccessor, SourceDbImportError> {
    let base_snapshot_id = fixture
        .store
        .v2_head(fixture.binding.vehicle_id)
        .expect("head")
        .expect("base")
        .0;
    publish_planned_direct_successor(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        PlannedSuccessorRun {
            run_id,
            source_id: fixture.binding.account_id,
            imported_at_ms: T + 3 * HOUR,
            base_snapshot_id,
            minimum_free_bytes: 0,
            fingerprint: Sha256Digest::of_bytes(format!("successor-{run_id}").as_bytes()),
            geofences: &[],
            materialised_car: &fixture.car,
            materialised_drives: &projected.snapshot.drives,
            materialised_charges: &projected.snapshot.charges,
        },
        &fixture.binding,
        &fixture.car,
        capture,
        planned,
    )
}

/// One pack's drives (id, start, end), positions (id, drive, instant)
/// and tombstones (entity, id).
#[derive(Debug, Default)]
struct PackRows {
    drives: Vec<(i64, i64, i64)>,
    positions: Vec<(i64, Option<i64>, i64)>,
    tombstones: Vec<(String, i64)>,
}

fn pack_rows(store: &HubStore, digest: Sha256Digest) -> PackRows {
    let path = ProjectionPackWriter::new(store.packs_dir()).content_path(digest);
    let sqlite =
        zstd::stream::decode_all(std::fs::File::open(&path).expect("pack")).expect("pack bytes");
    let temporary = crate::private_tempdir().expect("inspection");
    let file = temporary.path().join("pack.sqlite");
    std::fs::write(&file, sqlite).expect("inspection copy");
    let connection = Connection::open(&file).expect("pack database");
    let table = |name: &str| -> bool {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                params![name],
                |row| row.get(0),
            )
            .expect("schema")
    };
    let mut rows = PackRows::default();
    if table("drives") {
        let mut statement = connection
            .prepare("SELECT id, start_date_ms, end_date_ms FROM drives ORDER BY id")
            .expect("drives");
        rows.drives = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("drive rows")
            .collect::<Result<_, _>>()
            .expect("drive rows");
    }
    if table("positions") {
        let mut statement = connection
            .prepare("SELECT id, drive_id, date_ms FROM positions ORDER BY id")
            .expect("positions");
        rows.positions = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("position rows")
            .collect::<Result<_, _>>()
            .expect("position rows");
    }
    if table("tombstones") {
        let mut statement = connection
            .prepare("SELECT entity, entity_id FROM tombstones ORDER BY entity, entity_id")
            .expect("tombstones");
        rows.tombstones = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("tombstone rows")
            .collect::<Result<_, _>>()
            .expect("tombstone rows");
    }
    rows
}

/// Every advertised head of the vehicle in order: the base's packs, then
/// one entry per delta, each with its head sequence.
fn heads(store: &HubStore, vehicle_id: Uuid) -> Vec<(u64, PackRows)> {
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("published lineage");
    let mut base = PackRows::default();
    for pack in &lineage.base.packs {
        let rows = pack_rows(store, pack.sha256);
        base.drives.extend(rows.drives);
        base.positions.extend(rows.positions);
        base.tombstones.extend(rows.tombstones);
    }
    let mut heads = vec![(lineage.base.sequence, base)];
    for delta in &lineage.deltas {
        heads.push((delta.to_sequence, pack_rows(store, delta.pack.sha256)));
    }
    heads
}

/// A client's mirror of one vehicle: drives and positions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Client {
    drives: BTreeMap<i64, (i64, i64)>,
    positions: BTreeMap<i64, (Option<i64>, i64)>,
}

impl Client {
    fn apply(&mut self, rows: &PackRows) {
        for (id, start, end) in &rows.drives {
            self.drives.insert(*id, (*start, *end));
        }
        for (id, drive, at) in &rows.positions {
            self.positions.insert(*id, (*drive, *at));
        }
        for (entity, id) in &rows.tombstones {
            if entity == "drive" {
                self.drives.remove(id);
            } else if entity == "position" {
                self.positions.remove(id);
            }
        }
    }

    /// No two drives overlap by more than the seam's slack: the client
    /// holds each session once.
    fn assert_each_session_once(&self, at: &str) {
        let drives: Vec<_> = self.drives.iter().collect();
        for (index, (a, (a_start, a_end))) in drives.iter().enumerate() {
            for (b, (b_start, b_end)) in &drives[index + 1..] {
                let overlap = a_end.min(b_end) - a_start.max(b_start);
                assert!(
                    overlap <= SEAM_SLACK_MS,
                    "{at}: drives {a} and {b} are the same session"
                );
            }
        }
    }
}

fn store_drives(store: &HubStore, vehicle_id: Uuid) -> BTreeMap<i64, (i64, i64)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT drive_id,
                    CAST(json_extract(drive_json, '$.start_date_ms') AS INTEGER),
                    CAST(json_extract(drive_json, '$.end_date_ms') AS INTEGER)
               FROM materialised_drives WHERE vehicle_id = ?1",
        )
        .expect("drives");
    statement
        .query_map(params![vehicle_id.to_string()], |row| {
            Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
        })
        .expect("drive rows")
        .collect::<Result<_, _>>()
        .expect("drive rows")
}

fn store_hub_positions(store: &HubStore, vehicle_id: Uuid) -> BTreeMap<i64, (Option<i64>, i64)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT position_id, drive_id,
                    CAST(json_extract(position_json, '$.date_ms') AS INTEGER)
               FROM materialised_positions WHERE vehicle_id = ?1 AND position_id >= ?2",
        )
        .expect("positions");
    statement
        .query_map(params![vehicle_id.to_string(), HUB_ID_BASE], |row| {
            Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
        })
        .expect("position rows")
        .collect::<Result<_, _>>()
        .expect("position rows")
}

/// The head sequences of the vehicle's live deltas: the Hub's sync, the
/// seam's among it, which publication makes visible with the packs that
/// carry its replacements (M03-T19).
fn live_heads(store: &HubStore, vehicle_id: Uuid) -> HashSet<u64> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare("SELECT to_sequence FROM sync_live_delta_spans WHERE vehicle_id = ?1")
        .expect("live spans");
    statement
        .query_map(params![vehicle_id.to_string()], |row| row.get::<_, i64>(0))
        .expect("live span rows")
        .map(|row| u64::try_from(row.expect("live span row")).expect("sequence"))
        .collect()
}

/// The import's own heads from `before` on: every head that is not live
/// sync.
fn import_heads(store: &HubStore, vehicle_id: Uuid, before: usize) -> Vec<(u64, PackRows)> {
    let live = live_heads(store, vehicle_id);
    heads(store, vehicle_id)
        .split_off(before)
        .into_iter()
        .filter(|(sequence, _)| !live.contains(sequence))
        .collect()
}

fn mutation_count(store: &HubStore, vehicle_id: Uuid) -> i64 {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("mutations")
}

/// The source's drive 11 lost to the Hub's H1 inside the window: it is in
/// no pack, and neither is its position inside coverage.
#[test]
fn import_seam_skipped_source_drive_is_not_in_the_pack() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let (h1, _) = collect(&fixture);
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let mut planned = plan(&fixture, &projected);
    let before = heads(&fixture.store, vehicle_id).len();
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    publish(&fixture, &projected, run_id, &mut planned).expect("publication");
    let import = import_heads(&fixture.store, vehicle_id, before);
    assert!(!import.is_empty(), "the import published its packs");
    let packed: Vec<i64> = import
        .iter()
        .flat_map(|(_, rows)| rows.drives.iter().map(|row| row.0))
        .collect();
    let positions: Vec<i64> = import
        .iter()
        .flat_map(|(_, rows)| rows.positions.iter().map(|row| row.0))
        .collect();
    assert_eq!(packed, vec![12, 13], "drive 11 is skipped; 10 is unchanged");
    assert_eq!(positions, vec![1201, 1302], "positions inside coverage stay the Hub's");
    assert!(store_drives(&fixture.store, vehicle_id).contains_key(&h1));
    assert!(!store_drives(&fixture.store, vehicle_id).contains_key(&11));
}

/// Every changed source row the publication read is packed or counted
/// as seam-skipped.
#[test]
fn import_seam_parity_accounts_for_seam_skipped_rows() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(&fixture);
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let mut planned = plan(&fixture, &projected);
    let before = heads(&fixture.store, vehicle_id).len();
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    publish(&fixture, &projected, run_id, &mut planned).expect("publication");
    let import = import_heads(&fixture.store, vehicle_id, before);
    let packed_drives: u64 = import.iter().map(|(_, rows)| rows.drives.len() as u64).sum();
    let packed_positions: u64 = import
        .iter()
        .map(|(_, rows)| rows.positions.len() as u64)
        .sum();
    let skipped = planned.seam.skipped;
    assert_eq!(
        skipped,
        SourceDbSeamSkipped {
            drives: 1,
            positions: 2,
            ..SourceDbSeamSkipped::default()
        }
    );
    // Changed source rows: drives 11, 12, 13 and positions 1101, 1201,
    // 1301, 1302.
    assert_eq!(packed_drives + skipped.drives, 3);
    assert_eq!(packed_positions + skipped.positions, 4);
    assert_eq!(skipped.total(), 3);
}

/// Sync carries the Hub's rows and the seam's tombstones, packs carry
/// source rows only, and together they give the client the store's
/// drives and the Hub's positions with the same ids.
#[test]
fn import_seam_sync_ids_against_pack_ids() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let (h1, h2) = collect(&fixture);
    let collected = heads(&fixture.store, vehicle_id).len();
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let mut planned = plan(&fixture, &projected);
    let planned_heads = heads(&fixture.store, vehicle_id).len();
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    publish(&fixture, &projected, run_id, &mut planned).expect("publication");
    let all = heads(&fixture.store, vehicle_id);
    assert_eq!(planned_heads, collected, "nothing goes ahead of the publication");
    let live = live_heads(&fixture.store, vehicle_id);
    let (seam_sync, packs): (Vec<_>, Vec<_>) = all[collected..]
        .iter()
        .partition(|(sequence, _)| live.contains(sequence));
    assert!(!seam_sync.is_empty(), "the seam's changes go out as live sync");
    assert!(
        seam_sync.iter().all(|(seam, _)| packs.iter().all(|(pack, _)| seam < pack)),
        "the seam's sync comes ahead of the packs, under the same publication"
    );

    // Sync: Hub-range upserts and the tombstone of the Hub drive the
    // source replaced; never a source row.
    for (_, rows) in &seam_sync {
        assert!(rows.drives.iter().all(|row| row.0 >= HUB_ID_BASE));
        assert!(rows.positions.iter().all(|row| row.0 >= HUB_ID_BASE));
    }
    let sync_tombstones: Vec<_> = seam_sync
        .iter()
        .flat_map(|(_, rows)| rows.tombstones.clone())
        .collect();
    assert_eq!(sync_tombstones, vec![("drive".to_owned(), h2)]);
    let moved: Vec<_> = seam_sync
        .iter()
        .flat_map(|(_, rows)| rows.positions.clone())
        .collect();
    assert_eq!(moved, vec![(HUB_ID_BASE + 2, Some(13), T + 55 * MINUTE)]);
    // Packs: source rows only, and no tombstone of a Hub row.
    for (_, rows) in &packs {
        assert!(rows.drives.iter().all(|row| row.0 < HUB_ID_BASE));
        assert!(rows.positions.iter().all(|row| row.0 < HUB_ID_BASE));
        assert!(rows.tombstones.iter().all(|row| row.1 < HUB_ID_BASE));
    }

    // The client that applied every head holds what the store holds.
    let mut client = Client::default();
    for (_, rows) in &all {
        client.apply(rows);
    }
    assert_eq!(client.drives, store_drives(&fixture.store, vehicle_id));
    let mut hub_positions = client.positions.clone();
    hub_positions.retain(|id, _| *id >= HUB_ID_BASE);
    assert_eq!(hub_positions, store_hub_positions(&fixture.store, vehicle_id));
    assert_eq!(
        client.drives.keys().copied().collect::<Vec<_>>(),
        vec![10, 12, 13, h1]
    );
    assert!(
        !fixture
            .store
            .has_unpublished_sync_mutations(vehicle_id)
            .expect("journal"),
        "nothing the packs rely on is left unpublished"
    );
}

/// A fresh client at each advertised head, and an existing client
/// applying the new heads one by one, never hold a session twice; a
/// replay of the same source changes nothing and keeps that true.
#[test]
fn import_seam_every_advertised_head_lists_each_session_once() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(&fixture);
    let existing_at = heads(&fixture.store, vehicle_id).len();
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let mut planned = plan(&fixture, &projected);
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    publish(&fixture, &projected, run_id, &mut planned).expect("publication");

    // Replay: the same source again.
    let journalled = mutation_count(&fixture.store, vehicle_id);
    let drives = store_drives(&fixture.store, vehicle_id);
    let mut replayed = plan(&fixture, &projected);
    let run_id = begin_run(&fixture, T + 4 * HOUR);
    publish(&fixture, &projected, run_id, &mut replayed).expect("replay");
    assert_eq!(mutation_count(&fixture.store, vehicle_id), journalled);
    assert_eq!(store_drives(&fixture.store, vehicle_id), drives);
    assert_eq!(replayed.seam.skipped, SourceDbSeamSkipped::default());

    let all = heads(&fixture.store, vehicle_id);
    // Fresh client: everything up to each head.
    for end in 1..=all.len() {
        let mut client = Client::default();
        for (_, rows) in &all[..end] {
            client.apply(rows);
        }
        client.assert_each_session_once(&format!("fresh client at head {}", all[end - 1].0));
    }
    // Existing client: at the head before the import, then one by one.
    let mut client = Client::default();
    for (_, rows) in &all[..existing_at] {
        client.apply(rows);
    }
    client.assert_each_session_once("existing client before the import");
    for (sequence, rows) in &all[existing_at..] {
        client.apply(rows);
        client.assert_each_session_once(&format!("existing client at head {sequence}"));
    }
    assert_eq!(client.drives, store_drives(&fixture.store, vehicle_id));
}

/// The store changed between the plan and publication (here a second
/// window): publication plans again, finds another plan and refuses
/// without publishing anything.
#[test]
fn import_seam_plan_digest_mismatch_refuses() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(&fixture);
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let mut planned = plan(&fixture, &projected);
    let head = fixture.store.v2_head(vehicle_id).expect("head");
    let drives = store_drives(&fixture.store, vehicle_id);
    fixture
        .store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
             VALUES (2, 'grant-2', ?1, ?2, '[]', 'handback')",
            params![T + 2 * HOUR - 5 * MINUTE, T + 3 * HOUR],
        )
        .expect("second window");
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    let refused = publish(&fixture, &projected, run_id, &mut planned);
    assert!(
        matches!(
            refused,
            Err(SourceDbImportError::Store(StoreError::ImportGenerationConflict))
        ),
        "{refused:?}"
    );
    assert_eq!(fixture.store.v2_head(vehicle_id).expect("head"), head);
    assert_eq!(store_drives(&fixture.store, vehicle_id), drives);
}

/// Publication paths that do not plan the seam refuse once the Hub has
/// collected the vehicle (review finding 9): the in-memory history
/// importer before it writes a pack, the open-session seed, and the
/// store's own refusal, which holds only after collection.
#[test]
fn import_seam_unplanned_publication_paths_refuse() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([43; 32]);
    let request = SourceDbImportRequest {
        new_import_source: true,
        source_key: "import-seam-unplanned".into(),
        scope: SourceDbImportScope::Selected(CAR),
        imported_at_ms: T - HOUR,
    };
    let base = publish_history(&store, &cursor_key, &request, &base_history()).expect("base");
    store
        .refuse_unplanned_import_seam(base.vehicle_id)
        .expect("before collection the unplanned paths are exact");

    let vehicle = base.vehicle_id.to_string();
    let connection = store.open().expect("catalogue");
    connection
        .execute(
            "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, sealed)
             VALUES (1, 'grant-1', ?1, ?2, '[]', 'handback')",
            params![T, T + HOUR],
        )
        .expect("coverage");
    let mut drive = project_vehicle(&base_history(), CAR).expect("projection").snapshot.drives[0]
        .clone();
    drive.id = HUB_ID_BASE + 1;
    drive.start_date_ms = T + 10 * MINUTE;
    drive.end_date_ms = T + 20 * MINUTE;
    connection
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                vehicle.as_str(),
                drive.id,
                CAR,
                serde_json::to_string(&drive).expect("drive json")
            ],
        )
        .expect("Hub drive");
    drop(connection);
    let lineage = store.lineage_manifest_for_vehicle(base.vehicle_id).expect("lineage");

    assert!(matches!(
        store.refuse_unplanned_import_seam(base.vehicle_id),
        Err(StoreError::ImportGenerationConflict)
    ));
    let changed = SourceDbImportRequest {
        new_import_source: false,
        imported_at_ms: T + 3 * HOUR,
        ..request
    };
    let refused = publish_history(&store, &cursor_key, &changed, &successor_history());
    assert!(
        matches!(
            refused,
            Err(SourceDbImportError::Store(StoreError::ImportGenerationConflict))
        ),
        "{refused:?}"
    );
    assert_eq!(
        store.lineage_manifest_for_vehicle(base.vehicle_id).expect("lineage"),
        lineage,
        "nothing was published"
    );
    let session = SourceDbOpenSession {
        car_id: CAR,
        standalone_positions: vec![source_position(1501, None, T + 30 * MINUTE)],
        ..Default::default()
    };
    assert!(matches!(
        store.seed_imported_open_session(base.source_id, base.vehicle_id, CAR, &session, T + 3 * HOUR),
        Err(StoreError::ImportGenerationConflict)
    ));
}

// Charges and charge samples through the seam (M03-T17); a child module so it
// shares this fixture.
#[path = "import_seam_charge_tests.rs"]
mod charges;

// The pack-side seam repair (M03-T19); a child module so it shares this
// fixture.
#[path = "seam_repair_packs_tests.rs"]
mod seam_repair_packs;

// The negotiated 1.4 route carrying the seam (M03-T21); a child module so it
// shares this fixture.
#[path = "seam_physical_route_tests.rs"]
mod seam_physical_route;

/// One pack's `hub_pack_metadata` as a map.
fn pack_metadata(store: &HubStore, digest: Sha256Digest) -> BTreeMap<String, String> {
    let path = ProjectionPackWriter::new(store.packs_dir()).content_path(digest);
    let sqlite =
        zstd::stream::decode_all(std::fs::File::open(&path).expect("pack")).expect("pack bytes");
    let temporary = crate::private_tempdir().expect("inspection");
    let file = temporary.path().join("pack.sqlite");
    std::fs::write(&file, sqlite).expect("inspection copy");
    let connection = Connection::open(&file).expect("pack database");
    let mut statement = connection
        .prepare("SELECT key, value FROM hub_pack_metadata")
        .expect("metadata");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("metadata rows")
        .collect::<Result<_, _>>()
        .expect("metadata rows")
}

/// The stop rule of M03-T05 (design M03-SEAM-R3, section 2): replacing
/// the import base after a window keeps the Hub's rows on clients only if
/// the live spans reach a fresh client independently of that base. They
/// do not: each live span is a typed delta of the import base's own
/// lineage (its snapshot, chained from its head, signed into the pack),
/// a fresh client sees the Hub's rows only through those deltas, and the
/// spans go with the lineage's deltas when the lineage is retired.
#[test]
fn import_seam_live_spans_are_bound_to_the_import_base() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let before = fixture
        .store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("import base");
    assert!(before.deltas.is_empty());
    let (h1, h2) = collect(&fixture);
    let after = fixture
        .store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("import base");

    // The live spans are deltas of the import base, in the lineage and
    // inside each signed pack.
    assert_eq!(after.base, before.base);
    assert!(!after.deltas.is_empty());
    assert_eq!(after.deltas[0].parent_chain_digest, before.head_digest);
    for delta in &after.deltas {
        assert_eq!(delta.pack.snapshot_id, before.base.snapshot_id);
        let metadata = pack_metadata(&fixture.store, delta.pack.sha256);
        assert_eq!(metadata["snapshot_id"], before.base.snapshot_id.to_string());
        assert_eq!(metadata["parent_digest"], delta.parent_chain_digest.to_string());
        assert_eq!(metadata["external_base"], "true");
    }
    let vehicle = vehicle_id.to_string();
    let mut connection = fixture.store.open().expect("catalogue");
    let live_spans = |connection: &Connection| -> i64 {
        connection
            .query_row(
                "SELECT count(*) FROM sync_live_delta_spans AS spans
                 JOIN sync_deltas AS deltas
                   ON deltas.vehicle_id = spans.vehicle_id
                  AND deltas.from_sequence = spans.from_sequence
                  AND deltas.to_sequence = spans.to_sequence
                 WHERE spans.vehicle_id = ?1",
                params![vehicle.as_str()],
                |row| row.get(0),
            )
            .expect("live spans")
    };
    assert_eq!(
        usize::try_from(live_spans(&connection)).expect("count"),
        after.deltas.len()
    );

    // A fresh client holds the Hub's rows only through those deltas.
    let all = heads(&fixture.store, vehicle_id);
    let mut base_only = Client::default();
    base_only.apply(&all[0].1);
    assert!(!base_only.drives.contains_key(&h1) && !base_only.drives.contains_key(&h2));
    let mut fresh = Client::default();
    for (_, rows) in &all {
        fresh.apply(rows);
    }
    assert!(fresh.drives.contains_key(&h1) && fresh.drives.contains_key(&h2));

    // Retiring the import lineage deletes its deltas, as
    // `retire_import_lineage_in_transaction` does; no live span is left
    // for a new base to carry. Rolled back.
    let transaction = connection.transaction().expect("transaction");
    transaction
        .execute(
            "DELETE FROM sync_deltas WHERE vehicle_id = ?1",
            params![vehicle.as_str()],
        )
        .expect("retire the deltas");
    assert_eq!(live_spans(&*transaction), 0);
    drop(transaction);
    assert_eq!(
        usize::try_from(live_spans(&connection)).expect("count"),
        after.deltas.len()
    );
}

/// One later import of `projected` the way the importer runs it: routed
/// against a lineage of at most `max_chunks` packs, then published as
/// deltas. A refused run is aborted, as the importer's run guard does.
fn import_successor(
    fixture: &Fixture,
    projected: &SourceDbProjection,
    at_ms: i64,
    max_chunks: usize,
) -> Result<PublishedDirectSuccessor, SourceDbImportError> {
    let run_id = begin_run(fixture, at_ms);
    let mut capture = capture(fixture, run_id, true, projected);
    let route = route_direct_successor(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        fixture.binding.vehicle_id,
        CAR,
        &projected.snapshot.drives,
        &projected.snapshot.charges,
        &mut capture,
        &fixture.binding,
        &fixture.car,
        max_chunks,
        |_| Ok(()),
    );
    let mut planned = match route {
        Ok(DirectSuccessorRoute::Deltas(planned)) => planned,
        Ok(DirectSuccessorRoute::NewBase(_)) => panic!("the import asked for a new base"),
        Err(error) => {
            fixture.store.abort_import_generation(run_id).expect("abort");
            return Err(error);
        }
    };
    publish_captured(fixture, projected, run_id, capture, &mut planned)
}

/// Every file under the pack store, for "the packs did not change".
fn pack_files(store: &HubStore) -> Vec<std::path::PathBuf> {
    fn walk(path: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("pack entry").path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(store.packs_dir(), &mut files);
    files.sort();
    files
}

/// Everything a refused import must leave as it was: the published lineage
/// and head, the Hub's drives and positions, the sync journal, the seam's
/// decisions and live spans, and the pack files.
fn observable(fixture: &Fixture) -> String {
    let store = &fixture.store;
    let vehicle_id = fixture.binding.vehicle_id;
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("published lineage");
    let count = |sql: &str| -> i64 {
        store
            .open()
            .expect("catalogue")
            .query_row(sql, params![vehicle_id.to_string()], |row| row.get(0))
            .expect("count")
    };
    format!(
        "{}|{:?}|{:?}|{:?}|{}|{}|{}|{}|{:?}",
        serde_json::to_string(&lineage).expect("lineage json"),
        store.v2_head(vehicle_id).expect("head"),
        store_drives(store, vehicle_id),
        store_hub_positions(store, vehicle_id),
        mutation_count(store, vehicle_id),
        count("SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1 AND published = 0"),
        count("SELECT COUNT(*) FROM import_seam_links WHERE vehicle_id = ?1"),
        count("SELECT COUNT(*) FROM sync_live_delta_spans WHERE vehicle_id = ?1"),
        pack_files(store),
    )
}

/// DEC-103: after a window the import base is never replaced. A later
/// import whose changes no longer fit in the lineage's packs would need a
/// new base, so it is refused with a message that names the cause, before
/// the seam changes anything: the store, the packs and the published head
/// stay as they were. A later import that fits still succeeds on the same
/// base, with the Hub's rows kept.
#[test]
fn import_seam_base_replacement_after_a_window_is_refused_clearly() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let (h1, _) = collect(&fixture);
    let base = fixture
        .store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("import base");
    let used = base.base.packs.len() + base.deltas.len();
    let before = observable(&fixture);
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");

    // One pack more than the lineage holds: the seam's own tombstones would
    // take it, so the source rows need a new base. Refused, unchanged.
    let refused = import_successor(&fixture, &projected, T + 3 * HOUR, used + 1)
        .expect_err("a new base after a window is refused");
    assert!(
        matches!(
            refused,
            SourceDbImportError::ImportBaseReplacementRefused { used: reported, limit }
                if reported == used && limit == used + 1
        ),
        "{refused:?}"
    );
    let message = refused.to_string();
    for cause in [
        "no longer fit in the vehicle's sync history",
        "would need a new import base",
        "a vehicle the Hub has collected is never replaced",
        "nothing was changed",
    ] {
        assert!(message.contains(cause), "the message names the cause: {message}");
    }
    assert_eq!(observable(&fixture), before, "the refusal changed nothing");

    // The same import with room on the lineage succeeds on the same base.
    let published = import_successor(
        &fixture,
        &projected,
        T + 4 * HOUR,
        ProtocolLimits::default().max_chunks,
    )
    .expect("a later import that fits");
    let after = fixture
        .store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("import base");
    assert_eq!(after.base, base.base, "the import base was not replaced");
    assert_eq!(after.deltas[..base.deltas.len()], base.deltas[..]);
    assert_eq!(published.sequence, after.head_sequence);
    assert!(store_drives(&fixture.store, vehicle_id).contains_key(&h1));
    assert!(store_drives(&fixture.store, vehicle_id).contains_key(&12));
}

/// The lineage's base and delta packs in use.
fn packs_in_use(fixture: &Fixture) -> usize {
    let lineage = fixture
        .store
        .lineage_manifest_for_vehicle(fixture.binding.vehicle_id)
        .expect("lineage")
        .expect("import base");
    lineage.base.packs.len() + lineage.deltas.len()
}

/// DEC-103, the interleaving behind the old backstop: the Hub collects the
/// vehicle (a window and its rows, not yet published) after the import
/// decided, on a vehicle without seam history, that its changes need a new
/// base, and before anything is applied. The decision is taken again on the
/// store as it now is, so the import is refused before it changes
/// anything: the store, the packs and the head are as the Hub's collection
/// left them.
#[test]
fn import_seam_base_replacement_backstop_collection_after_the_decision_is_refused_unchanged() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let used = packs_in_use(&fixture);
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    let mut capture = capture(&fixture, run_id, true, &projected);
    let mut collected = None;
    let mut after_collection = None;
    let route = route_direct_successor(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        vehicle_id,
        CAR,
        &projected.snapshot.drives,
        &projected.snapshot.charges,
        &mut capture,
        &fixture.binding,
        &fixture.car,
        used,
        |_| {
            if collected.is_none() {
                collected = Some(collect_unpublished(&fixture));
                after_collection = Some(observable(&fixture));
            }
            Ok(())
        },
    );
    let (h1, h2) = collected.expect("the Hub collected between decision and seam");
    match route {
        Err(SourceDbImportError::ImportBaseReplacementRefused { used: reported, limit }) => {
            assert_eq!((reported, limit), (used, used));
        }
        Err(error) => panic!("refused for another cause: {error:?}"),
        Ok(_) => panic!("a new base after the Hub collected"),
    }
    assert_eq!(
        Some(observable(&fixture)),
        after_collection,
        "the refusal changed nothing after the collection"
    );
    let drives = store_drives(&fixture.store, vehicle_id);
    assert!(drives.contains_key(&h1) && drives.contains_key(&h2), "the Hub's rows are kept");
}

/// DEC-103, a vehicle with no seam history whose own seam would change the
/// store: the source dropped drive 10, so the seam would tombstone it and
/// publish that as a live span (seam history). When its changes need a new
/// base, the decision comes before the seam and the new base applies none
/// of it, so its own seam never writes history it could then be refused
/// for: routing changes nothing.
#[test]
fn import_seam_base_replacement_backstop_own_seam_without_history_applies_nothing() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let used = packs_in_use(&fixture);
    let base = fixture
        .store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("import base");
    let newer = history(
        vec![source_drive(12, T + 2 * HOUR, T + 2 * HOUR + 10 * MINUTE)],
        vec![source_position(1201, Some(12), T + 2 * HOUR + 5 * MINUTE)],
    );
    let projected = project_vehicle(&newer, CAR).expect("projection");
    let dry_run = fixture
        .store
        .dry_run_import_seam_ahead_of_packs(
            &fixture.gate,
            vehicle_id,
            CAR,
            &projected.snapshot.drives,
            &projected.snapshot.charges,
        )
        .expect("dry run");
    assert!(!dry_run.seam_history, "no seam history yet");
    assert!(dry_run.ahead.changed_the_store, "its own seam would tombstone drive 10");
    let before = observable(&fixture);
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    let mut capture = capture(&fixture, run_id, true, &projected);
    let mut decisions = 0;
    let route = route_direct_successor(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        vehicle_id,
        CAR,
        &projected.snapshot.drives,
        &projected.snapshot.charges,
        &mut capture,
        &fixture.binding,
        &fixture.car,
        used,
        |_| {
            decisions += 1;
            Ok(())
        },
    );
    let replaced = match route {
        Ok(DirectSuccessorRoute::NewBase(replaced)) => replaced,
        Ok(DirectSuccessorRoute::Deltas(_)) => panic!("deltas past the lineage's capacity"),
        Err(error) => panic!("refused a vehicle without seam history: {error:?}"),
    };
    assert_eq!(decisions, 1);
    assert_eq!(replaced.existing, base);
    assert_eq!(replaced.existing_pack_count, used);
    assert_eq!(observable(&fixture), before, "routing applied none of the seam");
    assert!(store_drives(&fixture.store, vehicle_id).contains_key(&10));
}

/// DEC-103, the importer's refusal arm through its own successor step
/// (`route_direct_successor_for_run`, which the PostgreSQL entry point
/// calls): the full-snapshot candidate packs are removed, the run is
/// aborted, and the store, the packs and the head are as they were.
#[test]
fn import_seam_base_replacement_refusal_discards_the_candidate_and_aborts_the_run() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(&fixture);
    let used = packs_in_use(&fixture);
    let before = observable(&fixture);
    let projected = project_vehicle(&successor_history(), CAR).expect("projection");
    let run_id = begin_run(&fixture, T + 3 * HOUR);
    let mut capture = capture(&fixture, run_id, true, &projected);
    let head = fixture.store.v2_head(vehicle_id).expect("head").expect("base").1;
    let sequence = u64::try_from(head).expect("sequence") + 100;
    let candidate = ProjectionPackWriter::new(fixture.store.packs_dir())
        .write_full_snapshot_with_states_and_updates(
            &ProjectionPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id: Uuid::new_v4(),
                ordinal: 0,
                binding: fixture.binding.clone(),
                sequence: SequenceRange {
                    from_exclusive: sequence,
                    to_inclusive: sequence,
                },
                snapshot: &projected.snapshot,
            },
            &[],
            &[],
        )
        .expect("candidate pack");
    assert!(candidate.may_remove_unpublished_file());
    let candidate_path = candidate.path.clone();
    assert!(candidate_path.exists());
    let mut direct = StagedProjectionPacks::new(
        vec![candidate],
        ProjectionReport::default(),
        Sha256Digest::of_bytes(b"candidate"),
        Vec::new(),
    );
    let mut run_guard = ImportGenerationGuard {
        store: &fixture.store,
        run_id: Some(run_id),
    };
    let refused = route_direct_successor_for_run(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        vehicle_id,
        CAR,
        &projected.snapshot.drives,
        &projected.snapshot.charges,
        &mut capture,
        &fixture.binding,
        &fixture.car,
        used + 1,
        &mut direct,
        &mut run_guard,
        |_| Ok(()),
    );
    match refused {
        Err(SourceDbImportError::ImportBaseReplacementRefused { used: reported, limit }) => {
            assert_eq!((reported, limit), (used, used + 1));
        }
        Err(error) => panic!("refused for another cause: {error:?}"),
        Ok(_) => panic!("routed past the lineage's capacity"),
    }
    assert!(run_guard.run_id.is_none(), "the run guard's run is aborted");
    drop(run_guard);
    let runs: i64 = fixture
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT COUNT(*) FROM import_generations WHERE run_id = ?1",
            params![run_id.to_string()],
            |row| row.get(0),
        )
        .expect("runs");
    assert_eq!(runs, 0, "the aborted run is gone");
    assert!(!candidate_path.exists(), "the candidate pack is removed");
    drop(direct);
    assert_eq!(observable(&fixture), before, "the refusal changed nothing");
}

/// The rows of every delta the lineage gained after `before`.
fn new_delta_rows(
    store: &HubStore,
    vehicle_id: Uuid,
    before: &crate::protocol::LineageManifestV2,
) -> PackRows {
    let after = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("published lineage");
    assert_eq!(after.base, before.base, "no new base");
    assert_eq!(after.deltas[..before.deltas.len()], before.deltas[..], "earlier deltas kept");
    let mut rows = PackRows::default();
    for delta in &after.deltas[before.deltas.len()..] {
        assert_eq!(delta.pack.snapshot_id, before.base.snapshot_id);
        let pack = pack_rows(store, delta.pack.sha256);
        rows.drives.extend(pack.drives);
        rows.positions.extend(pack.positions);
        rows.tombstones.extend(pack.tombstones);
    }
    rows
}

/// The store's materialised drives as stored (id, JSON).
fn stored_drive_json(store: &HubStore, vehicle_id: Uuid) -> BTreeMap<i64, String> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare("SELECT drive_id, drive_json FROM materialised_drives WHERE vehicle_id = ?1")
        .expect("drives");
    statement
        .query_map(params![vehicle_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("drive rows")
        .collect::<Result<_, _>>()
        .expect("drive rows")
}

/// M03-T05: a later read-only import from a newer source snapshot adds only
/// its new rows, as typed deltas on the same base; the history the base
/// already holds is not transferred again.
#[test]
fn import_incremental_a_newer_snapshot_adds_only_its_new_rows() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let before = fixture
        .store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("import base");
    let mut drives = base_history().drives;
    drives.push(source_drive(12, T + 2 * HOUR, T + 2 * HOUR + 10 * MINUTE));
    let newer = history(
        drives,
        vec![source_position(1201, Some(12), T + 2 * HOUR + 5 * MINUTE)],
    );
    let projected = project_vehicle(&newer, CAR).expect("projection");
    let published = import_successor(
        &fixture,
        &projected,
        T + 3 * HOUR,
        ProtocolLimits::default().max_chunks,
    )
    .expect("incremental import");
    let rows = new_delta_rows(&fixture.store, vehicle_id, &before);
    assert_eq!(
        rows.drives.iter().map(|row| row.0).collect::<Vec<_>>(),
        vec![12],
        "drive 10 is not sent again"
    );
    assert_eq!(rows.positions.iter().map(|row| row.0).collect::<Vec<_>>(), vec![1201]);
    assert!(rows.tombstones.is_empty());
    assert_eq!(published.projected_rows, 2, "the new drive and its position only");
    assert_eq!(
        store_drives(&fixture.store, vehicle_id).keys().copied().collect::<Vec<_>>(),
        vec![10, 12]
    );

    // The same snapshot again adds nothing.
    let unchanged = import_successor(
        &fixture,
        &projected,
        T + 4 * HOUR,
        ProtocolLimits::default().max_chunks,
    )
    .expect("repeat import");
    let after = fixture
        .store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("import base");
    let repeated = new_delta_rows(&fixture.store, vehicle_id, &after);
    assert!(repeated.drives.is_empty() && repeated.positions.is_empty());
    assert!(unchanged.projected_rows <= 1, "at most the car's metadata row");
}

/// M03-T05: one corrected drive in a later import is the only row the
/// import sends and the only drive the store rewrites; the unchanged
/// history, its packs and the base stay as they were.
#[test]
fn rebuild_incremental_one_corrected_drive_is_the_only_row_sent_or_rewritten() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    let mut drives = base_history().drives;
    drives.push(source_drive(12, T + 2 * HOUR, T + 2 * HOUR + 10 * MINUTE));
    drives.push(source_drive(14, T + 4 * HOUR, T + 4 * HOUR + 10 * MINUTE));
    let positions = vec![
        source_position(1201, Some(12), T + 2 * HOUR + 5 * MINUTE),
        source_position(1401, Some(14), T + 4 * HOUR + 5 * MINUTE),
    ];
    let first = project_vehicle(&history(drives.clone(), positions.clone()), CAR)
        .expect("projection");
    import_successor(
        &fixture,
        &first,
        T + 5 * HOUR,
        ProtocolLimits::default().max_chunks,
    )
    .expect("first later import");
    let before = fixture
        .store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("import base");
    let stored_before = stored_drive_json(&fixture.store, vehicle_id);
    let packs_before = pack_files(&fixture.store);

    // The source corrects drive 12's distance.
    let corrected = drives
        .iter()
        .cloned()
        .map(|mut drive| {
            if drive.id == 12 {
                drive.end_km = Some(25.0);
                drive.distance_km = Some(15.0);
            }
            drive
        })
        .collect();
    let second = project_vehicle(&history(corrected, positions), CAR).expect("projection");
    let published = import_successor(
        &fixture,
        &second,
        T + 6 * HOUR,
        ProtocolLimits::default().max_chunks,
    )
    .expect("corrected import");
    let rows = new_delta_rows(&fixture.store, vehicle_id, &before);
    assert_eq!(rows.drives.iter().map(|row| row.0).collect::<Vec<_>>(), vec![12]);
    assert!(rows.positions.is_empty(), "no position is sent again");
    assert!(rows.tombstones.is_empty());
    assert_eq!(published.projected_rows, 1, "the corrected drive only");

    let stored_after = stored_drive_json(&fixture.store, vehicle_id);
    assert_eq!(stored_after.len(), stored_before.len());
    for (id, json) in &stored_before {
        if *id == 12 {
            assert_ne!(stored_after[id], *json, "drive 12 is corrected");
        } else {
            assert_eq!(stored_after[id], *json, "drive {id} is not rewritten");
        }
    }
    let packs_after = pack_files(&fixture.store);
    assert!(
        packs_before.iter().all(|path| packs_after.contains(path)),
        "every earlier pack is kept"
    );
    assert!(packs_after.len() > packs_before.len(), "the delta adds its pack");
}
