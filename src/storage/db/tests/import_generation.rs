// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn lineage_catalog_requires_verified_packs_and_is_restart_safe() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    let base_snapshot_id = Uuid::new_v4();
    let make_pack = |snapshot_id: Uuid, ordinal: u32, sequence: SequenceRange, bytes: &[u8]| {
        let digest = Sha256Digest::of_bytes(bytes);
        TransportPack {
            pack_id: Uuid::new_v4(),
            snapshot_id,
            ordinal,
            schema: SchemaVersion { major: 1, minor: 0 },
            format: PackFormat::SqliteTransport,
            compression: PackCompression::Zstd,
            relative_path: TransportPack::canonical_relative_path(digest),
            sha256: digest,
            compressed_bytes: bytes.len() as u64,
            uncompressed_bytes: 100,
            row_count: 1,
            sequence,
            tables: vec![MirrorTable::Vehicle],
        }
    };
    let base_pack = make_pack(
        base_snapshot_id,
        0,
        SequenceRange {
            from_exclusive: 10,
            to_inclusive: 10,
        },
        b"base-pack",
    );
    let delta_pack = make_pack(
        base_snapshot_id,
        1,
        SequenceRange {
            from_exclusive: 10,
            to_inclusive: 11,
        },
        b"delta-pack",
    );
    let base_digest = Sha256Digest::of_bytes(b"base-chain");
    let chain_digest = canonical_delta_chain_digest(base_digest, delta_pack.sha256);
    let cursor = OpaqueCursor::issue(
        &CursorKey::from_bytes([7; 32]),
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: SchemaVersion { major: 1, minor: 0 },
            installation_id: store.installation_id().expect("installation"),
            account_id: Uuid::new_v4(),
            vehicle_id: vehicle.vehicle_id,
            generation: 1,
            sequence: 11,
        },
    )
    .expect("cursor");
    let lineage = LineageManifestV2 {
        protocol: LINEAGE_PROTOCOL_V2,
        capability: LineageCapability::ImmutableBaseOrderedDeltas,
        schema: SchemaVersion { major: 1, minor: 0 },
        installation_id: store.installation_id().expect("installation"),
        account_id: Uuid::new_v4(),
        vehicle_id: vehicle.vehicle_id,
        generation: 1,
        base: LineageBase {
            snapshot_id: base_snapshot_id,
            sequence: 10,
            digest: base_digest,
            packs: vec![base_pack.clone()],
        },
        deltas: vec![LineageDelta {
            from_sequence: 10,
            to_sequence: 11,
            parent_chain_digest: base_digest,
            chain_digest,
            pack_digest: delta_pack.sha256,
            pack: delta_pack.clone(),
        }],
        head_sequence: 11,
        head_digest: chain_digest,
        terminal_cursor: cursor,
    };
    assert!(matches!(
        store.commit_lineage_catalog(&lineage),
        Err(StoreError::LineagePackNotReady)
    ));
    let connection = store.open().expect("open after rejected publication");
    for table in ["sync_bases", "sync_deltas", "sync_heads", "sync_packs"] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("rejected publication count");
        assert_eq!(count, 0, "rejected publication must not activate {table}");
    }
    drop(connection);
    let pack_dir = store.packs_dir().join("sha256");
    fs::create_dir_all(&pack_dir).expect("pack directory");
    for pack in [&base_pack, &delta_pack] {
        fs::write(
            pack_dir.join(format!("{}.sqlite.zst", pack.sha256)),
            if pack.pack_id == base_pack.pack_id {
                b"base-pack".as_slice()
            } else {
                b"delta-pack".as_slice()
            },
        )
        .expect("pack");
    }
    store
        .commit_lineage_catalog(&lineage)
        .expect("catalog commit");
    store
        .commit_lineage_catalog(&lineage)
        .expect("same commit is idempotent");
    let reopened = HubStore::initialize(temp.path()).expect("reopen");
    let count: i64 = reopened
        .open()
        .expect("open")
        .query_row("SELECT COUNT(*) FROM sync_deltas", [], |row| row.get(0))
        .expect("delta count");
    assert_eq!(count, 1);

    let mut conflict = lineage.clone();
    conflict.deltas[0].chain_digest = Sha256Digest::of_bytes(b"conflict-chain");
    conflict.head_digest = conflict.deltas[0].chain_digest;
    let head_before_conflict = reopened
        .v2_head(vehicle.vehicle_id)
        .expect("head before conflicting replay");
    assert!(matches!(
        reopened.commit_lineage_catalog(&conflict),
        Err(StoreError::Manifest(
            crate::protocol::ProtocolError::LineageChainMismatch
        ))
    ));
    assert_eq!(
        reopened
            .v2_head(vehicle.vehicle_id)
            .expect("head after conflicting replay"),
        head_before_conflict
    );
}

