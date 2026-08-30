// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn digest_projection_state_is_atomic_with_base_and_successor_heads() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let car = import_delta_test_car(binding.selected_car_id);
    let inventory = TeslaMateImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let state = test_projection_state(temporary.path(), &car);
    store
        .finalize_teslamate_import_snapshot_with_projection_state(
            &manifest,
            Sha256Digest::of_bytes(b"projection-state-base"),
            &[],
            &binding,
            &inventory,
            &state,
        )
        .expect("catalogue base and state atomically");

    let mut lookup = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("verified projection-state lookup");
    assert_eq!(lookup.header().base_snapshot_id, manifest.snapshot_id);
    assert_eq!(lookup.header().head_sequence, manifest.head_sequence);
    assert!(
        lookup
            .digest(TeslaMateProjectionStateEntity::Car, binding.selected_car_id)
            .expect("lookup car digest")
            .is_some()
    );
    let page = lookup.page_after(None, 10).expect("bounded state page");
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].entity, TeslaMateProjectionStateEntity::Car);
    drop(lookup);

    let connection = store.open().expect("state catalogue");
    let (rows, digest_bytes): (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), COALESCE(MAX(length(projection_sha256)), 0)
                   FROM teslamate_import_projection_state_rows",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("digest-only rows");
    assert_eq!((rows, digest_bytes), (1, 32));
    drop(connection);

    let base = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("base lineage")
        .expect("base lineage exists");
    let delta = imported_typed_delta(&store, &binding, &base);
    let prior = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("prior state");
    let successor_state = TeslaMateProjectionState::create(
        temporary.path(),
        crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("successor state");
    let mut successor =
        crate::teslamate_projection_state::TeslaMateProjectionStateCapture::for_successor(
            successor_state,
            Box::new(prior),
        );
    assert_eq!(
        successor.record_car(&car).expect("capture unchanged car"),
        crate::teslamate_projection_state::TeslaMateProjectionStateChange::Unchanged
    );
    successor.seal().expect("seal successor state");
    let successor_state = successor.into_state();
    store
        .finalize_teslamate_import_delta_successor_with_projection_state(
            vehicle.vehicle_id,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
            Sha256Digest::of_bytes(b"projection-state-successor"),
            &[],
            &inventory,
            &successor_state,
        )
        .expect("catalogue successor and replacement state atomically");
    let lookup = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("updated state lookup");
    assert_eq!(lookup.header().head_sequence, delta.to_sequence);
}

#[test]
fn direct_import_successor_batch_is_atomic_and_advances_every_durable_head() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let car = import_delta_test_car(binding.selected_car_id);
    let inventory = TeslaMateImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let base_state = test_projection_state(temporary.path(), &car);
    store
        .finalize_teslamate_import_snapshot_with_projection_state(
            &manifest,
            Sha256Digest::of_bytes(b"direct-batch-base"),
            &[],
            &binding,
            &inventory,
            &base_state,
        )
        .expect("catalogue base");
    let base = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("base lineage")
        .expect("base lineage exists");
    let first = imported_typed_delta(&store, &binding, &base);
    let second = imported_typed_delta_after(
        &store,
        &binding,
        base.base.snapshot_id,
        first.to_sequence,
        first.chain_digest,
        first.pack.ordinal + 1,
    );
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
        )
        .expect("staging generation");
    store
        .stage_import_generation_session(
            run_id,
            &TeslaMateOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("stage direct-import tail");
    let successor_state = unchanged_direct_successor_projection_state(
        &store,
        run_id,
        vehicle.vehicle_id,
        &binding,
        &car,
    );

    store
        .finalize_import_generation_delta_successors_with_projection_state(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
            &[first.clone(), second.clone()],
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, second.to_sequence),
            Sha256Digest::of_bytes(b"direct-batch-successor"),
            &[],
            &successor_state,
            false,
        )
        .expect("atomically publish the complete direct-import batch");

    let lineage = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("lineage")
        .expect("lineage exists");
    assert_eq!(lineage.deltas, vec![first, second.clone()]);
    assert_eq!(lineage.head_sequence, second.to_sequence);
    let state = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("replacement digest state");
    assert_eq!(state.header().head_sequence, second.to_sequence);
    let connection = store.open().expect("catalogue");
    let generations: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("generation count");
    assert_eq!(generations, 0, "successful batch consumes its generation");
}

