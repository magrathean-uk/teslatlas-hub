// SPDX-License-Identifier: AGPL-3.0-only

//! Charges and charge samples through the import seam (design M03-SEAM-R3,
//! section 4, M03-T17), through the real store and importer path: the plan
//! goes ahead of the packs, publication proves it again, and every head a
//! client can read holds each charge once. Charges follow the extent rule
//! and the recorded links; samples go by instant and are never removed with
//! their charge. Each scenario replays the same source and checks that the
//! replay changes nothing.

use std::collections::BTreeMap;

use rusqlite::params;

use super::*;
use crate::source_db_projection::{SourceDbCharge, SourceDbChargingProcess};

/// A source charging process.
fn source_process(id: i64, start_ms: i64, end_ms: i64) -> SourceDbChargingProcess {
    serde_json::from_value(serde_json::json!({
        "id": id, "car_id": CAR, "start_date_ms": start_ms, "end_date_ms": end_ms,
        "charge_energy_added": 5.0, "start_battery_level": 40, "end_battery_level": 60
    }))
    .expect("source charging process")
}

/// A source charge sample.
fn source_sample(id: i64, process: i64, date_ms: i64) -> SourceDbCharge {
    serde_json::from_value(serde_json::json!({
        "id": id, "charging_process_id": process, "date_ms": date_ms, "battery_level": 50
    }))
    .expect("source charge sample")
}

/// The base's history with these charges added.
fn charge_history(
    processes: Vec<SourceDbChargingProcess>,
    samples: Vec<SourceDbCharge>,
) -> SourceDbHistory {
    let mut history = base_history();
    history.charging_processes = processes;
    history.charges = samples;
    history
}

/// The successor's projection state, every entity recorded.
fn capture_all(
    fixture: &Fixture,
    run_id: Uuid,
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
        true,
        state_limits(),
    )
    .expect("capture");
    for car in &projected.snapshot.cars {
        capture.record_car(car).expect("car");
    }
    for drive in &projected.snapshot.drives {
        capture.record_drive(drive).expect("drive");
    }
    for charge in &projected.snapshot.charges {
        capture.record_charge(charge).expect("charge");
    }
    for sample in &projected.snapshot.charge_samples {
        capture.record_charge_sample(CAR, sample).expect("charge sample");
    }
    capture.seal().expect("seal");
    capture
}

/// Plans, writes and publishes one successor of `history`, as the importer
/// does; returns the publication's seam-skipped counts.
fn import(fixture: &Fixture, history: &SourceDbHistory, at_ms: i64) -> SourceDbSeamSkipped {
    let projected = project_vehicle(history, CAR).expect("projection");
    let mut planned = plan(fixture, &projected);
    let run_id = begin_run(fixture, at_ms);
    let capture = capture_all(fixture, run_id, &projected);
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
            imported_at_ms: at_ms,
            base_snapshot_id,
            minimum_free_bytes: 0,
            fingerprint: Sha256Digest::of_bytes(format!("charges-{run_id}").as_bytes()),
            geofences: &[],
            materialised_car: &fixture.car,
            materialised_drives: &projected.snapshot.drives,
            materialised_charges: &projected.snapshot.charges,
        },
        &fixture.binding,
        &fixture.car,
        capture,
        &mut planned,
    )
    .expect("publication");
    planned.seam.skipped
}