#[test]
fn import_generation_staging_survives_active_state_and_promotes_once() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    let active = crate::teslamate_projection::TeslaMateOpenSession {
        car_id: 10,
        state: Some(crate::teslamate_projection::TeslaMateState {
            id: 1,
            car_id: 10,
            state: "online".into(),
            start_date_ms: 1_000,
            end_date_ms: None,
        }),
        ..Default::default()
    };
    store
        .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 10, &active, 1_000)
        .expect("active seed");

    let run = store
        .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 2_000)
        .expect("generation");
    store
        .stage_import_generation_session(run, &active)
        .expect("stage");
    let staged_count: i64 = store
        .open()
        .expect("open")
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("staged count");
    assert_eq!(staged_count, 1);
    assert_eq!(
        store
            .load_imported_open_session(source.source_id, vehicle.vehicle_id)
            .expect("active load"),
        Some(active.clone())
    );

    let reopened = HubStore::initialize(temp.path()).expect("restart cleanup");
    let cleaned_count: i64 = reopened
        .open()
        .expect("open after restart")
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("cleaned count");
    assert_eq!(cleaned_count, 0);
    assert_eq!(
        reopened
            .load_imported_open_session(source.source_id, vehicle.vehicle_id)
            .expect("active survives restart"),
        Some(active.clone())
    );

    let successful = reopened
        .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 3_000)
        .expect("second generation");
    let mut promoted = active.clone();
    promoted.watermarks.positions.max_id = Some(12);
    reopened
        .stage_import_generation_session(successful, &promoted)
        .expect("stage second generation");
    reopened
        .promote_import_generation(successful, source.source_id, vehicle.vehicle_id, 10, 3_000)
        .expect("promote generation");
    assert_eq!(
        reopened
            .load_imported_open_session(source.source_id, vehicle.vehicle_id)
            .expect("promoted load"),
        Some(promoted)
    );
    let lifecycle = reopened
        .load_lifecycle_state(vehicle.vehicle_id)
        .expect("promoted lifecycle load")
        .expect("promoted lifecycle state");
    let lifecycle = crate::lifecycle::OpenSessionState::decode(&lifecycle.open_session_json)
        .expect("promoted lifecycle decode");
    assert_eq!(lifecycle.next_position_id, 13);
    assert!(lifecycle.id_cursors_seeded);
}

#[test]
fn imported_open_rows_with_reused_source_ids_are_isolated_by_vehicle() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "multi-car"), 1_000)
        .expect("source");
    let first_vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "first-car"),
            1_001,
        )
        .expect("first vehicle");
    let second_vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "second-car"),
            1_002,
        )
        .expect("second vehicle");
    let first_session = crate::teslamate_projection::TeslaMateOpenSession {
        car_id: 10,
        state: Some(crate::teslamate_projection::TeslaMateState {
            id: 1,
            car_id: 10,
            state: "online".into(),
            start_date_ms: 1_000,
            end_date_ms: None,
        }),
        ..Default::default()
    };
    let second_session = crate::teslamate_projection::TeslaMateOpenSession {
        car_id: 20,
        state: Some(crate::teslamate_projection::TeslaMateState {
            id: 1,
            car_id: 20,
            state: "asleep".into(),
            start_date_ms: 2_000,
            end_date_ms: None,
        }),
        ..Default::default()
    };

    let first = store
        .seed_imported_open_session(
            source.source_id,
            first_vehicle.vehicle_id,
            10,
            &first_session,
            1_000,
        )
        .expect("first open session");
    let second = store
        .seed_imported_open_session(
            source.source_id,
            second_vehicle.vehicle_id,
            20,
            &second_session,
            2_000,
        )
        .expect("second open session");
    assert_eq!(first.provisional_rows_inserted, 1);
    assert_eq!(second.provisional_rows_inserted, 1);
    assert_eq!(
        store
            .load_imported_open_session(source.source_id, first_vehicle.vehicle_id)
            .expect("load first session"),
        Some(first_session)
    );
    assert_eq!(
        store
            .load_imported_open_session(source.source_id, second_vehicle.vehicle_id)
            .expect("load second session"),
        Some(second_session)
    );
    let rows: i64 = store
        .open()
        .expect("open")
        .query_row(
            "SELECT COUNT(*) FROM lifecycle_open_rows WHERE source_id = ?1",
            params![source.source_id.to_string()],
            |row| row.get(0),
        )
        .expect("open row count");
    assert_eq!(rows, 2);
}

