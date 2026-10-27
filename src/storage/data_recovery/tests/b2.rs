// SPDX-License-Identifier: AGPL-3.0-only

// B2 fix round, part P5: the sealed-lease stop gap, the atomic sealing of a
// catalogue copy, the restore-only Fleet counts notice and the historical
// (schema 69 and 70) restores. Synthetic data only, in temporary stores; no
// network, no Tesla traffic and no collector run.

use super::*;
use crate::db::{
    CollectionGapRecord, CollectorAccountRow, CollectorCounterRow, CollectorRowsWrite,
    CollectorRunStartRequest, CollectorVehicleRow, SupervisedCollectorState,
};
use rusqlite::types::ValueRef;

/// The wall time the leases of these tests were acquired at.
const B2_ACQUIRED_MS: i64 = 1_760_000_000_000;
/// The retained lease's last heartbeat H: distinct from the acquisition, from the
/// time of the seal and from the time of the restored start.
const B2_HEARTBEAT_MS: i64 = B2_ACQUIRED_MS + 25_000;
const B2_GAP_MIN_MS: i64 = 65_000;
const B2_GAP_ALERT_MS: i64 = 600_000;
const B2_FINGERPRINT: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn b2_register_vehicle(store: &HubStore, key: &str) -> Uuid {
    let source = store
        .register_source(
            &SourceDescriptor::new("tesla_owner_api", "b2-recovery-account"),
            1_000,
        )
        .expect("b2 source");
    store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, key), 1_001)
        .expect("b2 vehicle")
        .vehicle_id
}

fn b2_vehicle_row(vehicle: Uuid, phase: &str) -> CollectorVehicleRow {
    CollectorVehicleRow {
        vehicle_id: vehicle,
        phase: phase.to_owned(),
        hold: "none".to_owned(),
        reason_code: "b2_fixture".to_owned(),
        phase_since_ms: B2_ACQUIRED_MS,
        suspended_until_ms: None,
        next_probe_ms: None,
        last_wake_read_at_ms: None,
    }
}

fn b2_account_row(state: &str) -> CollectorAccountRow {
    CollectorAccountRow {
        mode: "fleet_polling".to_owned(),
        config_fingerprint: B2_FINGERPRINT.to_owned(),
        state: state.to_owned(),
        reason_code: state.to_owned(),
        since_ms: B2_ACQUIRED_MS,
        rate_limited_until_ms: None,
        next_probe_ms: None,
    }
}

fn b2_counter(category: &str, sent: i64, billable: i64, reserved: i64) -> CollectorCounterRow {
    CollectorCounterRow {
        month: "2026-10".to_owned(),
        mode: "fleet_polling".to_owned(),
        category: category.to_owned(),
        sent,
        billable,
        reserved,
    }
}

fn b2_write(store: &HubStore, write: &CollectorRowsWrite) {
    store
        .collector_write_transaction(
            |transaction| {
                let wrote = HubStore::write_collector_rows_in_transaction(transaction, write)?;
                Ok::<_, StoreError>(((), wrote))
            },
            || Ok(()),
        )
        .expect("b2 collector rows");
}

fn b2_gap_insert(vehicle: Option<Uuid>, started: i64, ended: Option<i64>) -> CollectorRowsWrite {
    CollectorRowsWrite {
        now_ms: started,
        gap_inserts: vec![crate::db::CollectorGapInsert {
            vehicle_id: vehicle,
            kind: "hub_stopped".to_owned(),
            started_at_ms: started,
            ended_at_ms: ended,
            alert: false,
        }],
        ..CollectorRowsWrite::default()
    }
}

/// Acquire the collector lease at `B2_ACQUIRED_MS` and renew it once, so its
/// heartbeat H differs from its acquisition.
fn b2_hold_lease(store: &HubStore) -> crate::db::SupervisedCollectorLease {
    let lease = store
        .acquire_supervised_collector_lease(B2_ACQUIRED_MS)
        .expect("b2 lease");
    store
        .heartbeat_supervised_collector_lease(
            lease,
            SupervisedCollectorState::Active,
            B2_HEARTBEAT_MS,
        )
        .expect("b2 heartbeat");
    lease
}

fn b2_start_request(start_ms: i64) -> CollectorRunStartRequest<'static> {
    CollectorRunStartRequest {
        mode: "fleet_polling",
        config_fingerprint: B2_FINGERPRINT,
        start_ms,
        previous_heartbeat_ms: None,
        gap_min_ms: B2_GAP_MIN_MS,
        gap_alert_ms: B2_GAP_ALERT_MS,
    }
}

