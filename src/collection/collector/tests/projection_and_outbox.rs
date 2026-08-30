// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn compatibility_collection_publishes_a_real_car_only_phone_snapshot() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let collected_at_ms = 1_800_000_000_000;
    let collection = ManualCollection {
        vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({
                "drive_state": {"timestamp": collected_at_ms - 1},
                "vehicle_config": {"car_type": "model3"},
                "vehicle_state": {"car_version": "2026.20"}
            }),
        )],
        failures: vec![],
    };

    persist_collection(&store, &collection, collected_at_ms).expect("raw observation");
    materialise_lifecycle_for_collection(&store, &collection, collected_at_ms).expect("lifecycle");
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let published = publish_compatibility_snapshots(
        &store,
        &publication_gate,
        &CursorKey::from_bytes([7; 32]),
        &collection,
        collected_at_ms,
    )
    .expect("typed projection");

    assert_eq!(published, 1);
    let vehicle_id = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("stored UUID");
    let manifest = store
        .manifest_for_vehicle(vehicle_id)
        .expect("manifest query")
        .expect("published manifest");
    assert_eq!(manifest.chunk_count, 1);
    assert_eq!(manifest.total_rows, 2);
    assert_eq!(
        manifest.chunks[0].tables,
        vec![
            crate::protocol::MirrorTable::Car,
            crate::protocol::MirrorTable::State,
            crate::protocol::MirrorTable::Update,
        ]
    );
    assert_eq!(store.published_vehicles().expect("published cars").len(), 1);
}

#[test]
fn fleet_collection_round_trips_sanitized_provider_raw_json_without_duplication() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let raw = json!({
        "response": {
            "drive_state": {"shift_state": "P", "timestamp": 1_800_000_000_000_i64},
            "charge_state": {
                "battery_level": 80,
                "charge_limit_soc": 90,
                "future_secret_name": "secret"
            },
            "vehicle_state": {
                "software_update": {
                    "status": "available",
                    "version": "2026.20",
                    "expected_duration_sec": 900
                }
            },
            "unknown_group": {"battery_level": 1}
        },
        "provider_trace": "fleet-trace"
    });
    let collection = ManualCollection {
        vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
        snapshots: vec![
            VehicleData::from_provider_raw_json(VehicleId::from_test(9), raw.clone())
                .expect("Fleet response"),
        ],
        failures: vec![],
    };

    persist_collection_atomic_for_provider(
        &store,
        &collection,
        1_800_000_000_001,
        CollectorProvider::Fleet,
    )
    .expect("Fleet raw observation persists");
    let vehicle_id = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("vehicle UUID");
    let observations = store
        .current_observations_for_vehicle(vehicle_id)
        .expect("current Fleet observation");
    let fleet = observations
        .iter()
        .find(|observation| observation.payload["record_type"] == "fleet_api_vehicle_data_v1")
        .expect("Fleet current observation");
    assert_eq!(
        fleet.payload["provider_raw_json"],
        json!({
            "response": {
                "drive_state": {
                    "shift_state": "P",
                    "timestamp": 1_800_000_000_000_i64
                },
                "charge_state": {"battery_level": 80, "charge_limit_soc": 90},
                "vehicle_state": {
                    "software_update": {
                        "status": "available",
                        "version": "2026.20"
                    }
                }
            }
        })
    );
    let rendered = fleet.payload["provider_raw_json"].to_string();
    for rejected in [
        "provider_trace",
        "unknown_group",
        "future_secret_name",
        "expected_duration_sec",
        "fleet-trace",
        "secret",
    ] {
        assert!(!rendered.contains(rejected), "field survived: {rejected}");
    }
    assert!(fleet.payload.get("vehicle_data").is_none());
}

