// SPDX-License-Identifier: AGPL-3.0-only

fn imported_typed_delta(
    store: &HubStore,
    binding: &ProjectionBinding,
    base: &LineageManifestV2,
) -> LineageDelta {
    let sequence = SequenceRange {
        from_exclusive: base.head_sequence,
        to_inclusive: base.head_sequence + 1,
    };
    let payload = ProjectionDelta {
        binding: binding.clone(),
        sequence,
        parent_digest: base.head_digest,
        cars: vec![import_delta_test_car(binding.selected_car_id)],
        car_settings: Vec::new(),
        drives: Vec::new(),
        positions: Vec::new(),
        charges: Vec::new(),
        charge_samples: Vec::new(),
        states: Vec::new(),
        updates: Vec::new(),
        tombstones: Vec::new(),
    };
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: base.base.snapshot_id,
            ordinal: store
                .next_v2_pack_ordinal(base.base.snapshot_id)
                .expect("fixture delta ordinal"),
            delta: &payload,
        })
        .expect("fixture typed delta");
    let chain_digest = canonical_delta_chain_digest(base.head_digest, pack.metadata.sha256);
    LineageDelta {
        from_sequence: sequence.from_exclusive,
        to_sequence: sequence.to_inclusive,
        parent_chain_digest: base.head_digest,
        chain_digest,
        pack_digest: pack.metadata.sha256,
        pack: pack.metadata,
    }
}

fn imported_typed_delta_after(
    store: &HubStore,
    binding: &ProjectionBinding,
    snapshot_id: Uuid,
    from_sequence: u64,
    parent_chain_digest: Sha256Digest,
    ordinal: u32,
) -> LineageDelta {
    let sequence = SequenceRange {
        from_exclusive: from_sequence,
        to_inclusive: from_sequence + 1,
    };
    let payload = ProjectionDelta {
        binding: binding.clone(),
        sequence,
        parent_digest: parent_chain_digest,
        cars: vec![import_delta_test_car(binding.selected_car_id)],
        car_settings: Vec::new(),
        drives: Vec::new(),
        positions: Vec::new(),
        charges: Vec::new(),
        charge_samples: Vec::new(),
        states: Vec::new(),
        updates: Vec::new(),
        tombstones: Vec::new(),
    };
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id,
            ordinal,
            delta: &payload,
        })
        .expect("fixture typed delta");
    let chain_digest = canonical_delta_chain_digest(parent_chain_digest, pack.metadata.sha256);
    LineageDelta {
        from_sequence: sequence.from_exclusive,
        to_sequence: sequence.to_inclusive,
        parent_chain_digest,
        chain_digest,
        pack_digest: pack.metadata.sha256,
        pack: pack.metadata,
    }
}

fn rewrite_import_delta_pack_for_test(
    store: &HubStore,
    delta: &mut LineageDelta,
    mutate: impl FnOnce(&Connection),
) {
    let original = store
        .packs_dir()
        .join("sha256")
        .join(format!("{}.sqlite.zst", delta.pack.sha256));
    let inspection = store
        .packs_dir()
        .join(format!(".import-delta-test-{}.sqlite", Uuid::new_v4()));
    fs::write(
        &inspection,
        zstd::stream::decode_all(File::open(original).expect("open typed delta"))
            .expect("decode typed delta"),
    )
    .expect("write typed delta inspection");
    let connection = Connection::open(&inspection).expect("open typed delta inspection");
    mutate(&connection);
    drop(connection);
    let raw = fs::read(&inspection).expect("read rewritten typed delta");
    fs::remove_file(&inspection).expect("remove typed delta inspection");
    let compressed = zstd::stream::encode_all(raw.as_slice(), 0).expect("recompress typed delta");
    let sha256 = Sha256Digest::of_bytes(&compressed);
    fs::write(
        store
            .packs_dir()
            .join("sha256")
            .join(format!("{}.sqlite.zst", sha256)),
        &compressed,
    )
    .expect("write rewritten typed delta");
    delta.pack.sha256 = sha256;
    delta.pack.relative_path = TransportPack::canonical_relative_path(sha256);
    delta.pack.compressed_bytes = u64::try_from(compressed.len()).expect("compressed bytes");
    delta.pack.uncompressed_bytes = u64::try_from(raw.len()).expect("uncompressed bytes");
    delta.pack_digest = sha256;
    delta.chain_digest = canonical_delta_chain_digest(delta.parent_chain_digest, sha256);
}

fn assert_import_delta_catalogue_unchanged(
    store: &HubStore,
    vehicle_id: Uuid,
    before: &LineageManifestV2,
) {
    assert_eq!(
        store
            .lineage_manifest_for_vehicle(vehicle_id)
            .expect("unchanged lineage lookup"),
        Some(before.clone()),
        "rejected import delta must not alter the public lineage"
    );
    let connection = store.open().expect("catalogue");
    for (table, expected) in [
        ("sync_bases", 1),
        ("sync_deltas", 0),
        ("sync_packs", 1),
        // The binding-aware base finalizer atomically records its source
        // fingerprint. A rejected successor must leave that one base
        // fingerprint untouched rather than adding a successor row.
        ("snapshot_fingerprints", 1),
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("catalogue count");
        assert_eq!(
            count, expected,
            "rejected import delta must not change {table}"
        );
    }
}