#[test]
fn finalize_import_generation_promotes_fresh_vehicle_from_zero_cursor() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    let session = crate::teslamate_projection::TeslaMateOpenSession {
        car_id: 10,
        state: Some(crate::teslamate_projection::TeslaMateState {
            id: 1,
            car_id: 10,
            state: "online".into(),
            start_date_ms: 1_000,
            end_date_ms: None,
        }),
        ..Default::default()
    };
    let run = store
        .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 1_000)
        .expect("generation");
    store
        .stage_import_generation_session(run, &session)
        .expect("stage");
    let mut manifest = test_manifest();
    manifest.vehicle_id = vehicle.vehicle_id;

    store
        .finalize_import_generation(
            run,
            source.source_id,
            vehicle.vehicle_id,
            10,
            1_000,
            &manifest,
            Sha256Digest::of_bytes(b"fresh import generation"),
            &[],
        )
        .expect("finalize fresh generation");

    assert_eq!(
        store
            .load_imported_open_session(source.source_id, vehicle.vehicle_id)
            .expect("load promoted session"),
        Some(session)
    );
}

#[test]
fn import_generation_promotion_rejects_newer_live_cursor_without_reopening_state() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("test", "race"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "race-car"), 1_000)
        .expect("vehicle");
    let active = crate::teslamate_projection::TeslaMateOpenSession {
        car_id: 10,
        state: Some(crate::teslamate_projection::TeslaMateState {
            id: 1,
            car_id: 10,
            state: "online".into(),
            start_date_ms: 1_000,
            end_date_ms: None,
        }),
        ..Default::default()
    };
    store
        .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 10, &active, 1_000)
        .expect("active seed");
    let run = store
        .begin_import_generation(source.source_id, vehicle.vehicle_id, 10, 2_000)
        .expect("generation");
    store
        .stage_import_generation_session(run, &active)
        .expect("stage");
    store
        .open()
        .expect("open")
        .execute(
            "UPDATE vehicle_lifecycle_state
                 SET last_observation_id = 9, updated_at_ms = 9_000
                 WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
        )
        .expect("simulate live close");
    let error = store
        .promote_import_generation(run, source.source_id, vehicle.vehicle_id, 10, 2_000)
        .expect_err("newer live cursor must settle import");
    assert!(matches!(error, StoreError::ImportGenerationConflict));
    let state = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .expect("state")
        .expect("live state remains");
    assert_eq!(state.last_observation_id, 9);
    assert_eq!(state.updated_at_ms, 9_000);
}

#[test]
fn export_outbox_coalesces_retries_survives_restart_and_respects_v2_base() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("test", "outbox"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "outbox-car"),
            1_000,
        )
        .expect("vehicle");
    let session = crate::teslamate_projection::TeslaMateOpenSession {
        car_id: 10,
        ..Default::default()
    };
    store
        .seed_imported_open_session(source.source_id, vehicle.vehicle_id, 10, &session, 1_000)
        .expect("dirty seed");
    let claim = store
        .claim_export_outbox(1_000)
        .expect("claim")
        .expect("outbox row");
    store
        .fail_export_outbox(&claim, "https://secret.invalid/token", 1_000)
        .expect("retry");
    drop(store);
    let reopened = HubStore::initialize(temp.path()).expect("restart");
    let error: String = reopened
        .open()
        .expect("database")
        .query_row(
            "SELECT last_error FROM export_outbox WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("error");
    assert_eq!(error, "publication_failed");
    let second = reopened
        .claim_export_outbox(4_000)
        .expect("retry claim")
        .expect("retry row");
    assert!(second.attempts >= 2);
    reopened.complete_export_outbox(&second).expect("complete");
    assert!(
        reopened
            .claim_export_outbox(4_001)
            .expect("completed outbox query")
            .is_none(),
        "a completed revision must remain quiescent"
    );

    drop(reopened);
    let reopened = HubStore::initialize(temp.path()).expect("restart after completion");
    assert!(
        reopened
            .claim_export_outbox(5_000)
            .expect("completed outbox after restart")
            .is_none(),
        "a completed revision must not reappear after restart"
    );

    let base_id = Uuid::new_v4();
    reopened
            .open()
            .expect("database")
            .execute(
                "INSERT INTO sync_bases(vehicle_id, snapshot_id, base_sequence, base_digest, packs_json)
                 VALUES (?1, ?2, 1, ?3, ?4)",
                params![
                    vehicle.vehicle_id.to_string(),
                    base_id.to_string(),
                    "0".repeat(64),
                    b"[]".as_slice()
                ],
            )
            .expect("base");
    assert!(
        reopened
            .vehicle_has_v2_base(vehicle.vehicle_id)
            .expect("base check")
    );
}