#[test]
fn fleet_atomic_collection_closes_located_drive_after_restart_without_duplicates() {
    let temp = crate::private_tempdir().expect("temporary store");
    let t0 = 1_800_000_100_000_i64;
    let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
    let collection = |vehicle_data: serde_json::Value| ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![
            VehicleData::from_provider_raw_json(
                VehicleId::from_test(9),
                json!({"response": vehicle_data}),
            )
            .expect("Fleet response"),
        ],
        failures: vec![],
    };

    let store = HubStore::initialize(temp.path()).expect("store");
    let first = collection(json!({
        "drive_state": {
            "shift_state": "D",
            "speed": 20,
            "latitude": 47.5,
            "longitude": 19.0,
            "timestamp": t0
        },
        "vehicle_state": {"odometer": 1000.0}
    }));
    persist_collection_atomic_for_provider(&store, &first, t0, CollectorProvider::Fleet)
        .expect("open Fleet drive");
    drop(store);

    let store = HubStore::initialize(temp.path()).expect("restart store");
    let second = collection(json!({
        "drive_state": {
            "shift_state": "D",
            "speed": 30,
            "latitude": 47.51,
            "longitude": 19.01,
            "timestamp": t0 + 60_000
        },
        "vehicle_state": {"odometer": 1001.0}
    }));
    persist_collection_atomic_for_provider(&store, &second, t0 + 60_000, CollectorProvider::Fleet)
        .expect("continue Fleet drive");

    let terminal = collection(json!({
        "drive_state": {
            "shift_state": null,
            "speed": null,
            "latitude": 47.52,
            "longitude": 19.02,
            "timestamp": t0 + 120_000
        },
        "vehicle_state": {"odometer": 1002.0}
    }));
    let report = persist_collection_atomic_for_provider(
        &store,
        &terminal,
        t0 + 120_000,
        CollectorProvider::Fleet,
    )
    .expect("close Fleet drive");
    assert_eq!(report.drives_closed, 1);
    assert_eq!(report.positions_materialised, 2);
    assert_eq!(report.lifecycle_quarantines, 0);

    let duplicate = persist_collection_atomic_for_provider(
        &store,
        &terminal,
        t0 + 120_001,
        CollectorProvider::Fleet,
    )
    .expect("repeat terminal sample");
    assert_eq!(duplicate.observations_already_present, 1);
    assert_eq!(duplicate.drives_closed, 0);

    let connection = store.open().expect("database");
    let vehicle_id = connection
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("vehicle UUID");
    let lifecycle = store
        .load_lifecycle_state(vehicle_id)
        .expect("lifecycle query")
        .expect("lifecycle state");
    let open = OpenSessionState::decode(&lifecycle.open_session_json).expect("open session");
    assert!(open.open_drive.is_none());
    assert!(!lifecycle.quarantined);
    let drive_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM materialised_drives", [], |row| {
            row.get(0)
        })
        .expect("drive count");
    assert_eq!(drive_count, 1);
    let position_counts: (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), COUNT(drive_id) FROM materialised_positions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("position counts");
    assert_eq!(position_counts, (4, 3));
}

#[test]
fn fleet_atomic_collection_discards_one_position_drive_without_open_row_leak() {
    let temp = crate::private_tempdir().expect("temporary store");
    let t0 = 1_800_000_400_000_i64;
    let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
    let collection = |vehicle_data: serde_json::Value| ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![
            VehicleData::from_provider_raw_json(
                VehicleId::from_test(9),
                json!({"response": vehicle_data}),
            )
            .expect("Fleet response"),
        ],
        failures: vec![],
    };

    let store = HubStore::initialize(temp.path()).expect("store");
    let moving = collection(json!({
        "drive_state": {
            "shift_state": "D",
            "speed": 20,
            "latitude": 47.5,
            "longitude": 19.0,
            "timestamp": t0
        },
        "vehicle_state": {"odometer": 1000.0}
    }));
    persist_collection_atomic_for_provider(&store, &moving, t0, CollectorProvider::Fleet)
        .expect("open Fleet drive");
    drop(store);

    let store = HubStore::initialize(temp.path()).expect("restart store");
    let parked = collection(json!({
        "drive_state": {
            "shift_state": null,
            "speed": null,
            "timestamp": t0 + 60_000
        },
        "vehicle_state": {"odometer": 1000.1}
    }));
    let report = persist_collection_atomic_for_provider(
        &store,
        &parked,
        t0 + 60_000,
        CollectorProvider::Fleet,
    )
    .expect("discard incomplete Fleet drive");
    assert_eq!(report.drives_closed, 0);

    let connection = store.open().expect("database");
    let drive_row_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_open_rows
                 WHERE domain IN ('drive', 'position')",
            [],
            |row| row.get(0),
        )
        .expect("drive row count");
    assert_eq!(drive_row_count, 0);
    let completed: (i64, i64) = connection
        .query_row(
            "SELECT
                    (SELECT COUNT(*) FROM materialised_drives),
                    (SELECT COUNT(*) FROM materialised_positions)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("completed row counts");
    assert_eq!(completed, (0, 0));
}

#[test]
fn non_atomic_batch_discards_short_drive_without_rows_after_restart() {
    let temp = crate::private_tempdir().expect("temporary store");
    let t0 = 1_800_000_500_000_i64;
    let collection = ManualCollection {
        vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
        snapshots: vec![
            VehicleData::for_test(
                9,
                json!({
                    "drive_state": {
                        "shift_state": "D",
                        "speed": 20,
                        "latitude": 47.5,
                        "longitude": 19.0,
                        "timestamp": t0
                    },
                    "vehicle_state": {"odometer": 1000.0}
                }),
            ),
            VehicleData::for_test(
                9,
                json!({
                    "drive_state": {
                        "shift_state": "P",
                        "speed": 0,
                        "timestamp": t0 + 60_000
                    },
                    "vehicle_state": {"odometer": 1000.1}
                }),
            ),
        ],
        failures: vec![],
    };
    let store = HubStore::initialize(temp.path()).expect("store");
    persist_collection(&store, &collection, t0 + 60_000).expect("persist observations");
    materialise_lifecycle_for_collection(&store, &collection, t0 + 60_000)
        .expect("discard incomplete drive");
    drop(store);

    let store = HubStore::initialize(temp.path()).expect("restart store");
    let connection = store.open().expect("database");
    let drive_row_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_open_rows
                 WHERE domain IN ('drive', 'position')",
            [],
            |row| row.get(0),
        )
        .expect("drive row count");
    assert_eq!(drive_row_count, 0);
    let completed: (i64, i64) = connection
        .query_row(
            "SELECT
                    (SELECT COUNT(*) FROM materialised_drives),
                    (SELECT COUNT(*) FROM materialised_positions)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("completed row counts");
    assert_eq!(completed, (0, 0));
}

