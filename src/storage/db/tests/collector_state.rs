// SPDX-License-Identifier: AGPL-3.0-only

// Tests of the durable collector state (C3 part K3: R4, R7, R8, R66). Synthetic
// data only, in a temporary store; no network. The scheduler's own use of these
// tables is tested with it, in `collector/tests/durable_state.rs`.

const CS_W0: i64 = 1_760_000_000_000;
const CS_FP_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CS_FP_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn cs_store() -> (tempfile::TempDir, HubStore) {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    (temporary, store)
}

/// One registered vehicle, the foreign key of the per-vehicle tables.
fn cs_vehicle(store: &HubStore, key: &str) -> Uuid {
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            CS_W0,
        )
        .expect("source");
    store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, key), CS_W0)
        .expect("vehicle")
        .vehicle_id
}

fn cs_write(store: &HubStore, write: &CollectorRowsWrite) {
    store
        .collector_write_transaction(
            |transaction| {
                let wrote = HubStore::write_collector_rows_in_transaction(transaction, write)?;
                Ok::<_, StoreError>(((), wrote))
            },
            || Ok(()),
        )
        .expect("collector rows");
}

fn cs_account(state: &str, reason: &str) -> CollectorAccountRow {
    CollectorAccountRow {
        mode: "legacy".to_owned(),
        config_fingerprint: CS_FP_A.to_owned(),
        state: state.to_owned(),
        reason_code: reason.to_owned(),
        since_ms: CS_W0,
        rate_limited_until_ms: None,
        next_probe_ms: None,
    }
}

fn cs_vehicle_row(vehicle: Uuid, phase: &str, hold: &str, reason: &str) -> CollectorVehicleRow {
    CollectorVehicleRow {
        vehicle_id: vehicle,
        phase: phase.to_owned(),
        hold: hold.to_owned(),
        reason_code: reason.to_owned(),
        phase_since_ms: CS_W0,
        suspended_until_ms: None,
        next_probe_ms: None,
        last_wake_read_at_ms: None,
    }
}

fn cs_gaps(store: &HubStore) -> Vec<CollectionGapRecord> {
    store.collection_gaps(100).expect("gaps")
}

#[test]
fn the_v70_step_adds_the_five_collector_tables_with_their_checks() {
    let (_temporary, store) = cs_store();
    let connection = store.open().expect("connection");
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    for table in [
        "collector_account_state",
        "collector_vehicle_state",
        "collector_request_counters",
        "collection_gaps",
        "collector_owner_actions",
    ] {
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                params![table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists, "{table}");
    }
    let vehicle = cs_vehicle(&store, "1");
    let fails = |sql: &str, parameters: &[&dyn rusqlite::ToSql]| {
        connection.execute(sql, parameters).is_err()
    };
    // The account state is closed, the singleton is one row and the fingerprint
    // is a 64-character digest.
    let account = "INSERT INTO collector_account_state(singleton_id, mode, config_fingerprint,
        state, reason_code, since_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4, 'x', 0, 0)";
    assert!(!fails(account, &[&1, &"legacy", &CS_FP_A, &"collecting"]));
    assert!(fails(account, &[&2, &"legacy", &CS_FP_A, &"collecting"]));
    connection
        .execute("DELETE FROM collector_account_state", [])
        .unwrap();
    assert!(fails(account, &[&1, &"legacy", &CS_FP_A, &"suspended"]));
    assert!(fails(account, &[&1, &"legacy", &"short", &"collecting"]));
    assert!(fails(account, &[&1, &"other", &CS_FP_A, &"collecting"]));
    // A counter cannot bill more than it sent, nor reserve less than it billed.
    let counter = "INSERT INTO collector_request_counters(month, mode, category, sent, billable,
        reserved, updated_at_ms) VALUES ('2026-10', 'fleet_polling', 'data', ?1, ?2, ?3, 0)";
    assert!(fails(counter, &[&5, &6, &6]));
    assert!(fails(counter, &[&5, &4, &3]));
    assert!(!fails(counter, &[&5, &4, &4]));
    // A gap cannot end before it began, and its kind is closed.
    let gap = "INSERT INTO collection_gaps(vehicle_id, kind, started_at_ms, ended_at_ms, alert,
        created_at_ms) VALUES (NULL, ?1, ?2, ?3, 0, 0)";
    assert!(fails(gap, &[&"hub_down", &10, &9]));
    assert!(fails(gap, &[&"unknown_kind", &10, &11]));
    assert!(!fails(gap, &[&"hub_down", &10, &11]));
    // `resume_vehicle` needs a vehicle and `retry_collection` takes none.
    let action = "INSERT INTO collector_owner_actions(action, vehicle_id, requested_at_ms)
        VALUES (?1, ?2, 0)";
    let vehicle_text = vehicle.to_string();
    assert!(fails(action, &[&"resume_vehicle", &Option::<String>::None]));
    assert!(fails(
        action,
        &[&"retry_collection", &Some(vehicle_text.clone())]
    ));
    assert!(!fails(
        action,
        &[&"retry_collection", &Option::<String>::None]
    ));
    assert!(!fails(action, &[&"resume_vehicle", &Some(vehicle_text)]));
}

#[test]
fn a_v69_catalogue_migrates_to_empty_collector_tables() {
    let (temporary, store) = cs_store();
    let connection = store.open().expect("connection");
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {REWIND_V70_SQL} PRAGMA user_version = 69; COMMIT;"
        ))
        .unwrap();
    let exists = |connection: &Connection| -> bool {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'collection_gaps')",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert!(!exists(&connection));
    drop(connection);
    drop(store);
    let migrated = HubStore::initialize(temporary.path()).expect("migrated");
    assert!(exists(&migrated.open().unwrap()));
    assert_eq!(
        migrated.load_collector_state_rows().unwrap(),
        CollectorStateRows::default(),
        "nothing is backfilled"
    );
}