#[test]
fn export_outbox_release_does_not_inflate_failure_attempts() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    mark_export_dirty_for_test(&store, vehicle.vehicle_id);

    let first = store
        .claim_export_outbox(1_000)
        .expect("first claim")
        .expect("outbox row");
    assert_eq!(first.attempts, 1);
    store
        .release_export_outbox(&first)
        .expect("release without failure");

    let second = store
        .claim_export_outbox(1_001)
        .expect("second claim")
        .expect("released outbox row");
    assert_eq!(second.attempts, 1);
    store
        .fail_export_outbox(&second, "transient", 1_001)
        .expect("real failure");
    let third = store
        .claim_export_outbox(3_001)
        .expect("third claim")
        .expect("failed outbox row");
    assert_eq!(third.attempts, 2);
    store
        .release_export_outbox(&third)
        .expect("release after earlier failure");
    let fourth = store
        .claim_export_outbox(3_002)
        .expect("fourth claim")
        .expect("released failed outbox row");
    assert_eq!(fourth.attempts, 2);
}

#[test]
fn export_outbox_completion_preserves_a_newer_revision_created_during_the_lease() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    mark_export_dirty_for_test(&store, vehicle.vehicle_id);

    let first = store
        .claim_export_outbox(1_000)
        .expect("first claim")
        .expect("first revision");
    assert_eq!(first.dirty_revision, 1);

    mark_export_dirty_for_test(&store, vehicle.vehicle_id);
    store
        .complete_export_outbox(&first)
        .expect("complete stale claim");

    let second = store
        .claim_export_outbox(1_001)
        .expect("newer claim")
        .expect("newer revision remains pending");
    assert_eq!(second.vehicle_id, vehicle.vehicle_id);
    assert_eq!(second.dirty_revision, 2);
    store
        .complete_export_outbox(&second)
        .expect("complete newer revision");
    assert!(
        store
            .claim_export_outbox(1_002)
            .expect("quiescent outbox")
            .is_none()
    );
}

#[test]
fn export_outbox_completion_advances_fairly_across_vehicles() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("test", "outbox-fairness"), 1_000)
        .expect("source");
    let first_vehicle_id =
        Uuid::parse_str("00000000-0000-4000-8000-000000000001").expect("first test vehicle ID");
    let second_vehicle_id =
        Uuid::parse_str("00000000-0000-4000-8000-000000000002").expect("second test vehicle ID");
    store
        .register_vehicle_with_id(
            &VehicleDescriptor::new(source.source_id, "outbox-car-1"),
            1_000,
            first_vehicle_id,
        )
        .expect("first vehicle");
    store
        .register_vehicle_with_id(
            &VehicleDescriptor::new(source.source_id, "outbox-car-2"),
            1_000,
            second_vehicle_id,
        )
        .expect("second vehicle");
    mark_export_dirty_for_test(&store, first_vehicle_id);
    mark_export_dirty_for_test(&store, second_vehicle_id);

    let first = store
        .claim_export_outbox(1_000)
        .expect("first claim")
        .expect("first vehicle pending");
    assert_eq!(first.vehicle_id, first_vehicle_id);
    mark_export_dirty_for_test(&store, first_vehicle_id);
    store
        .complete_export_outbox(&first)
        .expect("complete first vehicle");

    let second = store
        .claim_export_outbox(1_001)
        .expect("second claim")
        .expect("second vehicle pending");
    assert_eq!(second.vehicle_id, second_vehicle_id);
    store
        .complete_export_outbox(&second)
        .expect("complete second vehicle");

    let newer_first = store
        .claim_export_outbox(1_001)
        .expect("newer first claim")
        .expect("newer first revision remains pending");
    assert_eq!(newer_first.vehicle_id, first_vehicle_id);
    assert_eq!(newer_first.dirty_revision, 2);
    store
        .complete_export_outbox(&newer_first)
        .expect("complete newer first revision");
    assert!(
        store
            .claim_export_outbox(1_002)
            .expect("all vehicles complete")
            .is_none()
    );
}