/// Every row of a table as strings (blobs in hex), sorted: a typed snapshot that
/// compares as a multiset.
fn b2_table_rows(connection: &Connection, table: &str) -> Vec<Vec<String>> {
    let mut statement = connection
        .prepare(&format!("SELECT * FROM \"{table}\""))
        .expect("snapshot statement");
    let columns = statement.column_count();
    let mut rows: Vec<Vec<String>> = statement
        .query_map([], |row| {
            (0..columns)
                .map(|index| {
                    Ok(match row.get_ref(index)? {
                        ValueRef::Null => "NULL".to_owned(),
                        ValueRef::Integer(value) => format!("i:{value}"),
                        ValueRef::Real(value) => format!("r:{value:?}"),
                        ValueRef::Text(value) => format!("t:{}", String::from_utf8_lossy(value)),
                        ValueRef::Blob(value) => format!("b:{}", hex::encode(value)),
                    })
                })
                .collect()
        })
        .expect("snapshot rows")
        .collect::<Result<_, _>>()
        .expect("snapshot row");
    rows.sort();
    rows
}

fn b2_gap_rows(connection: &Connection) -> Vec<Vec<String>> {
    b2_table_rows(connection, "collection_gaps")
}

/// The row of a stop gap sealed from a lease: account level, started at the
/// heartbeat, open, never notified.
fn b2_sealed_stop_row(gap_id: i64, alert: bool, created_at_ms: i64) -> Vec<String> {
    vec![
        format!("i:{gap_id}"),
        "NULL".to_owned(),
        "t:hub_stopped".to_owned(),
        format!("i:{B2_HEARTBEAT_MS}"),
        "NULL".to_owned(),
        format!("i:{}", i64::from(alert)),
        "NULL".to_owned(),
        format!("i:{created_at_ms}"),
    ]
}

fn b2_lease_rows(connection: &Connection) -> Vec<Vec<String>> {
    b2_table_rows(connection, "supervised_collector_lease")
}

fn b2_catalogue_gaps(data: &Path) -> Vec<CollectionGapRecord> {
    HubStore::open_immutable_read_only(data)
        .expect("immutable store")
        .collection_gaps(100)
        .expect("catalogue gaps")
}

fn b2_wall_clock_ms() -> i64 {
    current_epoch_ms().expect("wall clock")
}

/// One restore of a sealed lease: the car's stored phase, the silence between
/// the lease's heartbeat and the restored start, and whether the closed gap
/// alerts.
fn b2_sealed_lease_case(phase: &str, elapsed_ms: i64, expected_final_alert: bool) {
    let (temporary, store) = create_fixture();
    let vehicle = b2_register_vehicle(&store, "b2-sealed");
    b2_write(
        &store,
        &CollectorRowsWrite {
            now_ms: B2_ACQUIRED_MS,
            vehicles: vec![b2_vehicle_row(vehicle, phase)],
            ..CollectorRowsWrite::default()
        },
    );
    b2_hold_lease(&store);
    let source_lease = b2_lease_rows(&store.open().expect("source connection"));
    assert_eq!(source_lease.len(), 1);
    let awake_at_stop = phase == "driving";

    let backup = temporary.path().join("sealed-lease-backup");
    let before_seal = b2_wall_clock_ms();
    create_data_backup(&store, &backup).expect("create backup");
    let after_seal = b2_wall_clock_ms();
    let backup_data = backup.join(DATA_DIRECTORY);

    // The copy has no lease and exactly one stop from H, with no end and no
    // notification, created at the seal.
    let copy = open_immutable_catalogue(&backup_data).expect("copied catalogue");
    assert!(b2_lease_rows(&copy).is_empty(), "the copy keeps no lease");
    let sealed = b2_catalogue_gaps(&backup_data);
    assert_eq!(sealed.len(), 1);
    let stop = &sealed[0];
    assert_eq!(stop.vehicle_id, None);
    assert_eq!(stop.kind, "hub_stopped");
    assert_eq!(stop.started_at_ms, B2_HEARTBEAT_MS);
    assert_eq!(stop.ended_at_ms, None);
    assert_eq!(stop.notified_at_ms, None);
    assert_eq!(
        stop.alert, awake_at_stop,
        "the initial alert is the awake phase"
    );
    assert!(
        (before_seal..=after_seal).contains(&stop.created_at_ms),
        "created at the seal, not at the heartbeat"
    );
    assert_ne!(stop.created_at_ms, B2_HEARTBEAT_MS);
    drop(copy);

    // Backup changes only its copy.
    assert_eq!(
        b2_lease_rows(&store.open().expect("source connection")),
        source_lease,
        "the source lease is untouched"
    );
    assert!(
        store.collection_gaps(10).expect("source gaps").is_empty(),
        "the source records no gap"
    );

    let backup_tree = tree_snapshot(&backup);
    verify_data_backup(&backup).expect("verify backup");
    let restored = temporary.path().join("sealed-lease-restored");
    restore_data_backup(&backup, &restored).expect("restore backup");
    assert_eq!(
        tree_snapshot(&backup),
        backup_tree,
        "restore leaves the backup as it was"
    );
    assert_eq!(
        b2_catalogue_gaps(&restored),
        sealed,
        "restore keeps the sealed stop"
    );
    assert!(
        b2_lease_rows(&open_immutable_catalogue(&restored).expect("restored catalogue")).is_empty()
    );

    // The restored Hub's first collector start closes the stop; it has no
    // retained heartbeat, so it records no downtime of its own.
    let restored_store = HubStore::initialize(&restored).expect("restored store");
    assert!(
        restored_store
            .supervised_collector_lease_status()
            .expect("restored lease")
            .is_none()
    );
    let start_ms = B2_HEARTBEAT_MS + elapsed_ms;
    let started = restored_store
        .begin_collector_run(&b2_start_request(start_ms))
        .expect("restored start");
    assert!(started.stopped_gap_closed);
    assert!(!started.hub_down_inserted);
    let gaps = restored_store.collection_gaps(10).expect("closed gaps");
    assert_eq!(gaps.len(), 1, "no downtime gap beside the closed stop");
    assert_eq!(gaps[0].kind, "hub_stopped");
    assert_eq!(gaps[0].started_at_ms, B2_HEARTBEAT_MS);
    assert_eq!(gaps[0].ended_at_ms, Some(start_ms));
    assert_eq!(gaps[0].alert, expected_final_alert);
}