/// Every row of a table as strings (blobs in hex), sorted: a typed snapshot of
/// its rows, columns and ciphertext included, that compares as a multiset.
fn cs_table_rows(connection: &Connection, table: &str) -> Vec<Vec<String>> {
    use rusqlite::types::ValueRef;
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

/// The rows of every ordinary table of the catalogue, by table name.
fn cs_catalogue_rows(connection: &Connection) -> std::collections::BTreeMap<String, Vec<Vec<String>>> {
    let tables: Vec<String> = connection
        .prepare(
            "SELECT name FROM sqlite_master
              WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .expect("table names")
        .query_map([], |row| row.get(0))
        .expect("table rows")
        .collect::<Result<_, _>>()
        .expect("table name");
    tables
        .into_iter()
        .map(|table| {
            let rows = cs_table_rows(connection, &table);
            (table, rows)
        })
        .collect()
}

#[test]
fn b2_a_populated_v69_catalogue_migrates_without_changing_existing_rows() {
    let (temporary, store) = cs_store();
    // A registered car, a bound credential pair with a completed refresh chain
    // (and its receipts), and a lease whose last heartbeat H differs from its
    // acquisition: the rows a catalogue of the previous release carries.
    let _vehicle = cs_vehicle(&store, "b2-v69");
    as_put_pair(&store, Some(as_gen(1)));
    as_refresh(&store, as_gen(1), as_gen(2));
    let lease = store
        .acquire_supervised_collector_lease(CS_W0)
        .expect("lease");
    let heartbeat_ms = CS_W0 + 12_000;
    store
        .heartbeat_supervised_collector_lease(lease, SupervisedCollectorState::Active, heartbeat_ms)
        .expect("heartbeat");

    // Rewind to the v69 shape and take the typed snapshot of every table.
    let connection = store.open().expect("connection");
    connection
        .execute_batch(&format!(
            "BEGIN IMMEDIATE; {REWIND_V70_SQL} PRAGMA user_version = 69; COMMIT;"
        ))
        .expect("rewind to v69");
    assert_eq!(schema_version(&connection).unwrap(), 69);
    let before = cs_catalogue_rows(&connection);
    for table in [
        "vehicles",
        "supervised_collector_lease",
        "teslamate_legacy_tokens",
        "outbound_request_receipts",
        "legacy_refresh_receipt_bindings",
    ] {
        assert!(!before[table].is_empty(), "{table} is populated");
    }
    for table in [
        "collector_account_state",
        "collector_vehicle_state",
        "collector_request_counters",
        "collection_gaps",
        "collector_owner_actions",
        "collector_auth_state",
    ] {
        assert!(!before.contains_key(table), "{table} does not exist in v69");
    }
    drop(connection);
    drop(store);

    let migrated = HubStore::initialize(temporary.path()).expect("migrated");
    let connection = migrated.open().expect("connection");
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    let after = cs_catalogue_rows(&connection);
    // Every pre-existing table holds exactly the rows it held.
    for (table, rows) in &before {
        assert_eq!(&after[table], rows, "{table}");
    }
    // Six tables are new: the five of C3 are empty, the credential status table
    // is present and its singleton row is absent.
    let added: Vec<&String> = after.keys().filter(|name| !before.contains_key(*name)).collect();
    assert_eq!(
        added,
        [
            "collection_gaps",
            "collector_account_state",
            "collector_auth_state",
            "collector_owner_actions",
            "collector_request_counters",
            "collector_vehicle_state",
        ]
        .iter()
        .collect::<Vec<_>>()
    );
    for table in added {
        assert!(after[table].is_empty(), "{table} is not backfilled");
    }
    assert_eq!(migrated.load_collector_auth_row().unwrap(), None);
    drop(connection);

    // The retained heartbeat is read before a replacement acquisition, and the
    // first start records exactly one downtime from it.
    let retained = migrated
        .supervised_collector_lease_status()
        .expect("lease status")
        .expect("retained lease");
    assert_eq!(retained.heartbeat_at_ms, heartbeat_ms);
    let start_ms = heartbeat_ms + 120_000;
    migrated
        .acquire_supervised_collector_lease(start_ms)
        .expect("replacement lease");
    let started = migrated
        .begin_collector_run(&CollectorRunStartRequest {
            mode: "legacy",
            config_fingerprint: CS_FP_A,
            start_ms,
            previous_heartbeat_ms: Some(retained.heartbeat_at_ms),
            gap_min_ms: 65_000,
            gap_alert_ms: 600_000,
        })
        .expect("first start");
    assert!(started.hub_down_inserted && !started.stopped_gap_closed);
    let gaps = cs_gaps(&migrated);
    assert_eq!(gaps.len(), 1);
    assert_eq!(
        (gaps[0].kind.as_str(), gaps[0].started_at_ms, gaps[0].ended_at_ms),
        ("hub_down", heartbeat_ms, Some(start_ms))
    );
}

#[test]
fn rows_written_in_one_transaction_load_exactly() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "1");
    let account = CollectorAccountRow {
        rate_limited_until_ms: Some(CS_W0 + 600_000),
        next_probe_ms: Some(CS_W0 + 21_600_000),
        ..cs_account("billing_suspended", "billing_suspended")
    };
    let mut car = cs_vehicle_row(vehicle, "suspended", "none", "suspended_idle");
    car.suspended_until_ms = Some(CS_W0 + 1_800_000);
    car.last_wake_read_at_ms = Some(CS_W0 - 60_000);
    let counter = CollectorCounterRow {
        month: "2026-10".to_owned(),
        mode: "fleet_polling".to_owned(),
        category: "data".to_owned(),
        sent: 7,
        billable: 6,
        reserved: 20,
    };
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0 + 5,
            account: Some(account.clone()),
            vehicles: vec![car.clone()],
            counters: vec![counter.clone()],
            gap_inserts: vec![CollectorGapInsert {
                vehicle_id: Some(vehicle),
                kind: "vehicle_paused".to_owned(),
                started_at_ms: CS_W0,
                ended_at_ms: None,
                alert: true,
            }],
            ..CollectorRowsWrite::default()
        },
    );
    let rows = store.load_collector_state_rows().unwrap();
    assert_eq!(rows.account, Some(account));
    assert_eq!(rows.vehicles, vec![car]);
    assert_eq!(rows.counters, vec![counter.clone()]);
    assert_eq!(rows.open_gaps.len(), 1);
    assert_eq!(rows.open_gaps[0].vehicle_id, Some(vehicle));
    assert_eq!(rows.open_gaps[0].kind, "vehicle_paused");
    assert!(rows.open_gaps[0].alert);
    assert_eq!(rows.open_gaps[0].notified_at_ms, None);
    assert_eq!(rows.open_gaps[0].created_at_ms, CS_W0 + 5);
    // A second write replaces the rows; counters update in place.
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0 + 9,
            vehicles: vec![cs_vehicle_row(vehicle, "asleep", "none", "reported_asleep")],
            counters: vec![CollectorCounterRow {
                sent: 8,
                billable: 7,
                reserved: 20,
                ..counter.clone()
            }],
            ..CollectorRowsWrite::default()
        },
    );
    let rows = store.load_collector_state_rows().unwrap();
    assert_eq!(rows.vehicles[0].phase, "asleep");
    assert_eq!(rows.vehicles[0].suspended_until_ms, None);
    assert_eq!(rows.counters.len(), 1);
    assert_eq!((rows.counters[0].sent, rows.counters[0].billable), (8, 7));
}

#[test]
fn an_empty_write_commits_nothing_and_a_failing_hook_rolls_back() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "1");
    // Nothing to write: the transaction rolls back and the hook never runs.
    let hook_ran = std::cell::Cell::new(false);
    store
        .collector_write_transaction(
            |transaction| {
                let wrote = HubStore::write_collector_rows_in_transaction(
                    transaction,
                    &CollectorRowsWrite::default(),
                )?;
                Ok::<_, StoreError>(((), wrote))
            },
            || {
                hook_ran.set(true);
                Ok(())
            },
        )
        .unwrap();
    assert!(!hook_ran.get());
    // A hook that fails rolls the whole transaction back.
    let result = store.collector_write_transaction(
        |transaction| {
            let wrote = HubStore::write_collector_rows_in_transaction(
                transaction,
                &CollectorRowsWrite {
                    now_ms: CS_W0,
                    vehicles: vec![cs_vehicle_row(vehicle, "online", "none", "reported_online")],
                    ..CollectorRowsWrite::default()
                },
            )?;
            Ok::<_, StoreError>(((), wrote))
        },
        || Err(StoreError::SyncMutation("injected".to_owned())),
    );
    assert!(result.is_err());
    assert!(
        store
            .load_collector_state_rows()
            .unwrap()
            .vehicles
            .is_empty()
    );
}