fn claimed_collector_delta(
    store: &HubStore,
    vehicle_id: Uuid,
    binding: &ProjectionBinding,
) -> (SyncMutationClaim, LineageDelta) {
    let (base_snapshot_id, head_sequence, parent_digest) = store
        .v2_head(vehicle_id)
        .expect("fixture head lookup")
        .expect("fixture V2 head");
    let from_sequence = u64::try_from(head_sequence).expect("non-negative fixture sequence");
    store
        .persist_materialised_car_if_absent(
            vehicle_id,
            &import_delta_test_car(binding.selected_car_id),
        )
        .expect("record a collector-shaped car mutation");
    let claim = store
        .claim_sync_mutations(vehicle_id, 2_000, 100)
        .expect("claim collector mutation")
        .expect("one collector mutation pending");
    let to_sequence = from_sequence
        .checked_add(u64::try_from(claim.mutations.len()).expect("claim length"))
        .expect("fixture sequence range");
    let payload = store
        .projection_delta_for_mutations(
            &claim,
            binding.clone(),
            SequenceRange {
                from_exclusive: from_sequence,
                to_inclusive: to_sequence,
            },
            parent_digest,
        )
        .expect("project claimed mutation");
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: base_snapshot_id,
            ordinal: store
                .next_v2_pack_ordinal(base_snapshot_id)
                .expect("fixture delta ordinal"),
            delta: &payload,
        })
        .expect("write collector-shaped delta");
    let chain_digest = canonical_delta_chain_digest(parent_digest, pack.metadata.sha256);
    let delta = LineageDelta {
        from_sequence,
        to_sequence,
        parent_chain_digest: parent_digest,
        chain_digest,
        pack_digest: pack.metadata.sha256,
        pack: pack.metadata,
    };
    (claim, delta)
}

#[test]
fn live_delta_publication_does_not_rehash_every_historical_pack() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);
    let base_pack = &base.base.packs[0];
    let base_path = store
        .packs_dir()
        .join("sha256")
        .join(format!("{}.sqlite.zst", base_pack.sha256));
    let original = fs::read(&base_path).expect("base pack bytes");
    let mut same_size_corruption = original.clone();
    same_size_corruption[0] ^= 0xff;
    fs::write(&base_path, &same_size_corruption).expect("same-size corrupt base");

    assert_eq!(
        store
            .v2_lineage_pack_count(vehicle.vehicle_id)
            .expect("metadata-only capacity count"),
        1
    );
    let (claim, delta) = claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
    store
        .commit_v2_delta_claim(
            &claim,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
        )
        .expect("append uses durable lineage metadata without rehashing history");

    assert!(matches!(
        store.lineage_manifest_for_vehicle(vehicle.vehicle_id),
        Err(StoreError::LineagePackDigestMismatch)
    ));
    fs::write(&base_path, original).expect("restore immutable base");
    let published = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("full serving verification after restore")
        .expect("published lineage");
    assert_eq!(published.deltas, vec![delta]);
}

fn sync_claim_publication_state(
    store: &HubStore,
    claim: &SyncMutationClaim,
) -> Vec<(i64, i64, i64)> {
    let connection = store.open().expect("claim state database");
    connection
        .prepare(
            "SELECT revision, published, claimed_until_ms
                 FROM sync_mutations
                 WHERE vehicle_id = ?1 AND revision BETWEEN ?2 AND ?3
                 ORDER BY revision",
        )
        .expect("claim state query")
        .query_map(
            params![
                claim.vehicle_id.to_string(),
                claim.from_revision,
                claim.to_revision
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("claim state rows")
        .map(|row| row.expect("claim state row"))
        .collect()
}

#[test]
fn v2_delta_claim_rejects_invalid_inputs_without_publishing_then_is_idempotent() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, _) = imported_v2_base(&store);
    let (claim, delta) = claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
    let head_before = store
        .v2_head(vehicle.vehicle_id)
        .expect("head before rejection");
    let claim_before = sync_claim_publication_state(&store, &claim);
    assert!(
        claim_before
            .iter()
            .all(|(_, published, claimed_until_ms)| *published == 0 && *claimed_until_ms > 2_000),
        "fixture must start with only leased, unpublished mutations"
    );
    let assert_rejected = || {
        assert_eq!(
            store
                .v2_head(vehicle.vehicle_id)
                .expect("head after rejection"),
            head_before,
            "rejected input must not advance the V2 head"
        );
        assert_eq!(
            sync_claim_publication_state(&store, &claim),
            claim_before,
            "rejected input must not publish or release the claimed mutations"
        );
    };
    let valid_cursor = import_delta_test_cursor(&binding, delta.to_sequence);

    let invalid_signature = OpaqueCursor::issue(
        &CursorKey::from_bytes([62; 32]),
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: delta.to_sequence,
        },
    )
    .expect("well-formed cursor with a wrong HMAC");
    assert!(matches!(
        store.commit_v2_delta_claim(
            &claim,
            &delta,
            &import_delta_test_cursor_key(),
            &invalid_signature,
        ),
        Err(StoreError::Manifest(ProtocolError::InvalidCursorSignature))
    ));
    assert_rejected();

    let wrong_claims = OpaqueCursor::issue(
        &import_delta_test_cursor_key(),
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: Uuid::new_v4(),
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: delta.to_sequence,
        },
    )
    .expect("validly signed cursor with wrong claims");
    assert!(matches!(
        store.commit_v2_delta_claim(
            &claim,
            &delta,
            &import_delta_test_cursor_key(),
            &wrong_claims,
        ),
        Err(StoreError::LineageCatalogConflict)
    ));
    assert_rejected();

    let mut noncanonical_chain = delta.clone();
    noncanonical_chain.chain_digest = Sha256Digest::of_bytes(b"noncanonical collector claim");
    assert!(matches!(
        store.commit_v2_delta_claim(
            &claim,
            &noncanonical_chain,
            &import_delta_test_cursor_key(),
            &valid_cursor,
        ),
        Err(StoreError::LineageCatalogConflict)
    ));
    assert_rejected();

    let mut malformed_pack = delta.clone();
    rewrite_import_delta_pack_for_test(&store, &mut malformed_pack, |connection| {
        connection
            .execute_batch(
                "CREATE TRIGGER unexpected_after_car_insert
                     AFTER INSERT ON cars BEGIN SELECT 1; END;",
            )
            .expect("make the typed delta schema malformed");
    });
    assert!(matches!(
        store.commit_v2_delta_claim(
            &claim,
            &malformed_pack,
            &import_delta_test_cursor_key(),
            &valid_cursor,
        ),
        Err(StoreError::LineageCatalogConflict)
    ));
    assert_rejected();

    let mut wrong_binding_pack = delta.clone();
    rewrite_import_delta_pack_for_test(&store, &mut wrong_binding_pack, |connection| {
        connection
            .execute(
                "UPDATE hub_pack_metadata SET value = ?1 WHERE key = 'account_id'",
                params![Uuid::new_v4().to_string()],
            )
            .expect("retarget typed delta metadata");
    });
    assert!(matches!(
        store.commit_v2_delta_claim(
            &claim,
            &wrong_binding_pack,
            &import_delta_test_cursor_key(),
            &valid_cursor,
        ),
        Err(StoreError::LineageCatalogConflict)
    ));
    assert_rejected();

    store
        .commit_v2_delta_claim(
            &claim,
            &delta,
            &import_delta_test_cursor_key(),
            &valid_cursor,
        )
        .expect("valid collector-shaped delta");
    let head_after = store
        .v2_head(vehicle.vehicle_id)
        .expect("head after success");
    assert_ne!(head_after, head_before, "valid delta advances the V2 head");
    assert_eq!(
        sync_claim_publication_state(&store, &claim),
        claim_before
            .iter()
            .map(|(revision, _, _)| (*revision, 1, 0))
            .collect::<Vec<_>>(),
        "valid delta marks every claimed mutation published"
    );

    store
        .commit_v2_delta_claim(
            &claim,
            &delta,
            &import_delta_test_cursor_key(),
            &valid_cursor,
        )
        .expect("idempotent collector delta replay");
    assert_eq!(
        store
            .v2_head(vehicle.vehicle_id)
            .expect("head after replay"),
        head_after,
        "idempotent replay leaves the V2 head unchanged"
    );
    assert_eq!(
        sync_claim_publication_state(&store, &claim),
        claim_before
            .iter()
            .map(|(revision, _, _)| (*revision, 1, 0))
            .collect::<Vec<_>>(),
        "idempotent replay keeps every claimed mutation published"
    );
}

