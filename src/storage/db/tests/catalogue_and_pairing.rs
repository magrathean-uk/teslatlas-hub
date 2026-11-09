// SPDX-License-Identifier: AGPL-3.0-only

#[test]
fn publishes_and_loads_a_canonical_manifest_catalog() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    let manifest = test_manifest();

    store.publish_manifest(&manifest).expect("publish manifest");
    let loaded = store
        .manifest_for_vehicle(manifest.vehicle_id)
        .expect("load manifest")
        .expect("manifest exists");
    assert_eq!(loaded, manifest);

    let pack = store
        .pack_for_digest(manifest.chunks[0].sha256)
        .expect("load pack")
        .expect("pack exists");
    assert_eq!(pack.compressed_bytes, manifest.chunks[0].compressed_bytes);
    assert!(pack.path.starts_with(store.packs_dir()));
}

#[test]
fn manifest_commit_fault_reconciles_exact_pre_or_post_state() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    let before_root = crate::private_tempdir().expect("before-commit root");
    let before = HubStore::initialize(before_root.path()).expect("before store");
    let manifest = test_manifest();
    let _before_fault = inject(DurabilityFaultPoint::CatalogueBeforeCommit);
    assert!(matches!(
        before.publish_manifest(&manifest),
        Err(StoreError::CatalogueDurability(_))
    ));
    assert!(
        before
            .manifest_for_vehicle(manifest.vehicle_id)
            .expect("prior lookup")
            .is_none(),
        "pre-commit fault retains the exact prior empty state"
    );

    let after_root = crate::private_tempdir().expect("after-commit root");
    let after = HubStore::initialize(after_root.path()).expect("after store");
    let _after_fault = inject(DurabilityFaultPoint::CatalogueAfterCommit);
    after
        .publish_manifest(&manifest)
        .expect("post-commit fault reconciles exact candidate to success");
    assert_eq!(
        after
            .manifest_for_vehicle(manifest.vehicle_id)
            .expect("candidate lookup")
            .expect("candidate visible"),
        manifest
    );
}

#[test]
fn receipted_catalogue_commit_is_exactly_prior_or_committed() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    for (point, expected_value) in [
        (DurabilityFaultPoint::CatalogueBeforeCommit, None),
        (
            DurabilityFaultPoint::CatalogueAfterCommit,
            Some("candidate"),
        ),
    ] {
        let root = crate::private_tempdir().expect("receipt root");
        let store = HubStore::initialize(root.path()).expect("store");
        let mut connection = store.open().expect("connection");
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .expect("transaction");
        transaction
            .execute(
                "INSERT INTO hub_metadata(key, value) VALUES('receipt_test_candidate', ?1)",
                ["candidate"],
            )
            .expect("candidate mutation");
        let result = {
            let _fault = inject(point);
            store.commit_catalogue_receipted_transaction(
                transaction,
                "test_candidate",
                Uuid::from_u128(71),
                Uuid::from_u128(72),
                StoreError::Query,
            )
        };
        match expected_value {
            None => assert!(matches!(result, Err(StoreError::CatalogueDurability(_)))),
            Some(_) => result.expect("post-commit receipt proves candidate"),
        }
        let stored = store
            .open()
            .expect("reopen")
            .query_row(
                "SELECT value FROM hub_metadata WHERE key = 'receipt_test_candidate'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .expect("candidate lookup");
        assert_eq!(stored.as_deref(), expected_value);
    }
}

#[test]
fn schema_22_manifest_is_catalogued_as_full_snapshot() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    let manifest = schema_22_test_manifest();
    let digest = manifest.chunks[0].sha256;
    assert!(matches!(
        store.publish_manifest(&manifest),
        Err(StoreError::Schema22PairPublicationRequired(vehicle_id))
            if vehicle_id == manifest.vehicle_id
    ));
    let noop = crate::updates_delivery::SignedNoOpState {
        schema: "teslatlas-hub-schema-22-noop-v1".into(),
        projection_schema: "2.2".into(),
        installation_id: manifest.installation_id,
        account_id: manifest.account_id,
        vehicle_id: manifest.vehicle_id,
        generation: manifest.generation,
        snapshot_id: manifest.snapshot_id,
        head_sequence: manifest.head_sequence,
        pack_sha256: digest.to_string(),
        terminal_cursor: manifest.terminal_cursor.clone(),
        source_witness: None,
    };
    let gate = store
        .try_acquire_publication_gate()
        .expect("schema 2.2 publication gate");
    store
        .publish_schema_22_noop(&gate, &noop)
        .expect("schema 2.2 no-op is published first");
    let mut mismatched = manifest.clone();
    mismatched.generation += 1;
    assert!(matches!(
        store.publish_schema_22_manifest(&gate, &mismatched),
        Err(StoreError::InvalidSchema22Pair(_))
    ));
    assert!(
        store
            .manifest_for_vehicle(manifest.vehicle_id)
            .expect("rejected pair lookup")
            .is_none()
    );
    store
        .publish_schema_22_manifest(&gate, &manifest)
        .expect("schema 2.2 full snapshot is catalogued");
    let loaded = store
        .manifest_for_vehicle(manifest.vehicle_id)
        .expect("catalogue lookup")
        .expect("schema 2.2 manifest");
    assert_eq!(loaded.schema, HUB_PROJECTION_SCHEMA_V3);
    assert_eq!(loaded.snapshot_id, manifest.snapshot_id);
    assert_eq!(loaded.chunks[0].sha256, digest);
}

#[test]
fn schema_22_noop_fault_matrix_is_old_or_exact_candidate() {
    use crate::durability_fault::{DurabilityFaultPoint, inject};

    for (point, expect_candidate) in [
        (DurabilityFaultPoint::Schema22NoOpWrite, false),
        (DurabilityFaultPoint::Schema22NoOpFsync, false),
        (DurabilityFaultPoint::Schema22NoOpRename, true),
        (DurabilityFaultPoint::Schema22NoOpDirectoryFsync, true),
    ] {
        let temp = crate::private_tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store initializes");
        let manifest = schema_22_test_manifest();
        let noop = crate::updates_delivery::SignedNoOpState {
            schema: "teslatlas-hub-schema-22-noop-v1".into(),
            projection_schema: "2.2".into(),
            installation_id: manifest.installation_id,
            account_id: manifest.account_id,
            vehicle_id: manifest.vehicle_id,
            generation: manifest.generation,
            snapshot_id: manifest.snapshot_id,
            head_sequence: manifest.head_sequence,
            pack_sha256: manifest.chunks[0].sha256.to_string(),
            terminal_cursor: manifest.terminal_cursor.clone(),
            source_witness: None,
        };
        let gate = store.try_acquire_publication_gate().expect("gate");
        let _fault = inject(point);
        let error = store
            .publish_schema_22_noop(&gate, &noop)
            .expect_err("armed no-op point must fail");
        assert!(error.to_string().contains("durability fault"));
        let stored = store
            .schema_22_noop_for_snapshot(noop.vehicle_id, noop.snapshot_id)
            .expect("inspect no-op after fault");
        assert_eq!(stored.is_some(), expect_candidate, "outcome for {point:?}");
        if let Some(bytes) = stored {
            assert_eq!(
                bytes,
                serde_json::to_vec(&noop).expect("canonical candidate")
            );
        }
        if expect_candidate {
            let _retry_fault = inject(DurabilityFaultPoint::Schema22NoOpDirectoryFsync);
            let retry_error = store
                .publish_schema_22_noop(&gate, &noop)
                .expect_err("same-payload retry must repeat directory sync");
            assert!(retry_error.to_string().contains("durability fault"));
        }
        assert!(
            store
                .manifest_for_vehicle(noop.vehicle_id)
                .expect("manifest lookup")
                .is_none(),
            "a no-op fault must never make a half pair current"
        );
        store
            .publish_schema_22_noop(&gate, &noop)
            .expect("exact retry converges");
    }
}

