// SPDX-License-Identifier: AGPL-3.0-only

// Receiver state (schema 73) and the retirement of the delivery consumer's
// tables (schema 74). Every store here is a temporary synthetic one.

/// The rows of one table as strings, sorted, so two snapshots compare exactly.
fn rr_rows(connection: &Connection, table: &str) -> Vec<Vec<String>> {
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

#[test]
fn accumulator_state_table_enforces_version_bounds_and_cascade() {
    let (temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "rr-state");
    let connection = store.open().unwrap();
    let insert = |version: i64, length: usize| {
        connection.execute(
            "INSERT INTO fleet_telemetry_accumulator_states(
                vehicle_id, state_version, state_json, updated_at_ms)
             VALUES (?1, ?2, ?3, 1)",
            rusqlite::params![vehicle.to_string(), version, vec![b'x'; length]],
        )
    };
    assert!(insert(1, 10).is_err(), "version one is refused");
    assert!(insert(3, 10).is_err(), "version three is refused");
    assert!(insert(2, 1).is_err(), "one byte is too short");
    assert!(insert(2, 262_145).is_err(), "262145 bytes is too long");
    assert_eq!(insert(2, 262_144).unwrap(), 1);
    connection
        .execute("DELETE FROM fleet_telemetry_accumulator_states", [])
        .unwrap();
    assert_eq!(insert(2, 2).unwrap(), 1);
    // The store accessors round-trip the bytes and only upsert version two.
    assert_eq!(
        store.load_receiver_state(vehicle).unwrap(),
        Some(vec![b'x'; 2])
    );
    let mut other = store.open().unwrap();
    let transaction = other.transaction().unwrap();
    HubStore::put_receiver_state_in_transaction(&transaction, vehicle, b"{\"a\":1}", 9).unwrap();
    transaction.commit().unwrap();
    assert_eq!(
        store.load_receiver_state(vehicle).unwrap(),
        Some(b"{\"a\":1}".to_vec())
    );
    // Deleting the vehicle removes its state.
    connection
        .execute(
            "DELETE FROM vehicles WHERE vehicle_id = ?1",
            [vehicle.to_string()],
        )
        .unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM fleet_telemetry_accumulator_states",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    drop(temporary);
}

// ---- receiver event retention (6A S5) ----

fn rr_event(source: Uuid, vehicle: Uuid, observed_at_ms: i64, txid: &str) -> ObservationInput {
    ObservationInput {
        source_id: source,
        vehicle_id: vehicle,
        observed_at_ms,
        payload: serde_json::json!({
            "record_type": RECEIVER_EVENT_RECORD_TYPE,
            "tx_type": "alerts",
            "txid": txid,
            "envelope_received_at_ms": observed_at_ms + 5,
            "timestamp_ms": observed_at_ms,
            "payload": {"name": "synthetic"},
        }),
    }
}

fn rr_source_of(store: &HubStore, vehicle: Uuid) -> Uuid {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT source_id FROM vehicles WHERE vehicle_id = ?1",
            [vehicle.to_string()],
            |row| row.get::<_, String>(0),
        )
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn receiver_event_rows_have_no_current_row_and_survive_the_processed_row_prune() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "rr-events");
    let source = rr_source_of(&store, vehicle);
    let mut connection = store.open().unwrap();
    let transaction = connection.transaction().unwrap();
    let event = HubStore::append_event_observation_in_transaction(
        &transaction,
        &rr_event(source, vehicle, 1_800_000_000_000, "tx-1"),
        1_800_000_000_900,
    )
    .unwrap();
    assert!(event.inserted);
    assert_eq!(event.observation.observed_at_ms, 1_800_000_000_000);
    assert_eq!(event.observation.received_at_ms, 1_800_000_000_900);
    // A processing row, as the lifecycle consumes it.
    let processed = append_observation_in_transaction(
        &transaction,
        &ObservationInput {
            source_id: source,
            vehicle_id: vehicle,
            observed_at_ms: 1_800_000_001_000,
            payload: serde_json::json!({
                "record_type": "fleet_api_vehicle_data_v1",
                "provider_raw_json": {"response": {"charge_state": {"battery_level": 55}}},
            }),
        },
        1_800_000_001_100,
    )
    .unwrap();
    // A replay of the same event inserts nothing, whatever its admission time.
    let replay = HubStore::append_event_observation_in_transaction(
        &transaction,
        &rr_event(source, vehicle, 1_800_000_000_000, "tx-1"),
        1_800_000_005_000,
    )
    .unwrap();
    assert!(!replay.inserted);
    assert_eq!(replay.observation.received_at_ms, 1_800_000_000_900);
    // Only the processing row is a current row; the event has none.
    let current: Vec<String> = transaction
        .prepare("SELECT record_type FROM current_observations WHERE vehicle_id = ?1")
        .unwrap()
        .query_map([vehicle.to_string()], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(current, vec!["fleet_api_vehicle_data_v1".to_owned()]);
    // The prune after projection takes the processing row and keeps the event.
    prune_processed_observations(&transaction, vehicle, processed.observation.observation_id)
        .unwrap();
    transaction.commit().unwrap();
    let remaining: Vec<(i64, String)> = connection
        .prepare(
            "SELECT observation_id, json_extract(payload_json, '$.record_type')
               FROM raw_observations WHERE vehicle_id = ?1 ORDER BY observation_id",
        )
        .unwrap()
        .query_map([vehicle.to_string()], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        remaining,
        vec![(
            event.observation.observation_id,
            RECEIVER_EVENT_RECORD_TYPE.to_owned()
        )],
        "the event outlives the prune; the processing row does not"
    );
}