#[test]
fn direct_import_successor_batch_rejects_out_of_scope_state_without_advancing_heads() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let car = import_delta_test_car(binding.selected_car_id);
    let inventory = TeslaMateImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let base_state = test_projection_state(temporary.path(), &car);
    store
        .finalize_teslamate_import_snapshot_with_projection_state(
            &manifest,
            Sha256Digest::of_bytes(b"direct-batch-rollback-base"),
            &[],
            &binding,
            &inventory,
            &base_state,
        )
        .expect("catalogue base");
    let base = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("base lineage")
        .expect("base lineage exists");
    let first = imported_typed_delta(&store, &binding, &base);
    let second = imported_typed_delta_after(
        &store,
        &binding,
        base.base.snapshot_id,
        first.to_sequence,
        first.chain_digest,
        first.pack.ordinal + 1,
    );
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
        )
        .expect("staging generation");
    store
        .stage_import_generation_session(
            run_id,
            &TeslaMateOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("stage direct-import tail");
    // A run-bound descriptor validates its selected-car scope before the
    // destination transaction begins, so a foreign-car spool cannot even
    // reach delta insertion.
    let wrong_car_state = direct_test_projection_state(
        &store,
        run_id,
        &import_delta_test_car(binding.selected_car_id + 1),
    );

    assert!(matches!(
        store.finalize_import_generation_delta_successors_with_projection_state(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
            &[first, second.clone()],
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, second.to_sequence),
            Sha256Digest::of_bytes(b"direct-batch-rollback"),
            &[],
            &wrong_car_state,
            false,
        ),
        Err(StoreError::TeslaMateProjectionState(
            TeslaMateProjectionStateError::TransferRowContractMismatch
        ))
    ));
    assert_eq!(
        store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("lineage after rollback"),
        Some(base),
        "a bad second-stage state must not expose a prefix of the delta batch"
    );
    let state = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("base digest state survives");
    assert_eq!(state.header().head_sequence, manifest.head_sequence);
    let connection = store.open().expect("catalogue");
    let generations: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("generation count");
    assert_eq!(generations, 1, "rollback retains the staging generation");
}

#[test]
fn direct_import_successor_batch_refuses_a_gap_in_pack_ordinals() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let car = import_delta_test_car(binding.selected_car_id);
    let inventory = TeslaMateImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let base_state = test_projection_state(temporary.path(), &car);
    store
        .finalize_teslamate_import_snapshot_with_projection_state(
            &manifest,
            Sha256Digest::of_bytes(b"direct-batch-ordinal-base"),
            &[],
            &binding,
            &inventory,
            &base_state,
        )
        .expect("catalogue base");
    let base = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("base lineage")
        .expect("base lineage exists");
    let gapped = imported_typed_delta_after(
        &store,
        &binding,
        base.base.snapshot_id,
        base.head_sequence,
        base.head_digest,
        // The base owns ordinal zero, so a successor must start at one.
        2,
    );
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
        )
        .expect("staging generation");
    store
        .stage_import_generation_session(
            run_id,
            &TeslaMateOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("stage direct-import tail");
    let successor_state = unchanged_direct_successor_projection_state(
        &store,
        run_id,
        vehicle.vehicle_id,
        &binding,
        &car,
    );

    assert!(matches!(
        store.finalize_import_generation_delta_successors_with_projection_state(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
            std::slice::from_ref(&gapped),
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, gapped.to_sequence),
            Sha256Digest::of_bytes(b"direct-batch-ordinal-gap"),
            &[],
            &successor_state,
            false,
        ),
        Err(StoreError::LineageCatalogConflict)
    ));
    assert_eq!(
        store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("lineage after ordinal rejection"),
        Some(base),
        "an ordinal gap must not advance the lineage head"
    );
    let connection = store.open().expect("catalogue");
    let generations: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("generation count");
    assert_eq!(generations, 1, "ordinal rejection retains the generation");
}