#[test]
fn source_and_vehicle_ids_are_stable_across_re_registration() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    let descriptor = SourceDescriptor::new("tesla_owner_api", "account-opaque-id");
    let source = store
        .register_source(&descriptor, 1_000)
        .expect("source registers");
    let same_source = store
        .register_source(&descriptor, 2_000)
        .expect("source re-registers");
    assert_eq!(source, same_source);
    assert_eq!(source.created_at_ms, 1_000);

    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor {
                source_id: source.source_id,
                source_vehicle_key: "vehicle-fleet-id".into(),
                vin: Some("5YJTESTVIN1234567".into()),
                display_name: Some("Road car".into()),
                tesla_eid: None,
                tesla_vid: None,
            },
            3_000,
        )
        .expect("vehicle registers");
    let same_vehicle = store
        .register_vehicle(
            &VehicleDescriptor {
                source_id: source.source_id,
                source_vehicle_key: "vehicle-fleet-id".into(),
                vin: None,
                display_name: Some("Renamed road car".into()),
                tesla_eid: None,
                tesla_vid: None,
            },
            4_000,
        )
        .expect("vehicle re-registers");
    assert_eq!(same_vehicle.vehicle_id, vehicle.vehicle_id);
    assert_eq!(same_vehicle.created_at_ms, 3_000);
    assert_eq!(same_vehicle.last_seen_at_ms, 4_000);
    assert_eq!(same_vehicle.vin.as_deref(), Some("5YJTESTVIN1234567"));
    assert_eq!(
        same_vehicle.display_name.as_deref(),
        Some("Renamed road car")
    );
}

#[test]
fn vehicle_retirement_is_scoped_idempotent_and_preserves_durable_state() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store initializes");
    let source = store
        .register_source(&SourceDescriptor::new("owner", "account-a"), 1_000)
        .expect("source A");
    let other_source = store
        .register_source(&SourceDescriptor::new("owner", "account-b"), 1_000)
        .expect("source B");
    let descriptor = VehicleDescriptor::new(source.source_id, "9")
        .with_tesla_identity(Some(9), None);
    let vehicle = store
        .register_vehicle_with_id(&descriptor, 2_000, Uuid::from_u128(9))
        .expect("vehicle A");
    let sibling = store
        .register_vehicle_with_id(
            &VehicleDescriptor::new(source.source_id, "10")
                .with_tesla_identity(Some(10), None),
            2_000,
            Uuid::from_u128(10),
        )
        .expect("vehicle A sibling");
    let other = store
        .register_vehicle_with_id(
            &VehicleDescriptor::new(other_source.source_id, "20")
                .with_tesla_identity(Some(20), None),
            2_000,
            Uuid::from_u128(20),
        )
        .expect("vehicle B");
    let mut manifest = test_manifest();
    manifest.vehicle_id = vehicle.vehicle_id;
    let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest bytes");
    let digest = manifest.chunks[0].sha256;
    store.publish_manifest(&manifest).expect("publish vehicle A");
    store
        .append_observation(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms: 3_000,
                payload: serde_json::json!({"record_type":"retirement-history"}),
            },
            3_000,
        )
        .expect("retained vehicle history");
    let invitation = store
        .create_pairing("retirement-test", 3_000, 30_000)
        .expect("pairing remains independent");

    assert!(store.retire_vehicle(vehicle.vehicle_id, 4_000).unwrap());
    assert!(!store.retire_vehicle(vehicle.vehicle_id, 5_000).unwrap());
    assert!(!store.vehicle_is_active(vehicle.vehicle_id).unwrap());
    assert!(store.published_vehicles().unwrap().is_empty());
    assert_eq!(
        serde_json::to_vec(
            &store
                .manifest_for_vehicle(vehicle.vehicle_id)
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        manifest_bytes
    );
    assert!(store.pack_for_digest(digest).unwrap().is_some());
    assert_eq!(
        store
            .observations_for_vehicle(vehicle.vehicle_id, ObservationQuery::from_start(10))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.source_vehicle_key(vehicle.vehicle_id).unwrap().as_deref(),
        Some("9")
    );
    assert_eq!(
        store
            .open()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );

    // Delayed observations and generic registration may refresh metadata but
    // cannot resurrect a vehicle retired by a complete inventory.
    store.register_vehicle(&descriptor, 6_000).unwrap();
    assert!(!store.vehicle_is_active(vehicle.vehicle_id).unwrap());

    // A complete inventory is source-scoped: it reactivates the returned UUID,
    // retires a missing sibling, and cannot affect another provider source.
    store
        .reconcile_provider_inventory(source.source_id, std::slice::from_ref(&descriptor), 7_000)
        .unwrap();
    assert!(store.vehicle_is_active(vehicle.vehicle_id).unwrap());
    assert!(!store.vehicle_is_active(sibling.vehicle_id).unwrap());
    assert!(store.vehicle_is_active(other.vehicle_id).unwrap());
    store
        .reconcile_provider_inventory(source.source_id, std::slice::from_ref(&descriptor), 8_000)
        .unwrap();
    assert!(store.vehicle_is_active(vehicle.vehicle_id).unwrap());
    assert!(!store.reactivate_vehicle(vehicle.vehicle_id).unwrap());
    assert!(store.reactivate_vehicle(sibling.vehicle_id).unwrap());
    assert!(!store.reactivate_vehicle(sibling.vehicle_id).unwrap());
    store.revoke_pairing(invitation.pairing_id).unwrap();
}