#[test]
fn b2_a_sealed_lease_gap_survives_restore_and_first_start() {
    // Alerts follow the persisted phase of the stop, otherwise the gap's length
    // at its close: 599999 ms stays quiet, 600000 ms alerts.
    b2_sealed_lease_case("driving", 599_999, true);
    b2_sealed_lease_case("asleep", 599_999, false);
    b2_sealed_lease_case("asleep", 600_000, true);

    // A wall clock before the heartbeat does not move the gap's start: the
    // heartbeat stays H and only the creation time follows the clock.
    let (temporary, store) = create_fixture();
    b2_hold_lease(&store);
    let payload = temporary.path().join("backward-clock-copy");
    store.backup_to(&payload).expect("unsealed copy");
    let backward_ms = B2_HEARTBEAT_MS - 10_000;
    seal_staged_catalogue_at(&payload, backward_ms).expect("seal with an earlier clock");
    let connection = Connection::open(payload.join("hub.sqlite")).expect("sealed copy");
    assert_eq!(
        b2_gap_rows(&connection),
        vec![b2_sealed_stop_row(1, false, backward_ms)]
    );
    assert!(b2_lease_rows(&connection).is_empty());
}

/// Seal an unsealed copy of `store` at `seal_ms` and return the copy's gap rows
/// and lease rows afterwards, with the source's gap rows from before.
fn b2_seal_copy(
    store: &HubStore,
    parent: &Path,
    name: &str,
    seal_ms: i64,
) -> (Vec<Vec<String>>, Vec<Vec<String>>, Vec<Vec<String>>) {
    let source_gaps = b2_gap_rows(&store.open().expect("source connection"));
    let payload = parent.join(name);
    store.backup_to(&payload).expect("unsealed copy");
    seal_staged_catalogue_at(&payload, seal_ms).expect("seal copy");
    let connection = Connection::open(payload.join("hub.sqlite")).expect("sealed copy");
    (
        source_gaps,
        b2_gap_rows(&connection),
        b2_lease_rows(&connection),
    )
}