#[test]
fn an_open_gap_is_inserted_once_and_closed_with_the_length_rule() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "1");
    let open = |alert: bool, vehicle_id: Option<Uuid>| CollectorGapInsert {
        vehicle_id,
        kind: "owner_api_refused".to_owned(),
        started_at_ms: CS_W0,
        ended_at_ms: None,
        alert,
    };
    // Inserting an already-open gap of the same kind and vehicle adds nothing.
    for _ in 0..2 {
        cs_write(
            &store,
            &CollectorRowsWrite {
                now_ms: CS_W0,
                gap_inserts: vec![open(false, None)],
                ..CollectorRowsWrite::default()
            },
        );
    }
    assert_eq!(cs_gaps(&store).len(), 1);
    // Another vehicle or kind is another gap.
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            gap_inserts: vec![open(false, Some(vehicle))],
            ..CollectorRowsWrite::default()
        },
    );
    assert_eq!(cs_gaps(&store).len(), 2);
    // A close below the alert length leaves alert 0; at the length it sets 1.
    let close = |ended: i64| CollectorGapClose {
        vehicle_id: None,
        kind: "owner_api_refused".to_owned(),
        ended_at_ms: ended,
        alert_at_or_over_ms: 600_000,
    };
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0 + 599_999,
            gap_closes: vec![close(CS_W0 + 599_999)],
            ..CollectorRowsWrite::default()
        },
    );
    let gaps = cs_gaps(&store);
    assert_eq!(gaps[0].ended_at_ms, Some(CS_W0 + 599_999));
    assert!(!gaps[0].alert);
    // The vehicle's gap is untouched, and a second gap of that kind may open
    // once the first has closed.
    assert_eq!(gaps[1].ended_at_ms, None);
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0 + 700_000,
            gap_inserts: vec![CollectorGapInsert {
                started_at_ms: CS_W0 + 700_000,
                ..open(false, None)
            }],
            gap_closes: vec![],
            ..CollectorRowsWrite::default()
        },
    );
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0 + 1_300_000,
            gap_closes: vec![close(CS_W0 + 1_300_000)],
            ..CollectorRowsWrite::default()
        },
    );
    let gaps = cs_gaps(&store);
    assert_eq!(gaps.len(), 3);
    assert_eq!(gaps[2].started_at_ms, CS_W0 + 700_000);
    assert!(gaps[2].alert, "600 s long: an alert whatever the cars did");
}

#[test]
fn start_closes_the_stopped_gap_or_records_the_downtime_from_the_heartbeat() {
    let (_temporary, store) = cs_store();
    let request = |start_ms: i64, heartbeat: Option<i64>| CollectorRunStartRequest {
        mode: "legacy",
        config_fingerprint: CS_FP_A,
        start_ms,
        previous_heartbeat_ms: heartbeat,
        gap_min_ms: 65_000,
        gap_alert_ms: 600_000,
    };
    // A silence of exactly the minimum records nothing; one millisecond more does.
    let started = store
        .begin_collector_run(&request(CS_W0 + 65_000, Some(CS_W0)))
        .unwrap();
    assert!(!started.hub_down_inserted);
    assert!(cs_gaps(&store).is_empty());
    let started = store
        .begin_collector_run(&request(CS_W0 + 65_001, Some(CS_W0)))
        .unwrap();
    assert!(started.hub_down_inserted);
    let gaps = cs_gaps(&store);
    assert_eq!(gaps.len(), 1);
    assert_eq!(
        (
            gaps[0].kind.as_str(),
            gaps[0].started_at_ms,
            gaps[0].ended_at_ms
        ),
        ("hub_down", CS_W0, Some(CS_W0 + 65_001))
    );
    assert!(!gaps[0].alert, "short, and no car was awake");
    // A graceful stop leaves an open gap that the next start closes, and then no
    // `hub_down` is written for the same silence.
    store.end_collector_run(None, CS_W0 + 1_000_000).unwrap();
    store.end_collector_run(None, CS_W0 + 1_000_500).unwrap();
    let open: Vec<_> = cs_gaps(&store)
        .into_iter()
        .filter(|gap| gap.ended_at_ms.is_none())
        .collect();
    assert_eq!(open.len(), 1, "a second stop adds no second open gap");
    let started = store
        .begin_collector_run(&request(CS_W0 + 4_600_000, Some(CS_W0 + 1_000_000)))
        .unwrap();
    assert!(started.stopped_gap_closed && !started.hub_down_inserted);
    let stopped: Vec<_> = cs_gaps(&store)
        .into_iter()
        .filter(|gap| gap.kind == "hub_stopped")
        .collect();
    assert_eq!(stopped.len(), 1);
    assert_eq!(stopped[0].started_at_ms, CS_W0 + 1_000_000);
    assert_eq!(stopped[0].ended_at_ms, Some(CS_W0 + 4_600_000));
    assert!(stopped[0].alert, "an hour long: over the alert length");
}

#[test]
fn a_gap_alerts_when_a_car_was_awake_at_the_stored_phase_whatever_its_length() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "1");
    let request = |start_ms: i64| CollectorRunStartRequest {
        mode: "legacy",
        config_fingerprint: CS_FP_A,
        start_ms,
        previous_heartbeat_ms: Some(CS_W0),
        gap_min_ms: 65_000,
        gap_alert_ms: 600_000,
    };
    for (phase, hold, alert) in [
        ("driving", "none", true),
        ("charging", "none", true),
        ("updating", "none", true),
        ("online", "none", true),
        ("suspended", "none", true),
        ("asleep", "none", false),
        ("offline", "none", false),
        ("unknown", "none", false),
        // A held car is not collected, so it cannot make a gap an alert.
        ("driving", "disabled", false),
    ] {
        cs_write(
            &store,
            &CollectorRowsWrite {
                now_ms: CS_W0,
                vehicles: vec![cs_vehicle_row(vehicle, phase, hold, "collecting")],
                ..CollectorRowsWrite::default()
            },
        );
        store
            .collector_write_transaction(
                |transaction| {
                    transaction
                        .execute("DELETE FROM collection_gaps", [])
                        .map_err(StoreError::Query)?;
                    Ok::<_, StoreError>(((), true))
                },
                || Ok(()),
            )
            .unwrap();
        store.begin_collector_run(&request(CS_W0 + 65_001)).unwrap();
        let gaps = cs_gaps(&store);
        assert_eq!(gaps.len(), 1, "{phase}");
        assert_eq!(gaps[0].alert, alert, "{phase} {hold}");
    }
}