#[test]
fn accepts_a_deterministic_vehicle_id_and_allocates_snapshot_markers() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let source = store
        .register_source(
            &SourceDescriptor::new(crate::db::IMPORT_SOURCE_KIND, "test-source"),
            1_000,
        )
        .expect("source registers");
    let expected_vehicle_id = Uuid::from_u128(7);
    let descriptor = VehicleDescriptor {
        source_id: source.source_id,
        source_vehicle_key: "vin:5YJTESTVIN1234567".into(),
        vin: Some("5YJTESTVIN1234567".into()),
        display_name: Some("Road car".into()),
        tesla_eid: None,
        tesla_vid: None,
    };
    let vehicle = store
        .register_vehicle_with_id(&descriptor, 2_000, expected_vehicle_id)
        .expect("vehicle registers");
    assert_eq!(vehicle.vehicle_id, expected_vehicle_id);
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    assert_eq!(
        store
            .reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)
            .expect("first marker"),
        1
    );
    assert_eq!(
        store
            .reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)
            .expect("second marker"),
        2
    );
    let base_snapshot_id = Uuid::from_u128(9).to_string();
    let connection = store.open().expect("database opens");
    connection
        .execute(
            "INSERT INTO sync_bases
                 (vehicle_id, snapshot_id, base_sequence, base_digest, packs_json)
                 VALUES (?1, ?2, 1, ?3, ?4)",
            params![
                vehicle.vehicle_id.to_string(),
                base_snapshot_id,
                "0".repeat(64),
                b"[]".to_vec()
            ],
        )
        .expect("base inserts");
    connection
        .execute(
            "INSERT INTO sync_heads
                 (vehicle_id, base_snapshot_id, head_sequence, head_digest, terminal_cursor)
                 VALUES (?1, ?2, 598, ?3, '{}')",
            params![
                vehicle.vehicle_id.to_string(),
                base_snapshot_id,
                "1".repeat(64)
            ],
        )
        .expect("advanced live head inserts");
    drop(connection);
    assert_eq!(
        store
            .reserve_next_full_snapshot_sequence(&publication_gate, vehicle.vehicle_id)
            .expect("marker after live head"),
        599
    );

    let conflicting = store
        .register_vehicle_with_id(&descriptor, 3_000, Uuid::from_u128(8))
        .expect_err("different stable identity must fail");
    assert!(matches!(
        conflicting,
        StoreError::VehicleIdentityMismatch { .. }
    ));
}

#[test]
fn pairing_preparation_is_inert_and_revocation_is_idempotent() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let invitation = store
        .prepare_pairing("iPhone", 1_000, 61_000)
        .expect("pairing prepares");
    let count = || {
        store
            .open()
            .expect("open database")
            .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("challenge count")
    };

    assert_eq!(count(), 0);
    store
        .persist_pairing("iPhone", &invitation)
        .expect("pairing persists");
    assert_eq!(count(), 1);
    store
        .revoke_pairing(invitation.pairing_id)
        .expect("first revocation");
    store
        .revoke_pairing(invitation.pairing_id)
        .expect("idempotent revocation");
    assert_eq!(count(), 0);
}

#[test]
fn pairing_secrets_are_redacted_and_zeroizable_on_drop() {
    use zeroize::Zeroize;

    fn assert_zeroize_on_drop<T: zeroize::ZeroizeOnDrop>() {}

    let mut pairing = PairingSecret("pairing-secret".to_owned());
    let mut access = DeviceAccessToken("device-access-secret".to_owned());
    assert!(!format!("{access:?}").contains(access.as_bearer()));
    assert_zeroize_on_drop::<PairingSecret>();
    assert_zeroize_on_drop::<DeviceAccessToken>();
    pairing.zeroize();
    access.zeroize();
    assert!(pairing.as_wire().bytes().all(|byte| byte == 0));
    assert!(access.as_bearer().bytes().all(|byte| byte == 0));
}

#[test]
fn pairing_is_single_use_and_persists_only_token_hashes() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let invitation = store
        .create_pairing("iPhone", 1_000, 61_000)
        .expect("pairing creates");
    assert!(format!("{invitation:?}").contains("[redacted]"));

    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "Bolyki iPhone",
            2_000,
        )
        .expect("claim succeeds");
    assert_eq!(
        format!("{:?}", access.access_token),
        "DeviceAccessToken([redacted])"
    );
    let authenticated = store
        .authenticate_device_at(access.access_token.as_bearer(), 2_001)
        .expect("device lookup")
        .expect("device exists");
    assert_eq!(authenticated.device_id, access.device_id);
    assert_eq!(authenticated.display_name, "Bolyki iPhone");
    assert!(
        store
            .claim_pairing(
                invitation.pairing_id,
                invitation.secret(),
                "Second phone",
                3_000,
            )
            .is_err()
    );

    let connection = store.open().expect("open database");
    let challenge_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| {
            row.get(0)
        })
        .expect("challenge count");
    assert_eq!(challenge_count, 0);
    let stored_token_hash: Vec<u8> = connection
        .query_row("SELECT token_sha256 FROM paired_devices", [], |row| {
            row.get(0)
        })
        .expect("token digest");
    assert_ne!(
        stored_token_hash,
        access.access_token.as_bearer().as_bytes()
    );
}

#[test]
fn pairing_claims_fail_closed_when_expired_or_malformed() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let invitation = store
        .create_pairing("iPad", 1_000, 2_000)
        .expect("pairing creates");
    assert!(matches!(
        store.claim_pairing(invitation.pairing_id, "not-a-token", "iPad", 1_500),
        Err(StoreError::PairingRejected)
    ));
    assert!(matches!(
        store.claim_pairing(invitation.pairing_id, invitation.secret(), "iPad", 2_000),
        Err(StoreError::PairingRejected)
    ));
    assert!(
        store
            .authenticate_device("not-a-token")
            .expect("malformed token lookup")
            .is_none()
    );
}

#[test]
fn bogus_pairing_proofs_never_wait_for_the_sqlite_writer() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let invitation = store
        .create_pairing("real invitation", 1_000, 61_000)
        .expect("pairing creates");
    let unrelated = store
        .prepare_pairing("unrelated invitation", 1_000, 61_000)
        .expect("pairing prepares");
    let unrelated_bearer = DeviceAccessToken::generate().expect("random bearer");

    let mut writer = store.open().expect("open writer");
    let transaction = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .expect("hold writer lock");

    assert!(matches!(
        store.claim_pairing(invitation.pairing_id, unrelated.secret(), "attacker", 2_000,),
        Err(StoreError::PairingRejected)
    ));
    assert!(matches!(
        store.rotate_device(unrelated_bearer.as_bearer(), 2_000),
        Err(StoreError::PairingRejected)
    ));

    transaction.rollback().expect("release writer lock");
}

#[test]
fn paired_bearer_expires_at_boundary_and_survives_restart() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let invitation = store
        .create_pairing("boundary", 10_000, 20_000)
        .expect("pairing creates");
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "phone", 15_000)
        .expect("claim succeeds");
    let bearer = access.access_token.as_bearer().to_owned();
    assert!(
        store
            .authenticate_device_at(&bearer, access.expires_at_ms)
            .expect("unused bearer at expiry")
            .is_none()
    );
    assert!(
        store
            .authenticate_device_at(&bearer, access.expires_at_ms - 1)
            .expect("before expiry")
            .is_some()
    );
    assert!(
        store
            .authenticate_device_at(&bearer, access.expires_at_ms)
            .expect("original expiry after sliding renewal")
            .is_some()
    );
    drop(store);
    let restarted = HubStore::open_existing(temporary.path()).expect("restart opens");
    assert!(
        restarted
            .authenticate_device_at(&bearer, access.expires_at_ms - 1)
            .expect("restart auth")
            .is_some()
    );
}