#[test]
fn receiver_event_append_refuses_any_other_record_type() {
    let (_temporary, store) = cs_store();
    let vehicle = cs_vehicle(&store, "rr-events-type");
    let source = rr_source_of(&store, vehicle);
    let mut connection = store.open().unwrap();
    let transaction = connection.transaction().unwrap();
    let mut input = rr_event(source, vehicle, 1_800_000_000_000, "tx-1");
    input.payload["record_type"] = serde_json::json!("fleet_api_vehicle_data_v1");
    assert!(matches!(
        HubStore::append_event_observation_in_transaction(&transaction, &input, 1_800_000_000_900),
        Err(StoreError::InvalidEventRecord)
    ));
}

// ---- export-outbox enumeration (6A S9) ----

#[test]
fn export_outbox_pages_are_ordered_keyset_and_bounded() {
    let (_temporary, store) = cs_store();
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            CS_W0,
        )
        .unwrap();
    let total = EXPORT_OUTBOX_VEHICLE_PAGE + 44;
    let mut expected = Vec::new();
    for index in 0..total {
        let id = Uuid::from_u128(0x1000_0000_0000_4000_8000_0000_0000_0000 + index as u128);
        store
            .register_vehicle_with_id(
                &VehicleDescriptor::new(source.source_id, format!("rr-outbox-{index}")),
                CS_W0,
                id,
            )
            .unwrap();
        expected.push(id);
    }
    let connection = store.open().unwrap();
    for id in &expected {
        connection
            .execute(
                "INSERT INTO export_outbox(vehicle_id, dirty_revision) VALUES (?1, 1)",
                [id.to_string()],
            )
            .unwrap();
    }
    let before = connection
        .query_row("SELECT COUNT(*) FROM export_outbox", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    // Whatever the caller asks, a page holds at most 256 vehicles.
    let first = store.export_outbox_vehicle_page(None, 10_000).unwrap();
    assert_eq!(first.len(), EXPORT_OUTBOX_VEHICLE_PAGE);
    assert_eq!(first, expected[..EXPORT_OUTBOX_VEHICLE_PAGE]);
    let second = store
        .export_outbox_vehicle_page(first.last().copied(), 10_000)
        .unwrap();
    assert_eq!(second, expected[EXPORT_OUTBOX_VEHICLE_PAGE..]);
    assert!(
        store
            .export_outbox_vehicle_page(second.last().copied(), 256)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.export_outbox_vehicle_page(None, 3).unwrap(),
        expected[..3]
    );
    // The enumeration claims, completes and fails nothing.
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM export_outbox", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        before
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM export_outbox
                  WHERE dirty_revision <> 1 OR attempts <> 0 OR claimed_until_ms <> 0
                     OR last_error IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

// ---- read-only identity resolution (6A S5, C-6) ----

fn rr_fleet_source() -> SourceDescriptor {
    SourceDescriptor::new("fleet_api_compat", "local_installation_v1")
}

fn rr_fleet_descriptor(vin: &str) -> VehicleDescriptor {
    let mut descriptor =
        VehicleDescriptor::new(Uuid::nil(), "70").with_tesla_identity(Some(70), None);
    descriptor.vin = Some(vin.to_owned());
    descriptor
}

/// The sources, vehicles and aliases of the store, to prove a lookup wrote none.
fn rr_identity_state(connection: &Connection) -> Vec<Vec<Vec<String>>> {
    [
        "sources",
        "source_identities",
        "vehicles",
        "vehicle_identity_aliases",
    ]
    .iter()
    .map(|table| rr_rows(connection, table))
    .collect()
}

#[test]
fn registered_vehicle_unchanged_reads_without_registering_or_writing() {
    const VIN: &str = "5YJ3E1EA7KF000001";
    let (_temporary, store) = cs_store();
    // Nothing registered yet: nothing is unchanged.
    assert_eq!(
        store
            .registered_vehicle_unchanged(&rr_fleet_source(), &rr_fleet_descriptor(VIN))
            .unwrap(),
        None
    );
    let source = store.register_source(&rr_fleet_source(), CS_W0).unwrap();
    let mut descriptor = rr_fleet_descriptor(VIN);
    descriptor.source_id = source.source_id;
    let registered = store.register_vehicle(&descriptor, CS_W0).unwrap();

    let state_before = rr_identity_state(&store.open().unwrap());
    store.instrumentation().reset();
    let sinks_before = store.sink_attempts();
    let found = store
        .registered_vehicle_unchanged(&rr_fleet_source(), &rr_fleet_descriptor(VIN))
        .unwrap()
        .expect("registered identity is unchanged");
    assert_eq!(found.vehicle_id, registered.vehicle_id);
    assert_eq!(found.source_id, source.source_id);
    assert_eq!(found.source_generation, source.generation);
    let counts = store.instrumentation().snapshot();
    assert_eq!(counts.registration_calls(), 0, "no registration call");
    assert_eq!(counts.writable_opens, 0, "one read-only connection only");
    assert_eq!(counts.read_only_opens, 1);
    assert_eq!(store.sink_attempts(), sinks_before, "no write attempted");
    assert_eq!(rr_identity_state(&store.open().unwrap()), state_before);
}

#[test]
fn registered_vehicle_unchanged_is_none_for_every_difference_registration_would_write() {
    const VIN: &str = "5YJ3E1EA7KF000001";
    let setup = || {
        let (temporary, store) = cs_store();
        let source = store.register_source(&rr_fleet_source(), CS_W0).unwrap();
        let mut descriptor = rr_fleet_descriptor(VIN);
        descriptor.source_id = source.source_id;
        let registered = store.register_vehicle(&descriptor, CS_W0).unwrap();
        (temporary, store, registered.vehicle_id)
    };
    let unchanged = |store: &HubStore, vin: &str| {
        store
            .registered_vehicle_unchanged(&rr_fleet_source(), &rr_fleet_descriptor(vin))
            .unwrap()
    };

    // A required alias is gone: source key, Tesla EID or VIN.
    for kind in ["source_key", "tesla_eid", "vin"] {
        let (_temporary, store, _) = setup();
        assert!(unchanged(&store, VIN).is_some());
        store
            .open()
            .unwrap()
            .execute(
                "DELETE FROM vehicle_identity_aliases WHERE alias_kind = ?1",
                [kind],
            )
            .unwrap();
        assert_eq!(unchanged(&store, VIN), None, "missing {kind} alias");
    }

    // The stored VIN is absent or differs.
    for stored in [None, Some("5YJ3E1EA7KF000009")] {
        let (_temporary, store, vehicle) = setup();
        store
            .open()
            .unwrap()
            .execute(
                "UPDATE vehicles SET vin = ?1 WHERE vehicle_id = ?2",
                rusqlite::params![stored, vehicle.to_string()],
            )
            .unwrap();
        assert_eq!(unchanged(&store, VIN), None, "stored VIN {stored:?}");
    }

    // A second EID alias on the vehicle: registration would delete it.
    let (_temporary, store, vehicle) = setup();
    store
        .open()
        .unwrap()
        .execute(
            "INSERT INTO vehicle_identity_aliases(alias_kind, alias_value, vehicle_id, source_id,
                source_vehicle_key)
             SELECT 'tesla_eid', '71', vehicle_id, source_id, source_vehicle_key
               FROM vehicles WHERE vehicle_id = ?1",
            [vehicle.to_string()],
        )
        .unwrap();
    assert_eq!(unchanged(&store, VIN), None, "stale EID alias");

    // An alias that belongs to another vehicle is a conflict, never unchanged.
    let (_temporary, store, vehicle) = setup();
    let other = store
        .register_vehicle(
            &VehicleDescriptor::new(
                store
                    .register_source(&rr_fleet_source(), CS_W0)
                    .unwrap()
                    .source_id,
                "other-key",
            ),
            CS_W0,
        )
        .unwrap();
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE vehicle_identity_aliases SET vehicle_id = ?1
              WHERE alias_kind = 'vin' AND vehicle_id = ?2",
            [other.vehicle_id.to_string(), vehicle.to_string()],
        )
        .unwrap();
    assert_eq!(unchanged(&store, VIN), None, "conflicting alias");
}