#[test]
fn legacy_inventory_without_digest_state_fails_closed_distinctly() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let inventory = TeslaMateImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    store
        .finalize_teslamate_import_snapshot(
            &manifest,
            Sha256Digest::of_bytes(b"legacy-inventory-only"),
            &[],
            &binding,
            &inventory,
        )
        .expect("legacy inventory base");
    assert!(matches!(
        store.teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        ),
        Err(StoreError::TeslaMateImportProjectionStateMissing(id)) if id == vehicle.vehicle_id
    ));
}

fn legacy_direct_bridge_fixture(
    store: &HubStore,
    legacy_fingerprint: Sha256Digest,
) -> (VehicleRecord, ProjectionBinding, SyncManifest) {
    let (vehicle, binding, manifest) = v2_base_manifest(store);
    let inventory = TeslaMateImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: vec![ProjectionTombstone {
            entity: ProjectionDeltaEntity::Position,
            id: 10,
            car_id: binding.selected_car_id,
        }],
    };
    store
        .finalize_teslamate_import_snapshot(
            &manifest,
            legacy_fingerprint,
            &[],
            &binding,
            &inventory,
        )
        .expect("legacy inventory-only base");
    (vehicle, binding, manifest)
}

fn legacy_direct_bridge_generation(
    store: &HubStore,
    vehicle: &VehicleRecord,
    binding: &ProjectionBinding,
) -> Uuid {
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
        )
        .expect("bridge staging generation");
    store
        .stage_import_generation_session(
            run_id,
            &TeslaMateOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("bridge staging session");
    run_id
}

#[test]
fn legacy_direct_bridge_attaches_state_without_pack_delta_or_sequence_and_logical_rerun_skips() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
    let logical_fingerprint = Sha256Digest::of_bytes(b"logical-direct-projection");
    let (vehicle, binding, manifest) = legacy_direct_bridge_fixture(&store, legacy_fingerprint);
    assert!(
        store
            .legacy_teslamate_direct_bridge_is_eligible(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("legacy base is bridge eligible")
    );
    let lineage_before = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("legacy lineage")
        .expect("legacy base lineage");
    let sequence_before: Option<i64> = store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT next_sequence FROM vehicle_snapshot_sequences WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .expect("sequence before bridge");
    let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
    let state = direct_projection_state_with_digest_rows(
        &store,
        run_id,
        binding.selected_car_id,
        &[(TeslaMateProjectionStateEntity::Position, 10)],
    );

    let bridged = store
        .bridge_legacy_teslamate_direct_import(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            legacy_fingerprint,
            logical_fingerprint,
            &state,
        )
        .expect("unchanged legacy base bridges atomically");
    assert_eq!(bridged.snapshot_id, manifest.snapshot_id);
    assert_eq!(bridged.head_sequence, manifest.head_sequence);
    assert_eq!(bridged.total_rows, manifest.total_rows);
    assert!(
        store
            .source_fingerprint_matches(vehicle.vehicle_id, logical_fingerprint)
            .expect("logical fingerprint is now current"),
        "the next logical direct capture must take the normal skip guard"
    );
    assert!(
        !store
            .source_fingerprint_matches(vehicle.vehicle_id, legacy_fingerprint)
            .expect("retired physical fingerprint is replaced")
    );
    assert!(
        store
            .teslamate_import_projection_state_exists(vehicle.vehicle_id)
            .expect("state head exists")
    );
    let state_lookup = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("bridged durable state lookup");
    assert_eq!(state_lookup.header().base_snapshot_id, manifest.snapshot_id);
    assert_eq!(state_lookup.header().head_sequence, manifest.head_sequence);
    drop(state_lookup);
    assert!(
        !store
            .legacy_teslamate_direct_bridge_is_eligible(
                vehicle.vehicle_id,
                binding.account_id,
                binding.selected_car_id,
            )
            .expect("bridge is one-time"),
        "the persisted state/marker prevents a second bridge"
    );
    assert_eq!(
        store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("lineage after bridge"),
        Some(lineage_before),
        "bridge must retain the exact immutable base/head"
    );
    let connection = store.open().expect("catalogue after bridge");
    for (table, expected) in [
        ("sync_bases", 1),
        ("sync_deltas", 0),
        ("sync_packs", 1),
        ("teslamate_import_projection_state_bridges", 1),
        ("import_generations", 0),
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, expected, "bridge must not add {table}");
    }
    let sequence_after: Option<i64> = connection
        .query_row(
            "SELECT next_sequence FROM vehicle_snapshot_sequences WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .expect("sequence after bridge");
    assert_eq!(
        sequence_after, sequence_before,
        "bridge must not reserve a sequence"
    );
}