#[test]
fn live_delta_compaction_preserves_the_base_and_rebuilds_from_durable_journal_payloads() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);

    let (car_claim, car_delta) = claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
    store
        .commit_v2_delta_claim(
            &car_claim,
            &car_delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, car_delta.to_sequence),
        )
        .expect("publish first live delta");
    let disabled = ProjectionCarSettings {
        enabled: false,
        ..ProjectionCarSettings::default()
    };
    store
        .upsert_car_settings(vehicle.vehicle_id, binding.selected_car_id, &disabled)
        .expect("record second live mutation");
    let (settings_claim, settings_delta) =
        claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
    store
        .commit_v2_delta_claim(
            &settings_claim,
            &settings_delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, settings_delta.to_sequence),
        )
        .expect("publish second live delta");

    let before = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("lineage before compaction")
        .expect("published lineage");
    assert_eq!(before.base, base.base, "immutable base must not drift");
    assert_eq!(
        before.deltas,
        vec![car_delta.clone(), settings_delta.clone()]
    );
    let old_paths = before
        .deltas
        .iter()
        .map(|delta| {
            store
                .packs_dir()
                .join("sha256")
                .join(format!("{}.sqlite.zst", delta.pack_digest))
        })
        .collect::<Vec<_>>();

    let plan = store
        .plan_live_delta_compaction(vehicle.vehicle_id)
        .expect("compaction plan")
        .expect("two live deltas form a compactable suffix");
    assert_eq!(plan.replaced_spans.len(), 2);
    let payload = store
        .projection_delta_for_compaction(&plan, binding.clone())
        .expect("journal-derived compacted payload");
    assert_eq!(payload.cars.len(), 1);
    assert!(payload.car_settings.is_empty());
    assert!(!payload.cars[0].settings.enabled);
    let built = ProjectionPackWriter::new(store.packs_dir())
        .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: plan.base_snapshot_id,
            ordinal: plan.first_ordinal,
            delta: &payload,
        })
        .expect("write compacted pack");
    let compacted = LineageDelta {
        from_sequence: plan.anchor_sequence,
        to_sequence: plan.head_sequence,
        parent_chain_digest: plan.anchor_digest,
        chain_digest: canonical_delta_chain_digest(plan.anchor_digest, built.metadata.sha256),
        pack_digest: built.metadata.sha256,
        pack: built.metadata,
    };

    // A newer mutable value can arrive while the immutable candidate is
    // being written. The compacted payload remains bound to its journal
    // window; the later revision stays unpublished for the next delta.
    let enabled = ProjectionCarSettings {
        enabled: true,
        ..ProjectionCarSettings::default()
    };
    store
        .upsert_car_settings(vehicle.vehicle_id, binding.selected_car_id, &enabled)
        .expect("record mutation after compaction plan");
    let retired_at_ms = retired_lineage_clock_ms().expect("retention clock");
    store
        .commit_live_delta_compaction_at(
            &plan,
            &compacted,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, compacted.to_sequence),
            retired_at_ms,
        )
        .expect("atomically swap compacted suffix");

    let after = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("lineage after compaction")
        .expect("published lineage");
    after.validate().expect("compacted lineage validates");
    assert_eq!(after.base, before.base);
    assert_eq!(after.head_sequence, before.head_sequence);
    assert_ne!(after.head_digest, before.head_digest);
    assert_eq!(after.deltas, vec![compacted.clone()]);
    let retention_connection = store.open().expect("retention catalogue");
    let (retired_manifest_json, expires_at_ms): (Vec<u8>, i64) = retention_connection
        .query_row(
            "SELECT manifest_json, expires_at_ms
                 FROM sync_retired_lineages
                 WHERE vehicle_id = ?1 AND head_digest = ?2",
            params![
                vehicle.vehicle_id.to_string(),
                before.head_digest.to_string()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("retired prior lineage");
    assert_eq!(
        serde_json::from_slice::<LineageManifestV2>(&retired_manifest_json)
            .expect("retired manifest JSON"),
        before,
        "retention authorization must be bound to the exact prior lineage"
    );
    assert_eq!(
        expires_at_ms,
        retired_at_ms + RETIRED_LINEAGE_PACK_RETENTION_MS
    );
    for delta in &before.deltas {
        assert!(
            store
                .retired_pack_for_digest_at(
                    &retention_connection,
                    delta.pack_digest,
                    retired_at_ms + 1,
                )
                .expect("retired pack authorization")
                .is_some(),
            "each replaced pack remains authorized through its prior lineage"
        );
    }
    drop(retention_connection);
    assert!(
        old_paths.iter().all(|path| path.is_file()),
        "old immutable objects remain available through bounded retention"
    );
    for delta in &before.deltas {
        assert!(
            store
                .pack_sha256_is_retained(&delta.pack_digest.to_string())
                .expect("cleanup retention lookup"),
            "candidate cleanup includes every unexpired retired-lineage pack"
        );
    }
    assert!(
        store
            .plan_live_delta_compaction(vehicle.vehicle_id)
            .expect("post-compaction plan")
            .is_none(),
        "one compacted live span cannot gain another pack"
    );

    let (newer_claim, newer_delta) = claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
    assert_eq!(newer_claim.mutations.len(), 1);
    store
        .commit_v2_delta_claim(
            &newer_claim,
            &newer_delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, newer_delta.to_sequence),
        )
        .expect("publish mutation that arrived during compaction");
    drop(store);

    let reopened = HubStore::initialize(temporary.path()).expect("restart compacted store");
    let restarted = reopened
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("lineage after restart")
        .expect("published lineage after restart");
    restarted.validate().expect("restart lineage validates");
    assert_eq!(restarted.base, base.base);
    assert_eq!(
        restarted.deltas,
        vec![compacted.clone(), newer_delta.clone()]
    );
    for delta in &before.deltas {
        assert!(
            reopened
                .pack_for_digest(delta.pack_digest)
                .expect("retired pack lookup after restart")
                .is_some(),
            "restart must retain prior-lineage authorization"
        );
    }

    let backup_root = temporary.path().join("retained-lineage-backup");
    reopened
        .backup_to(&backup_root)
        .expect("backup includes unexpired retired packs");
    let restored = HubStore::initialize(&backup_root).expect("restore retained backup");
    for delta in &before.deltas {
        assert!(
            restored
                .pack_for_digest(delta.pack_digest)
                .expect("restored retired pack lookup")
                .is_some(),
            "restore must preserve unexpired prior-lineage downloads"
        );
    }
    drop(restored);

    let retention_connection = reopened.open().expect("retention after restart");
    for delta in &before.deltas {
        assert!(
                reopened
                    .retired_pack_for_digest_at(
                        &retention_connection,
                        delta.pack_digest,
                        expires_at_ms,
                    )
                    .expect("retired pack at exact expiry")
                    .is_none(),
                "authorization expires at the declared boundary"
            );
    }
    drop(retention_connection);
    reopened
        .repair_at(expires_at_ms + 1)
        .expect("repair inside physical-delete grace");
    assert!(
        old_paths.iter().all(|path| path.is_file()),
        "physical grace protects a just-authorized in-flight open"
    );
    reopened
        .repair_at(expires_at_ms + RETIRED_LINEAGE_PACK_DELETE_GRACE_MS)
        .expect("repair after retired-pack grace");
    assert!(
        old_paths.iter().all(|path| !path.exists()),
        "expired retired objects are eventually removed"
    );
    assert_eq!(
        reopened
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("current lineage after retired cleanup")
            .expect("current lineage remains published"),
        restarted
    );
}