#[test]
fn b2_sealing_preserves_existing_stops_and_requires_a_lease_for_a_new_stop() {
    let seal_ms = B2_HEARTBEAT_MS + 3_600_000;

    // An existing open account-level stop is preserved byte for byte,
    // notification and alert included, whether or not a lease is retained.
    for with_lease in [true, false] {
        let (temporary, store) = create_fixture();
        // Recorded twice: the open-stop guard must keep it a single row.
        for _ in 0..2 {
            store
                .end_collector_run(None, B2_ACQUIRED_MS - 90_000)
                .expect("stop gap");
        }
        store
            .open()
            .expect("source connection")
            .execute(
                "UPDATE collection_gaps SET alert = 1, notified_at_ms = 77",
                [],
            )
            .expect("mark the stop");
        if with_lease {
            b2_hold_lease(&store);
        }
        let (source, sealed, lease) = b2_seal_copy(&store, temporary.path(), "existing", seal_ms);
        assert_eq!(source.len(), 1);
        assert_eq!(
            sealed, source,
            "the existing stop is preserved (lease: {with_lease})"
        );
        assert!(lease.is_empty());
    }

    // Neither a lease nor a stop: no gap is invented.
    {
        let (temporary, store) = create_fixture();
        let (source, sealed, lease) = b2_seal_copy(&store, temporary.path(), "neither", seal_ms);
        assert!(source.is_empty() && sealed.is_empty() && lease.is_empty());
    }

    // A lease without a stop becomes exactly one stop from its heartbeat; a
    // second sealing of the sealed copy adds none.
    {
        let (temporary, store) = create_fixture();
        b2_hold_lease(&store);
        let (source, sealed, lease) = b2_seal_copy(&store, temporary.path(), "lease", seal_ms);
        assert!(source.is_empty());
        assert_eq!(sealed, vec![b2_sealed_stop_row(1, false, seal_ms)]);
        assert!(lease.is_empty());
        let payload = temporary.path().join("lease");
        seal_staged_catalogue_at(&payload, seal_ms + 1_000).expect("seal again");
        let connection = Connection::open(payload.join("hub.sqlite")).expect("sealed copy");
        assert_eq!(
            b2_gap_rows(&connection),
            sealed,
            "repeated sealing adds no stop"
        );
    }

    // Control: a graceful stop releases the lease; its stop is preserved.
    {
        let (temporary, store) = create_fixture();
        let lease = b2_hold_lease(&store);
        store
            .end_collector_run(None, B2_HEARTBEAT_MS + 500)
            .expect("graceful stop");
        store
            .release_supervised_collector_lease(lease)
            .expect("lease release");
        let (source, sealed, lease_rows) =
            b2_seal_copy(&store, temporary.path(), "graceful", seal_ms);
        assert_eq!(source.len(), 1);
        assert_eq!(sealed, source);
        assert!(lease_rows.is_empty());
    }

    // An open stop of one car is not the account-level stop: the lease still
    // yields its own account stop and the car's gap is preserved.
    {
        let (temporary, store) = create_fixture();
        let vehicle = b2_register_vehicle(&store, "b2-vehicle-stop");
        b2_write(
            &store,
            &b2_gap_insert(Some(vehicle), B2_ACQUIRED_MS - 5_000, None),
        );
        b2_hold_lease(&store);
        let (source, sealed, _) = b2_seal_copy(&store, temporary.path(), "vehicle-stop", seal_ms);
        assert_eq!(source.len(), 1);
        assert_eq!(sealed.len(), 2);
        assert_eq!(sealed[0], source[0], "the vehicle's stop is preserved");
        assert_eq!(sealed[1], b2_sealed_stop_row(2, false, seal_ms));
    }

    // A closed account stop does not stand for the open stop the lease needs.
    {
        let (temporary, store) = create_fixture();
        b2_write(
            &store,
            &b2_gap_insert(None, B2_ACQUIRED_MS - 90_000, Some(B2_ACQUIRED_MS - 80_000)),
        );
        b2_hold_lease(&store);
        let (source, sealed, _) = b2_seal_copy(&store, temporary.path(), "closed-stop", seal_ms);
        assert_eq!(source.len(), 1);
        assert_eq!(sealed.len(), 2);
        assert_eq!(sealed[0], source[0], "the closed stop is preserved");
        assert_eq!(sealed[1], b2_sealed_stop_row(2, false, seal_ms));
    }
}