#[test]
fn export_outbox_restart_reclaims_an_expired_lease_and_fences_stale_completion() {
    let temp = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (_, vehicle) = test_registered_vehicle(&store);
    mark_export_dirty_for_test(&store, vehicle.vehicle_id);
    let abandoned = store
        .claim_export_outbox(1_000)
        .expect("initial claim")
        .expect("pending revision");
    drop(store);

    let reopened = HubStore::initialize(temp.path()).expect("restart with active lease");
    assert!(
        reopened
            .claim_export_outbox(abandoned.lease_until_ms - 1)
            .expect("leased query")
            .is_none(),
        "restart must preserve a live lease"
    );
    let reclaimed = reopened
        .claim_export_outbox(abandoned.lease_until_ms)
        .expect("expired lease query")
        .expect("expired revision is reclaimable");
    assert_eq!(reclaimed.dirty_revision, abandoned.dirty_revision);
    assert!(reclaimed.attempts > abandoned.attempts);

    reopened
        .complete_export_outbox(&abandoned)
        .expect("stale completion is harmless");
    assert!(
        reopened
            .claim_export_outbox(abandoned.lease_until_ms)
            .expect("new lease remains fenced")
            .is_none(),
        "a stale publisher must not consume or release the new lease"
    );
    reopened
        .complete_export_outbox(&reclaimed)
        .expect("active completion");
    assert!(
        reopened
            .claim_export_outbox(reclaimed.lease_until_ms)
            .expect("completed revision")
            .is_none()
    );
}

#[test]
fn stream_session_terminal_completion_is_explicit_and_idempotence_fenced() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let correlation_id = Uuid::new_v4();
    let session = store
        .begin_stream_session(correlation_id, 123)
        .expect("begin stream session");
    store
        .complete_stream_session_terminal(
            session,
            StreamSessionTerminalOutcome::CancelledBeforeSubscription,
        )
        .expect("terminalize normal pre-subscription cancellation");
    assert!(matches!(
        store.complete_stream_session_terminal(session, StreamSessionTerminalOutcome::Failed,),
        Err(StoreError::StreamSessionReceiptNotStarted)
    ));
    let connection = store.open().expect("stream receipt catalogue");
    let (outcome, unsubscribe, completed): (String, Option<i64>, Option<i64>) = connection
        .query_row(
            "SELECT outcome, unsubscribe_receipt_id, completed_at_ms
                   FROM stream_session_receipts WHERE id = ?1",
            params![session.0],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("terminal receipt");
    assert_eq!(outcome, "cancelled_before_subscription");
    assert_eq!(unsubscribe, None);
    assert!(completed.is_some());
}

#[test]
fn upgrades_v39_with_durable_live_delta_compaction_provenance() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let connection = store.open().expect("current catalogue");
    remove_v50_current_observation_schema(&connection);
    connection
        .execute_batch(
            "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE supervised_collector_lease;
                 DROP TABLE sync_retired_lineage_packs;
                 DROP TABLE sync_retired_lineages;
                 DROP TABLE sync_live_delta_spans;
                 DROP INDEX sync_mutations_compaction_latest;
                 PRAGMA user_version = 39;",
        )
        .expect("recreate historical v39 boundary");
    drop(connection);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v39 store");
    let connection = upgraded.open().expect("upgraded catalogue");
    assert_eq!(
        schema_version(&connection).expect("schema version"),
        SCHEMA_VERSION
    );
    for object in [
        "sync_live_delta_spans",
        "sync_live_delta_spans_revision_range",
        "sync_mutations_compaction_latest",
    ] {
        let found: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                     WHERE name = ?1 AND type IN ('table', 'index')",
                params![object],
                |row| row.get(0),
            )
            .expect("compaction schema object query");
        assert_eq!(found, 1, "missing migrated object {object}");
    }
}

#[test]
fn upgrades_v41_with_bounded_prior_lineage_pack_authorization() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let connection = store.open().expect("current catalogue");
    remove_v50_current_observation_schema(&connection);
    connection
        .execute_batch(
            "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE supervised_collector_lease;
                 DROP TABLE sync_retired_lineage_packs;
                 DROP TABLE sync_retired_lineages;
                 PRAGMA user_version = 41;",
        )
        .expect("recreate historical v41 boundary");
    drop(connection);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v41 store");
    let connection = upgraded.open().expect("upgraded catalogue");
    assert_eq!(
        schema_version(&connection).expect("schema version"),
        SCHEMA_VERSION
    );
    for object in [
        "sync_retired_lineages",
        "sync_retired_lineage_packs",
        "sync_retired_lineage_packs_authorization",
        "sync_retired_lineages_expiry",
    ] {
        let found: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                     WHERE name = ?1 AND type IN ('table', 'index')",
                params![object],
                |row| row.get(0),
            )
            .expect("retired-lineage schema object query");
        assert_eq!(found, 1, "missing migrated object {object}");
    }
}