#[test]
fn oversized_compaction_defers_only_while_an_aggregate_slot_remains() {
    let limits = ProtocolLimits::default();
    assert!(!live_delta_compaction_required(
        limits.max_chunks - 9,
        limits
    ));
    assert!(live_delta_compaction_required(
        limits.max_chunks - 8,
        limits
    ));
    let row_capacity = ProjectionPackError::TooManyRows;
    assert!(may_defer_compaction_capacity_error(
        &row_capacity,
        limits.max_chunks - 1,
        limits,
    ));
    assert!(!may_defer_compaction_capacity_error(
        &row_capacity,
        limits.max_chunks,
        limits,
    ));
    assert!(is_compaction_pack_capacity_error(
        &ProjectionPackError::Protocol(ProtocolError::UncompressedSizeOutOfBounds(
            limits.max_uncompressed_pack_bytes + 1,
        )),
    ));
    assert!(!may_defer_compaction_capacity_error(
        &ProjectionPackError::Invalid("malformed compaction payload".into()),
        limits.max_chunks - 1,
        limits,
    ));
    assert!(is_compaction_catalog_capacity_error(&StoreError::Manifest(
        ProtocolError::LineageAggregateLimitExceeded
    )));
}

#[test]
fn near_limit_collection_compacts_live_suffix_before_consuming_the_next_slot() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([18; 32]);
    let now = 1_800_000_000_000_i64;
    let collection = ManualCollection {
        vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({
                "drive_state": {"timestamp": now},
                "vehicle_config": {"car_type": "model3"},
                "vehicle_state": {"car_version": "2026.20"}
            }),
        )],
        failures: vec![],
    };
    persist_collection(&store, &collection, now).expect("raw observation");
    materialise_lifecycle_for_collection(&store, &collection, now).expect("lifecycle");
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    publish_compatibility_snapshots(&store, &publication_gate, &cursor_key, &collection, now)
        .expect("base publication");
    drop(publication_gate);
    let vehicle_id = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("vehicle UUID");
    let base = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("base lineage")
        .expect("published base")
        .base;

    for (index, enabled) in [false, true].into_iter().enumerate() {
        store
            .upsert_car_settings(
                vehicle_id,
                9,
                &crate::hub_pack::ProjectionCarSettings {
                    enabled,
                    ..crate::hub_pack::ProjectionCarSettings::default()
                },
            )
            .expect("settings mutation");
        let claim = store
            .claim_sync_mutations(vehicle_id, now + index as i64 + 1, 100)
            .expect("claim mutation")
            .expect("pending live mutation");
        publish_v2_delta(&store, &cursor_key, &claim).expect("publish live delta");
    }
    assert_eq!(
        store.v2_lineage_pack_count(vehicle_id).expect("pack count"),
        3
    );

    let tiny_limit = ProtocolLimits {
        max_chunks: 4,
        ..ProtocolLimits::default()
    };
    compact_v2_lineage_if_needed_with_limits(&store, &cursor_key, vehicle_id, tiny_limit)
        .expect("compact before simulated fourth pack");
    let compacted = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("compacted lineage")
        .expect("published compacted lineage");
    compacted.validate().expect("compacted lineage validates");
    assert_eq!(compacted.base, base);
    assert_eq!(compacted.deltas.len(), 1);
    assert_eq!(
        store.v2_lineage_pack_count(vehicle_id).expect("pack count"),
        2
    );

    store
        .upsert_car_settings(
            vehicle_id,
            9,
            &crate::hub_pack::ProjectionCarSettings {
                enabled: false,
                ..crate::hub_pack::ProjectionCarSettings::default()
            },
        )
        .expect("post-compaction mutation");
    let claim = store
        .claim_sync_mutations(vehicle_id, now + 3, 100)
        .expect("post-compaction claim")
        .expect("pending post-compaction mutation");
    publish_v2_delta(&store, &cursor_key, &claim).expect("publish after compaction");
    let final_lineage = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("final lineage")
        .expect("published final lineage");
    final_lineage.validate().expect("final lineage validates");
    assert_eq!(final_lineage.base, base);
    assert_eq!(final_lineage.deltas.len(), 2);
    assert_eq!(
        store.v2_lineage_pack_count(vehicle_id).expect("pack count"),
        3
    );
}