/// The Hub's window `[T, T + 1 h)`, handed back, with these charges
/// (id, start, end) and samples (id, charge, instant) as the collector
/// stores and journals them, published to clients as live sync.
fn collect(fixture: &Fixture, charges: &[(i64, i64, i64)], samples: &[(i64, i64, i64)]) {
    let template = project_vehicle(
        &charge_history(vec![source_process(1, T, T + MINUTE)], vec![]),
        CAR,
    )
    .expect("projection")
    .snapshot
    .charges[0]
        .clone();
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
    for (id, start_ms, end_ms) in charges {
        let mut charge = template.clone();
        charge.id = *id;
        charge.start_date_ms = *start_ms;
        charge.end_date_ms = Some(*end_ms);
        let json = serde_json::to_string(&charge).expect("charge json");
        transaction
            .execute(
                "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![vehicle.as_str(), id, CAR, json],
            )
            .expect("Hub charge");
        journal("charge", *id, &json);
    }
    for (id, charge, at_ms) in samples {
        let sample = crate::source_db_projection::project_charge_sample(&source_sample(
            *id, *charge, *at_ms,
        ));
        let json = serde_json::to_string(&sample).expect("sample json");
        transaction
            .execute(
                "INSERT INTO materialised_charge_samples(vehicle_id, sample_id, charge_id, sample_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![vehicle.as_str(), id, charge, json],
            )
            .expect("Hub sample");
        journal("charge_sample", *id, &json);
    }
    transaction.commit().expect("commit");
    publish_sync_mutations_ahead_of_packs(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        fixture.binding.vehicle_id,
    )
    .expect("live sync of the Hub's collection");
}

/// One pack's charges (id, start, end), samples (id, charge, instant) and
/// tombstones (entity, id).
#[derive(Debug, Default)]
struct ChargeRows {
    charges: Vec<(i64, i64, Option<i64>)>,
    samples: Vec<(i64, i64, i64)>,
    tombstones: Vec<(String, i64)>,
}

fn charge_rows(store: &HubStore, digest: Sha256Digest) -> ChargeRows {
    let path = ProjectionPackWriter::new(store.packs_dir()).content_path(digest);
    let sqlite =
        zstd::stream::decode_all(std::fs::File::open(&path).expect("pack")).expect("pack bytes");
    let temporary = crate::private_tempdir().expect("inspection");
    let file = temporary.path().join("pack.sqlite");
    std::fs::write(&file, sqlite).expect("inspection copy");
    let connection = rusqlite::Connection::open(&file).expect("pack database");
    let has = |table: &str, column: &str| -> bool {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
                params![table, column],
                |row| row.get(0),
            )
            .expect("schema")
    };
    let mut rows = ChargeRows::default();
    if has("charges", "start_date_ms") {
        let mut statement = connection
            .prepare("SELECT id, start_date_ms, end_date_ms FROM charges ORDER BY id")
            .expect("charges");
        rows.charges = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("charge rows")
            .collect::<Result<_, _>>()
            .expect("charge rows");
    }
    if has("charge_samples", "timestamp_ms") {
        let mut statement = connection
            .prepare("SELECT id, charge_process_id, timestamp_ms FROM charge_samples ORDER BY id")
            .expect("samples");
        rows.samples = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("sample rows")
            .collect::<Result<_, _>>()
            .expect("sample rows");
    }
    if has("tombstones", "entity") {
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

/// Every advertised head of the vehicle in order: the base, then one entry
/// per delta (the seam's live sync and the import's packs alike).
fn charge_heads(store: &HubStore, vehicle_id: Uuid) -> Vec<ChargeRows> {
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage")
        .expect("published lineage");
    let mut base = ChargeRows::default();
    for pack in &lineage.base.packs {
        let rows = charge_rows(store, pack.sha256);
        base.charges.extend(rows.charges);
        base.samples.extend(rows.samples);
        base.tombstones.extend(rows.tombstones);
    }
    let mut heads = vec![base];
    for delta in &lineage.deltas {
        heads.push(charge_rows(store, delta.pack.sha256));
    }
    heads
}

/// A client's mirror of one vehicle's charges and samples.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ChargeClient {
    charges: BTreeMap<i64, (i64, Option<i64>)>,
    samples: BTreeMap<i64, (i64, i64)>,
}

impl ChargeClient {
    fn apply(&mut self, rows: &ChargeRows) {
        for (id, start, end) in &rows.charges {
            self.charges.insert(*id, (*start, *end));
        }
        for (id, charge, at) in &rows.samples {
            self.samples.insert(*id, (*charge, *at));
        }
        for (entity, id) in &rows.tombstones {
            if entity == "charge" {
                self.charges.remove(id);
            } else if entity == "charge_sample" {
                self.samples.remove(id);
            }
        }
    }