#[test]
fn a_changed_configuration_resets_a_stored_refusal_and_the_holds_only_it_can_lift() {
    let (_temporary, store) = cs_store();
    let paused = cs_vehicle(&store, "1");
    let other = cs_vehicle(&store, "2");
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            account: Some(CollectorAccountRow {
                rate_limited_until_ms: Some(CS_W0 + 100_000),
                next_probe_ms: Some(CS_W0 + 200_000),
                ..cs_account("provider_refused", "provider_gone")
            }),
            vehicles: vec![
                cs_vehicle_row(paused, "unknown", "paused", "paused_access_refused"),
                cs_vehicle_row(other, "unknown", "paused", "paused_persistent_failure"),
            ],
            gap_inserts: vec![
                CollectorGapInsert {
                    vehicle_id: None,
                    kind: "provider_refused".to_owned(),
                    started_at_ms: CS_W0,
                    ended_at_ms: None,
                    alert: false,
                },
                CollectorGapInsert {
                    vehicle_id: Some(paused),
                    kind: "vehicle_paused".to_owned(),
                    started_at_ms: CS_W0,
                    ended_at_ms: None,
                    alert: false,
                },
                CollectorGapInsert {
                    vehicle_id: Some(other),
                    kind: "vehicle_paused".to_owned(),
                    started_at_ms: CS_W0,
                    ended_at_ms: None,
                    alert: false,
                },
            ],
            ..CollectorRowsWrite::default()
        },
    );
    let request = |fingerprint: &'static str, mode: &'static str| CollectorRunStartRequest {
        mode,
        config_fingerprint: fingerprint,
        start_ms: CS_W0 + 50_000,
        previous_heartbeat_ms: None,
        gap_min_ms: 65_000,
        gap_alert_ms: 600_000,
    };
    // The same configuration restores the refusal untouched.
    let kept = store
        .begin_collector_run(&request(CS_FP_A, "legacy"))
        .unwrap();
    assert!(!kept.account_reset && kept.holds_cleared == 0);
    assert_eq!(
        store
            .load_collector_state_rows()
            .unwrap()
            .account
            .unwrap()
            .state,
        "provider_refused"
    );
    // A different fingerprint resets it, clears its deadlines, closes its gap
    // and lifts the access-refused pause; the other pause stays.
    let reset = store
        .begin_collector_run(&request(CS_FP_B, "legacy"))
        .unwrap();
    assert!(reset.account_reset);
    assert_eq!(reset.holds_cleared, 1);
    let rows = store.load_collector_state_rows().unwrap();
    assert_eq!(
        rows.account,
        Some(CollectorAccountRow {
            config_fingerprint: CS_FP_B.to_owned(),
            since_ms: CS_W0 + 50_000,
            ..cs_account("collecting", "config_changed")
        })
    );
    let by_vehicle = |id: Uuid| {
        rows.vehicles
            .iter()
            .find(|row| row.vehicle_id == id)
            .unwrap()
    };
    assert_eq!(by_vehicle(paused).hold, "none");
    assert_eq!(by_vehicle(paused).reason_code, "config_changed");
    assert_eq!(by_vehicle(other).hold, "paused");
    let open: Vec<(String, Option<Uuid>)> = rows
        .open_gaps
        .iter()
        .map(|gap| (gap.kind.clone(), gap.vehicle_id))
        .collect();
    assert_eq!(open, vec![("vehicle_paused".to_owned(), Some(other))]);
    // A mode change alone is also a change.
    store
        .begin_collector_run(&request(CS_FP_B, "provider"))
        .unwrap();
    assert_eq!(
        store
            .load_collector_state_rows()
            .unwrap()
            .account
            .unwrap()
            .mode,
        "provider"
    );
}

#[test]
fn owner_actions_load_oldest_first_until_applied() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "1");
    let retry = store
        .insert_collector_owner_action("retry_collection", None, CS_W0)
        .unwrap();
    let resume = store
        .insert_collector_owner_action("resume_vehicle", Some(vehicle), CS_W0 + 1)
        .unwrap();
    let rows = store.load_collector_state_rows().unwrap();
    assert_eq!(
        rows.actions
            .iter()
            .map(|action| (action.action_id, action.action.as_str(), action.vehicle_id))
            .collect::<Vec<_>>(),
        vec![
            (retry, "retry_collection", None),
            (resume, "resume_vehicle", Some(vehicle))
        ]
    );
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0 + 10,
            applied_actions: vec![retry],
            ..CollectorRowsWrite::default()
        },
    );
    let rows = store.load_collector_state_rows().unwrap();
    assert_eq!(rows.actions.len(), 1);
    assert_eq!(rows.actions[0].action_id, resume);
}

#[test]
fn collection_attention_does_not_change_service_readiness() {
    use CollectionAttentionCode as Code;
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "1");
    store.acquire_supervised_collector_lease(CS_W0).unwrap();
    let now = CS_W0 + 1;
    let view = |ms: i64| store.collector_readiness_view(&ReadinessContext::context_free(true, ms));
    let quiet = view(now);
    assert_eq!(quiet.service, Ok(()));
    assert_eq!(quiet.attention, None);
    assert!(quiet.detail.is_empty());
    // The seven C3 stops: exact attention, service stays ready.
    for (state, code) in [
        ("owner_api_refused", Code::CollectorApiRefused),
        ("provider_refused", Code::CollectorProviderRefused),
        ("fleet_access_refused", Code::CollectorFleetAccessRefused),
        ("billing_suspended", Code::CollectorBillingSuspended),
        ("cap_reached", Code::CollectorCapReached),
        ("config_error", Code::CollectorConfigError),
        ("account_locked", Code::CollectorAccountLocked),
    ] {
        cs_write(
            &store,
            &CollectorRowsWrite {
                now_ms: CS_W0,
                account: Some(cs_account(state, state)),
                ..CollectorRowsWrite::default()
            },
        );
        let at = view(now);
        assert_eq!(at.attention, Some(code), "{state}");
        assert_eq!(at.service, Ok(()), "{state} is not a service failure");
        assert_eq!(code.code(), format!("collector_{}", match state {
            "owner_api_refused" => "api_refused",
            "provider_refused" => "provider_refused",
            other => other,
        }));
    }
    // Collecting with a rate-limit wait, a paused car and an open outage is
    // healthy detail, in the established order, with no attention.
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            account: Some(CollectorAccountRow {
                rate_limited_until_ms: Some(CS_W0 + 60_000),
                ..cs_account("collecting", "cap_warning")
            }),
            vehicles: vec![cs_vehicle_row(vehicle, "unknown", "paused", "paused_legal_privacy")],
            gap_inserts: vec![CollectorGapInsert {
                vehicle_id: None,
                kind: "tesla_unreachable".to_owned(),
                started_at_ms: CS_W0,
                ended_at_ms: None,
                alert: true,
            }],
            ..CollectorRowsWrite::default()
        },
    );
    let healthy = view(now);
    assert_eq!(healthy.attention, None);
    assert_eq!(healthy.service, Ok(()));
    assert_eq!(
        healthy.detail,
        [
            "collector_cap_warning",
            "collector_rate_limited",
            "collector_vehicle_paused",
            "collector_tesla_unreachable",
            "collector_gap_unnotified"
        ]
    );
    assert!(
        !view(CS_W0 + 60_000).detail.contains(&"collector_rate_limited"),
        "the wait has ended"
    );
    // The durable audit failure on a collected car, and the live witness alone.
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            vehicles: vec![cs_vehicle_row(vehicle, "online", "none", "stream_audit_failed")],
            ..CollectorRowsWrite::default()
        },
    );
    let durable = view(now);
    assert_eq!(durable.attention, Some(Code::CollectorStreamAuditFailed));
    assert_eq!(durable.service, Ok(()));
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            vehicles: vec![cs_vehicle_row(vehicle, "online", "none", "none")],
            ..CollectorRowsWrite::default()
        },
    );
    assert_eq!(view(now).attention, None);
    store.set_stream_audit_failed_cars(1);
    let live = view(now);
    assert_eq!(live.attention, Some(Code::CollectorStreamAuditFailed));
    assert_eq!(live.service, Ok(()));
    store.set_stream_audit_failed_cars(0);
    // A stop on a Disabled car is not an audit failure.
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            vehicles: vec![cs_vehicle_row(vehicle, "unknown", "disabled", "stream_audit_failed")],
            ..CollectorRowsWrite::default()
        },
    );
    assert_eq!(view(now).attention, None);
}