#[test]
fn legacy_direct_bridge_rejects_changed_physical_fingerprint_without_mutation() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
    let (vehicle, binding, manifest) = legacy_direct_bridge_fixture(&store, legacy_fingerprint);
    let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
    let state = direct_projection_state_with_digest_rows(
        &store,
        run_id,
        binding.selected_car_id,
        &[(TeslaMateProjectionStateEntity::Position, 10)],
    );
    let changed_physical = Sha256Digest::of_bytes(b"changed-direct-physical");

    assert!(matches!(
        store.bridge_legacy_teslamate_direct_import(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            changed_physical,
            Sha256Digest::of_bytes(b"logical-direct-projection"),
            &state,
        ),
        Err(StoreError::TeslaMateLegacyDirectRebaseRequired(id)) if id == vehicle.vehicle_id
    ));
    let connection = store.open().expect("catalogue after rejection");
    for (table, expected) in [
        ("teslamate_import_projection_state_heads", 0),
        ("teslamate_import_projection_state_rows", 0),
        ("teslamate_import_projection_state_bridges", 0),
        ("sync_deltas", 0),
        ("sync_packs", 1),
        ("import_generations", 1),
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, expected, "mismatch must not alter {table}");
    }
    assert!(
        store
            .source_fingerprint_matches(vehicle.vehicle_id, legacy_fingerprint)
            .expect("legacy fingerprint remains current")
    );
    assert_eq!(
        store
            .manifest_for_vehicle(vehicle.vehicle_id)
            .expect("base manifest remains"),
        Some(manifest)
    );
}

#[test]
fn legacy_direct_bridge_rolls_back_state_when_inventory_semantics_mismatch() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
    let (vehicle, binding, _manifest) = legacy_direct_bridge_fixture(&store, legacy_fingerprint);
    let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
    let mismatched_state = direct_projection_state_with_digest_rows(
        &store,
        run_id,
        binding.selected_car_id,
        &[
            (TeslaMateProjectionStateEntity::Position, 10),
            (TeslaMateProjectionStateEntity::Position, 11),
        ],
    );

    assert!(matches!(
        store.bridge_legacy_teslamate_direct_import(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            legacy_fingerprint,
            Sha256Digest::of_bytes(b"logical-direct-projection"),
            &mismatched_state,
        ),
        Err(StoreError::TeslaMateLegacyDirectRebaseRequired(id)) if id == vehicle.vehicle_id
    ));
    let connection = store.open().expect("catalogue after rollback");
    for (table, expected) in [
        ("teslamate_import_projection_state_heads", 0),
        ("teslamate_import_projection_state_rows", 0),
        ("teslamate_import_projection_state_bridges", 0),
        ("snapshot_fingerprints", 1),
        ("import_generations", 1),
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, expected, "failed bridge must roll back {table}");
    }
    assert!(
        store
            .source_fingerprint_matches(vehicle.vehicle_id, legacy_fingerprint)
            .expect("fingerprint rollback")
    );
}