#[test]
#[ignore = "requires TESLATLAS_REAL_CORPUS_ROOT pointing to a disposable clone"]
fn real_imported_corpus_crosses_the_production_compaction_trigger() {
    let root = std::env::var_os("TESLATLAS_REAL_CORPUS_ROOT")
        .map(std::path::PathBuf::from)
        .expect("set TESLATLAS_REAL_CORPUS_ROOT to a disposable store clone");
    let store = HubStore::initialize(&root).expect("open and migrate corpus clone");
    let connection = store.open().expect("catalogue");
    let (vehicle_key, car_id): (String, i64) = connection
        .query_row(
            "SELECT vehicles.vehicle_id, car_settings.car_id
                   FROM vehicles
                   JOIN car_settings USING (vehicle_id)
                  ORDER BY vehicles.vehicle_id
                  LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("one imported vehicle and settings row");
    drop(connection);
    let vehicle_id = vehicle_key.parse::<Uuid>().expect("vehicle UUID");
    let original = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage query")
        .expect("published lineage");
    original.validate().expect("original lineage validates");
    let original_base = original.base.clone();
    let initial_pack_count = store
        .v2_lineage_pack_count(vehicle_id)
        .expect("initial pack count");
    assert!(
        initial_pack_count >= 400,
        "this acceptance seam requires a production-scale imported corpus"
    );

    let cursor_key = CursorKey::from_bytes([29; 32]);
    let mut previous_pack_count = initial_pack_count;
    let mut compacted = false;
    for index in 0..128_i64 {
        store
            .upsert_car_settings(
                vehicle_id,
                car_id,
                &crate::hub_pack::ProjectionCarSettings {
                    enabled: index % 2 == 0,
                    ..crate::hub_pack::ProjectionCarSettings::default()
                },
            )
            .expect("settings mutation");
        let claim = store
            .claim_sync_mutations(vehicle_id, 2_100_000_000_000 + index, 100)
            .expect("claim mutation")
            .expect("pending mutation");
        publish_v2_delta(&store, &cursor_key, &claim).expect("publish production-scale delta");
        let current_pack_count = store
            .v2_lineage_pack_count(vehicle_id)
            .expect("current pack count");
        if current_pack_count < previous_pack_count {
            compacted = true;
            break;
        }
        previous_pack_count = current_pack_count;
    }
    assert!(compacted, "production compaction trigger was not crossed");

    let final_lineage = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("final lineage query")
        .expect("final lineage");
    final_lineage.validate().expect("final lineage validates");
    assert_eq!(final_lineage.base, original_base);
    assert!(
        store
            .v2_lineage_pack_count(vehicle_id)
            .expect("final pack count")
            < previous_pack_count
    );
    store.catalogue_check().expect("final corpus integrity");
}

#[test]
fn outbox_uses_sparse_delta_after_immutable_base_and_preserves_base_pack() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([17; 32]);
    let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
    let first_time = 1_800_000_000_000_i64;
    let first = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({
                "drive_state": {
                    "shift_state": "D", "speed": 12, "latitude": 47.0,
                    "longitude": 19.0, "timestamp": first_time
                },
                "vehicle_config": {"car_type": "model3"},
                "vehicle_state": {"car_version": "2026.20"}
            }),
        )],
        failures: vec![],
    };
    persist_collection(&store, &first, first_time).expect("first raw observation");
    materialise_lifecycle_for_collection(&store, &first, first_time).expect("first lifecycle");
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    publish_compatibility_snapshots(&store, &publication_gate, &cursor_key, &first, first_time)
        .expect("base publication");
    drop(publication_gate);
    let vehicle_id = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("UUID");
    let (base_digest, base_path): (String, _) = {
        let connection = store.open().expect("database");
        let digest: String = connection
            .query_row(
                "SELECT base_digest FROM sync_bases WHERE vehicle_id = ?1",
                rusqlite::params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("base digest");
        let path = store
            .packs_dir()
            .join("sha256")
            .join(format!("{digest}.sqlite.zst"));
        (digest, path)
    };
    let base_metadata = std::fs::metadata(&base_path).expect("base pack metadata");
    let base_modified = base_metadata.modified().expect("base pack mtime");
    let base_bytes = std::fs::read(&base_path).expect("base pack bytes");

    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime")
        .block_on(replay_export_outbox(
            &store,
            &cursor_key,
            std::slice::from_ref(&vehicle),
            first_time,
        ))
        .expect("clear base outbox");
    let second_time = first_time + 60_000;
    let second = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({
                "drive_state": {
                    "shift_state": "P", "speed": 0, "latitude": 47.01,
                    "longitude": 19.01, "timestamp": second_time
                },
                "vehicle_config": {"car_type": null},
                "vehicle_state": {"car_version": null}
            }),
        )],
        failures: vec![],
    };
    persist_collection(&store, &second, second_time).expect("second raw observation");
    materialise_lifecycle_for_collection(&store, &second, second_time).expect("second lifecycle");
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime")
        .block_on(replay_export_outbox(
            &store,
            &cursor_key,
            std::slice::from_ref(&vehicle),
            second_time,
        ))
        .expect("delta publication");

    let connection = store.open().expect("database");
    let delta_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sync_deltas WHERE vehicle_id = ?1",
            rusqlite::params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("delta count");
    let unpublished: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sync_mutations
                 WHERE vehicle_id = ?1 AND published = 0",
            rusqlite::params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("pending mutations");
    assert_eq!(delta_count, 1);
    assert_eq!(unpublished, 0);
    drop(connection);
    assert_eq!(std::fs::read(&base_path).expect("base bytes"), base_bytes);
    assert_eq!(
        std::fs::metadata(&base_path)
            .expect("base metadata")
            .modified()
            .expect("base mtime"),
        base_modified
    );
    assert_eq!(
        store
            .open()
            .expect("database")
            .query_row(
                "SELECT base_digest FROM sync_bases WHERE vehicle_id = ?1",
                rusqlite::params![vehicle_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .expect("base digest after delta"),
        base_digest
    );
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage manifest")
        .expect("published lineage");
    lineage.validate().expect("valid published lineage");
    assert_eq!(lineage.base.digest.to_string(), base_digest);
    assert_eq!(lineage.deltas.len(), 1);
    assert_eq!(lineage.deltas[0].pack.snapshot_id, lineage.base.snapshot_id);
    assert!(lineage.deltas[0].pack.ordinal > lineage.base.packs[0].ordinal);
    assert_eq!(
        store
            .manifest_for_vehicle(vehicle_id)
            .expect("legacy fallback manifest")
            .expect("legacy fallback available")
            .head_sequence,
        lineage.base.sequence
    );

    let delta = lineage.deltas[0].clone();
    let mutation_count =
        usize::try_from(delta.to_sequence - delta.from_sequence).expect("delta mutation count");
    let connection = store.open().expect("database");
    let mut statement = connection
        .prepare(
            "SELECT vehicle_id, revision, entity, entity_id, car_id,
                        operation, payload_json
                 FROM sync_mutations
                 WHERE vehicle_id = ?1
                 ORDER BY revision DESC LIMIT ?2",
        )
        .expect("mutation query");
    let mut mutations: Vec<SyncMutation> = statement
        .query_map(
            rusqlite::params![vehicle_id.to_string(), mutation_count as i64],
            |row| {
                Ok(SyncMutation {
                    vehicle_id: row.get::<_, String>(0)?.parse().map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    revision: row.get(1)?,
                    entity: row.get(2)?,
                    entity_id: row.get(3)?,
                    car_id: row.get(4)?,
                    operation: row.get(5)?,
                    payload_json: row.get(6)?,
                })
            },
        )
        .expect("mutation rows")
        .map(|row| row.expect("mutation row"))
        .collect();
    drop(statement);
    drop(connection);
    mutations.reverse();
    assert_eq!(mutations.len(), mutation_count);
    let replay_claim = SyncMutationClaim {
        vehicle_id,
        from_revision: mutations.first().expect("first mutation").revision,
        to_revision: mutations.last().expect("last mutation").revision,
        mutations,
    };
    store
        .commit_v2_delta_claim(&replay_claim, &delta, &cursor_key, &lineage.terminal_cursor)
        .expect("idempotent delta replay");
    let head_before_conflict = store.v2_head(vehicle_id).expect("head before conflict");
    let binding = store
        .v2_projection_binding(vehicle_id)
        .expect("immutable binding");
    let bad_hmac_cursor = OpaqueCursor::issue(
        &CursorKey::from_bytes([18; 32]),
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: delta.to_sequence,
        },
    )
    .expect("bad-HMAC cursor shape");
    assert!(matches!(
        store.commit_v2_delta_claim(&replay_claim, &delta, &cursor_key, &bad_hmac_cursor),
        Err(StoreError::Manifest(_))
    ));
    let wrong_claim_cursor = OpaqueCursor::issue(
        &cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: delta.to_sequence + 1,
        },
    )
    .expect("wrong-claim cursor shape");
    assert!(matches!(
        store.commit_v2_delta_claim(&replay_claim, &delta, &cursor_key, &wrong_claim_cursor,),
        Err(StoreError::LineageCatalogConflict)
    ));
    assert_eq!(
        store
            .v2_head(vehicle_id)
            .expect("head after rejected cursors"),
        head_before_conflict
    );
    let mut conflicting_delta = delta;
    conflicting_delta.chain_digest = Sha256Digest::of_bytes(b"conflicting-replay");
    assert!(matches!(
        store.commit_v2_delta_claim(
            &replay_claim,
            &conflicting_delta,
            &cursor_key,
            &lineage.terminal_cursor,
        ),
        Err(StoreError::LineageCatalogConflict)
    ));
    assert_eq!(
        store.v2_head(vehicle_id).expect("head after conflict"),
        head_before_conflict
    );
}