#[test]
fn live_delta_compaction_coalesces_cross_table_settings_and_tombstones_by_revision() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, _) = imported_v2_base(&store);
    let plan = |mutations| LiveDeltaCompactionPlan {
        vehicle_id: vehicle.vehicle_id,
        base_snapshot_id: Uuid::new_v4(),
        anchor_sequence: 1,
        anchor_digest: Sha256Digest::of_bytes(b"coalescing anchor"),
        head_sequence: 3,
        head_digest: Sha256Digest::of_bytes(b"coalescing old head"),
        first_ordinal: 1,
        from_revision: 1,
        to_revision: 2,
        mutations,
        replaced_spans: Vec::new(),
    };
    let mutation =
        |revision: i64, entity: &str, entity_id: i64, operation: &str, payload_json: String| {
            SyncMutation {
                vehicle_id: vehicle.vehicle_id,
                revision,
                entity: entity.into(),
                entity_id,
                car_id: binding.selected_car_id,
                operation: operation.into(),
                payload_json,
            }
        };

    let disabled = ProjectionCarSettings {
        enabled: false,
        ..ProjectionCarSettings::default()
    };
    let car = import_delta_test_car(binding.selected_car_id);
    let older_setting_newer_car = plan(vec![
        mutation(
            2,
            "car",
            binding.selected_car_id,
            "upsert",
            serde_json::to_string(&car).expect("serialize newer car"),
        ),
        mutation(
            1,
            "car_setting",
            binding.selected_car_id,
            "upsert",
            serde_json::to_string(&disabled).expect("serialize older settings"),
        ),
    ]);
    let payload = store
        .projection_delta_for_compaction(&older_setting_newer_car, binding.clone())
        .expect("newer full car wins over older settings patch");
    assert_eq!(payload.cars, vec![car.clone()]);
    assert!(payload.car_settings.is_empty());

    let older_car_newer_setting = plan(vec![
        mutation(
            2,
            "car_setting",
            binding.selected_car_id,
            "upsert",
            serde_json::to_string(&disabled).expect("serialize newer settings"),
        ),
        mutation(
            1,
            "car",
            binding.selected_car_id,
            "upsert",
            serde_json::to_string(&car).expect("serialize older car"),
        ),
    ]);
    let payload = store
        .projection_delta_for_compaction(&older_car_newer_setting, binding.clone())
        .expect("newer settings patch is folded into older full car");
    assert_eq!(payload.cars.len(), 1);
    assert!(!payload.cars[0].settings.enabled);
    assert!(payload.car_settings.is_empty());

    let state = crate::hub_pack::ProjectionState {
        id: 77,
        car_id: binding.selected_car_id,
        state: "online".into(),
        start_date_ms: 1_000,
        end_date_ms: Some(2_000),
    };
    let newer_tombstone = plan(vec![
        mutation(2, "state", state.id, "tombstone", "{}".into()),
        mutation(
            1,
            "state",
            state.id,
            "upsert",
            serde_json::to_string(&state).expect("serialize older state"),
        ),
    ]);
    let payload = store
        .projection_delta_for_compaction(&newer_tombstone, binding.clone())
        .expect("newer tombstone wins");
    assert!(payload.states.is_empty());
    assert_eq!(
        payload.tombstones,
        vec![ProjectionTombstone {
            entity: ProjectionDeltaEntity::State,
            id: state.id,
            car_id: binding.selected_car_id,
        }]
    );

    let newer_upsert = plan(vec![
        mutation(
            2,
            "state",
            state.id,
            "upsert",
            serde_json::to_string(&state).expect("serialize newer state"),
        ),
        mutation(1, "state", state.id, "tombstone", "{}".into()),
    ]);
    let payload = store
        .projection_delta_for_compaction(&newer_upsert, binding)
        .expect("newer upsert wins");
    assert_eq!(payload.states, vec![state]);
    assert!(payload.tombstones.is_empty());
}