#[test]
fn paired_bearer_authentication_slides_lifetime_without_clock_regression() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let invitation = store
        .create_pairing("sliding lifetime", 10_000, 20_000)
        .expect("pairing creates");
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "phone", 15_000)
        .expect("claim succeeds");
    let bearer = access.access_token.as_bearer();
    let authenticated = store
        .authenticate_device_at(bearer, 20_000)
        .expect("current bearer auth")
        .expect("current bearer is valid");
    assert_eq!(
        authenticated.expires_at_ms,
        20_000 + PAIRED_DEVICE_TOKEN_LIFETIME_MS
    );
    assert_eq!(authenticated.last_authenticated_at_ms, Some(20_000));
    store
        .authenticate_device_at(bearer, 19_000)
        .expect("clock regression auth")
        .expect("current bearer remains valid");
    assert!(
        store
            .authenticate_device_at("invalid", 21_000)
            .unwrap()
            .is_none()
    );
    drop(store);

    let restarted = HubStore::open_existing(temporary.path()).expect("restart opens");
    let persisted = restarted.list_paired_devices().expect("paired devices");
    assert_eq!(
        persisted[0].expires_at_ms,
        20_000 + PAIRED_DEVICE_TOKEN_LIFETIME_MS
    );
    assert_eq!(persisted[0].last_authenticated_at_ms, Some(20_000));
}

#[test]
fn paired_bearer_rotation_grace_allows_lost_response_retry_and_revoke() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let invitation = store
        .create_pairing("rotation", 10_000, 20_000)
        .expect("pairing creates");
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "phone", 15_000)
        .expect("claim succeeds");
    let old_bearer = access.access_token.as_bearer().to_owned();
    let rotated = store
        .rotate_device(&old_bearer, 16_000)
        .expect("rotation succeeds");
    assert_eq!(rotated.device_id, access.device_id);
    assert!(
        store
            .authenticate_device_at(&old_bearer, 16_000)
            .expect("old auth")
            .is_some()
    );
    assert!(
        store
            .authenticate_device_at(rotated.access_token.as_bearer(), 16_000)
            .expect("new auth")
            .is_some()
    );
    let before_retry = credential_rows(&store);
    let retried = store
        .rotate_device(&old_bearer, 16_001)
        .expect("lost-response retry succeeds during grace");
    assert_eq!(retried.device_id, access.device_id);
    // The retry never issues a different token: it is the successor the first
    // rotation made, and it changes no row.
    assert_eq!(
        retried.access_token.as_bearer(),
        rotated.access_token.as_bearer()
    );
    assert_eq!(retried.expires_at_ms, rotated.expires_at_ms);
    assert!(!retried.at_maximum_lifetime);
    assert_eq!(credential_rows(&store), before_retry);
    assert!(
        store
            .authenticate_device_at(retried.access_token.as_bearer(), 16_001)
            .expect("retry bearer auth")
            .is_some()
    );
    store
        .revoke_device_at(rotated.device_id, 17_000)
        .expect("revoke succeeds");
    assert!(matches!(
        store.revoke_device_at(Uuid::new_v4(), 17_000),
        Err(StoreError::PairingRejected)
    ));
    assert!(
        store
            .authenticate_device_at(rotated.access_token.as_bearer(), 17_000)
            .expect("revoked auth")
            .is_none()
    );
    assert!(
        store
            .authenticate_device_at(retried.access_token.as_bearer(), 17_000)
            .expect("revoked retry bearer auth")
            .is_none()
    );
    let listed = store.list_paired_devices().expect("list devices");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].revoked_at_ms, Some(17_000));
}

#[test]
fn paired_bearer_rotation_grace_expires_at_24_hours() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let invitation = store
        .create_pairing("grace expiry", 10_000, 20_000)
        .expect("pairing creates");
    let access = store
        .claim_pairing(invitation.pairing_id, invitation.secret(), "phone", 15_000)
        .expect("claim succeeds");
    let old = access.access_token.as_bearer().to_owned();
    let rotated = store.rotate_device(&old, 16_000).expect("rotate succeeds");
    assert!(
        store
            .authenticate_device_at(&old, 16_000 + PAIRED_DEVICE_ROTATION_GRACE_MS - 1)
            .expect("last grace millisecond")
            .is_some()
    );
    // The lost-response retry is served through the last grace millisecond
    // with the very successor the first rotation issued, and not after it.
    assert_eq!(
        store
            .rotate_device(&old, 16_000 + PAIRED_DEVICE_ROTATION_GRACE_MS - 1)
            .expect("retry in the last grace millisecond")
            .access_token
            .as_bearer(),
        rotated.access_token.as_bearer()
    );
    assert!(matches!(
        store.rotate_device(&old, 16_000 + PAIRED_DEVICE_ROTATION_GRACE_MS),
        Err(StoreError::PairingRejected)
    ));
    assert!(
        store
            .authenticate_device_at(&old, 16_000 + PAIRED_DEVICE_ROTATION_GRACE_MS)
            .expect("after grace")
            .is_none()
    );
    assert!(
        store
            .authenticate_device_at(
                rotated.access_token.as_bearer(),
                16_000 + PAIRED_DEVICE_ROTATION_GRACE_MS,
            )
            .expect("current bearer after grace")
            .is_some()
    );
}

#[test]
fn generic_receipt_api_rejects_reserved_legacy_refresh_start() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    assert!(matches!(
        store.begin_outbound_request(&OutboundRequestStart {
            correlation_id: Uuid::new_v4(),
            vehicle_tesla_id: None,
            transport: OutboundRequestTransport::LegacyAuth,
            operation: OutboundRequestOperation::TokenRefresh,
            safety_class: OutboundRequestSafetyClass::NonWakeEndpoint,
            precondition: OutboundRequestPrecondition::NotRequired,
        }),
        Err(StoreError::ReservedCredentialRefreshReceipt)
    ));
    assert_eq!(
        store
            .outbound_request_watermark()
            .expect("request watermark")
            .receipt_id,
        0
    );
}

#[test]
fn stream_audit_summary_reports_recovery_and_unresolved_sessions_without_payloads() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let correlation_id = Uuid::new_v4();
    let complete = |operation, outcome| {
        let receipt = store
            .begin_outbound_request(&OutboundRequestStart {
                correlation_id,
                vehicle_tesla_id: Some(9),
                transport: OutboundRequestTransport::Stream,
                operation,
                safety_class: OutboundRequestSafetyClass::NonWakeEndpoint,
                precondition: OutboundRequestPrecondition::NotRequired,
            })
            .expect("stream attempt begins");
        store
            .complete_outbound_request(
                receipt,
                &OutboundRequestCompletion {
                    outcome,
                    http_status: None,
                    retry_after_seconds: None,
                },
            )
            .expect("stream attempt completes");
    };
    complete(
        OutboundRequestOperation::StreamConnect,
        OutboundRequestOutcome::Success,
    );
    complete(
        OutboundRequestOperation::StreamSubscribe,
        OutboundRequestOutcome::TransportError,
    );
    complete(
        OutboundRequestOperation::StreamSubscribe,
        OutboundRequestOutcome::Success,
    );
    store
        .begin_stream_session(correlation_id, 9)
        .expect("stream session begins");

    let summary = store
        .stream_audit_summary_since(0)
        .expect("stream diagnostic summary");
    assert_eq!(summary.connect_attempts, 1);
    assert_eq!(summary.successful_connects, 1);
    assert_eq!(summary.subscribe_attempts, 2);
    assert_eq!(summary.successful_subscriptions, 1);
    assert_eq!(summary.transport_errors, 1);
    assert_eq!(summary.authentication_rejections, 0);
    assert_eq!(summary.protocol_errors, 0);
    assert_eq!(summary.unresolved_attempts, 0);
    assert_eq!(summary.sessions, 1);
    assert_eq!(summary.unresolved_sessions, 1);
    assert!(summary.last_subscription_success_at_ms.is_some());
    assert!(summary.last_failure_at_ms.is_some());
    assert!(matches!(
        store.stream_audit_summary_since(-1),
        Err(StoreError::InvalidStreamAuditWindow)
    ));
}