#[test]
fn outbox_remains_scheduled_until_every_bounded_mutation_batch_is_published() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let cursor_key = CursorKey::from_bytes([23; 32]);
    let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
    let now = 1_800_000_000_000_i64;
    let collection = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({
                "drive_state": {
                    "shift_state": "P", "speed": 0, "latitude": 47.0,
                    "longitude": 19.0, "timestamp": now
                },
                "vehicle_config": {"car_type": "model3"},
                "vehicle_state": {"car_version": "2026.20"}
            }),
        )],
        failures: vec![],
    };
    persist_collection(&store, &collection, now).expect("raw observation");
    materialise_lifecycle_for_collection(&store, &collection, now).expect("lifecycle");
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    publish_compatibility_snapshots(&store, &publication_gate, &cursor_key, &collection, now)
        .expect("base publication");
    drop(publication_gate);
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime")
        .block_on(replay_export_outbox(
            &store,
            &cursor_key,
            std::slice::from_ref(&vehicle),
            now,
        ))
        .expect("clear base outbox");

    let vehicle_id = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("UUID");
    let binding = store
        .v2_projection_binding(vehicle_id)
        .expect("immutable binding");
    let settings_payload =
        serde_json::to_string(&store.load_car_settings(vehicle_id).expect("car settings"))
            .expect("settings payload");
    let connection = store.open().expect("database");
    let transaction = connection.unchecked_transaction().expect("transaction");
    let first_revision: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM sync_mutations
                 WHERE vehicle_id = ?1",
            rusqlite::params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("next revision");
    {
        let mut insert = transaction
            .prepare_cached(
                "INSERT INTO sync_mutations(
                        vehicle_id, revision, entity, entity_id, car_id,
                        operation, payload_json, published, claimed_until_ms
                     ) VALUES (?1, ?2, 'car_setting', ?3, ?3, 'upsert', ?4, 0, 0)",
            )
            .expect("mutation insert");
        for offset in 0..10_001_i64 {
            insert
                .execute(rusqlite::params![
                    vehicle_id.to_string(),
                    first_revision + offset,
                    binding.selected_car_id,
                    settings_payload,
                ])
                .expect("mutation row");
        }
    }
    transaction
        .execute(
            "INSERT INTO sync_mutation_sequences(vehicle_id, next_revision)
                 VALUES (?1, ?2)
                 ON CONFLICT(vehicle_id) DO UPDATE SET next_revision = excluded.next_revision",
            rusqlite::params![vehicle_id.to_string(), first_revision + 10_001],
        )
        .expect("mutation sequence");
    transaction
        .execute(
            "INSERT INTO export_outbox(
                    vehicle_id, dirty_revision, attempts, next_attempt_ms,
                    claimed_until_ms, last_error
                 ) VALUES (?1, 1, 0, 0, 0, NULL)",
            rusqlite::params![vehicle_id.to_string()],
        )
        .expect("outbox row");
    transaction.commit().expect("commit synthetic backlog");
    drop(connection);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    runtime
        .block_on(replay_export_outbox(
            &store,
            &cursor_key,
            std::slice::from_ref(&vehicle),
            now + 1,
        ))
        .expect("first bounded batch");
    let connection = store.open().expect("database");
    let (unpublished, outbox): (i64, i64) = connection
        .query_row(
            "SELECT
                    (SELECT COUNT(*) FROM sync_mutations
                      WHERE vehicle_id = ?1 AND published = 0),
                    (SELECT COUNT(*) FROM export_outbox WHERE vehicle_id = ?1)",
            rusqlite::params![vehicle_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("mid-backlog state");
    assert_eq!((unpublished, outbox), (1, 1));
    drop(connection);

    runtime
        .block_on(replay_export_outbox(
            &store,
            &cursor_key,
            std::slice::from_ref(&vehicle),
            now + 2,
        ))
        .expect("final bounded batch");
    let connection = store.open().expect("database");
    let (unpublished, outbox): (i64, i64) = connection
        .query_row(
            "SELECT
                    (SELECT COUNT(*) FROM sync_mutations
                      WHERE vehicle_id = ?1 AND published = 0),
                    (SELECT COUNT(*) FROM export_outbox WHERE vehicle_id = ?1)",
            rusqlite::params![vehicle_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("completed backlog state");
    assert_eq!((unpublished, outbox), (0, 0));
}

#[test]
fn sparse_live_metadata_preserves_durable_car_and_new_pack_metadata_after_restart() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let vehicle = Vehicle::for_test(9, "5YJFULLVIN123456", "online");
    let full = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({
                "display_name": "Road car",
                "vin": "5YJFULLVIN123456",
                "drive_state": {"shift_state":"D", "speed":20, "latitude":47.0, "longitude":19.0, "timestamp":1800000000000_i64},
                "vehicle_config": {"car_type":"model3", "trim_badging":"74d", "exterior_color":"Pearl White"},
                "vehicle_state": {"car_version":"2026.20"}
            }),
        )],
        failures: vec![],
    };
    persist_collection(&store, &full, 1_800_000_000_000).expect("persist full");
    materialise_lifecycle_for_collection(&store, &full, 1_800_000_000_000)
        .expect("materialise full");
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    publish_compatibility_snapshots(
        &store,
        &publication_gate,
        &CursorKey::from_bytes([11; 32]),
        &full,
        1_800_000_000_000,
    )
    .expect("publish full");
    drop(publication_gate);
    let vehicle_id = store
        .open()
        .expect("db")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("uuid");
    let before = store
        .materialised_history(vehicle_id)
        .expect("history")
        .car
        .expect("car");

    let store = HubStore::initialize(temp.path()).expect("restart");
    let sparse = ManualCollection {
        vehicles: vec![vehicle],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({
                "drive_state": {"shift_state":"D", "speed":21, "latitude":47.01, "longitude":19.01, "timestamp":1800000060000_i64},
                "vehicle_config": {"car_type":null, "trim_badging":null, "exterior_color":null},
                "vehicle_state": {"car_version":null}
            }),
        )],
        failures: vec![],
    };
    persist_collection(&store, &sparse, 1_800_000_060_000).expect("persist sparse");
    materialise_lifecycle_for_collection(&store, &sparse, 1_800_000_060_000)
        .expect("materialise sparse");
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    publish_compatibility_snapshots(
        &store,
        &publication_gate,
        &CursorKey::from_bytes([11; 32]),
        &sparse,
        1_800_000_060_000,
    )
    .expect("publish sparse");
    let after = store
        .materialised_history(vehicle_id)
        .expect("history")
        .car
        .expect("car");
    assert_eq!(before, after);
    let manifest = store
        .manifest_for_vehicle(vehicle_id)
        .expect("manifest")
        .expect("published");
    let pack = store
        .pack_for_digest(manifest.chunks[0].sha256)
        .expect("pack")
        .expect("pack file");
    let bytes = zstd::stream::decode_all(std::fs::File::open(pack.path).expect("pack open"))
        .expect("decode");
    let inspect = temp.path().join("metadata.sqlite");
    std::fs::write(&inspect, bytes).expect("write inspect");
    let connection = rusqlite::Connection::open(inspect).expect("inspect");
    let packed: (
        String,
        String,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
    ) = connection
        .query_row(
            "SELECT name, model, vin, source_eid, exterior_color, firmware_version FROM cars",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("packed car");
    assert_eq!(
        packed,
        (
            after.name,
            after.model,
            after.vin,
            after.source_eid,
            after.exterior_color,
            after.firmware_version
        )
    );
}