    /// No two charges overlap by more than the seam's slack.
    fn assert_each_charge_once(&self, at: &str) {
        let charges: Vec<_> = self.charges.iter().collect();
        for (index, (a, (a_start, a_end))) in charges.iter().enumerate() {
            for (b, (b_start, b_end)) in &charges[index + 1..] {
                let a_end = a_end.unwrap_or(*a_start);
                let b_end = b_end.unwrap_or(*b_start);
                let overlap = a_end.min(b_end) - a_start.max(b_start);
                assert!(
                    overlap <= SEAM_SLACK_MS,
                    "{at}: charges {a} and {b} are the same session"
                );
            }
        }
    }
}

/// Every head, for a fresh client at each head and for a client that held
/// the heads up to `existing_at` and then applies the rest one by one;
/// returns the client at the last head.
fn assert_every_head_holds_each_charge_once(fixture: &Fixture, existing_at: usize) -> ChargeClient {
    let all = charge_heads(&fixture.store, fixture.binding.vehicle_id);
    for end in 1..=all.len() {
        let mut client = ChargeClient::default();
        for rows in &all[..end] {
            client.apply(rows);
        }
        client.assert_each_charge_once(&format!("fresh client at head {end}"));
    }
    let mut client = ChargeClient::default();
    for rows in &all[..existing_at] {
        client.apply(rows);
    }
    for (index, rows) in all[existing_at..].iter().enumerate() {
        client.apply(rows);
        client.assert_each_charge_once(&format!("existing client at head {}", existing_at + index));
    }
    client
}

fn store_charges(store: &HubStore, vehicle_id: Uuid) -> BTreeMap<i64, (i64, Option<i64>)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT charge_id,
                    CAST(json_extract(charge_json, '$.start_date_ms') AS INTEGER),
                    CAST(json_extract(charge_json, '$.end_date_ms') AS INTEGER)
               FROM materialised_charges WHERE vehicle_id = ?1",
        )
        .expect("charges");
    statement
        .query_map(params![vehicle_id.to_string()], |row| {
            Ok((row.get(0)?, (row.get(1)?, row.get(2)?)))
        })
        .expect("charge rows")
        .collect::<Result<_, _>>()
        .expect("charge rows")
}

fn store_samples(store: &HubStore, vehicle_id: Uuid) -> BTreeMap<i64, (i64, i64)> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT sample_id, charge_id,
                    CAST(json_extract(sample_json, '$.charge_process_id') AS INTEGER),
                    CAST(json_extract(sample_json, '$.timestamp_ms') AS INTEGER)
               FROM materialised_charge_samples WHERE vehicle_id = ?1",
        )
        .expect("samples");
    statement
        .query_map(params![vehicle_id.to_string()], |row| {
            let (id, charge, json_charge, at): (i64, i64, i64, i64) =
                (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
            assert_eq!(charge, json_charge, "sample {id}: key and row agree");
            Ok((id, (charge, at)))
        })
        .expect("sample rows")
        .collect::<Result<_, _>>()
        .expect("sample rows")
}