#[test]
fn live_delta_admission_refuses_an_unservable_next_pack_when_no_compaction_can_gain_space() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, _) = imported_v2_base(&store);
    let two_pack_limit = ProtocolLimits {
        max_chunks: 2,
        ..ProtocolLimits::default()
    };

    let (first_claim, first_delta) = claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
    store
        .commit_v2_delta_claim_with_limits(
            &first_claim,
            &first_delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, first_delta.to_sequence),
            two_pack_limit,
        )
        .expect("fill the reduced release bound exactly");
    let prior = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("prior lineage")
        .expect("published lineage");
    prior
        .validate_with_limits(two_pack_limit)
        .expect("prior manifest remains servable at the bound");
    assert!(
        store
            .plan_live_delta_compaction(vehicle.vehicle_id)
            .expect("compaction availability")
            .is_none(),
        "one live delta cannot be replaced by fewer packs"
    );

    store
        .upsert_car_settings(
            vehicle.vehicle_id,
            binding.selected_car_id,
            &ProjectionCarSettings {
                enabled: false,
                ..ProjectionCarSettings::default()
            },
        )
        .expect("new mutation at capacity");
    let (second_claim, second_delta) =
        claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
    let claim_before = sync_claim_publication_state(&store, &second_claim);
    assert!(matches!(
        store.commit_v2_delta_claim_with_limits(
            &second_claim,
            &second_delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, second_delta.to_sequence),
            two_pack_limit,
        ),
        Err(StoreError::LineageCapacityExhausted)
    ));
    assert_eq!(
        store
            .lineage_manifest_for_vehicle(vehicle.vehicle_id)
            .expect("lineage after refused admission")
            .expect("prior lineage still published"),
        prior
    );
    assert_eq!(
        sync_claim_publication_state(&store, &second_claim),
        claim_before
    );
}

#[test]
fn settings_only_sync_delta_is_emitted_and_retained() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, _) = imported_v2_base(&store);

    let (car_claim, car_delta) = claimed_collector_delta(&store, vehicle.vehicle_id, &binding);
    store
        .commit_v2_delta_claim(
            &car_claim,
            &car_delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, car_delta.to_sequence),
        )
        .expect("publish the materialised-car precursor");

    let settings = ProjectionCarSettings {
        enabled: false,
        ..ProjectionCarSettings::default()
    };
    store
        .upsert_car_settings(vehicle.vehicle_id, binding.selected_car_id, &settings)
        .expect("record standalone settings mutation");
    let claim = store
        .claim_sync_mutations(vehicle.vehicle_id, 3_000, 100)
        .expect("claim settings mutation")
        .expect("settings mutation pending");
    assert_eq!(claim.mutations.len(), 1);
    assert_eq!(claim.mutations[0].entity, "car_setting");

    let (base_snapshot_id, head_sequence, parent_digest) = store
        .v2_head(vehicle.vehicle_id)
        .expect("V2 head")
        .expect("published V2 base");
    let from_sequence = u64::try_from(head_sequence).expect("non-negative sequence");
    let to_sequence = from_sequence
        .checked_add(u64::try_from(claim.mutations.len()).expect("claim length"))
        .expect("sequence range");
    let payload = store
        .projection_delta_for_mutations(
            &claim,
            binding.clone(),
            SequenceRange {
                from_exclusive: from_sequence,
                to_inclusive: to_sequence,
            },
            parent_digest,
        )
        .expect("project standalone settings mutation");
    assert!(payload.cars.is_empty());
    assert_eq!(
        payload.car_settings,
        vec![ProjectionCarSettingsPatch {
            car_id: binding.selected_car_id,
            settings: settings.clone(),
        }]
    );
    assert!(
        !payload.is_empty(),
        "a settings-only patch must not take the typed-delta no-op path"
    );

    let built = ProjectionPackWriter::new(store.packs_dir())
        .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: base_snapshot_id,
            ordinal: store
                .next_v2_pack_ordinal(base_snapshot_id)
                .expect("settings delta ordinal"),
            delta: &payload,
        })
        .expect("write settings-only typed delta");
    assert_eq!(built.metadata.row_count, 1);
    assert_eq!(built.metadata.tables, vec![MirrorTable::Car]);
    let inspection_path = temporary.path().join("settings-only-delta.sqlite");
    fs::write(
        &inspection_path,
        zstd::stream::decode_all(File::open(&built.path).expect("open settings delta"))
            .expect("decode settings delta"),
    )
    .expect("write settings delta inspection");
    let inspection = Connection::open(inspection_path).expect("open settings delta inspection");
    let cars: i64 = inspection
        .query_row("SELECT COUNT(*) FROM cars", [], |row| row.get(0))
        .expect("count delta cars");
    let car_settings: i64 = inspection
        .query_row("SELECT COUNT(*) FROM car_settings", [], |row| row.get(0))
        .expect("count delta settings patches");
    assert_eq!((cars, car_settings), (0, 1));

    let delta = LineageDelta {
        from_sequence,
        to_sequence,
        parent_chain_digest: parent_digest,
        chain_digest: canonical_delta_chain_digest(parent_digest, built.metadata.sha256),
        pack_digest: built.metadata.sha256,
        pack: built.metadata,
    };
    store
        .commit_v2_delta_claim(
            &claim,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
        )
        .expect("retain settings-only typed delta");
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("published lineage")
        .expect("lineage exists");
    lineage.validate().expect("settings-only lineage validates");
    assert_eq!(lineage.deltas, vec![car_delta, delta]);
}