#[test]
fn attention_is_suppressed_when_collection_is_disabled() {
    use crate::config::CollectorMode::FleetPolling;
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "1");
    // Stored stops, a durable audit failure, an identified unresolved Fleet
    // refresh and a live audit witness, with no lease at all.
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            account: Some(CollectorAccountRow {
                mode: "fleet_polling".to_owned(),
                ..cs_account("cap_reached", "cap_reached")
            }),
            vehicles: vec![cs_vehicle_row(vehicle, "online", "paused", "stream_audit_failed")],
            gap_inserts: vec![CollectorGapInsert {
                vehicle_id: None,
                kind: "hub_down".to_owned(),
                started_at_ms: CS_W0,
                ended_at_ms: Some(CS_W0 + 70_000),
                alert: true,
            }],
            ..CollectorRowsWrite::default()
        },
    );
    let generation = Uuid::from_u128(0xD15AB1ED);
    store
        .replace_fleet_tokens(
            &FleetTokenStore::new(
                b"a".to_vec(),
                b"r".to_vec(),
                "client".to_owned(),
                "eu".to_owned(),
                2_000,
                1_750,
                Some(generation),
            )
            .unwrap(),
        )
        .unwrap();
    store.begin_fleet_refresh(generation).unwrap();
    store.set_stream_audit_failed_cars(2);
    for context in [
        ReadinessContext::context_free(false, CS_W0),
        ReadinessContext::configured(false, CS_W0, FleetPolling, None),
        ReadinessContext::configured(
            false,
            CS_W0,
            FleetPolling,
            Some(crate::config::CadenceViolation {
                key: "collector.max_backoff_seconds",
                value: 600,
                rule: crate::config::CadenceRule::GreaterThanZero,
            }),
        ),
    ] {
        let view = store.collector_readiness_view(&context);
        assert_eq!(view.attention, None);
        assert!(view.detail.is_empty());
        assert!(!view.running);
        // No lease is demanded either.
        assert_eq!(view.service, Ok(()));
    }
    // The same store, collection required: everything shows.
    let required = store.collector_readiness_view(&ReadinessContext::configured(true, CS_W0, FleetPolling, None));
    assert_eq!(required.service, Err(ReadinessFailure { code: ReadinessReasonCode::CollectorAbsent }));
    assert_eq!(required.attention, Some(CollectionAttentionCode::CollectorFleetRefreshOutcomeUnknown));
}

/// Every collector table the wait must not touch, as text, gaps excluded.
fn cs_state_dump(store: &HubStore) -> String {
    let rows = store.load_collector_state_rows().unwrap();
    let auth = store.load_collector_auth_row().unwrap();
    format!(
        "{:?}|{:?}|{:?}|{:?}|{:?}",
        rows.account, rows.vehicles, rows.counters, rows.actions, auth
    )
}

fn cs_gap_dump(store: &HubStore) -> String {
    format!("{:?}", cs_gaps(store))
}

fn cs_wait_request(start_ms: i64, previous: Option<i64>, alert_ms: i64) -> CadenceWaitStartRequest {
    CadenceWaitStartRequest {
        start_ms,
        previous_heartbeat_ms: previous,
        gap_min_ms: 60_000,
        gap_alert_ms: alert_ms,
    }
}

#[test]
fn cadence_wait_gap_bookkeeping_preserves_collection_state() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "1");
    // A refusal, a hold, counts, a pending owner action and a credential status,
    // and a previous run that stopped (an open `hub_stopped` gap).
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            account: Some(cs_account("owner_api_refused", "owner_api_refused")),
            vehicles: vec![cs_vehicle_row(vehicle, "online", "paused", "paused_access_refused")],
            counters: vec![CollectorCounterRow {
                month: "2026-10".to_owned(),
                mode: "legacy".to_owned(),
                category: "data".to_owned(),
                sent: 7,
                billable: 0,
                reserved: 0,
            }],
            gap_inserts: vec![CollectorGapInsert {
                vehicle_id: None,
                kind: "hub_stopped".to_owned(),
                started_at_ms: CS_W0,
                ended_at_ms: None,
                alert: false,
            }],
            ..CollectorRowsWrite::default()
        },
    );
    store.insert_collector_owner_action("retry_collection", None, CS_W0).unwrap();
    store
        .write_collector_auth_row(&as_row("observer_waiting", Some("token_expired"), false), CS_W0)
        .unwrap();
    let state_before = cs_state_dump(&store);
    let gaps_before = cs_gap_dump(&store);

    // A failed transaction rolls back atomically: the earlier closing of the
    // previous stop is undone with the gap that could not be opened.
    store
        .open()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER cs_fail_wait BEFORE INSERT ON collection_gaps
                WHEN NEW.kind = 'hub_stopped'
                BEGIN SELECT RAISE(ABORT, 'injected commit fault'); END;",
        )
        .unwrap();
    assert!(store.begin_cadence_wait(&cs_wait_request(CS_W0 + 100_000, Some(CS_W0 + 40_000), 600_000)).is_err());
    assert_eq!(cs_gap_dump(&store), gaps_before, "nothing of a failed wait start stays");
    assert_eq!(cs_state_dump(&store), state_before);
    store.open().unwrap().execute_batch("DROP TRIGGER cs_fail_wait;").unwrap();

    // The start itself: the previous stop is closed, the wait's own stop opens,
    // and every other table is exactly as it was. The refusal is not lifted, the
    // hold is not cleared, counts and actions stay, and no `hub_down` is added.
    let started = store
        .begin_cadence_wait(&cs_wait_request(CS_W0 + 100_000, Some(CS_W0 + 40_000), 600_000))
        .unwrap();
    assert_eq!(started, CadenceWaitStart { stopped_gap_closed: true, hub_down_inserted: false });
    assert_eq!(cs_state_dump(&store), state_before);
    let gaps = cs_gaps(&store);
    assert_eq!(gaps.len(), 2);
    assert_eq!((gaps[0].kind.as_str(), gaps[0].ended_at_ms), ("hub_stopped", Some(CS_W0 + 100_000)));
    assert_eq!((gaps[1].kind.as_str(), gaps[1].started_at_ms, gaps[1].ended_at_ms), ("hub_stopped", CS_W0 + 100_000, None));
    assert!(gaps[1].alert, "a car was awake at the persisted phases");
    assert_eq!(store.pending_collector_owner_actions().unwrap().len(), 1);
}