fn charge_links(store: &HubStore, vehicle_id: Uuid) -> BTreeMap<i64, i64> {
    let connection = store.open().expect("catalogue");
    let mut statement = connection
        .prepare(
            "SELECT loser_id, winner_id FROM import_seam_links
              WHERE vehicle_id = ?1 AND entity = 'charge'",
        )
        .expect("links");
    statement
        .query_map(params![vehicle_id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("link rows")
        .collect::<Result<_, _>>()
        .expect("link rows")
}

/// Replays `history` and checks that nothing changes: no new journal entry,
/// the same charges, samples and links, nothing seam-skipped (unchanged rows
/// are not read again).
fn assert_replay_changes_nothing(fixture: &Fixture, history: &SourceDbHistory, at_ms: i64) {
    let vehicle_id = fixture.binding.vehicle_id;
    let journalled = mutation_count(&fixture.store, vehicle_id);
    let charges = store_charges(&fixture.store, vehicle_id);
    let samples = store_samples(&fixture.store, vehicle_id);
    let links = charge_links(&fixture.store, vehicle_id);
    assert_eq!(import(fixture, history, at_ms), SourceDbSeamSkipped::default());
    assert_eq!(mutation_count(&fixture.store, vehicle_id), journalled);
    assert_eq!(store_charges(&fixture.store, vehicle_id), charges);
    assert_eq!(store_samples(&fixture.store, vehicle_id), samples);
    assert_eq!(charge_links(&fixture.store, vehicle_id), links);
}

const H1: i64 = HUB_ID_BASE + 1;
const H2: i64 = HUB_ID_BASE + 2;
const H3: i64 = HUB_ID_BASE + 3;

/// The source's charge 21 starts inside the window and crosses its end; the
/// Hub's H1 is its part inside. The source's extent covers the Hub's, so it
/// is stored once, as 21: H1 goes with a tombstone ahead of the packs, its
/// samples stay (inside coverage) and move to 21, and the source sample
/// inside coverage is not packed.
#[test]
fn import_seam_charge_across_an_edge_is_stored_once() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(
        &fixture,
        &[(H1, T + 50 * MINUTE, T + HOUR)],
        &[(H1, H1, T + 52 * MINUTE), (H2, H1, T + 58 * MINUTE)],
    );
    let collected = charge_heads(&fixture.store, vehicle_id).len();
    let history = charge_history(
        vec![source_process(21, T + 50 * MINUTE, T + 80 * MINUTE)],
        vec![
            source_sample(2101, 21, T + 52 * MINUTE),
            source_sample(2102, 21, T + 70 * MINUTE),
        ],
    );
    let skipped = import(&fixture, &history, T + 3 * HOUR);
    assert_eq!(
        skipped,
        SourceDbSeamSkipped {
            charge_samples: 1,
            ..SourceDbSeamSkipped::default()
        }
    );

    assert!(store_charges(&fixture.store, vehicle_id).is_empty(), "H1 lost; import stores no source charge");
    assert_eq!(
        store_samples(&fixture.store, vehicle_id),
        BTreeMap::from([(H1, (21, T + 52 * MINUTE)), (H2, (21, T + 58 * MINUTE))])
    );
    assert_eq!(charge_links(&fixture.store, vehicle_id), BTreeMap::from([(H1, 21)]));
    // The tombstone of H1 is at a head before the pack that carries 21.
    let all = charge_heads(&fixture.store, vehicle_id);
    let tombstoned = all
        .iter()
        .position(|rows| rows.tombstones.contains(&("charge".to_owned(), H1)))
        .expect("H1's tombstone");
    let packed = all
        .iter()
        .position(|rows| rows.charges.iter().any(|row| row.0 == 21))
        .expect("charge 21 packed");
    assert!(collected <= tombstoned && tombstoned < packed);
    let import_samples: Vec<_> = all[packed..]
        .iter()
        .flat_map(|rows| rows.samples.clone())
        .collect();
    assert_eq!(import_samples, vec![(2102, 21, T + 70 * MINUTE)]);

    let client = assert_every_head_holds_each_charge_once(&fixture, collected);
    assert_eq!(
        client.charges,
        BTreeMap::from([(21, (T + 50 * MINUTE, Some(T + 80 * MINUTE)))])
    );
    assert_eq!(
        client.samples,
        BTreeMap::from([
            (2102, (21, T + 70 * MINUTE)),
            (H1, (21, T + 52 * MINUTE)),
            (H2, (21, T + 58 * MINUTE)),
        ])
    );
    assert_replay_changes_nothing(&fixture, &history, T + 4 * HOUR);
}

/// The source's charge 22 is the Hub's H1 inside the window: the Hub's
/// copy stands, 22 and its samples inside coverage are not packed, and the
/// decision is recorded. A later source time correction does not undo it.
#[test]
fn import_seam_charge_inside_a_window_is_the_hubs() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(
        &fixture,
        &[(H1, T + 10 * MINUTE, T + 30 * MINUTE)],
        &[(H1, H1, T + 15 * MINUTE), (H2, H1, T + 25 * MINUTE)],
    );
    let collected = charge_heads(&fixture.store, vehicle_id).len();
    let charges = store_charges(&fixture.store, vehicle_id);
    let samples = store_samples(&fixture.store, vehicle_id);
    let history = charge_history(
        vec![source_process(22, T + 10 * MINUTE, T + 30 * MINUTE)],
        vec![
            source_sample(2201, 22, T + 15 * MINUTE),
            source_sample(2202, 22, T + 25 * MINUTE),
        ],
    );
    let skipped = import(&fixture, &history, T + 3 * HOUR);
    assert_eq!(
        skipped,
        SourceDbSeamSkipped {
            charges: 1,
            charge_samples: 2,
            ..SourceDbSeamSkipped::default()
        }
    );
    assert_eq!(store_charges(&fixture.store, vehicle_id), charges);
    assert_eq!(store_samples(&fixture.store, vehicle_id), samples);
    assert_eq!(charge_links(&fixture.store, vehicle_id), BTreeMap::from([(22, H1)]));
    let client = assert_every_head_holds_each_charge_once(&fixture, collected);
    assert_eq!(client.charges.keys().copied().collect::<Vec<_>>(), vec![H1]);
    assert_eq!(client.samples.keys().copied().collect::<Vec<_>>(), vec![H1, H2]);
    assert_replay_changes_nothing(&fixture, &history, T + 4 * HOUR);

    // The source corrects 22's times: the recorded link keeps it skipped.
    let corrected = charge_history(
        vec![source_process(22, T + 12 * MINUTE, T + 33 * MINUTE)],
        vec![
            source_sample(2201, 22, T + 16 * MINUTE),
            source_sample(2202, 22, T + 25 * MINUTE),
        ],
    );
    let skipped = import(&fixture, &corrected, T + 5 * HOUR);
    assert_eq!(skipped.charges, 1);
    assert_eq!(charge_links(&fixture.store, vehicle_id), BTreeMap::from([(22, H1)]));
    let client = assert_every_head_holds_each_charge_once(&fixture, collected);
    assert_eq!(client.charges.keys().copied().collect::<Vec<_>>(), vec![H1]);
    assert_eq!(store_charges(&fixture.store, vehicle_id), charges);
}