#[test]
fn generic_completion_cannot_close_bound_legacy_refresh_receipt() {
    let (_directory, store) = lc_store_with_pair();
    let receipt = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
    })
    .expect("begin bound refresh");
    assert!(matches!(
        store.complete_outbound_request(
            receipt,
            &OutboundRequestCompletion {
                outcome: OutboundRequestOutcome::Success,
                http_status: Some(200),
                retry_after_seconds: None,
            },
        ),
        Err(StoreError::ReservedCredentialRefreshReceipt)
    ));
    assert!(
        s_test_with_credential_connection(&store, |connection| {
            connection.has_unresolved_credential_refresh(CredentialSlot::OwnerApi)
        })
        .expect("reserved receipt stays started")
    );
    let cancelled = s_test_with_credential_connection(&store, |connection| {
        connection.cancel_unsent_credential_refresh(receipt, lc_gen(1))
    });
    let state: (String, Option<i64>, i64) = store.open().unwrap().query_row(
        "SELECT outcome, http_status, (SELECT count(*) FROM credential_refresh_attempts WHERE receipt_id=?1) FROM outbound_request_receipts WHERE id=?1",
        [receipt.0], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(
        (cancelled.is_ok(), state),
        (true, ("cancelled".to_owned(), None, 0)),
        "INTENT-GENERIC-01 reserved generic refusal leaves dedicated unsent cancellation usable"
    );
}

#[test]
fn legacy_refresh_terminalization_clamps_backwards_clock() {
    fn move_start_into_future(store: &HubStore, receipt: OutboundRequestReceiptId) -> i64 {
        let future = i64::MAX - 1;
        store
            .open()
            .expect("open store")
            .execute(
                "UPDATE outbound_request_receipts SET started_at_ms = ?2 WHERE id = ?1",
                params![receipt.0, future],
            )
            .expect("move receipt start into future");
        future
    }

    fn assert_clamped(store: &HubStore, receipt: OutboundRequestReceiptId, expected: i64) {
        let (completed, duration): (i64, i64) = store
            .open()
            .expect("open store")
            .query_row(
                "SELECT completed_at_ms, duration_ms
                       FROM outbound_request_receipts WHERE id = ?1",
                params![receipt.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("terminal receipt");
        assert_eq!(completed, expected);
        assert_eq!(duration, 0);
    }

    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let first_generation = Uuid::from_u128(51);
    let first = s_test_with_credential_connection(&store, |connection| {
        s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            first_generation,
            b"clamp-first-access",
            b"clamp-first-refresh",
            0,
            0,
        )
    })
    .expect("first generation");
    s_test_with_credential_connection(&store, |connection| {
        connection.replace_credential_pair(
            &first,
            &EntryProvenance::Supplied,
            &LegacyCandidates(Vec::new()),
            ReplacePolicy::ReplaceAny,
        )
    })
    .expect("first tokens persist");

    let cancelled = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, first_generation)
    })
    .expect("cancelled refresh begins");
    let future = move_start_into_future(&store, cancelled);
    s_test_with_credential_connection(&store, |connection| {
        connection.cancel_unsent_credential_refresh(cancelled, first_generation)
    })
    .expect("cancel clamps clock");
    assert_clamped(&store, cancelled, future);

    let replaced = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, first_generation)
    })
    .expect("replacement refresh begins");
    let future = move_start_into_future(&store, replaced);
    let second_generation = Uuid::from_u128(52);
    let second = s_test_with_credential_connection(&store, |connection| {
        s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            second_generation,
            b"clamp-second-access",
            b"clamp-second-refresh",
            0,
            0,
        )
    })
    .expect("second generation");
    s_test_with_credential_connection(&store, |connection| {
        connection.replace_credential_pair(
            &second,
            &EntryProvenance::Supplied,
            &LegacyCandidates(Vec::new()),
            ReplacePolicy::ReplaceAny,
        )
    })
    .expect("replacement clamps clock");
    assert_clamped(&store, replaced, future);

    let signed_out = s_test_with_credential_connection(&store, |connection| {
        connection.begin_credential_refresh(CredentialSlot::OwnerApi, second_generation)
    })
    .expect("sign-out refresh begins");
    let future = move_start_into_future(&store, signed_out);
    s_test_with_credential_connection(&store, |connection| {
        connection.clear_credential_pair(CredentialSlot::OwnerApi)
    })
    .expect("sign out clamps clock");
    assert_clamped(&store, signed_out, future);
}