#[test]
fn cadence_wait_gap_closes_only_on_a_later_collection_start() {
    // A stopped Hub whose cars slept, and one whose car was awake; each with the
    // configured timeout and with a zero timeout.
    for (awake, alert_ms) in [(false, 600_000_i64), (true, 600_000), (false, 0), (true, 0)] {
        let (_temporary, store) = cs_store();
        let vehicle = cs_vehicle(&store, "1");
        cs_write(
            &store,
            &CollectorRowsWrite {
                now_ms: CS_W0,
                account: Some(cs_account("collecting", "collecting")),
                vehicles: vec![cs_vehicle_row(vehicle, if awake { "online" } else { "asleep" }, "none", "none")],
                ..CollectorRowsWrite::default()
            },
        );
        // No preceding stop: only a heartbeat 5 minutes before the wait, so the
        // preceding downtime is recorded under the ordinary rule.
        let wait_at = CS_W0 + 400_000;
        let started = store
            .begin_cadence_wait(&cs_wait_request(wait_at, Some(wait_at - 300_000), alert_ms))
            .unwrap();
        assert!(started.hub_down_inserted && !started.stopped_gap_closed);
        let gaps = cs_gaps(&store);
        let kinds: Vec<&str> = gaps.iter().map(|gap| gap.kind.as_str()).collect();
        assert_eq!(kinds, ["hub_down", "hub_stopped"], "awake {awake} alert {alert_ms}");
        // The downtime alerts when a car was awake or it is at least the alert
        // length; the zero timeout meets the threshold at once.
        assert_eq!(gaps[0].alert, awake || 300_000 >= alert_ms, "hub_down awake {awake} alert {alert_ms}");
        assert_eq!(gaps[0].ended_at_ms, Some(wait_at));
        // The wait's stop alerts with the persisted awake phase, and is open.
        assert_eq!(gaps[1].alert, awake);
        assert_eq!(gaps[1].ended_at_ms, None);
        // An orderly end of the wait writes nothing: still one open stop. A
        // second wait start (a restart still in the fault) closes it and opens
        // its own, without a `hub_down`.
        let again = store
            .begin_cadence_wait(&cs_wait_request(wait_at + 30_000, Some(wait_at + 25_000), alert_ms))
            .unwrap();
        assert_eq!(again, CadenceWaitStart { stopped_gap_closed: true, hub_down_inserted: false });
        assert_eq!(cs_gaps(&store).iter().filter(|gap| gap.kind == "hub_stopped").count(), 2);
        // The corrected ordinary start closes the open wait gap, with no
        // duplicate `hub_down` and no second stop.
        let ordinary_at = wait_at + 30_000 + 700_000;
        let start = store
            .begin_collector_run(&CollectorRunStartRequest {
                mode: "legacy",
                config_fingerprint: CS_FP_A,
                start_ms: ordinary_at,
                previous_heartbeat_ms: Some(ordinary_at - 5_000),
                gap_min_ms: 60_000,
                gap_alert_ms: alert_ms,
            })
            .unwrap();
        assert!(start.stopped_gap_closed && !start.hub_down_inserted);
        let gaps = cs_gaps(&store);
        assert!(gaps.iter().all(|gap| gap.ended_at_ms.is_some()), "no gap stays open");
        assert_eq!(gaps.iter().filter(|gap| gap.kind == "hub_down").count(), 1);
        let closed = gaps.last().unwrap();
        assert_eq!(closed.kind, "hub_stopped");
        assert_eq!(closed.ended_at_ms, Some(ordinary_at));
        // Closed after 700 s: it alerts when it is at least the alert length
        // (always for the zero timeout), or when it opened awake.
        assert_eq!(closed.alert, awake || 700_000 >= alert_ms, "closed awake {awake} alert {alert_ms}");
    }
}

#[test]
fn an_unnotified_alert_gap_is_a_detail_until_it_is_acknowledged() {
    let (_temporary, store) = cs_store();
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            gap_inserts: vec![
                CollectorGapInsert {
                    vehicle_id: None,
                    kind: "hub_down".to_owned(),
                    started_at_ms: CS_W0,
                    ended_at_ms: Some(CS_W0 + 70_000),
                    alert: true,
                },
                CollectorGapInsert {
                    vehicle_id: None,
                    kind: "host_suspended".to_owned(),
                    started_at_ms: CS_W0 + 100_000,
                    ended_at_ms: Some(CS_W0 + 200_000),
                    alert: false,
                },
            ],
            ..CollectorRowsWrite::default()
        },
    );
    assert_eq!(store.collection_gap_counts().unwrap(), (0, 1));
    let readiness = store.collector_readiness_at(CS_W0).unwrap();
    assert_eq!(readiness.failure, None, "healthy: detail only");
    assert_eq!(readiness.detail, vec!["collector_gap_unnotified"]);
    assert_eq!(
        store.acknowledge_collection_gaps(CS_W0 + 300_000).unwrap(),
        1
    );
    let gaps = cs_gaps(&store);
    assert_eq!(gaps[0].notified_at_ms, Some(CS_W0 + 300_000));
    assert_eq!(
        gaps[1].notified_at_ms, None,
        "a non-alert gap is never notified"
    );
    assert!(
        store
            .collector_readiness_at(CS_W0)
            .unwrap()
            .detail
            .is_empty()
    );
    assert_eq!(
        store.acknowledge_collection_gaps(CS_W0 + 400_000).unwrap(),
        0
    );
}