#[test]
fn b2_sealing_rolls_back_its_gap_when_lease_removal_fails() {
    let (temporary, store) = create_fixture();
    // Authority that the earlier deletions of the transaction really remove: a
    // claimed device and the fixture's open invitation.
    let pairing = store
        .create_pairing("Rollback device", 2_000, 20_000)
        .expect("device pairing");
    store
        .claim_pairing(
            pairing.pairing_id,
            pairing.secret(),
            "Rollback device",
            3_000,
        )
        .expect("paired device");
    let vehicle = b2_register_vehicle(&store, "b2-rollback");
    b2_write(
        &store,
        &CollectorRowsWrite {
            now_ms: B2_ACQUIRED_MS,
            vehicles: vec![b2_vehicle_row(vehicle, "driving")],
            ..CollectorRowsWrite::default()
        },
    );
    b2_hold_lease(&store);

    let payload = temporary.path().join("failing-copy");
    store.backup_to(&payload).expect("unsealed copy");
    let catalogue = payload.join("hub.sqlite");
    let connection = Connection::open(&catalogue).expect("copied catalogue");
    connection
        .execute_batch(
            "INSERT INTO device_vehicle_grants (device_id, vehicle_id, granted_at_ms)
                 SELECT paired_devices.device_id, vehicles.vehicle_id, 3000
                   FROM paired_devices, vehicles;
             INSERT INTO paired_device_token_grace (device_id, token_sha256, valid_until_ms)
                 SELECT device_id, zeroblob(32), 99999 FROM paired_devices;
             CREATE TRIGGER b2_refuse_lease_removal BEFORE DELETE ON supervised_collector_lease
             BEGIN SELECT RAISE(ABORT, 'lease removal refused'); END;",
        )
        .expect("aborting trigger");
    let authority = |connection: &Connection| {
        (
            b2_table_rows(connection, "pairing_challenges"),
            b2_table_rows(connection, "paired_devices"),
            b2_table_rows(connection, "device_vehicle_grants"),
            b2_table_rows(connection, "paired_device_token_grace"),
            b2_lease_rows(connection),
            b2_gap_rows(connection),
        )
    };
    let before = authority(&connection);
    assert!(!before.0.is_empty() && !before.1.is_empty());
    assert!(!before.2.is_empty() && !before.3.is_empty());
    assert_eq!(before.4.len(), 1);
    assert!(before.5.is_empty());
    drop(connection);

    let failure = seal_staged_catalogue_at(&payload, B2_HEARTBEAT_MS + 1_000)
        .expect_err("a refused lease removal fails the sealing");
    assert!(
        failure.to_string().contains("lease removal refused"),
        "{failure}"
    );

    // An independently reopened copy shows neither the gap nor any deletion
    // that preceded the failure.
    let reopened = Connection::open(&catalogue).expect("reopened copy");
    assert_eq!(authority(&reopened), before);
}

#[test]
fn b2_same_month_restore_reports_rewound_counts_without_forcing_a_cap() {
    const NOTICE: &str = "Fleet request counts and reservations come from the backup. A restore from earlier in the same UTC calendar month can undercount requests already billed and allow the Hub's local monthly cap to be exceeded. No cap has been forced. Tesla's billing remains authoritative.";
    let counters_at_backup = vec![b2_counter("data", 4, 4, 4), b2_counter("state", 10, 8, 10)];
    let counters_live = vec![
        b2_counter("data", 12, 12, 12),
        b2_counter("state", 20, 18, 20),
    ];

    for backed_up_state in ["collecting", "cap_reached"] {
        let (temporary, store) = create_fixture();
        let account_at_backup = b2_account_row(backed_up_state);
        b2_write(
            &store,
            &CollectorRowsWrite {
                now_ms: B2_ACQUIRED_MS,
                account: Some(account_at_backup.clone()),
                counters: counters_at_backup.clone(),
                ..CollectorRowsWrite::default()
            },
        );
        let backup = temporary.path().join("same-month-backup");
        let created = create_data_backup(&store, &backup).expect("create backup");
        let verified = verify_data_backup(&backup).expect("verify backup");

        // The live Hub goes on: more requests, and a collecting account.
        b2_write(
            &store,
            &CollectorRowsWrite {
                now_ms: B2_ACQUIRED_MS + 600_000,
                account: Some(b2_account_row("collecting")),
                counters: counters_live.clone(),
                ..CollectorRowsWrite::default()
            },
        );

        let restored = temporary.path().join("same-month-restored");
        let report = restore_data_backup(&backup, &restored).expect("restore backup");

        // The restored rows are the backup's, read before any scheduler start.
        let rows = HubStore::open_immutable_read_only(&restored)
            .expect("restored store")
            .load_collector_state_rows()
            .expect("restored collector rows");
        assert_eq!(rows.account, Some(account_at_backup), "{backed_up_state}");
        assert_eq!(rows.counters, counters_at_backup, "{backed_up_state}");
        assert!(rows.open_gaps.is_empty());

        // Only the restore report carries the notice, under its exact key.
        for (name, omitted) in [("created", &created), ("verified", &verified)] {
            let json = serde_json::to_value(omitted).expect("report JSON");
            assert!(
                !json
                    .as_object()
                    .expect("report object")
                    .contains_key("fleetRequestCountsNotice"),
                "{name} report omits the notice"
            );
            assert_eq!(omitted.fleet_request_counts_notice, None);
        }
        let json = serde_json::to_value(&report).expect("restore report JSON");
        assert_eq!(json["status"], "data_restored");
        assert_eq!(json["fleetRequestCountsNotice"], NOTICE);
        assert_eq!(report.fleet_request_counts_notice, Some(NOTICE));
    }
}