#[test]
fn legacy_direct_bridge_rejects_a_carless_sealed_state_without_installing_a_head() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let legacy_fingerprint = Sha256Digest::of_bytes(b"legacy-direct-physical");
    let (vehicle, binding, _manifest) = legacy_direct_bridge_fixture(&store, legacy_fingerprint);
    let run_id = legacy_direct_bridge_generation(&store, &vehicle, &binding);
    let mut carless_state = create_direct_import_projection_state(&store, run_id, 10);
    carless_state
        .record(
            TeslaMateProjectionStateEntity::Position,
            10,
            binding.selected_car_id,
            &serde_json::json!({"id": 10}),
        )
        .expect("record matching non-car row");
    carless_state.seal().expect("seal carless state");

    assert!(matches!(
        store.bridge_legacy_teslamate_direct_import(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            legacy_fingerprint,
            Sha256Digest::of_bytes(b"logical-direct-projection"),
            &carless_state,
        ),
        Err(StoreError::TeslaMateLegacyDirectRebaseRequired(id)) if id == vehicle.vehicle_id
    ));
    let connection = store.open().expect("catalogue after rejection");
    for table in [
        "teslamate_import_projection_state_heads",
        "teslamate_import_projection_state_rows",
        "teslamate_import_projection_state_bridges",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0, "carless bridge must not write {table}");
    }
    let generations: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("staging generation count");
    assert_eq!(generations, 1, "failed bridge remains retryable");
}

#[test]
fn unsealed_state_refuses_base_finalization_without_partial_catalogue() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let inventory = TeslaMateImportProjectionInventory {
        source_id: binding.account_id,
        selected_car_id: binding.selected_car_id,
        rows: Vec::new(),
    };
    let mut unsealed = TeslaMateProjectionState::create(
        temporary.path(),
        crate::teslamate_projection_state::TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("unsealed state");
    unsealed
        .record_car(&import_delta_test_car(binding.selected_car_id))
        .expect("capture car");
    assert!(matches!(
        store.finalize_teslamate_import_snapshot_with_projection_state(
            &manifest,
            Sha256Digest::of_bytes(b"unsealed-projection-state"),
            &[],
            &binding,
            &inventory,
            &unsealed,
        ),
        Err(StoreError::TeslaMateProjectionState(
            TeslaMateProjectionStateError::StateNotSealed
        ))
    ));
    assert!(
        store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("lineage lookup")
            .is_none()
    );
    let connection = store.open().expect("catalogue");
    for table in [
        "sync_bases",
        "sync_heads",
        "teslamate_import_projection_state_heads",
        "teslamate_import_projection_state_rows",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0, "failed finalizer must not write {table}");
    }
}