/// The source's open charge 23 continued live on the Hub through the
/// window (it keeps its source id; the Hub's samples have Hub ids). The
/// source later closed it earlier than the Hub did, so its copy does not
/// cover the stored extent: the stored row and every Hub sample stay, the
/// source copy is not packed, and only its sample outside coverage is.
#[test]
fn import_seam_inherited_charge_closed_earlier_keeps_hub_samples() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(
        &fixture,
        &[(23, T - 20 * MINUTE, T + 40 * MINUTE)],
        &[
            (H1, 23, T + 5 * MINUTE),
            (H2, 23, T + 20 * MINUTE),
            (H3, 23, T + 35 * MINUTE),
        ],
    );
    let collected = charge_heads(&fixture.store, vehicle_id).len();
    let charges = store_charges(&fixture.store, vehicle_id);
    let samples = store_samples(&fixture.store, vehicle_id);
    let history = charge_history(
        vec![source_process(23, T - 20 * MINUTE, T + 5 * MINUTE)],
        vec![
            source_sample(2301, 23, T - 15 * MINUTE),
            source_sample(2302, 23, T + 2 * MINUTE),
        ],
    );
    let skipped = import(&fixture, &history, T + 3 * HOUR);
    assert_eq!(
        skipped,
        SourceDbSeamSkipped {
            charges: 1,
            charge_samples: 1,
            ..SourceDbSeamSkipped::default()
        }
    );
    assert_eq!(store_charges(&fixture.store, vehicle_id), charges);
    assert_eq!(store_samples(&fixture.store, vehicle_id), samples);
    assert!(charge_links(&fixture.store, vehicle_id).is_empty(), "one id on both sides");
    let client = assert_every_head_holds_each_charge_once(&fixture, collected);
    assert_eq!(
        client.charges,
        BTreeMap::from([(23, (T - 20 * MINUTE, Some(T + 40 * MINUTE)))])
    );
    assert_eq!(
        client.samples,
        BTreeMap::from([
            (2301, (23, T - 15 * MINUTE)),
            (H1, (23, T + 5 * MINUTE)),
            (H2, (23, T + 20 * MINUTE)),
            (H3, (23, T + 35 * MINUTE)),
        ])
    );
    assert_replay_changes_nothing(&fixture, &history, T + 4 * HOUR);
}