#[test]
fn legacy_refresh_intent_fences_restart_and_commits_atomically() {
    let (directory, store) = lc_store_with_pair();
    s_test_with_credential_connection(&store, |connection| {
        let stale = connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(9));
        let intents: i64 = connection
            .connection
            .query_row(
                "SELECT count(*) FROM credential_refresh_attempts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            (
                matches!(
                    stale,
                    Err(StoreError::RefreshOutcomeUnknown {
                        reason: BeginRefusal::RowChanged
                    })
                ),
                intents
            ),
            (true, 0),
            "INTENT-OWNER-01 stale input creates no intent"
        );
    });
    let receipt = s_test_with_credential_connection(&store, |connection| {
        let receipt = connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))?;
        connection.credential_durability_barrier()?;
        Ok::<_, StoreError>(receipt)
    })
    .expect("begin intent");
    assert!(
        matches!(
            s_test_with_credential_connection(&store, |connection| connection
                .begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))),
            Err(StoreError::RefreshOutcomeUnknown {
                reason: BeginRefusal::AttemptInFlight
            })
        ),
        "INTENT-OWNER-02 duplicate refused before restart"
    );
    // Drop the actual once-held SQLite connection as well as the store handle.
    let identity = std::sync::Arc::as_ptr(&store.credential_connection_taken) as usize;
    drop(
        S_TEST_CREDENTIAL_CONNECTIONS
            .with(|connections| connections.borrow_mut().remove(&identity))
            .unwrap(),
    );
    drop(store);
    let reopened = HubStore::initialize(directory.path()).expect("restart store");
    let completed_receipt = s_test_with_credential_connection(&reopened, |connection| {
        assert!(
            connection
                .has_unresolved_credential_refresh(CredentialSlot::OwnerApi)
                .expect("intent survives restart")
        );
        assert!(matches!(
            connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1)),
            Err(StoreError::RefreshOutcomeUnknown {
                reason: BeginRefusal::AttemptInFlight,
            })
        ));
        let cancelled = connection.cancel_unsent_credential_refresh(receipt, lc_gen(1));
        let retry = connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1));
        assert_eq!(
            (cancelled.is_ok(), retry.is_ok()),
            (true, true),
            "INTENT-OWNER-03 unsent cancellation permits retry after restart"
        );
        let receipt = retry.unwrap();
        let invalid = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(1),
            b"invalid-owner-access",
            b"invalid-owner-refresh",
            0,
            0,
        )
        .unwrap();
        assert!(
            matches!(
                connection.complete_credential_refresh(
                    receipt,
                    lc_gen(1),
                    &invalid,
                    &LegacyCandidates(Vec::new())
                ),
                Err(StoreError::RefreshOutcomeUnknown {
                    reason: BeginRefusal::RowChanged
                })
            ),
            "INTENT-OWNER-04 C10 successor generation must differ from input"
        );
        let successor = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(2),
            b"restart-successor-access",
            b"restart-successor-refresh",
            0,
            0,
        )
        .expect("seal successor");
        assert_eq!(
            connection
                .complete_credential_refresh(
                    receipt,
                    lc_gen(1),
                    &successor,
                    &LegacyCandidates(Vec::new())
                )
                .expect("complete intent"),
            RefreshCompletion::Completed
        );
        connection
            .credential_durability_barrier()
            .expect("completion barrier");
        assert_eq!(
            connection
                .load_credential_pair(CredentialSlot::OwnerApi)
                .expect("load stored successor")
                .expect("pair")
                .generation,
            lc_gen(2)
        );
        assert!(
            !connection
                .has_unresolved_credential_refresh(CredentialSlot::OwnerApi)
                .expect("receipt closed with pair")
        );
        receipt
    });
    let identity = std::sync::Arc::as_ptr(&reopened.credential_connection_taken) as usize;
    drop(
        S_TEST_CREDENTIAL_CONNECTIONS
            .with(|connections| connections.borrow_mut().remove(&identity))
            .unwrap(),
    );
    drop(reopened);
    let persisted = HubStore::initialize(directory.path()).unwrap();
    s_test_with_credential_connection(&persisted, |connection| {
        let pair = connection
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap();
        let keys = crate::auth::credential_keys::KeyringHandle::new(
            crate::auth::credential_keys::Keyring {
                current: crate::auth::credential_keys::KeyMaterial {
                    id: KeyId([0x53; 8]),
                    created_at_s: 1_800_000_000,
                    key: Zeroizing::new([0x53; 32]),
                },
                previous: None,
            },
        );
        let tokens = pair
            .open(&keys)
            .map(|(access, refresh)| (access.to_vec(), refresh.to_vec()));
        let state: (String, Option<i64>, Option<i64>, Option<String>) = connection.connection.query_row(
            "SELECT r.outcome,r.http_status,r.retry_after_seconds,a.output_generation FROM outbound_request_receipts r JOIN credential_refresh_attempts a ON a.receipt_id=r.id WHERE r.id=?1",
            [completed_receipt.0], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).unwrap();
        assert_eq!(
            (pair.generation, tokens, state),
            (
                lc_gen(2),
                Ok((
                    b"restart-successor-access".to_vec(),
                    b"restart-successor-refresh".to_vec()
                )),
                (
                    "success".to_owned(),
                    Some(200),
                    None,
                    Some(lc_gen(2).to_string())
                )
            ),
            "INTENT-OWNER-05 successor material and receipt closure persist together across actual reopen"
        );
    });
}

#[test]
fn explicit_new_credentials_supersede_ambiguous_refresh_without_reusing_input() {
    let (_directory, store) = lc_store_with_pair();
    s_test_with_credential_connection(&store, |connection| {
        let receipt = connection
            .begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1))
            .expect("ambiguous input");
        let original_envelope = connection
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap()
            .refresh_envelope;
        for same_token_generation in [lc_gen(3), lc_gen(4)] {
            let same_token = s_test_synthetic_pair(
                connection,
                CredentialSlot::OwnerApi,
                same_token_generation,
                b"same-token-new-access",
                format!("synthetic-refresh-{}", lc_gen(1)).as_bytes(),
                0,
                0,
            )
            .unwrap();
            let reencrypted = same_token.refresh_envelope != original_envelope;
            let refused = connection.replace_credential_pair(
                &same_token,
                &EntryProvenance::Supplied,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny,
            );
            assert_eq!(
                (
                    reencrypted,
                    matches!(refused, Err(StoreError::RefreshTokenSpent))
                ),
                (true, true),
                "INTENT-AMBIG-01 the ambiguous refresh token stays spent under fresh encryption and generation"
            );
        }
        let replacement = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(2),
            b"external-replacement-access",
            b"external-replacement-refresh",
            0,
            0,
        )
        .expect("external pair");
        connection
            .replace_credential_pair(
                &replacement,
                &EntryProvenance::Supplied,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny,
            )
            .expect("external replacement resolves ambiguity");
        assert!(
            !connection
                .has_unresolved_credential_refresh(CredentialSlot::OwnerApi)
                .expect("no unresolved attempt")
        );
        let outcome: String = connection
            .connection
            .query_row(
                "SELECT outcome FROM outbound_request_receipts WHERE id = ?1",
                [receipt.0],
                |row| row.get(0),
            )
            .expect("retained terminal receipt");
        assert_eq!(outcome, "cancelled");
        let old_fence: bool = connection.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_refresh_attempts WHERE receipt_id = ?1 AND fence_id IS NOT NULL)",
            [receipt.0], |row| row.get(0),
        ).expect("spent input remains fenced");
        assert!(old_fence);
        let stored = connection
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap();
        let keys = crate::auth::credential_keys::KeyringHandle::new(
            crate::auth::credential_keys::Keyring {
                current: crate::auth::credential_keys::KeyMaterial {
                    id: KeyId([0x53; 8]),
                    created_at_s: 1_800_000_000,
                    key: Zeroizing::new([0x53; 32]),
                },
                previous: None,
            },
        );
        assert_eq!(
            (
                stored.generation,
                stored
                    .open(&keys)
                    .map(|(access, refresh)| (access.to_vec(), refresh.to_vec()))
            ),
            (
                lc_gen(2),
                Ok((
                    b"external-replacement-access".to_vec(),
                    b"external-replacement-refresh".to_vec()
                ))
            ),
            "INTENT-AMBIG-02 the different external pair and both token values are stored"
        );
        let former = connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(1));
        let next = connection.begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(2));
        assert_eq!(
            (
                matches!(
                    former,
                    Err(StoreError::RefreshOutcomeUnknown {
                        reason: BeginRefusal::RowChanged
                    })
                ),
                next.is_ok()
            ),
            (true, true),
            "INTENT-AMBIG-03 former input stays unusable while the new input can refresh"
        );
        let next = next.unwrap();
        connection
            .cancel_unsent_credential_refresh(next, lc_gen(2))
            .expect("cleanup");
    });
}