#[test]
fn import_delta_finalizer_rejects_full_base_relabel_without_catalogue_mutation() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);
    let mut forged_pack = base.base.packs[0].clone();
    forged_pack.sequence = SequenceRange {
        from_exclusive: base.head_sequence,
        to_inclusive: base.head_sequence + 1,
    };
    let forged = LineageDelta {
        from_sequence: forged_pack.sequence.from_exclusive,
        to_sequence: forged_pack.sequence.to_inclusive,
        parent_chain_digest: base.head_digest,
        chain_digest: canonical_delta_chain_digest(base.head_digest, forged_pack.sha256),
        pack_digest: forged_pack.sha256,
        pack: forged_pack,
    };

    let error = store
        .finalize_import_delta_successor(
            vehicle.vehicle_id,
            &forged,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, forged.to_sequence),
            Sha256Digest::of_bytes(b"forged-base-relabel"),
            &[],
        )
        .expect_err("a full base cannot be relabelled as a typed delta");
    assert!(matches!(error, StoreError::LineageCatalogConflict));
    assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
}

#[test]
fn import_delta_finalizer_rejects_wrong_chain_then_accepts_the_written_typed_delta() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);
    let delta = imported_typed_delta(&store, &binding, &base);
    let mut wrong_chain = delta.clone();
    wrong_chain.chain_digest = Sha256Digest::of_bytes(b"wrong import delta chain");

    let error = store
        .finalize_import_delta_successor(
            vehicle.vehicle_id,
            &wrong_chain,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, wrong_chain.to_sequence),
            Sha256Digest::of_bytes(b"wrong-chain"),
            &[],
        )
        .expect_err("a caller-supplied noncanonical chain must be rejected");
    assert!(matches!(error, StoreError::LineageCatalogConflict));
    assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);

    store
        .finalize_import_delta_successor(
            vehicle.vehicle_id,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
            Sha256Digest::of_bytes(b"valid-typed-delta"),
            &[],
        )
        .expect("the writer-produced typed delta is accepted");
    let lineage = store
        .lineage_manifest_for_vehicle(vehicle.vehicle_id)
        .expect("published lineage")
        .expect("lineage exists");
    lineage.validate().expect("published lineage validates");
    assert_eq!(lineage.deltas, vec![delta]);
}

#[test]
fn import_delta_writer_rejects_geofence_and_address_tombstones() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);
    let sequence = SequenceRange {
        from_exclusive: base.head_sequence,
        to_inclusive: base.head_sequence + 1,
    };
    for entity in [
        ProjectionDeltaEntity::Geofence,
        ProjectionDeltaEntity::Address,
    ] {
        let payload = ProjectionDelta {
            binding: binding.clone(),
            sequence,
            parent_digest: base.head_digest,
            cars: Vec::new(),
            car_settings: Vec::new(),
            drives: Vec::new(),
            positions: Vec::new(),
            charges: Vec::new(),
            charge_samples: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
            tombstones: vec![ProjectionTombstone {
                entity,
                id: 90,
                car_id: binding.selected_car_id,
            }],
        };
        let error = ProjectionPackWriter::new(store.packs_dir())
            .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
                pack_id: Uuid::new_v4(),
                snapshot_id: base.base.snapshot_id,
                ordinal: store
                    .next_v2_pack_ordinal(base.base.snapshot_id)
                    .expect("fixture delta ordinal"),
                delta: &payload,
            })
            .expect_err("writer rejects unsupported source-owned tombstone entities");
        assert!(matches!(
            error,
            ProjectionPackError::Invalid(message)
                if message.contains("unsupported source-owned delta tombstone entity")
        ));
        assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
    }
}

#[test]
fn import_delta_finalizer_requires_the_exact_signed_terminal_cursor_claims() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);
    let delta = imported_typed_delta(&store, &binding, &base);

    let wrong_generation = OpaqueCursor::issue(
        &import_delta_test_cursor_key(),
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation + 1,
            sequence: delta.to_sequence,
        },
    )
    .expect("valid cursor for a different generation");
    let error = store
        .finalize_import_delta_successor(
            vehicle.vehicle_id,
            &delta,
            &import_delta_test_cursor_key(),
            &wrong_generation,
            Sha256Digest::of_bytes(b"wrong-cursor-claims"),
            &[],
        )
        .expect_err("a cursor with mismatched claims must not publish a delta");
    assert!(matches!(error, StoreError::LineageCatalogConflict));
    assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);

    let wrong_key = CursorKey::from_bytes([62; 32]);
    let invalid_signature = OpaqueCursor::issue(
        &wrong_key,
        CursorClaims {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: delta.to_sequence,
        },
    )
    .expect("well-formed cursor signed with another key");
    let error = store
        .finalize_import_delta_successor(
            vehicle.vehicle_id,
            &delta,
            &import_delta_test_cursor_key(),
            &invalid_signature,
            Sha256Digest::of_bytes(b"invalid-cursor-signature"),
            &[],
        )
        .expect_err("a cursor signed with another key must not publish a delta");
    assert!(matches!(
        error,
        StoreError::Manifest(ProtocolError::InvalidCursorSignature)
    ));
    assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
}