#[test]
fn upgrades_v40_stream_receipts_without_reclassifying_crash_evidence() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let correlation_id = Uuid::new_v4();
    let connection = store.open().expect("current catalogue");
    remove_v50_current_observation_schema(&connection);
    connection
        .execute_batch(
            "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE supervised_collector_lease;
                 DROP TABLE sync_retired_lineage_packs;
                 DROP TABLE sync_retired_lineages;
                 DROP INDEX stream_session_receipts_proof;
                 DROP INDEX stream_session_receipts_retention;
                 DROP TABLE stream_session_receipts;
                 CREATE TABLE stream_session_receipts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    correlation_id TEXT NOT NULL CHECK(length(correlation_id) = 36),
                    vehicle_tesla_id INTEGER NOT NULL CHECK(vehicle_tesla_id > 0),
                    started_at_ms INTEGER NOT NULL CHECK(started_at_ms >= 0),
                    completed_at_ms INTEGER,
                    duration_ms INTEGER,
                    outcome TEXT NOT NULL CHECK(outcome IN ('started', 'orderly_shutdown')),
                    unsubscribe_receipt_id INTEGER,
                    CHECK((outcome = 'started' AND completed_at_ms IS NULL
                           AND duration_ms IS NULL AND unsubscribe_receipt_id IS NULL)
                          OR (outcome = 'orderly_shutdown' AND completed_at_ms IS NOT NULL
                              AND duration_ms IS NOT NULL
                              AND completed_at_ms >= started_at_ms AND duration_ms >= 0
                              AND unsubscribe_receipt_id IS NOT NULL))
                 ) STRICT;
                 CREATE INDEX stream_session_receipts_proof
                    ON stream_session_receipts(correlation_id, outcome, id);
                 CREATE INDEX stream_session_receipts_retention
                    ON stream_session_receipts(outcome, completed_at_ms, id);
                 PRAGMA user_version = 40;",
        )
        .expect("recreate v40 stream receipt schema");
    connection
        .execute(
            "INSERT INTO stream_session_receipts(
                    id, correlation_id, vehicle_tesla_id, started_at_ms, outcome
                 ) VALUES (1, ?1, 123, 1000, 'started')",
            params![correlation_id.to_string()],
        )
        .expect("historical unresolved receipt");
    connection
        .execute(
            "INSERT INTO stream_session_receipts(
                    id, correlation_id, vehicle_tesla_id, started_at_ms,
                    completed_at_ms, duration_ms, outcome, unsubscribe_receipt_id
                 ) VALUES (2, ?1, 123, 1000, 1100, 100, 'orderly_shutdown', 77)",
            params![correlation_id.to_string()],
        )
        .expect("historical orderly receipt");
    drop(connection);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v40 store");
    let connection = upgraded.open().expect("upgraded catalogue");
    assert_eq!(
        schema_version(&connection).expect("schema version"),
        SCHEMA_VERSION
    );
    let outcomes = connection
        .prepare("SELECT outcome FROM stream_session_receipts ORDER BY id")
        .expect("receipt query")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("receipt rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("receipt outcomes");
    assert_eq!(outcomes, ["started", "orderly_shutdown"]);
    drop(connection);
    upgraded
        .complete_stream_session_terminal(
            StreamSessionReceiptId(1),
            StreamSessionTerminalOutcome::TransportEnded,
        )
        .expect("resolve retained crash receipt explicitly");
}