#[test]
fn legacy_refresh_rejects_successor_generation_already_consumed() {
    let (_directory, store) = lc_store_with_pair();
    as_refresh(&store, lc_gen(1), lc_gen(2));
    s_test_with_credential_connection(&store, |connection| {
        let receipt = connection
            .begin_credential_refresh(CredentialSlot::OwnerApi, lc_gen(2))
            .unwrap();
        // R31 step 3 follows the consumed token, independent of a fresh UUID.
        let pair = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(3),
            b"spent-successor-access",
            format!("synthetic-refresh-{}", lc_gen(1)).as_bytes(),
            AS_T0 + 57_600,
            AS_T0 + 50_400,
        )
        .expect("successor candidate");
        assert!(
            matches!(
                connection.complete_credential_refresh(
                    receipt,
                    lc_gen(2),
                    &pair,
                    &LegacyCandidates(Vec::new())
                ),
                Err(StoreError::RefreshTokenSpent)
            ),
            "INTENT-CONSUMED-01 a fresh successor generation cannot reuse an earlier consumed refresh token"
        );
        assert!(
            connection
                .has_unresolved_credential_refresh(CredentialSlot::OwnerApi)
                .expect("refused completion leaves receipt started")
        );
        assert_eq!(
            connection
                .load_credential_pair(CredentialSlot::OwnerApi)
                .expect("input row")
                .expect("pair")
                .generation,
            lc_gen(2)
        );
        let stored = connection
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap();
        let keys = crate::auth::credential_keys::KeyringHandle::new(
            crate::auth::credential_keys::Keyring {
                current: crate::auth::credential_keys::KeyMaterial {
                    id: KeyId([0x53; 8]),
                    created_at_s: 1_800_000_000,
                    key: Zeroizing::new([0x53; 32]),
                },
                previous: None,
            },
        );
        let state: (String, Option<i64>, Option<i64>, bool) = connection.connection.query_row(
            "SELECT r.outcome,r.http_status,r.retry_after_seconds,EXISTS(SELECT 1 FROM credential_refresh_attempts a JOIN credential_refresh_fences f ON f.fence_id=a.fence_id WHERE a.receipt_id=r.id) FROM outbound_request_receipts r WHERE r.id=?1",
            [receipt.0], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        ).unwrap();
        assert_eq!(
            (
                stored.generation,
                stored
                    .open(&keys)
                    .map(|(access, refresh)| (access.to_vec(), refresh.to_vec())),
                state
            ),
            (
                lc_gen(2),
                Ok((
                    b"synthetic-successor-access".to_vec(),
                    format!("synthetic-refresh-{}", lc_gen(2)).into_bytes()
                )),
                ("started".to_owned(), None, None, true)
            ),
            "INTENT-CONSUMED-02 spent-successor refusal preserves current material and the started fenced receipt"
        );
    });
}

#[test]
fn explicit_replacement_rejects_previously_consumed_generation_after_successor_commit() {
    let (_directory, store) = lc_store_with_pair();
    as_refresh(&store, lc_gen(1), lc_gen(2));
    s_test_with_credential_connection(&store, |connection| {
        let replacement = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            lc_gen(3),
            b"spent-replacement-access",
            format!("synthetic-refresh-{}", lc_gen(1)).as_bytes(),
            0,
            0,
        )
        .expect("real envelope of the previously consumed input token");
        assert!(matches!(
            connection.replace_credential_pair(
                &replacement,
                &EntryProvenance::Supplied,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny,
            ),
            Err(StoreError::RefreshTokenSpent)
        ));
        assert_eq!(
            connection
                .load_credential_pair(CredentialSlot::OwnerApi)
                .expect("successor row")
                .expect("pair")
                .generation,
            lc_gen(2)
        );
        let stored = connection
            .load_credential_pair(CredentialSlot::OwnerApi)
            .unwrap()
            .unwrap();
        let keys = crate::auth::credential_keys::KeyringHandle::new(
            crate::auth::credential_keys::Keyring {
                current: crate::auth::credential_keys::KeyMaterial {
                    id: KeyId([0x53; 8]),
                    created_at_s: 1_800_000_000,
                    key: Zeroizing::new([0x53; 32]),
                },
                previous: None,
            },
        );
        assert_eq!(
            (
                stored.generation,
                stored
                    .open(&keys)
                    .map(|(access, refresh)| (access.to_vec(), refresh.to_vec()))
            ),
            (
                lc_gen(2),
                Ok((
                    b"synthetic-successor-access".to_vec(),
                    format!("synthetic-refresh-{}", lc_gen(2)).into_bytes()
                ))
            ),
            "INTENT-REPLACE-01 spent-token replacement refusal preserves successor generation and token material"
        );
    });
}

