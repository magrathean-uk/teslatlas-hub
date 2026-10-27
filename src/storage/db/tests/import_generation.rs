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
    assert_eq!(
        store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("generic reader"),
        Some(lineage.clone())
    );
    let reopened = HubStore::initialize(temp.path()).expect("reopen");
    assert_eq!(
        reopened
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("generic reopen reader"),
        Some(lineage.clone())
    );
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
    // Identity is recovered from the immutable envelope, and must still be
    // bound to this route/base instead of silently accepting a changed row.
    let mut wrong_vehicle = lineage;
    wrong_vehicle.vehicle_id = Uuid::new_v4();
    reopened
        .open()
        .unwrap()
        .execute(
            "UPDATE sync_manifests SET manifest_json = ?1 WHERE snapshot_id = ?2",
            params![
                serde_json::to_vec(&wrong_vehicle).unwrap(),
                base_snapshot_id.to_string()
            ],
        )
        .expect("inject mismatched generic envelope");
    assert!(matches!(
        reopened.lineage_manifest_for_vehicle(vehicle.vehicle_id),
        Err(StoreError::LineageCatalogConflict)
    ));
}

#[test]
fn typed_lineage_reader_still_requires_immutable_binding() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, _, prior) = imported_v2_base(&store);
    assert_eq!(
        store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .unwrap(),
        Some(prior)
    );
    store
        .open()
        .unwrap()
        .execute(
            "DELETE FROM v2_base_bindings WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
        )
        .expect("remove immutable binding");
    assert!(matches!(
        store.lineage_manifest_for_vehicle(vehicle.vehicle_id),
        Err(StoreError::ImmutableBaseBindingMissing(id)) if id == vehicle.vehicle_id
    ));
}

#[test]
fn import_generation_staging_survives_active_state_and_promotes_once() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let (source, vehicle) = test_registered_vehicle(&store);
    let active = crate::source_db_projection::SourceDbOpenSession {
        car_id: 10,
        state: Some(crate::source_db_projection::SourceDbState {
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
        .register_source(
            &SourceDescriptor::new(crate::db::IMPORT_SOURCE_KIND, "multi-car"),
            1_000,
        )
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
    let first_session = crate::source_db_projection::SourceDbOpenSession {
        car_id: 10,
        state: Some(crate::source_db_projection::SourceDbState {
            id: 1,
            car_id: 10,
            state: "online".into(),
            start_date_ms: 1_000,
            end_date_ms: None,
        }),
        ..Default::default()
    };
    let second_session = crate::source_db_projection::SourceDbOpenSession {
        car_id: 20,
        state: Some(crate::source_db_projection::SourceDbState {
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
    let session = crate::source_db_projection::SourceDbOpenSession {
        car_id: 10,
        state: Some(crate::source_db_projection::SourceDbState {
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
    let active = crate::source_db_projection::SourceDbOpenSession {
        car_id: 10,
        state: Some(crate::source_db_projection::SourceDbState {
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
    let session = crate::source_db_projection::SourceDbOpenSession {
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