#[test]
fn direct_base_set_transfer_preserves_state_and_legacy_inventory_semantics() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
        )
        .expect("staging generation");
    store
        .stage_import_generation_session(
            run_id,
            &TeslaMateOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("staging session");
    let rows = [
        (TeslaMateProjectionStateEntity::Drive, 11),
        (TeslaMateProjectionStateEntity::Position, 12),
        (TeslaMateProjectionStateEntity::Charge, 13),
        (TeslaMateProjectionStateEntity::ChargeSample, 14),
        (TeslaMateProjectionStateEntity::State, 15),
    ];
    let state =
        direct_projection_state_with_digest_rows(&store, run_id, binding.selected_car_id, &rows);
    let expected_state = state
        .page(None, MAX_PAGE_SIZE)
        .expect("sealed state page")
        .rows;

    store
        .finalize_import_generation_with_projection_state(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
            &manifest,
            Sha256Digest::of_bytes(b"set-transfer-semantic-parity"),
            &[],
            &binding,
            &state,
            false,
        )
        .expect("set-based direct base finalization");

    let mut lookup = store
        .teslamate_import_projection_state_lookup(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("durable state lookup");
    let actual_state = lookup
        .page_after_store(None, MAX_PAGE_SIZE)
        .expect("durable state page")
        .rows;
    assert_eq!(
        actual_state, expected_state,
        "set transfer preserves every digest row"
    );
    drop(lookup);
    let inventory = store
        .teslamate_import_projection_inventory(
            vehicle.vehicle_id,
            binding.account_id,
            binding.selected_car_id,
        )
        .expect("legacy inventory");
    assert_eq!(
        inventory
            .rows
            .iter()
            .map(|row| (row.entity, row.id, row.car_id))
            .collect::<Vec<_>>(),
        vec![
            (ProjectionDeltaEntity::Charge, 13, binding.selected_car_id),
            (
                ProjectionDeltaEntity::ChargeSample,
                14,
                binding.selected_car_id
            ),
            (ProjectionDeltaEntity::Drive, 11, binding.selected_car_id),
            (ProjectionDeltaEntity::Position, 12, binding.selected_car_id),
            (ProjectionDeltaEntity::State, 15, binding.selected_car_id),
        ],
        "legacy inventory remains the non-car projection-state view"
    );
    let connection = store.open().expect("catalogue after transfer");
    for table in [
        "teslamate_import_projection_heads",
        "teslamate_import_projection_rows",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("direct import leaves no duplicate legacy inventory");
        assert_eq!(count, 0, "direct import does not duplicate {table}");
    }
    let remaining_generations: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("generation count");
    assert_eq!(
        remaining_generations, 0,
        "commit removes the staged generation"
    );
}

#[test]
fn direct_base_rejects_carless_sealed_state_before_catalogue_mutation() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let run_id = store
        .begin_import_generation(
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
        )
        .expect("staging generation");
    store
        .stage_import_generation_session(
            run_id,
            &TeslaMateOpenSession {
                car_id: binding.selected_car_id,
                ..Default::default()
            },
        )
        .expect("staging session");
    let mut state = create_direct_import_projection_state(&store, run_id, 10);
    state
        .record(
            TeslaMateProjectionStateEntity::Position,
            12,
            binding.selected_car_id,
            &serde_json::json!({"id": 12}),
        )
        .expect("record carless state row");
    state.seal().expect("seal carless state");

    assert!(matches!(
        store.finalize_import_generation_with_projection_state(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            binding.selected_car_id,
            2_000,
            &manifest,
            Sha256Digest::of_bytes(b"carless-state"),
            &[],
            &binding,
            &state,
            false,
        ),
        Err(StoreError::TeslaMateProjectionState(
            TeslaMateProjectionStateError::TransferCarContractMismatch
        ))
    ));
    let connection = store.open().expect("catalogue after rejection");
    for table in [
        "sync_bases",
        "sync_heads",
        "teslamate_import_projection_state_heads",
        "teslamate_import_projection_state_rows",
        "teslamate_import_projection_heads",
        "teslamate_import_projection_rows",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(count, 0, "carless state must not write {table}");
    }
    let generations: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("staging generation count");
    assert_eq!(generations, 1, "rejected generation remains retryable");
}

#[test]
fn sealed_transfer_rejects_a_same_shape_spool_substitution() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, _) = v2_base_manifest(&store);
    let state = projection_state_with_digest_rows(
        temporary.path(),
        binding.selected_car_id,
        &[(TeslaMateProjectionStateEntity::Position, 12)],
    );
    let transfer = state
        .sealed_transfer(binding.selected_car_id)
        .expect("sealed transfer descriptor");
    let replacement = projection_state_with_digest_rows(
        temporary.path(),
        binding.selected_car_id,
        &[(TeslaMateProjectionStateEntity::Position, 99)],
    );
    std::fs::rename(replacement.path_for_test(), transfer.path())
        .expect("replace descriptor path with same-shape foreign spool");

    let connection = store.open().expect("catalogue connection");
    assert!(matches!(
        attach_teslamate_projection_state_transfer(&connection, &transfer),
        Err(StoreError::TeslaMateProjectionState(
            TeslaMateProjectionStateError::TransferDigestMismatch
        ))
    ));
    let state_heads: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM teslamate_import_projection_state_heads WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
            |row| row.get(0),
        )
        .expect("catalogue remains untouched");
    assert_eq!(state_heads, 0);
}