#[test]
fn live_publication_includes_v2_state_and_update_history() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
    let t0 = 1_800_000_000_000_i64;
    let t1 = t0 + 1_000;
    let t2 = t0 + 2_000;
    let t3 = t0 + 3_000;
    let collections = [
        (
            ManualCollection {
                vehicles: vec![vehicle.clone()],
                snapshots: vec![VehicleData::for_test(
                    9,
                    json!({
                        "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t0},
                        "vehicle_state": {"timestamp": t0, "car_version": "2026.1"}
                    }),
                )],
                failures: vec![],
            },
            t0,
        ),
        (
            ManualCollection {
                vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "asleep")],
                snapshots: vec![VehicleData::for_test(
                    9,
                    json!({"vehicle_state": {"timestamp": t1, "car_version": "2026.1"}}),
                )],
                failures: vec![],
            },
            t1,
        ),
        (
            ManualCollection {
                vehicles: vec![vehicle.clone()],
                snapshots: vec![VehicleData::for_test(
                    9,
                    json!({
                        "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t2},
                        "vehicle_state": {
                            "timestamp": t2,
                            "car_version": "2026.1",
                            "software_update": {"status": "installing"}
                        }
                    }),
                )],
                failures: vec![],
            },
            t2,
        ),
        (
            ManualCollection {
                vehicles: vec![vehicle.clone()],
                snapshots: vec![VehicleData::for_test(
                    9,
                    json!({
                        "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t3},
                        "vehicle_state": {
                            "timestamp": t3,
                            "car_version": "2026.20.1",
                            "software_update": {"status": ""}
                        }
                    }),
                )],
                failures: vec![],
            },
            t3,
        ),
    ];

    for (collection, received_at_ms) in &collections {
        persist_collection(&store, collection, *received_at_ms).expect("persist fixture");
        materialise_lifecycle_for_collection(&store, collection, *received_at_ms)
            .expect("materialise fixture");
    }

    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    publish_compatibility_snapshots(
        &store,
        &publication_gate,
        &CursorKey::from_bytes([8; 32]),
        &collections[3].0,
        t3,
    )
    .expect("publish v2 projection");

    let vehicle_id = store
        .open()
        .expect("database")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("vehicle id")
        .parse::<Uuid>()
        .expect("stored UUID");
    let manifest = store
        .manifest_for_vehicle(vehicle_id)
        .expect("manifest query")
        .expect("published manifest");
    assert_eq!(
        manifest.chunks[0].schema,
        crate::hub_pack::HUB_PROJECTION_SCHEMA_V2
    );

    let stored_pack = store
        .pack_for_digest(manifest.chunks[0].sha256)
        .expect("pack catalog")
        .expect("stored pack");
    let sqlite_bytes =
        zstd::stream::decode_all(std::fs::File::open(&stored_pack.path).expect("pack file"))
            .expect("decompress pack");
    let inspect_path = temp.path().join("inspect-v2.sqlite");
    std::fs::write(&inspect_path, sqlite_bytes).expect("write inspection copy");
    let connection = rusqlite::Connection::open(inspect_path).expect("inspect sqlite");
    let states: Vec<(i64, String, i64, Option<i64>)> = connection
        .prepare("SELECT id, state, start_date_ms, end_date_ms FROM states ORDER BY id")
        .expect("states query")
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("states rows")
        .map(|row| row.expect("state row"))
        .collect();
    assert_eq!(
        states,
        vec![
            (1, "online".to_owned(), t0, Some(t1)),
            (2, "asleep".to_owned(), t1, Some(t2)),
            (3, "online".to_owned(), t2, None),
        ]
    );
    let update: (i64, i64, i64, String) = connection
        .query_row(
            "SELECT id, start_date_ms, end_date_ms, version FROM updates",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("update row");
    assert_eq!(update, (1, t2, t3, "2026.20.1".to_owned()));
}