/// The other side of the inherited rule: the source saw the whole session,
/// so its copy covers the stored extent and replaces the stored row in the
/// store and on clients, under the same id; the Hub's samples inside
/// coverage stay.
#[test]
fn import_seam_inherited_charge_the_source_covers_takes_the_source_copy() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(
        &fixture,
        &[(23, T - 20 * MINUTE, T + 30 * MINUTE)],
        &[(H1, 23, T + 10 * MINUTE)],
    );
    let collected = charge_heads(&fixture.store, vehicle_id).len();
    let history = charge_history(
        vec![source_process(23, T - 20 * MINUTE, T + 45 * MINUTE)],
        vec![source_sample(2301, 23, T - 15 * MINUTE)],
    );
    assert_eq!(import(&fixture, &history, T + 3 * HOUR), SourceDbSeamSkipped::default());
    let covered = BTreeMap::from([(23, (T - 20 * MINUTE, Some(T + 45 * MINUTE)))]);
    assert_eq!(store_charges(&fixture.store, vehicle_id), covered);
    assert_eq!(
        store_samples(&fixture.store, vehicle_id),
        BTreeMap::from([(H1, (23, T + 10 * MINUTE))])
    );
    // The store's replacement goes out as live sync after the pack.
    publish_sync_mutations_ahead_of_packs(
        &fixture.store,
        &fixture.gate,
        &fixture.cursor_key,
        vehicle_id,
    )
    .expect("live sync");
    let client = assert_every_head_holds_each_charge_once(&fixture, collected);
    assert_eq!(client.charges, covered);
    assert_eq!(client.samples.keys().copied().collect::<Vec<_>>(), vec![2301, H1]);
    assert_replay_changes_nothing(&fixture, &history, T + 4 * HOUR);
}

/// Samples go by instant whatever their charge's fate: the Hub's sample of
/// a charge that lost stays (inside coverage) and moves to the winner; a
/// Hub sample outside coverage goes with a tombstone though its charge is
/// kept; source samples outside coverage are packed and inside are not.
#[test]
fn import_seam_charge_samples_follow_the_instant_rule() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(
        &fixture,
        &[(H1, T, T + 10 * MINUTE), (H2, T + 50 * MINUTE, T + 65 * MINUTE)],
        &[
            (H1, H1, T + 3 * MINUTE),
            (H2, H2, T + 55 * MINUTE),
            (H3, H2, T + 62 * MINUTE),
        ],
    );
    let collected = charge_heads(&fixture.store, vehicle_id).len();
    let history = charge_history(
        vec![source_process(24, T - 10 * MINUTE, T + 10 * MINUTE)],
        vec![
            source_sample(2401, 24, T - 5 * MINUTE),
            source_sample(2402, 24, T + 5 * MINUTE),
        ],
    );
    let skipped = import(&fixture, &history, T + 3 * HOUR);
    assert_eq!(
        skipped,
        SourceDbSeamSkipped {
            charge_samples: 1,
            ..SourceDbSeamSkipped::default()
        }
    );
    assert_eq!(
        store_charges(&fixture.store, vehicle_id),
        BTreeMap::from([(H2, (T + 50 * MINUTE, Some(T + 65 * MINUTE)))])
    );
    assert_eq!(
        store_samples(&fixture.store, vehicle_id),
        BTreeMap::from([(H1, (24, T + 3 * MINUTE)), (H2, (H2, T + 55 * MINUTE))])
    );
    let tombstones: Vec<_> = charge_heads(&fixture.store, vehicle_id)
        .into_iter()
        .flat_map(|rows| rows.tombstones)
        .collect();
    assert!(tombstones.contains(&("charge".to_owned(), H1)));
    assert!(tombstones.contains(&("charge_sample".to_owned(), H3)));
    assert!(!tombstones.contains(&("charge_sample".to_owned(), H1)));
    let client = assert_every_head_holds_each_charge_once(&fixture, collected);
    assert_eq!(client.charges.keys().copied().collect::<Vec<_>>(), vec![24, H2]);
    assert_eq!(
        client.samples,
        BTreeMap::from([
            (2401, (24, T - 5 * MINUTE)),
            (H1, (24, T + 3 * MINUTE)),
            (H2, (H2, T + 55 * MINUTE)),
        ])
    );
    assert_replay_changes_nothing(&fixture, &history, T + 4 * HOUR);
}