#[test]
fn upgrades_a_v2_database_without_losing_existing_tables() {
    let temp = crate::private_tempdir().expect("temp directory");
    let database_path = temp.path().join("hub.sqlite");
    let legacy_source_id = Uuid::new_v4();
    let connection = Connection::open(&database_path).expect("open v2 database");
    connection
            .execute_batch(
                "
                CREATE TABLE sources (
                    source_id TEXT PRIMARY KEY NOT NULL,
                    source_kind TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    created_at_ms INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE sync_manifests (
                    snapshot_id TEXT PRIMARY KEY NOT NULL,
                    vehicle_id TEXT NOT NULL,
                    head_sequence INTEGER NOT NULL CHECK (head_sequence >= 0),
                    manifest_json BLOB NOT NULL
                ) STRICT;
                CREATE TABLE sync_packs (
                    sha256 TEXT PRIMARY KEY NOT NULL,
                    snapshot_id TEXT NOT NULL REFERENCES sync_manifests(snapshot_id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                    relative_path TEXT NOT NULL,
                    compressed_bytes INTEGER NOT NULL CHECK (compressed_bytes > 0),
                    uncompressed_bytes INTEGER NOT NULL CHECK (uncompressed_bytes >= 100),
                    UNIQUE(snapshot_id, ordinal)
                ) STRICT;
                PRAGMA user_version = 2;
                ",
            )
            .expect("make v2 schema");
    connection
        .execute(
            "INSERT INTO sources (source_id, source_kind, generation, created_at_ms) \
                 VALUES (?1, 'legacy', 1, 1)",
            params![legacy_source_id.to_string()],
        )
        .expect("legacy source");
    drop(connection);
    // Schema migration is exercised only after the caller has established
    // the split-UID catalogue contract. A legacy 0644 catalogue remains
    // an explicit fail-closed admission case.
    fs::set_permissions(
        &database_path,
        fs::Permissions::from_mode(SHARED_SQLITE_FILE_MODE),
    )
    .expect("protect v2 catalogue for split identities");

    let store = HubStore::initialize(temp.path()).expect("migrate v2 store");
    let migrated = store.open().expect("open migrated store");
    assert_eq!(
        schema_version(&migrated).expect("schema version"),
        SCHEMA_VERSION
    );
    let legacy_count: i64 = migrated
        .query_row("SELECT COUNT(*) FROM sources", [], |row| row.get(0))
        .expect("legacy source preserved");
    assert_eq!(legacy_count, 1);
    let raw_table_count: i64 = migrated
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'raw_observations'",
            [],
            |row| row.get(0),
        )
        .expect("raw table exists");
    assert_eq!(raw_table_count, 1);
}

#[test]
fn upgrades_a_v1_database_through_v2_and_v3() {
    let temp = crate::private_tempdir().expect("temp directory");
    let database_path = temp.path().join("hub.sqlite");
    let connection = Connection::open(&database_path).expect("open v1 database");
    connection
        .execute_batch(
            "
                CREATE TABLE hub_metadata (
                    key TEXT PRIMARY KEY NOT NULL,
                    value TEXT NOT NULL
                ) STRICT;
                CREATE TABLE sources (
                    source_id TEXT PRIMARY KEY NOT NULL,
                    source_kind TEXT NOT NULL,
                    generation INTEGER NOT NULL CHECK (generation >= 1),
                    created_at_ms INTEGER NOT NULL
                ) STRICT;
                CREATE TABLE sync_ledger (
                    source_id TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
                    sequence INTEGER NOT NULL CHECK (sequence >= 1),
                    entity_kind TEXT NOT NULL,
                    entity_key TEXT NOT NULL,
                    operation TEXT NOT NULL CHECK (operation IN ('upsert', 'delete')),
                    committed_at_ms INTEGER NOT NULL,
                    PRIMARY KEY (source_id, sequence, entity_kind, entity_key)
                ) STRICT;
                PRAGMA user_version = 1;
                ",
        )
        .expect("make v1 schema");
    drop(connection);
    // See the v2 migration test: old catalogue permissions are not
    // upgraded in place by a service process.
    fs::set_permissions(
        &database_path,
        fs::Permissions::from_mode(SHARED_SQLITE_FILE_MODE),
    )
    .expect("protect v1 catalogue for split identities");

    let store = HubStore::initialize(temp.path()).expect("migrate v1 store");
    let migrated = store.open().expect("open migrated store");
    assert_eq!(
        schema_version(&migrated).expect("schema version"),
        SCHEMA_VERSION
    );
    for table in [
        "sync_manifests",
        "sync_packs",
        "source_identities",
        "vehicles",
        "raw_observations",
    ] {
        let found: i64 = migrated
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |row| row.get(0),
            )
            .expect("migrated table query");
        assert_eq!(found, 1, "missing table {table}");
    }
}

