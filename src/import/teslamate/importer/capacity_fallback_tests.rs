use super::*;

#[test]
fn exhausted_import_lineage_rotation_is_atomic_and_preserves_pairing_and_grace() {
    let temp = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temp.path()).unwrap();
    let source = store
        .register_source(
            &SourceDescriptor::new("teslamate", "rotation"),
            1_700_000_000_000,
        )
        .unwrap();
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "rotation-car"),
            1_700_000_000_000,
        )
        .unwrap();
    let binding = ProjectionBinding {
        installation_id: store.installation_id().unwrap(),
        account_id: source.source_id,
        vehicle_id: vehicle.vehicle_id,
        generation: source.generation,
        selected_car_id: 1,
    };
    let gate = store.try_acquire_publication_gate().unwrap();
    let key = CursorKey::from_bytes([43; 32]);
    let base_projection = project_vehicle(&history(), 1).unwrap();
    let base_run = begin_direct_state_generation(&store, &binding, 1_700_000_000_000);
    let mut capture = direct_projection_state_capture(
        &store,
        &gate,
        base_run,
        binding.vehicle_id,
        binding.account_id,
        1,
        false,
        direct_state_test_limits(),
    )
    .unwrap();
    record_projected_direct_state(&mut capture, &base_projection, 1);
    capture.seal().unwrap();
    let base_state = capture.into_state();
    let make_pack = |projection: &TeslaMateProjection| {
        let sequence = store
            .reserve_next_full_snapshot_sequence(&gate, binding.vehicle_id)
            .unwrap();
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: binding.clone(),
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &projection.snapshot,
        };
        let pack = ProjectionPackWriter::new(store.packs_dir())
            .write_full_snapshot_with_states_and_updates(
                &request,
                &projection.states,
                &projection.updates,
            )
            .unwrap();
        let manifest = request
            .signed_manifest_with_states_and_updates(
                &pack,
                &projection.states,
                &projection.updates,
                &key,
            )
            .unwrap();
        (pack, manifest)
    };
    let (base_pack, base_manifest) = make_pack(&base_projection);
    let base_fingerprint = Sha256Digest::of_bytes(b"rotation-base");
    store
        .finalize_import_generation_with_projection_state(
            base_run,
            binding.account_id,
            binding.vehicle_id,
            1,
            1_700_000_000_000,
            &base_manifest,
            base_fingerprint,
            &[],
            &binding,
            &base_state,
            false,
        )
        .unwrap();
    let initial = store
        .lineage_manifest_for_vehicle(binding.vehicle_id)
        .unwrap()
        .unwrap();
    // A normal successor still uses a typed delta, then rotation retains both
    // its checkpoint and the original base objects.
    let delta_run = begin_direct_state_generation(&store, &binding, 1_700_000_000_001);
    let mut delta_capture = direct_projection_state_capture(
        &store,
        &gate,
        delta_run,
        binding.vehicle_id,
        binding.account_id,
        1,
        true,
        direct_state_test_limits(),
    )
    .unwrap();
    record_projected_direct_state(&mut delta_capture, &base_projection, 1);
    delta_capture.seal().unwrap();
    let delta_sequence = store
        .reserve_next_full_snapshot_sequence(&gate, binding.vehicle_id)
        .unwrap();
    let mut built_delta = None;
    direct_delta_rows_from_capture(
        &mut delta_capture,
        &binding,
        &base_projection.snapshot.cars[0],
        1,
        |batch| {
            let delta = batch.into_delta(
                binding.clone(),
                SequenceRange {
                    from_exclusive: initial.head_sequence,
                    to_inclusive: delta_sequence,
                },
                initial.head_digest,
            );
            built_delta = Some(
                ProjectionPackWriter::new(store.packs_dir())
                    .write_delta(&ProjectionDeltaPackRequest {
                        pack_id: Uuid::new_v4(),
                        snapshot_id: initial.base.snapshot_id,
                        ordinal: 1,
                        delta: &delta,
                    })
                    .unwrap(),
            );
            Ok(())
        },
    )
    .unwrap();
    let delta_pack = built_delta.unwrap();
    let chain = canonical_delta_chain_digest(initial.head_digest, delta_pack.metadata.sha256);
    let delta = LineageDelta {
        from_sequence: initial.head_sequence,
        to_sequence: delta_sequence,
        parent_chain_digest: initial.head_digest,
        chain_digest: chain,
        pack_digest: delta_pack.metadata.sha256,
        pack: delta_pack.metadata.clone(),
    };
    let cursor = OpaqueCursor::issue(
        &key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: delta_sequence,
        },
    )
    .unwrap();
    let delta_state = delta_capture.into_state();
    store
        .finalize_import_generation_delta_successors_with_projection_state(
            delta_run,
            binding.account_id,
            binding.vehicle_id,
            1,
            1_700_000_000_001,
            &[delta],
            &key,
            &cursor,
            base_fingerprint,
            &[],
            &delta_state,
            false,
        )
        .unwrap();
    let old = store
        .lineage_manifest_for_vehicle(binding.vehicle_id)
        .unwrap()
        .unwrap();
    assert_eq!(old.base.snapshot_id, initial.base.snapshot_id);
    assert_eq!(old.deltas.len(), 1);
    let invitation = store
        .create_pairing("rotation-device", 1000, 61000)
        .unwrap();
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "rotation-device",
            2000,
        )
        .unwrap();
    let identities = identity_registry_image(&store);
    let mut next_history = history();
    next_history.drives.push(completed_drive(99));
    let next_projection = project_vehicle(&next_history, 1).unwrap();
    let next_run = begin_direct_state_generation(&store, &binding, 1_700_000_000_001);
    let mut next_capture = direct_projection_state_capture(
        &store,
        &gate,
        next_run,
        binding.vehicle_id,
        binding.account_id,
        1,
        true,
        direct_state_test_limits(),
    )
    .unwrap();
    record_projected_direct_state(&mut next_capture, &next_projection, 1);
    next_capture.seal().unwrap();
    assert!(
        direct_delta_capacity_exhausted(
            &mut next_capture,
            &binding,
            &next_projection.snapshot.cars[0],
            0
        )
        .unwrap()
    );
    assert!(
        !direct_delta_capacity_exhausted(
            &mut next_capture,
            &binding,
            &next_projection.snapshot.cars[0],
            1
        )
        .unwrap()
    );
    let mut wrong_car = binding.clone();
    wrong_car.selected_car_id = 2;
    assert!(
        direct_delta_capacity_exhausted(
            &mut next_capture,
            &wrong_car,
            &next_projection.snapshot.cars[0],
            1
        )
        .is_err(),
        "identity errors must propagate rather than select replacement"
    );
    let next_state = next_capture.into_state();
    let (next_pack, next_manifest) = make_pack(&next_projection);
    assert_ne!(base_pack.metadata.sha256, next_pack.metadata.sha256);
    let next_fingerprint = Sha256Digest::of_bytes(b"rotation-next");
    let rotate = || {
        store.finalize_import_generation_replacing_base_with_materialisation(
            &gate,
            &old,
            next_run,
            binding.account_id,
            binding.vehicle_id,
            1,
            1_700_000_000_001,
            &next_manifest,
            next_fingerprint,
            &[],
            &binding,
            &next_state,
            &next_projection.snapshot.cars[0],
            &next_projection.snapshot.drives,
        )
    };
    let connection = store.open().unwrap();
    connection.execute_batch("CREATE TRIGGER fail_import_rotation BEFORE INSERT ON sync_heads BEGIN SELECT RAISE(ABORT,'injected rotation failure'); END;").unwrap();
    assert!(rotate().is_err());
    assert_eq!(
        store.v2_head(binding.vehicle_id).unwrap().unwrap().0,
        old.base.snapshot_id
    );
    assert!(
        store
            .source_fingerprint_matches(binding.vehicle_id, base_fingerprint)
            .unwrap()
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sync_retired_lineages", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    connection
        .execute_batch("DROP TRIGGER fail_import_rotation")
        .unwrap();
    rotate().unwrap();
    let new = store
        .lineage_manifest_for_vehicle(binding.vehicle_id)
        .unwrap()
        .unwrap();
    assert_eq!(new.base.snapshot_id, next_manifest.snapshot_id);
    assert!(new.head_sequence > old.head_sequence);
    assert!(new.deltas.is_empty());
    assert_eq!(identity_registry_image(&store), identities);
    assert_eq!(
        store
            .authenticate_device_at(access.access_token.as_bearer(), 2001)
            .unwrap()
            .unwrap()
            .device_id,
        access.device_id
    );
    assert!(
        store
            .retired_lineage_contains_checkpoint(
                binding.vehicle_id,
                old.head_sequence,
                old.head_digest
            )
            .unwrap()
    );
    assert!(
        store
            .pack_for_digest(base_pack.metadata.sha256)
            .unwrap()
            .is_some()
    );
    assert!(base_pack.path.is_file());
    assert!(
        store
            .pack_for_digest(delta_pack.metadata.sha256)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .retired_lineage_contains_checkpoint(
                binding.vehicle_id,
                initial.head_sequence,
                initial.head_digest
            )
            .unwrap()
    );
    let state = store
        .teslamate_import_projection_state_lookup(binding.vehicle_id, binding.account_id, 1)
        .unwrap();
    assert_eq!(state.header().base_snapshot_id, next_manifest.snapshot_id);
    drop(state);
    assert!(
        store
            .source_fingerprint_matches(binding.vehicle_id, next_fingerprint)
            .unwrap()
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM import_generations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    connection
        .execute(
            "UPDATE sync_retired_lineages SET retired_at_ms=0,expires_at_ms=1",
            [],
        )
        .unwrap();
    assert!(
        !store
            .retired_lineage_contains_checkpoint(
                binding.vehicle_id,
                old.head_sequence,
                old.head_digest
            )
            .unwrap()
    );
    assert!(
        store
            .pack_for_digest(base_pack.metadata.sha256)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .pack_for_digest(delta_pack.metadata.sha256)
            .unwrap()
            .is_none()
    );
    assert!(
        base_pack.path.is_file(),
        "authorization expiry does not unlink an in-flight object's file"
    );
}