/// One window with every case at once (a charge across its end, one inside
/// it, an inherited one the source closed earlier, and one after it), then
/// two replays of the same source: every charge is kept exactly once in the
/// store and on clients at every head, and the replays change nothing.
#[test]
fn import_seam_replay_after_a_window_keeps_every_charge() {
    let fixture = fixture();
    let vehicle_id = fixture.binding.vehicle_id;
    collect(
        &fixture,
        &[
            (23, T - 20 * MINUTE, T + 8 * MINUTE),
            (H1, T + 15 * MINUTE, T + 30 * MINUTE),
            (H2, T + 50 * MINUTE, T + HOUR),
        ],
        &[
            (H1, 23, T + 5 * MINUTE),
            (H2, H1, T + 20 * MINUTE),
            (H3, H2, T + 55 * MINUTE),
        ],
    );
    let collected = charge_heads(&fixture.store, vehicle_id).len();
    let history = charge_history(
        vec![
            source_process(21, T + 50 * MINUTE, T + 80 * MINUTE),
            source_process(22, T + 15 * MINUTE, T + 30 * MINUTE),
            source_process(23, T - 20 * MINUTE, T + 2 * MINUTE),
            source_process(25, T + 2 * HOUR, T + 2 * HOUR + 30 * MINUTE),
        ],
        vec![
            source_sample(2101, 21, T + 70 * MINUTE),
            source_sample(2201, 22, T + 20 * MINUTE),
            source_sample(2301, 23, T - 10 * MINUTE),
            source_sample(2501, 25, T + 2 * HOUR + 5 * MINUTE),
        ],
    );
    import(&fixture, &history, T + 3 * HOUR);
    let expected = BTreeMap::from([
        (21, (T + 50 * MINUTE, Some(T + 80 * MINUTE))),
        (23, (T - 20 * MINUTE, Some(T + 8 * MINUTE))),
        (25, (T + 2 * HOUR, Some(T + 2 * HOUR + 30 * MINUTE))),
        (H1, (T + 15 * MINUTE, Some(T + 30 * MINUTE))),
    ]);
    let client = assert_every_head_holds_each_charge_once(&fixture, collected);
    assert_eq!(client.charges, expected);
    assert_eq!(
        charge_links(&fixture.store, vehicle_id),
        BTreeMap::from([(22, H1), (H2, 21)])
    );

    let existing = charge_heads(&fixture.store, vehicle_id).len();
    assert_replay_changes_nothing(&fixture, &history, T + 4 * HOUR);
    assert_replay_changes_nothing(&fixture, &history, T + 5 * HOUR);
    let client = assert_every_head_holds_each_charge_once(&fixture, existing);
    assert_eq!(client.charges, expected);
    let mut hub_samples = client.samples.clone();
    hub_samples.retain(|id, _| *id >= HUB_ID_BASE);
    assert_eq!(hub_samples, store_samples(&fixture.store, vehicle_id));
    assert_eq!(
        client.samples.keys().copied().collect::<Vec<_>>(),
        vec![2101, 2301, 2501, H1, H2, H3]
    );
}

// The pack-side seam repair for charges (M03-T19); a child module so it
// shares these helpers.
#[path = "seam_repair_packs_charge_tests.rs"]
mod seam_repair_packs;