#[test]
fn upgrades_v36_catalogue_to_a_separate_digest_only_projection_state() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let connection = store.open().expect("current catalogue");
    remove_v50_current_observation_schema(&connection);
    connection
        .execute_batch(
            "
                DROP TABLE legacy_refresh_input_fences;
                DROP INDEX legacy_refresh_receipt_output_generation;
                DROP TABLE legacy_refresh_receipt_bindings;
                DROP TABLE supervised_collector_lease;
                DROP TABLE sync_retired_lineage_packs;
                DROP TABLE sync_retired_lineages;
                DROP TABLE teslamate_import_projection_state_rows;
                DROP TABLE teslamate_import_projection_state_heads;
                PRAGMA user_version = 36;
                ",
        )
        .expect("recreate historical v36 boundary");
    drop(connection);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade from v36");
    let connection = upgraded.open().expect("upgraded catalogue");
    assert_eq!(
        schema_version(&connection).expect("schema version"),
        SCHEMA_VERSION
    );
    for table in [
        "teslamate_import_projection_state_heads",
        "teslamate_import_projection_state_rows",
    ] {
        let found: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |row| row.get(0),
            )
            .expect("state table exists");
        assert_eq!(found, 1, "missing {table}");
    }
    let payload_column_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('teslamate_import_projection_state_rows')
                  WHERE name = 'payload_json'",
            [],
            |row| row.get(0),
        )
        .expect("state schema");
    assert_eq!(
        payload_column_count, 0,
        "Hub state must retain digests only"
    );
}

#[test]
fn upgrades_v38_teslamate_projection_catalogues_for_update_history() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("current store");
    let connection = store.open().expect("current catalogue");
    remove_v50_current_observation_schema(&connection);
    connection
            .execute_batch(
                "
                DROP TABLE legacy_refresh_input_fences;
                DROP INDEX legacy_refresh_receipt_output_generation;
                DROP TABLE legacy_refresh_receipt_bindings;
                DROP TABLE supervised_collector_lease;
                DROP TABLE sync_retired_lineage_packs;
                DROP TABLE sync_retired_lineages;
                DROP TABLE teslamate_import_projection_rows;
                DROP TABLE teslamate_import_projection_state_rows;
                CREATE TABLE teslamate_import_projection_rows (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'drive', 'position', 'charge', 'charge_sample', 'state'
                    )),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    PRIMARY KEY(vehicle_id, entity, entity_id)
                ) STRICT;
                CREATE TABLE teslamate_import_projection_state_rows (
                    vehicle_id TEXT NOT NULL
                        REFERENCES teslamate_import_projection_state_heads(vehicle_id) ON DELETE CASCADE,
                    entity TEXT NOT NULL CHECK(entity IN (
                        'car', 'drive', 'position', 'charge', 'charge_sample', 'state'
                    )),
                    entity_ordinal INTEGER NOT NULL CHECK(entity_ordinal BETWEEN 0 AND 5),
                    entity_id INTEGER NOT NULL CHECK(entity_id > 0),
                    car_id INTEGER NOT NULL CHECK(car_id > 0),
                    projection_sha256 BLOB NOT NULL CHECK(length(projection_sha256) = 32),
                    CHECK(
                        (entity = 'car' AND entity_ordinal = 0) OR
                        (entity = 'drive' AND entity_ordinal = 1) OR
                        (entity = 'position' AND entity_ordinal = 2) OR
                        (entity = 'charge' AND entity_ordinal = 3) OR
                        (entity = 'charge_sample' AND entity_ordinal = 4) OR
                        (entity = 'state' AND entity_ordinal = 5)
                    ),
                    PRIMARY KEY(vehicle_id, entity_ordinal, entity_id),
                    UNIQUE(vehicle_id, entity, entity_id)
                ) STRICT, WITHOUT ROWID;
                PRAGMA user_version = 38;
                ",
            )
            .expect("recreate historical v38 boundary");
    drop(connection);

    let upgraded = HubStore::initialize(temporary.path()).expect("upgrade v38 store");
    let connection = upgraded.open().expect("upgraded catalogue");
    assert_eq!(
        schema_version(&connection).expect("schema version"),
        SCHEMA_VERSION
    );
    for table in [
        "teslamate_import_projection_rows",
        "teslamate_import_projection_state_rows",
    ] {
        let sql: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |row| row.get(0),
            )
            .expect("upgraded table SQL");
        assert!(sql.contains("'update'"), "{table} accepts update rows");
    }
    let state_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master
                  WHERE type = 'table' AND name = 'teslamate_import_projection_state_rows'",
            [],
            |row| row.get(0),
        )
        .expect("upgraded state table SQL");
    assert!(
        state_sql.contains("BETWEEN 0 AND 6"),
        "durable update state has ordinal 6"
    );
}