#[test]
fn the_discovery_rule_stores_a_change_and_withholds_a_repeat() {
    use DiscoveryAction::{Deliver, Skip, WriteCurrentOnly};
    // A reachability state equal to the lifecycle's is withheld; with no current
    // row only that row is written.
    assert_eq!(
        discovery_action("asleep", Some("asleep"), Some("asleep")),
        Skip
    );
    assert_eq!(
        discovery_action("asleep", Some("asleep"), Some("online")),
        Skip
    );
    assert_eq!(
        discovery_action("asleep", Some("asleep"), None),
        WriteCurrentOnly
    );
    // A different or unknown last reachability delivers it.
    assert_eq!(
        discovery_action("online", Some("asleep"), Some("asleep")),
        Deliver
    );
    assert_eq!(discovery_action("asleep", None, None), Deliver);
    assert_eq!(
        discovery_action("offline", Some("online"), Some("offline")),
        Deliver
    );
    // A state that gives no reachability is stored only when it differs from the
    // current discovery row's.
    assert_eq!(discovery_action("odd", Some("online"), Some("odd")), Skip);
    assert_eq!(
        discovery_action("odd", Some("online"), Some("online")),
        Deliver
    );
    assert_eq!(discovery_action("odd", None, None), Deliver);
}

#[test]
fn the_lifecycle_snapshot_reads_the_view_the_discovery_rule_needs() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "1");
    let snapshot = |store: &HubStore| store.collector_lifecycle_snapshot(vehicle).unwrap();
    assert_eq!(snapshot(&store), CollectorLifecycleSnapshot::default());
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            CS_W0,
        )
        .unwrap();
    let discovery = |state: &str, observed_at_ms: i64| ObservationInput {
        source_id: source.source_id,
        vehicle_id: vehicle,
        observed_at_ms,
        payload: serde_json::json!({
            "record_type": "owner_api_discovery_v1",
            "source_vehicle_id": "1",
            "source_vehicle_state": state,
        }),
    };
    store
        .accept_owner_observation_and_lifecycle(&discovery("asleep", CS_W0 + 10), CS_W0 + 11, 1)
        .unwrap();
    let view = snapshot(&store);
    assert_eq!(view.last_reach.as_deref(), Some("asleep"));
    assert!(!view.drive_open);
    assert_eq!(view.current_discovery_state.as_deref(), Some("asleep"));
    assert_eq!(
        view.last_stored_obs_ms,
        Some(CS_W0 + 11),
        "received time, not observed"
    );
    // The current row alone, with no lifecycle reach, is visible too.
    store
        .open()
        .unwrap()
        .execute("DELETE FROM current_observations", [])
        .unwrap();
    let view = snapshot(&store);
    assert_eq!(view.current_discovery_state, None);
    assert_eq!(view.last_stored_obs_ms, None);
    assert_eq!(
        view.last_reach.as_deref(),
        Some("asleep"),
        "the lifecycle keeps it"
    );
}

// ---- B2 P3: the refusal decision and the C5 lift of the ordinary start ------------------------

fn cs_p3_request(
    mode: &'static str,
    fingerprint: &'static str,
    start_ms: i64,
) -> CollectorRunStartRequest<'static> {
    CollectorRunStartRequest {
        mode,
        config_fingerprint: fingerprint,
        start_ms,
        previous_heartbeat_ms: None,
        gap_min_ms: 65_000,
        gap_alert_ms: 600_000,
    }
}

/// A C5 `api_refused` row stopped in `stopped_mode`, with a fuse and a skew.
fn cs_p3_refused_row(stopped_mode: &str) -> CollectorAuthRow {
    CollectorAuthRow {
        state: "api_refused".to_owned(),
        reason: Some("client".to_owned()),
        collecting: false,
        retry_at: Some(AS_T0 + 600),
        stopped_generation: None,
        stopped_mode: Some(stopped_mode.to_owned()),
        fuse_consecutive: 2,
        fuse_generation: Some(as_gen(9)),
        fuse_last_rejection_at: Some(AS_T0 - 30),
        fuse_pause_until: None,
        fuse_rejected_access_digest: None,
        clock_skew: true,
    }
}