#[test]
fn sealed_transfer_attachment_is_read_only() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (_, binding, _) = v2_base_manifest(&store);
    let state = projection_state_with_digest_rows(
        temporary.path(),
        binding.selected_car_id,
        &[(TeslaMateProjectionStateEntity::Position, 12)],
    );
    let transfer = state
        .sealed_transfer(binding.selected_car_id)
        .expect("sealed transfer descriptor");
    let connection = store.open().expect("catalogue connection");
    attach_teslamate_projection_state_transfer(&connection, &transfer)
        .expect("attach sealed spool read-only");
    assert!(
        connection
            .execute(
                "DELETE FROM teslamate_projection_state_spool.current_rows",
                [],
            )
            .is_err(),
        "SQLite mode=ro attachment must reject source mutation"
    );
    detach_teslamate_projection_state_transfer(&store, &connection)
        .expect("detach read-only sealed spool");
    assert_eq!(
        state
            .page(None, MAX_PAGE_SIZE)
            .expect("source state remains readable")
            .rows
            .len(),
        2,
        "failed attachment write must leave the source spool intact"
    );
}

#[test]
fn direct_import_base_refuses_a_staging_car_outside_its_v2_binding() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let wrong_car_id = binding.selected_car_id + 1;
    let run_id = store
        .begin_import_generation(binding.account_id, vehicle.vehicle_id, wrong_car_id, 2_000)
        .expect("staging generation");
    store
        .stage_import_generation_session(
            run_id,
            &TeslaMateOpenSession {
                car_id: wrong_car_id,
                ..Default::default()
            },
        )
        .expect("stage direct-import tail");
    let state = direct_test_projection_state(
        &store,
        run_id,
        &import_delta_test_car(binding.selected_car_id),
    );

    assert!(matches!(
        store.finalize_import_generation_with_projection_state(
            run_id,
            binding.account_id,
            vehicle.vehicle_id,
            wrong_car_id,
            2_000,
            &manifest,
            Sha256Digest::of_bytes(b"wrong-staging-car"),
            &[],
            &binding,
            &state,
            false,
        ),
        Err(StoreError::LineageCatalogConflict)
    ));
    assert!(
        store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("lineage lookup")
            .is_none(),
        "a mismatched staging car must not publish the base"
    );
    let connection = store.open().expect("catalogue");
    let generations: i64 = connection
        .query_row("SELECT COUNT(*) FROM import_generations", [], |row| {
            row.get(0)
        })
        .expect("generation count");
    assert_eq!(generations, 1, "rejected generation remains retryable");
}

#[test]
fn v2_base_requires_immutable_binding_at_generic_publication_boundaries() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    let base_digest = manifest.chunks[0].sha256;
    let lineage = LineageManifestV2 {
        protocol: LINEAGE_PROTOCOL_V2,
        capability: LineageCapability::ImmutableBaseOrderedDeltas,
        schema: HUB_PROJECTION_SCHEMA_V2,
        installation_id: manifest.installation_id,
        account_id: manifest.account_id,
        vehicle_id: manifest.vehicle_id,
        generation: manifest.generation,
        base: LineageBase {
            snapshot_id: manifest.snapshot_id,
            sequence: manifest.base_sequence,
            digest: base_digest,
            packs: manifest.chunks.clone(),
        },
        deltas: Vec::new(),
        head_sequence: manifest.head_sequence,
        head_digest: base_digest,
        terminal_cursor: manifest.terminal_cursor.clone(),
    };
    lineage.validate().expect("valid V2 base lineage");

    assert!(matches!(
        store.publish_manifest(&manifest),
        Err(StoreError::ImmutableBaseBindingMissing(vehicle_id)) if vehicle_id == vehicle.vehicle_id
    ));
    assert!(matches!(
        store.commit_lineage_catalog(&lineage),
        Err(StoreError::ImmutableBaseBindingMissing(vehicle_id)) if vehicle_id == vehicle.vehicle_id
    ));
    let connection = store.open().expect("open rejected catalogue");
    for table in [
        "sync_manifests",
        "sync_packs",
        "sync_bases",
        "sync_heads",
        "v2_base_bindings",
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("rejected catalogue count");
        assert_eq!(count, 0, "generic V2 publication must not write {table}");
    }
    drop(connection);

    store
        .finalize_import_snapshot_with_binding(
            &manifest,
            Sha256Digest::of_bytes(b"generic-v2-binding-regression"),
            &[],
            &binding,
        )
        .expect("binding-aware V2 finalizer");
    assert_eq!(
        store
            .v2_projection_binding(vehicle.vehicle_id)
            .expect("persisted V2 binding"),
        binding
    );
}