#[test]
fn import_delta_finalizer_accepts_a_car_and_completed_drive() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);
    let sequence = SequenceRange {
        from_exclusive: base.head_sequence,
        to_inclusive: base.head_sequence + 1,
    };
    let payload = ProjectionDelta {
        binding: binding.clone(),
        sequence,
        parent_digest: base.head_digest,
        cars: vec![import_delta_test_car(binding.selected_car_id)],
        car_settings: Vec::new(),
        drives: vec![ProjectionDrive {
            id: 99,
            car_id: binding.selected_car_id,
            optimized_at_ms: None,
            start_date_ms: 2_000,
            end_date_ms: 3_000,
            distance_km: Some(10.0),
            duration_min: Some(1),
            efficiency: None,
            outside_temp_avg: None,
            inside_temp_avg: None,
            speed_max: Some(50),
            power_max: None,
            power_min: None,
            start_ideal_range_km: None,
            end_ideal_range_km: None,
            start_address: None,
            end_address: None,
            start_geofence: None,
            end_geofence: None,
            start_latitude: None,
            start_longitude: None,
            end_latitude: None,
            end_longitude: None,
            start_soc: None,
            end_soc: None,
            start_rated_range_km: Some(300.0),
            end_rated_range_km: Some(280.0),
            ascent: None,
            descent: None,
        }],
        positions: Vec::new(),
        charges: Vec::new(),
        charge_samples: Vec::new(),
        states: Vec::new(),
        updates: Vec::new(),
        tombstones: Vec::new(),
    };
    let pack = ProjectionPackWriter::new(store.packs_dir())
        .write_delta(&crate::hub_pack::ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: base.base.snapshot_id,
            ordinal: store
                .next_v2_pack_ordinal(base.base.snapshot_id)
                .expect("fixture delta ordinal"),
            delta: &payload,
        })
        .expect("fixture typed delta");
    let delta = LineageDelta {
        from_sequence: sequence.from_exclusive,
        to_sequence: sequence.to_inclusive,
        parent_chain_digest: base.head_digest,
        chain_digest: canonical_delta_chain_digest(base.head_digest, pack.metadata.sha256),
        pack_digest: pack.metadata.sha256,
        pack: pack.metadata,
    };

    store
        .finalize_import_delta_successor(
            vehicle.vehicle_id,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
            Sha256Digest::of_bytes(b"car-and-drive"),
            &[],
        )
        .expect("writer-produced completed drive delta is accepted");
}

#[test]
fn import_delta_finalizer_rejects_a_typed_delta_for_another_selected_car() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);
    let mut delta = imported_typed_delta(&store, &binding, &base);
    let forged_car_id = binding.selected_car_id + 1;
    rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
        // Model a malicious external SQLite pack. The resulting rows are
        // internally consistent, but its selected-car identity differs
        // from the immutable catalogue binding.
        connection
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .expect("allow synthetic retargeting");
        connection
            .execute("UPDATE cars SET id = ?1", params![forged_car_id])
            .expect("retarget typed delta car");
        connection
            .execute(
                "UPDATE car_settings SET car_id = ?1",
                params![forged_car_id],
            )
            .expect("retarget typed delta settings");
        connection
            .execute(
                "UPDATE hub_pack_metadata SET value = ?1 WHERE key = 'selected_car_id'",
                params![forged_car_id.to_string()],
            )
            .expect("rebind typed delta metadata");
    });

    let error = store
        .finalize_import_delta_successor(
            vehicle.vehicle_id,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
            Sha256Digest::of_bytes(b"wrong-selected-car"),
            &[],
        )
        .expect_err("a typed delta for another selected car must be rejected");
    assert!(matches!(error, StoreError::LineageCatalogConflict));
    assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
}

#[test]
fn import_delta_finalizer_rejects_matching_metadata_with_an_extra_schema_object() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);
    let mut delta = imported_typed_delta(&store, &binding, &base);
    rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
        connection
            .execute_batch(
                "CREATE TRIGGER unexpected_after_car_insert
                     AFTER INSERT ON cars BEGIN SELECT 1; END;",
            )
            .expect("add unexpected trigger");
    });

    let error = store
        .finalize_import_delta_successor(
            vehicle.vehicle_id,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
            Sha256Digest::of_bytes(b"unexpected-schema-object"),
            &[],
        )
        .expect_err("metadata cannot bless an unexpected SQLite program");
    assert!(matches!(error, StoreError::LineageCatalogConflict));
    assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
}

#[test]
fn import_delta_finalizer_rejects_forged_unsupported_tombstone_entities() {
    for entity in ["not-an-entity", "car", "car_setting", "geofence", "address"] {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let (vehicle, binding, base) = imported_v2_base(&store);
        let mut delta = imported_typed_delta(&store, &binding, &base);
        rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
            connection
                .execute(
                    "INSERT INTO tombstones(entity, entity_id, car_id) VALUES (?1, 1, 10)",
                    params![entity],
                )
                .expect("insert unsupported tombstone");
            connection
                .execute(
                    "UPDATE hub_pack_metadata SET value = '2' WHERE key = 'row_count'",
                    [],
                )
                .expect("update declared row count");
        });
        delta.pack.row_count = 2;
        delta.pack.tables.push(MirrorTable::Tombstone);

        let error = store
            .finalize_import_delta_successor(
                vehicle.vehicle_id,
                &delta,
                &import_delta_test_cursor_key(),
                &import_delta_test_cursor(&binding, delta.to_sequence),
                Sha256Digest::of_bytes(entity.as_bytes()),
                &[],
            )
            .expect_err("typed delta semantics must be valid before publication");
        assert!(matches!(error, StoreError::LineageCatalogConflict));
        assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
    }
}

#[test]
fn import_delta_finalizer_rejects_forged_upsert_tombstone_overlap() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);
    let mut delta = imported_typed_delta(&store, &binding, &base);
    rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
        connection
            .execute_batch(
                "INSERT INTO positions(
                        id, drive_id, car_id, date_ms, latitude, longitude
                     ) VALUES (99, NULL, 10, 2_000, 51.5, -0.1);
                     INSERT INTO tombstones(entity, entity_id, car_id)
                        VALUES ('position', 99, 10);",
            )
            .expect("forge a validly shaped position/tombstone overlap");
        connection
            .execute(
                "UPDATE hub_pack_metadata SET value = '3' WHERE key = 'row_count'",
                [],
            )
            .expect("update declared row count");
    });
    delta.pack.row_count = 3;
    delta
        .pack
        .tables
        .extend([MirrorTable::Position, MirrorTable::Tombstone]);

    let error = store
        .finalize_import_delta_successor(
            vehicle.vehicle_id,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
            Sha256Digest::of_bytes(b"forged-upsert-tombstone-overlap"),
            &[],
        )
        .expect_err("a forged typed row cannot be both upserted and tombstoned");
    assert!(matches!(error, StoreError::LineageCatalogConflict));
    assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
}