/// The write time of the C5 row, which `CollectorAuthRow` does not carry.
fn cs_p3_auth_written_ms(store: &HubStore) -> Option<i64> {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT updated_at_ms FROM collector_auth_state WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

#[test]
fn cs_p3_refusal_lift_conditions() {
    // (c3 mode, c3 fingerprint, c3 state; c5 stopped mode of an api_refused row;
    // requested mode and fingerprint; expected (account_reset, refusal_reset)).
    #[allow(clippy::type_complexity)]
    let matrix: &[(
        Option<(&str, &str, &str)>,
        Option<&str>,
        (&'static str, &'static str),
        (bool, bool),
        &str,
    )] = &[
        (Some(("legacy", CS_FP_A, "owner_api_refused")), None, ("legacy", CS_FP_A), (false, false), "same refusal, same start"),
        (Some(("legacy", CS_FP_A, "owner_api_refused")), None, ("legacy", CS_FP_B), (true, true), "(i) owner refusal, fingerprint"),
        (Some(("provider", CS_FP_A, "provider_refused")), None, ("provider", CS_FP_B), (true, true), "(i) provider refusal, fingerprint"),
        (Some(("legacy", CS_FP_A, "owner_api_refused")), None, ("provider", CS_FP_A), (true, true), "(i)/(ii) refusal, mode"),
        (Some(("legacy", CS_FP_A, "account_locked")), None, ("legacy", CS_FP_B), (true, false), "same-mode account_locked: account reset only"),
        (Some(("legacy", CS_FP_A, "account_locked")), None, ("fleet_polling", CS_FP_A), (true, true), "(ii) another state, mode"),
        (Some(("legacy", CS_FP_A, "collecting")), None, ("provider", CS_FP_A), (false, true), "(ii) collecting row, mode"),
        (Some(("legacy", CS_FP_A, "collecting")), None, ("legacy", CS_FP_B), (false, false), "collecting row, fingerprint only"),
        (Some(("fleet_polling", CS_FP_A, "cap_reached")), None, ("fleet_polling", CS_FP_B), (true, false), "fleet cap stop, fingerprint only"),
        (None, Some("legacy"), ("provider", CS_FP_A), (false, true), "(iii) no C3 row, C5 refusal of another mode"),
        (None, Some("legacy"), ("fleet_polling", CS_FP_A), (false, true), "(iii) no C3 row, C5 refusal, Fleet"),
        (None, Some("legacy"), ("legacy", CS_FP_A), (false, false), "no C3 row, C5 refusal of the same mode"),
        (None, None, ("provider", CS_FP_A), (false, false), "no rows at all"),
        (Some(("legacy", CS_FP_A, "collecting")), Some("provider"), ("legacy", CS_FP_A), (false, false), "a C3 row exists: (iii) does not apply"),
    ];
    for (c3, c5_refusal, (mode, fingerprint), (account_reset, refusal_reset), label) in matrix {
        let (_temporary, store) = cs_store();
        if let Some((c3_mode, c3_fingerprint, state)) = c3 {
            cs_write(
                &store,
                &CollectorRowsWrite {
                    now_ms: CS_W0,
                    account: Some(CollectorAccountRow {
                        mode: (*c3_mode).to_owned(),
                        config_fingerprint: (*c3_fingerprint).to_owned(),
                        ..cs_account(state, "seeded")
                    }),
                    ..CollectorRowsWrite::default()
                },
            );
        }
        if let Some(stopped_mode) = c5_refusal {
            store
                .write_collector_auth_row(&cs_p3_refused_row(stopped_mode), AS_T0 * 1_000)
                .unwrap();
        }
        let started = store
            .begin_collector_run(&cs_p3_request(mode, fingerprint, CS_W0 + 50_000))
            .unwrap();
        assert_eq!(
            (started.account_reset, started.refusal_reset),
            (*account_reset, *refusal_reset),
            "{label}"
        );
        // The durable C5 refusal is lifted exactly when the decision says so.
        let lifted = c5_refusal.is_some()
            && store.load_collector_auth_row().unwrap().unwrap().state == "ready";
        assert_eq!(lifted, c5_refusal.is_some() && *refusal_reset, "{label}");
    }
}

#[test]
fn cs_p3_lift_row_is_neutral_and_preserves_fuse() {
    let (_temporary, store) = cs_store();
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            account: Some(cs_account("collecting", "collecting")),
            ..CollectorRowsWrite::default()
        },
    );
    store
        .write_collector_auth_row(&cs_p3_refused_row("legacy"), AS_T0 * 1_000)
        .unwrap();
    assert_eq!(cs_p3_auth_written_ms(&store), Some(AS_T0 * 1_000));
    // Provider mode differs from the stored Legacy mode: the ordinary start lifts.
    let start_ms = CS_W0 + 50_000;
    let started = store
        .begin_collector_run(&cs_p3_request("provider", CS_FP_A, start_ms))
        .unwrap();
    assert!(started.refusal_reset);
    let lifted = store.load_collector_auth_row().unwrap().unwrap();
    assert_eq!(
        lifted,
        CollectorAuthRow {
            state: "ready".to_owned(),
            reason: None,
            collecting: true,
            retry_at: None,
            stopped_generation: None,
            stopped_mode: None,
            // The fuse and the skew flag are C5's and stay.
            fuse_consecutive: 2,
            fuse_generation: Some(as_gen(9)),
            fuse_last_rejection_at: Some(AS_T0 - 30),
            fuse_pause_until: None,
            fuse_rejected_access_digest: None,
            clock_skew: true,
        }
    );
    assert_eq!(cs_p3_auth_written_ms(&store), Some(start_ms), "the start's timestamp");
    // The row decodes as the unit's own ready status, so the next restore reads it.
    let status = crate::legacy_auth::state::AuthStatus::from_row(&lifted).expect("a valid status");
    assert!(status.is_consistent());
    assert_eq!(status.stop(), None);
    assert!(status.collecting);

    // Another state is left as it is, and a missing row is not inserted.
    for (row, label) in [
        (
            CollectorAuthRow {
                stopped_generation: Some(as_gen(4)),
                ..as_row("signed_out", Some("refresh_outcome_unknown"), false)
            },
            "signed_out",
        ),
        (
            CollectorAuthRow {
                stopped_generation: Some(as_gen(4)),
                ..as_row("sign_in_required", Some("refresh_token_rejected"), true)
            },
            "sign_in_required",
        ),
        (as_row("ready", None, true), "ready"),
    ] {
        let (_temporary, store) = cs_store();
        cs_write(
            &store,
            &CollectorRowsWrite {
                now_ms: CS_W0,
                account: Some(cs_account("collecting", "collecting")),
                ..CollectorRowsWrite::default()
            },
        );
        store.write_collector_auth_row(&row, AS_T0 * 1_000).unwrap();
        let before = store.load_collector_auth_row().unwrap();
        assert!(
            store
                .begin_collector_run(&cs_p3_request("provider", CS_FP_A, start_ms))
                .unwrap()
                .refusal_reset
        );
        assert_eq!(store.load_collector_auth_row().unwrap(), before, "{label}");
        assert_eq!(cs_p3_auth_written_ms(&store), Some(AS_T0 * 1_000), "{label}");
    }
    let (_temporary, store) = cs_store();
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            account: Some(cs_account("collecting", "collecting")),
            ..CollectorRowsWrite::default()
        },
    );
    assert!(
        store
            .begin_collector_run(&cs_p3_request("fleet_polling", CS_FP_A, start_ms))
            .unwrap()
            .refusal_reset
    );
    assert_eq!(store.load_collector_auth_row().unwrap(), None, "no row is inserted");
}

#[test]
fn cs_p3_lift_and_c3_reset_roll_back_together() {
    let (_temporary, store) = cs_store();
    let paused = cs_vehicle(&store, "1");
    cs_write(
        &store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            account: Some(CollectorAccountRow {
                rate_limited_until_ms: Some(CS_W0 + 100_000),
                ..cs_account("provider_refused", "provider_gone")
            }),
            vehicles: vec![cs_vehicle_row(paused, "unknown", "paused", "paused_access_refused")],
            gap_inserts: vec![
                CollectorGapInsert {
                    vehicle_id: None,
                    kind: "provider_refused".to_owned(),
                    started_at_ms: CS_W0,
                    ended_at_ms: None,
                    alert: false,
                },
                CollectorGapInsert {
                    vehicle_id: Some(paused),
                    kind: "vehicle_paused".to_owned(),
                    started_at_ms: CS_W0,
                    ended_at_ms: None,
                    alert: false,
                },
            ],
            ..CollectorRowsWrite::default()
        },
    );
    store
        .write_collector_auth_row(&cs_p3_refused_row("legacy"), AS_T0 * 1_000)
        .unwrap();
    let state_before = cs_state_dump(&store);
    let gaps_before = cs_gap_dump(&store);

    // An abort in the C5 update rolls the C3 reset back with it: the account, the
    // hold, both gaps and the C5 row are all as they were.
    store
        .open()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER cs_p3_fail_lift BEFORE UPDATE ON collector_auth_state
                BEGIN SELECT RAISE(ABORT, 'injected lift fault'); END;",
        )
        .unwrap();
    assert!(
        store
            .begin_collector_run(&cs_p3_request("legacy", CS_FP_B, CS_W0 + 50_000))
            .is_err()
    );
    assert_eq!(cs_state_dump(&store), state_before);
    assert_eq!(cs_gap_dump(&store), gaps_before);
    store
        .open()
        .unwrap()
        .execute_batch("DROP TRIGGER cs_p3_fail_lift;")
        .unwrap();

    // Without the fault both commit together.
    let started = store
        .begin_collector_run(&cs_p3_request("legacy", CS_FP_B, CS_W0 + 50_000))
        .unwrap();
    assert!(started.account_reset && started.refusal_reset);
    assert_eq!(started.holds_cleared, 1);
    let rows = store.load_collector_state_rows().unwrap();
    assert_eq!(rows.account.unwrap().state, "collecting");
    assert_eq!(rows.vehicles[0].hold, "none");
    assert!(rows.open_gaps.is_empty());
    let auth = store.load_collector_auth_row().unwrap().unwrap();
    assert_eq!((auth.state.as_str(), auth.collecting), ("ready", true));
    assert_eq!(cs_p3_auth_written_ms(&store), Some(CS_W0 + 50_000));
}