#[test]
fn fleet_refresh_intent_fences_restart_and_commits_successor_atomically() {
    let (directory, store) = cs_store();
    let input = Uuid::from_u128(0xF101);
    let output = Uuid::from_u128(0xF102);
    let receipt = s_test_with_credential_connection(&store, |connection| {
        let pair = s_test_synthetic_pair(
            connection,
            CredentialSlot::FleetApi,
            input,
            b"fleet-restart-access",
            b"fleet-restart-refresh",
            AS_T0 + 28_800,
            AS_T0 + 21_600,
        )?;
        connection.replace_credential_pair(
            &pair,
            &EntryProvenance::Supplied,
            &LegacyCandidates(Vec::new()),
            ReplacePolicy::ReplaceAny,
        )?;
        let stale =
            connection.begin_credential_refresh(CredentialSlot::FleetApi, Uuid::from_u128(0xF100));
        let intents: i64 = connection
            .connection
            .query_row(
                "SELECT count(*) FROM credential_refresh_attempts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            (
                matches!(
                    stale,
                    Err(StoreError::RefreshOutcomeUnknown {
                        reason: BeginRefusal::RowChanged
                    })
                ),
                intents
            ),
            (true, 0),
            "INTENT-FLEET-01 stale input creates no Fleet intent"
        );
        let receipt = connection.begin_credential_refresh(CredentialSlot::FleetApi, input)?;
        connection.credential_durability_barrier()?;
        Ok::<_, StoreError>(receipt)
    })
    .expect("durable Fleet intent");
    assert!(
        matches!(
            s_test_with_credential_connection(&store, |connection| connection
                .begin_credential_refresh(CredentialSlot::FleetApi, input)),
            Err(StoreError::RefreshOutcomeUnknown {
                reason: BeginRefusal::AttemptInFlight
            })
        ),
        "INTENT-FLEET-02 duplicate refused before restart"
    );
    assert!(
        matches!(
            store.complete_outbound_request(
                receipt,
                &OutboundRequestCompletion {
                    outcome: OutboundRequestOutcome::Success,
                    http_status: Some(200),
                    retry_after_seconds: None
                }
            ),
            Err(StoreError::ReservedCredentialRefreshReceipt)
        ),
        "INTENT-FLEET-03 generic completion cannot close the reserved Fleet receipt"
    );
    assert!(
        matches!(
            store.complete_outbound_request(
                receipt,
                &OutboundRequestCompletion {
                    outcome: OutboundRequestOutcome::Cancelled,
                    http_status: None,
                    retry_after_seconds: None
                }
            ),
            Err(StoreError::ReservedCredentialRefreshReceipt)
        ),
        "INTENT2-FLEET-01 generic cancellation without HTTP status refuses the bound Fleet receipt"
    );
    let identity = std::sync::Arc::as_ptr(&store.credential_connection_taken) as usize;
    drop(
        S_TEST_CREDENTIAL_CONNECTIONS
            .with(|connections| connections.borrow_mut().remove(&identity))
            .unwrap(),
    );
    drop(store);
    let reopened = HubStore::initialize(directory.path()).expect("restart Fleet store");
    let completed_receipt = s_test_with_credential_connection(&reopened, |connection| {
        let receipt_state: (String, Option<i64>, Option<i64>) = connection
            .connection
            .query_row(
                "SELECT outcome,http_status,retry_after_seconds FROM outbound_request_receipts WHERE id=?1",
                [receipt.0],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (
                connection
                    .has_unresolved_credential_refresh(CredentialSlot::FleetApi)
                    .expect("Fleet intent survives restart"),
                receipt_state,
            ),
            (true, ("started".to_owned(), None, None)),
            "INTENT2-FLEET-02 refused generic cancellation leaves the Fleet attempt started without HTTP or retry across reopen"
        );
        assert!(matches!(
            connection.begin_credential_refresh(CredentialSlot::FleetApi, input),
            Err(StoreError::RefreshOutcomeUnknown {
                reason: BeginRefusal::AttemptInFlight
            })
        ));
        let cancelled = connection.cancel_unsent_credential_refresh(receipt, input);
        let retry = connection.begin_credential_refresh(CredentialSlot::FleetApi, input);
        assert_eq!(
            (cancelled.is_ok(), retry.is_ok()),
            (true, true),
            "INTENT-FLEET-04 unsent Fleet cancellation permits retry after restart"
        );
        let receipt = retry.unwrap();
        let successor = s_test_synthetic_pair(
            connection,
            CredentialSlot::FleetApi,
            output,
            b"fleet-successor-access",
            b"fleet-successor-refresh",
            AS_T0 + 57_600,
            AS_T0 + 50_400,
        )
        .expect("Fleet successor");
        assert_eq!(
            connection
                .complete_credential_refresh(
                    receipt,
                    input,
                    &successor,
                    &LegacyCandidates(Vec::new()),
                )
                .expect("Fleet completion"),
            RefreshCompletion::Completed
        );
        connection
            .credential_durability_barrier()
            .expect("Fleet completion barrier");
        assert_eq!(
            connection
                .load_credential_pair(CredentialSlot::FleetApi)
                .expect("Fleet row")
                .expect("pair")
                .generation,
            output
        );
        assert!(
            !connection
                .has_unresolved_credential_refresh(CredentialSlot::FleetApi)
                .expect("Fleet receipt closed atomically")
        );
        receipt
    });
    let identity = std::sync::Arc::as_ptr(&reopened.credential_connection_taken) as usize;
    drop(
        S_TEST_CREDENTIAL_CONNECTIONS
            .with(|connections| connections.borrow_mut().remove(&identity))
            .unwrap(),
    );
    drop(reopened);
    let persisted = HubStore::initialize(directory.path()).unwrap();
    s_test_with_credential_connection(&persisted, |connection| {
        let stored = connection
            .load_credential_pair(CredentialSlot::FleetApi)
            .unwrap()
            .unwrap();
        let keys = crate::auth::credential_keys::KeyringHandle::new(
            crate::auth::credential_keys::Keyring {
                current: crate::auth::credential_keys::KeyMaterial {
                    id: KeyId([0x53; 8]),
                    created_at_s: 1_800_000_000,
                    key: Zeroizing::new([0x53; 32]),
                },
                previous: None,
            },
        );
        let state: (String, Option<i64>, Option<i64>, Option<String>) = connection.connection.query_row(
            "SELECT r.outcome,r.http_status,r.retry_after_seconds,a.output_generation FROM outbound_request_receipts r JOIN credential_refresh_attempts a ON a.receipt_id=r.id WHERE r.id=?1",
            [completed_receipt.0], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).unwrap();
        assert_eq!(
            (
                stored.generation,
                stored
                    .open(&keys)
                    .map(|(access, refresh)| (access.to_vec(), refresh.to_vec())),
                state
            ),
            (
                output,
                Ok((
                    b"fleet-successor-access".to_vec(),
                    b"fleet-successor-refresh".to_vec()
                )),
                (
                    "success".to_owned(),
                    Some(200),
                    None,
                    Some(output.to_string())
                )
            ),
            "INTENT-FLEET-05 Fleet successor material and receipt closure persist together across actual reopen"
        );
        let consumed = s_test_synthetic_pair(
            connection,
            CredentialSlot::FleetApi,
            Uuid::from_u128(0xF103),
            b"fleet-consumed-access",
            b"fleet-restart-refresh",
            AS_T0 + 57_600,
            AS_T0 + 50_400,
        )
        .unwrap();
        assert!(
            matches!(
                connection.replace_credential_pair(
                    &consumed,
                    &EntryProvenance::Supplied,
                    &LegacyCandidates(Vec::new()),
                    ReplacePolicy::ReplaceAny
                ),
                Err(StoreError::RefreshTokenSpent)
            ),
            "INTENT-FLEET-06 consumed Fleet token cannot be re-entered under a fresh generation"
        );
        let ambiguous = connection
            .begin_credential_refresh(CredentialSlot::FleetApi, output)
            .unwrap();
        let same_token = s_test_synthetic_pair(
            connection,
            CredentialSlot::FleetApi,
            Uuid::from_u128(0xF104),
            b"fleet-ambiguous-access",
            b"fleet-successor-refresh",
            AS_T0 + 57_600,
            AS_T0 + 50_400,
        )
        .unwrap();
        assert!(
            matches!(
                connection.replace_credential_pair(
                    &same_token,
                    &EntryProvenance::Supplied,
                    &LegacyCandidates(Vec::new()),
                    ReplacePolicy::ReplaceAny
                ),
                Err(StoreError::RefreshTokenSpent)
            ),
            "INTENT-FLEET-07 ambiguous Fleet token cannot be re-entered under fresh encryption"
        );
        let recovered_generation = Uuid::from_u128(0xF105);
        let recovered = s_test_synthetic_pair(
            connection,
            CredentialSlot::FleetApi,
            recovered_generation,
            b"fleet-operator-access",
            b"fleet-operator-refresh",
            AS_T0 + 57_600,
            AS_T0 + 50_400,
        )
        .unwrap();
        connection
            .replace_credential_pair(
                &recovered,
                &EntryProvenance::Supplied,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny,
            )
            .unwrap();
        let recovered = connection
            .load_credential_pair(CredentialSlot::FleetApi)
            .unwrap()
            .unwrap();
        let cancelled: String = connection
            .connection
            .query_row(
                "SELECT outcome FROM outbound_request_receipts WHERE id=?1",
                [ambiguous.0],
                |row| row.get(0),
            )
            .unwrap();
        let former = connection.begin_credential_refresh(CredentialSlot::FleetApi, output);
        let next =
            connection.begin_credential_refresh(CredentialSlot::FleetApi, recovered_generation);
        assert_eq!(
            (
                recovered.generation,
                recovered
                    .open(&keys)
                    .map(|(access, refresh)| (access.to_vec(), refresh.to_vec())),
                cancelled,
                matches!(
                    former,
                    Err(StoreError::RefreshOutcomeUnknown {
                        reason: BeginRefusal::RowChanged
                    })
                ),
                next.is_ok()
            ),
            (
                recovered_generation,
                Ok((
                    b"fleet-operator-access".to_vec(),
                    b"fleet-operator-refresh".to_vec()
                )),
                "cancelled".to_owned(),
                true,
                true
            ),
            "INTENT-FLEET-08 different operator credentials recover the Fleet slot and only the new input can refresh"
        );
        connection
            .cancel_unsent_credential_refresh(next.unwrap(), recovered_generation)
            .unwrap();
    });
}