#[test]
fn synthetic_drive_and_charge_survive_mid_session_restart() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let t0 = 1_800_000_500_000_i64;
    let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");

    // Open a drive.
    let open_drive = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![
            VehicleData::for_test(
                9,
                json!({
                    "drive_state": {
                        "shift_state": "D",
                        "speed": 25,
                        "latitude": 47.0,
                        "longitude": 19.0,
                        "timestamp": t0
                    },
                    "vehicle_state": {"odometer": 1000.0},
                    "charge_state": {"battery_level": 70, "battery_range": 200.0}
                }),
            ),
            VehicleData::for_test(
                9,
                json!({
                    "drive_state": {
                        "shift_state": "D",
                        "speed": 30,
                        "latitude": 47.01,
                        "longitude": 19.01,
                        "timestamp": t0 + 60_000
                    },
                    "vehicle_state": {"odometer": 1001.0},
                    "charge_state": {"battery_level": 69, "battery_range": 198.0}
                }),
            ),
        ],
        failures: vec![],
    };
    persist_collection(&store, &open_drive, t0).expect("persist open");
    materialise_lifecycle_for_collection(&store, &open_drive, t0).expect("materialise open");

    let vehicle_id = store
        .open()
        .expect("db")
        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("id")
        .parse::<Uuid>()
        .expect("uuid");
    let open_state = store
        .load_lifecycle_state(vehicle_id)
        .expect("load")
        .expect("open state exists");
    let decoded = OpenSessionState::decode(&open_state.open_session_json).expect("decode");
    assert!(decoded.open_drive.is_some());

    // Simulate process restart: reopen store path and finish the drive.
    let store = HubStore::initialize(temp.path()).expect("reopen store");
    let close_drive = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({
                "drive_state": {
                    "shift_state": "P",
                    "speed": 0,
                    "latitude": 47.01,
                    "longitude": 19.01,
                    "timestamp": t0 + 120_000
                },
                "charge_state": {"battery_level": 68, "battery_range": 195.0}
            }),
        )],
        failures: vec![],
    };
    persist_collection(&store, &close_drive, t0 + 120_000).expect("persist close");
    let lifecycle = materialise_lifecycle_for_collection(&store, &close_drive, t0 + 120_000)
        .expect("materialise close");
    assert_eq!(lifecycle.drives_closed, 1);
    assert_eq!(lifecycle.positions_materialised, 3);

    // Charge lifecycle on the same durable vehicle.
    let charge_open = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({
                "charge_state": {
                    "charging_state": "Charging",
                    "battery_level": 40,
                    "charge_energy_added": 1.0,
                    "charger_power": 11.0,
                    "battery_range": 120.0
                },
                "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t0 + 200_000}
            }),
        )],
        failures: vec![],
    };
    persist_collection(&store, &charge_open, t0 + 200_000).expect("persist charge open");
    materialise_lifecycle_for_collection(&store, &charge_open, t0 + 200_000)
        .expect("materialise charge open");

    let store = HubStore::initialize(temp.path()).expect("second reopen");
    let charge_close = ManualCollection {
        vehicles: vec![vehicle],
        snapshots: vec![VehicleData::for_test(
            9,
            json!({
                "charge_state": {
                    "charging_state": "Complete",
                    "battery_level": 80,
                    "charge_energy_added": 12.0,
                    "charger_power": 0.0,
                    "battery_range": 220.0
                },
                "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t0 + 800_000}
            }),
        )],
        failures: vec![],
    };
    persist_collection(&store, &charge_close, t0 + 800_000).expect("persist charge close");
    let lifecycle = materialise_lifecycle_for_collection(&store, &charge_close, t0 + 800_000)
        .expect("materialise charge close");
    assert_eq!(lifecycle.charges_closed, 1);
    assert!(lifecycle.charge_samples_materialised >= 1);

    let history = store.materialised_history(vehicle_id).expect("history");
    assert_eq!(history.drives.len(), 1);
    assert_eq!(history.charges.len(), 1);
    assert_eq!(history.charges[0].end_battery_level, Some(80));
    assert_eq!(history.charges[0].charge_energy_added, Some(11.0));

    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    publish_compatibility_snapshots(
        &store,
        &publication_gate,
        &CursorKey::from_bytes([9; 32]),
        &charge_close,
        t0 + 800_000,
    )
    .expect("publish");
    let manifest = store
        .manifest_for_vehicle(vehicle_id)
        .expect("manifest")
        .expect("published");
    assert!(manifest.total_rows > 1);
}
