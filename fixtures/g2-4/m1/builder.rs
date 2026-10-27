// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

#[test]
#[ignore = "writes synthetic schema fixtures under routed scratch"]
fn g2_4_m1_build_stepwise_fixtures() {
    let output = PathBuf::from(std::env::var("G2_4_M1_FIXTURE_SCRATCH").expect("fixture scratch"));
    let scratch = PathBuf::from(std::env::var("TMPDIR").expect("routed scratch"));
    assert!(output.starts_with(scratch));
    fs::create_dir_all(&output).unwrap();
    let previous = output.join("store-f-minus-one.sqlite");
    let connection = Connection::open(&previous).unwrap();
    configure(&connection).unwrap();
    migrate_to_previous_import_names(&connection).unwrap();
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION - 1);
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(connection);
    let root = output.join("seed-store");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let current = root.join("hub.sqlite");
    fs::copy(&previous, &current).unwrap();
    fs::set_permissions(&current, fs::Permissions::from_mode(0o600)).unwrap();
    let connection = Connection::open(&current).unwrap();
    configure(&connection).unwrap();
    migrate(&connection).unwrap();
    assert_eq!(schema_version(&connection).unwrap(), SCHEMA_VERSION);
    let mut statement = connection
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY rowid")
        .unwrap();
    let schema = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    fs::write(
        output.join("schema-raw.json"),
        serde_json::to_vec_pretty(&schema).unwrap(),
    )
    .unwrap();
    drop(statement);
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    drop(connection);
    let store = HubStore::initialize(&root).unwrap();
    let live = "10000000-0000-4000-8000-000000000001";
    let imported = "10000000-0000-4000-8000-000000000002";
    let vehicles = [
        "20000000-0000-4000-8000-000000000001",
        "20000000-0000-4000-8000-000000000002",
        "20000000-0000-4000-8000-000000000003",
    ];
    let connection = store.open().unwrap();
    for (source, kind, key) in [
        (live, "owner_api", "live"),
        (imported, "source_db", "default"),
    ] {
        connection
            .execute(
                "INSERT INTO sources VALUES (?1,?2,1,0)",
                params![source, kind],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO source_identities VALUES (?1,?2,?3)",
                params![source, kind, key],
            )
            .unwrap();
    }
    for (index, vehicle) in vehicles.iter().enumerate() {
        let source = if index == 1 { imported } else { live };
        connection.execute("INSERT INTO vehicles(vehicle_id,source_id,source_vehicle_key,created_at_ms,last_seen_at_ms) VALUES (?1,?2,?3,0,0)",params![vehicle,source,format!("car-{}",index+1)]).unwrap();
    }
    connection.execute("INSERT INTO vehicle_identity_aliases VALUES ('source_key','migrated-import',?1,?2,'car-3')",params![vehicles[2],imported]).unwrap();
    for vehicle in &vehicles[1..] {
        for (domain, maximum) in [("positions", 19), ("charging_processes", 7)] {
            connection
                .execute(
                    "INSERT INTO lifecycle_source_watermarks VALUES (?1,?2,?3,?4,?5)",
                    params![imported, vehicle, domain, maximum, T69_T0],
                )
                .unwrap();
        }
    }
    drop(connection);
    let migrated = vehicles[2].parse().unwrap();
    store
        .seed_imported_open_session(
            imported.parse().unwrap(),
            migrated,
            1,
            &t69_s((Some(7), Some(T69_T0))),
            T69_T0,
        )
        .unwrap();
    let state = store.load_lifecycle_state(migrated).unwrap().unwrap();
    let state = crate::lifecycle::OpenSessionState::decode(&state.open_session_json).unwrap();
    let sample = cost_ob(
        1,
        T69_T0 - 9 * T69_MIN,
        "Charging",
        serde_json::json!({"charge_energy_added":3.0,"charger_power":11,"charger_voltage":230,"charger_actual_current":16,"charger_phases":3}),
    );
    let step = crate::lifecycle::apply_sample(state, 1, &sample).unwrap();
    assert_eq!(step.state.open_charge.as_ref().unwrap().id, 7);
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: migrated,
            car_id: 1,
            open_session_json: &step.state.encode().unwrap(),
            last_observation_id: 1,
            quarantined: step.quarantined,
            updated_at_ms: T69_T0 + T69_MIN,
            delta: &step.delta,
        })
        .unwrap();
    store.checkpoint_catalogue_for_immutable_read().unwrap();
    fs::copy(&current, output.join("store-f.sqlite")).unwrap();
}