#[test]
fn import_delta_finalizer_requires_the_companion_settings_for_each_car() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle, binding, base) = imported_v2_base(&store);
    let mut delta = imported_typed_delta(&store, &binding, &base);
    rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
        connection
            .execute("DELETE FROM car_settings WHERE car_id = 10", [])
            .expect("remove required car settings");
        connection
            .execute(
                "INSERT INTO tombstones(entity, entity_id, car_id) VALUES ('drive', 77, 10)",
                [],
            )
            .expect("add otherwise valid logical row");
    });
    // The forged metadata matches the old row-count arithmetic: without
    // the companion row it can still claim one tombstone-backed logical
    // row, so the car/settings invariant must reject it explicitly.
    delta.pack.row_count = 1;
    delta.pack.tables.push(MirrorTable::Tombstone);

    let error = store
        .finalize_import_delta_successor(
            vehicle.vehicle_id,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
            Sha256Digest::of_bytes(b"missing-car-settings"),
            &[],
        )
        .expect_err("a car row must carry its companion settings row");
    assert!(matches!(error, StoreError::LineageCatalogConflict));
    assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
}

#[test]
fn import_delta_finalizer_rejects_row_semantics_the_writer_would_refuse() {
    fn partial_coordinate(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO drives(
                        id, car_id, start_date_ms, end_date_ms, start_latitude, start_longitude
                     ) VALUES (99, 10, 2_000, 3_000, 51.5, NULL)",
                [],
            )
            .expect("insert partial coordinates");
    }

    fn non_finite_real(connection: &Connection) {
        connection
            .execute(
                "UPDATE cars SET efficiency_wh_per_km = 1e999 WHERE id = 10",
                [],
            )
            .expect("write an infinite REAL");
    }

    fn invalid_soc(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO positions(
                        id, drive_id, car_id, date_ms, latitude, longitude, battery_level
                     ) VALUES (100, NULL, 10, 2_000, 51.5, -0.1, 101)",
                [],
            )
            .expect("insert out-of-range SOC");
    }

    fn negative_range(connection: &Connection) {
        connection
            .execute(
                "INSERT INTO positions(
                        id, drive_id, car_id, date_ms, latitude, longitude, ideal_battery_range_km
                     ) VALUES (101, NULL, 10, 2_000, 51.5, -0.1, -1.0)",
                [],
            )
            .expect("insert negative range");
    }

    fn nul_text(connection: &Connection) {
        connection
            .execute(
                "UPDATE cars SET name = ?1 WHERE id = 10",
                params!["safe\0name"],
            )
            .expect("write NUL-containing text");
    }

    fn overlong_text(connection: &Connection) {
        connection
            .execute(
                "UPDATE cars SET model = ?1 WHERE id = 10",
                params!["x".repeat(16 * 1024 + 1)],
            )
            .expect("write overlong text");
    }

    fn multiple_open_states(connection: &Connection) {
        connection
            .execute_batch(
                "INSERT INTO states(id, car_id, state, start_date_ms, end_date_ms)
                        VALUES (200, 10, 'online', 2_000, NULL);
                     INSERT INTO states(id, car_id, state, start_date_ms, end_date_ms)
                        VALUES (201, 10, 'asleep', 3_000, NULL);",
            )
            .expect("insert incompatible open states");
    }

    let assert_rejected =
        |label: &str, added_rows: u64, table: Option<MirrorTable>, mutate: fn(&Connection)| {
            let temporary = crate::private_tempdir().expect("temporary store");
            let store = HubStore::initialize(temporary.path()).expect("store");
            let (vehicle, binding, base) = imported_v2_base(&store);
            let mut delta = imported_typed_delta(&store, &binding, &base);
            let row_count = delta
                .pack
                .row_count
                .checked_add(added_rows)
                .expect("fixture row count");
            rewrite_import_delta_pack_for_test(&store, &mut delta, |connection| {
                mutate(connection);
                connection
                    .execute(
                        "UPDATE hub_pack_metadata SET value = ?1 WHERE key = 'row_count'",
                        params![row_count.to_string()],
                    )
                    .expect("update declared row count");
            });
            delta.pack.row_count = row_count;
            if let Some(table) = table {
                delta.pack.tables.push(table);
            }

            let error = store
                .finalize_import_delta_successor(
                    vehicle.vehicle_id,
                    &delta,
                    &import_delta_test_cursor_key(),
                    &import_delta_test_cursor(&binding, delta.to_sequence),
                    Sha256Digest::of_bytes(label.as_bytes()),
                    &[],
                )
                .expect_err(label);
            assert!(
                matches!(error, StoreError::LineageCatalogConflict),
                "{label}"
            );
            assert_import_delta_catalogue_unchanged(&store, vehicle.vehicle_id, &base);
        };

    assert_rejected(
        "partial optional coordinate pair",
        1,
        Some(MirrorTable::Drive),
        partial_coordinate,
    );
    assert_rejected("non-finite numeric value", 0, None, non_finite_real);
    assert_rejected(
        "out-of-range SOC",
        1,
        Some(MirrorTable::Position),
        invalid_soc,
    );
    assert_rejected(
        "negative battery range",
        1,
        Some(MirrorTable::Position),
        negative_range,
    );
    assert_rejected("NUL-containing text", 0, None, nul_text);
    assert_rejected("overlong text", 0, None, overlong_text);
    assert_rejected(
        "more than one open state",
        2,
        Some(MirrorTable::State),
        multiple_open_states,
    );
}