#[test]
fn schema_35_upgrade_recovers_v2_binding_from_immutable_base_not_mutable_source() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    store
        .finalize_import_snapshot_with_binding(
            &manifest,
            Sha256Digest::of_bytes(b"legacy-binding-upgrade"),
            &[],
            &binding,
        )
        .expect("catalogue historical V2 base");

    let connection = store.open().expect("historical catalogue");
    remove_v50_current_observation_schema(&connection);
    connection
        .execute(
            "DELETE FROM v2_base_bindings WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
        )
        .expect("remove post-v35 binding");
    connection
        .execute(
            "UPDATE sources SET generation = 9 WHERE source_id = ?1",
            params![binding.account_id.to_string()],
        )
        .expect("mutate current source generation");
    connection
        .execute(
            "UPDATE vehicles SET source_vehicle_key = '999999' WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
        )
        .expect("mutate current source vehicle key");
    connection
        .execute_batch(
            "DROP TABLE legacy_refresh_input_fences;
                 DROP INDEX legacy_refresh_receipt_output_generation;
                 DROP TABLE legacy_refresh_receipt_bindings;
                 DROP TABLE supervised_collector_lease;
                 DROP TABLE sync_retired_lineage_packs;
                 DROP TABLE sync_retired_lineages;
                 DROP TABLE teslamate_import_projection_rows;
                 DROP TABLE teslamate_import_projection_heads;
                 DROP TABLE v2_base_bindings;
                 PRAGMA user_version = 35;",
        )
        .expect("restore historical schema boundary");
    drop(connection);

    let upgraded = HubStore::initialize(temporary.path()).expect("recover immutable binding");
    assert_eq!(
        upgraded
            .v2_projection_binding(vehicle.vehicle_id)
            .expect("recovered V2 binding"),
        binding,
        "upgrade must use the stored base identity and packed source car"
    );
    assert!(
        upgraded
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("upgraded lineage lookup")
            .is_some()
    );
}

#[test]
fn legacy_v2_binding_recovery_fails_closed_on_manifest_pack_identity_conflict() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, manifest) = v2_base_manifest(&store);
    store
        .finalize_import_snapshot_with_binding(
            &manifest,
            Sha256Digest::of_bytes(b"legacy-binding-conflict"),
            &[],
            &binding,
        )
        .expect("catalogue historical V2 base");

    let mut conflicting_manifest = manifest.clone();
    conflicting_manifest.account_id = Uuid::new_v4();
    let conflicting_json = serde_json::to_vec(&conflicting_manifest).expect("manifest JSON");
    let connection = store.open().expect("historical catalogue");
    connection
        .execute(
            "DELETE FROM v2_base_bindings WHERE vehicle_id = ?1",
            params![vehicle.vehicle_id.to_string()],
        )
        .expect("remove binding");
    connection
        .execute(
            "UPDATE sync_manifests SET manifest_json = ?1 WHERE snapshot_id = ?2",
            params![conflicting_json, manifest.snapshot_id.to_string()],
        )
        .expect("inject manifest conflict");
    drop(connection);

    assert!(matches!(
        HubStore::initialize(temporary.path()),
        Err(StoreError::LineageCatalogConflict)
    ));
    let connection = store.open().expect("catalogue after failed recovery");
    let bindings: i64 = connection
        .query_row("SELECT COUNT(*) FROM v2_base_bindings", [], |row| {
            row.get(0)
        })
        .expect("binding count");
    assert_eq!(bindings, 0, "failed recovery must roll back atomically");
}
