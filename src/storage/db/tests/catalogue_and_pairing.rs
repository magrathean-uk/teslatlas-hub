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
fn accepts_a_deterministic_vehicle_id_and_allocates_snapshot_markers() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let source = store
        .register_source(&SourceDescriptor::new("teslamate", "test-source"), 1_000)
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
            .authenticate_device_at(&bearer, access.expires_at_ms - 1)
            .expect("before expiry")
            .is_some()
    );
    assert!(
        store
            .authenticate_device_at(&bearer, access.expires_at_ms)
            .expect("at expiry")
            .is_none()
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
fn paired_bearer_revoke_and_rotation_invalidate_old_material() {
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
            .is_none()
    );
    assert!(
        store
            .authenticate_device_at(rotated.access_token.as_bearer(), 16_000)
            .expect("new auth")
            .is_some()
    );
    assert!(matches!(
        store.rotate_device(&old_bearer, 16_001),
        Err(StoreError::PairingRejected)
    ));
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
    let listed = store.list_paired_devices().expect("list devices");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].revoked_at_ms, Some(17_000));
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
        Err(StoreError::ReservedLegacyRefreshReceipt)
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
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let input_generation = Uuid::from_u128(31);
    let input =
        TeslaMateLegacyTokenStore::imported(b"old-access".to_vec(), b"old-refresh".to_vec())
            .expect("input tokens")
            .with_credential_generation(input_generation)
            .expect("input generation");
    store
        .replace_teslamate_legacy_tokens(&input)
        .expect("input persists");
    let receipt = store
        .begin_legacy_refresh(input_generation)
        .expect("refresh begins");

    assert!(matches!(
        store.complete_outbound_request(
            receipt,
            &OutboundRequestCompletion {
                outcome: OutboundRequestOutcome::Success,
                http_status: Some(200),
                retry_after_seconds: None,
            },
        ),
        Err(StoreError::ReservedLegacyRefreshReceipt)
    ));
    assert!(
        store
            .has_unresolved_legacy_refresh()
            .expect("refresh remains unresolved")
    );
    store
        .cancel_unsent_legacy_refresh(receipt, input_generation)
        .expect("dedicated API can cancel unsent refresh");
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
    let first =
        TeslaMateLegacyTokenStore::imported(b"first-access".to_vec(), b"first-refresh".to_vec())
            .expect("first tokens")
            .with_credential_generation(first_generation)
            .expect("first generation");
    store
        .replace_teslamate_legacy_tokens(&first)
        .expect("first tokens persist");

    let cancelled = store
        .begin_legacy_refresh(first_generation)
        .expect("cancelled refresh begins");
    let future = move_start_into_future(&store, cancelled);
    store
        .cancel_unsent_legacy_refresh(cancelled, first_generation)
        .expect("cancel clamps clock");
    assert_clamped(&store, cancelled, future);

    let replaced = store
        .begin_legacy_refresh(first_generation)
        .expect("replacement refresh begins");
    let future = move_start_into_future(&store, replaced);
    let second_generation = Uuid::from_u128(52);
    let second =
        TeslaMateLegacyTokenStore::imported(b"second-access".to_vec(), b"second-refresh".to_vec())
            .expect("second tokens")
            .with_credential_generation(second_generation)
            .expect("second generation");
    store
        .replace_teslamate_legacy_tokens(&second)
        .expect("replacement clamps clock");
    assert_clamped(&store, replaced, future);

    let signed_out = store
        .begin_legacy_refresh(second_generation)
        .expect("sign-out refresh begins");
    let future = move_start_into_future(&store, signed_out);
    store
        .clear_teslamate_legacy_tokens()
        .expect("sign out clamps clock");
    assert_clamped(&store, signed_out, future);
}

#[test]
fn legacy_refresh_intent_fences_restart_and_commits_atomically() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let input = Uuid::from_u128(1);
    let initial =
        TeslaMateLegacyTokenStore::imported(b"old-access".to_vec(), b"old-refresh".to_vec())
            .expect("initial token pair")
            .with_credential_generation(input)
            .expect("initial generation");
    store
        .replace_teslamate_legacy_tokens(&initial)
        .expect("initial pair persists");
    assert!(matches!(
        store.begin_legacy_refresh(Uuid::new_v4()),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    assert!(
        !store
            .has_unresolved_legacy_refresh()
            .expect("stale input creates no intent")
    );
    let receipt = store.begin_legacy_refresh(input).expect("intent persists");
    assert!(
        store
            .has_unresolved_legacy_refresh()
            .expect("intent lookup")
    );
    assert!(matches!(
        store.begin_legacy_refresh(input),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    drop(store);
    let restarted = HubStore::open_existing(temporary.path()).expect("restart opens");
    assert!(
        restarted
            .has_unresolved_legacy_refresh()
            .expect("restart intent")
    );
    restarted
        .cancel_unsent_legacy_refresh(receipt, input)
        .expect("pre-send cancellation");
    assert!(
        !restarted
            .has_unresolved_legacy_refresh()
            .expect("cancelled intent")
    );

    let receipt = restarted
        .begin_legacy_refresh(input)
        .expect("second intent persists");
    let output = Uuid::from_u128(2);
    let successor = TeslaMateLegacyTokenStore::refreshed(
        b"new-access".to_vec(),
        b"new-refresh".to_vec(),
        2_000,
        1_750,
    )
    .expect("successor token pair")
    .with_credential_generation(output)
    .expect("successor generation");
    assert!(matches!(
        restarted.complete_legacy_refresh(receipt, input, Uuid::new_v4(), &successor),
        Err(StoreError::InvalidLegacyRefreshGeneration)
    ));
    restarted
        .complete_legacy_refresh(receipt, input, output, &successor)
        .expect("atomic successor commit");
    assert!(
        !restarted
            .has_unresolved_legacy_refresh()
            .expect("completed intent")
    );
    assert_eq!(
        restarted
            .load_teslamate_legacy_tokens()
            .expect("successor load")
            .expect("successor")
            .access(),
        successor.access()
    );
}

#[test]
fn explicit_new_credentials_supersede_ambiguous_refresh_without_reusing_input() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let old_generation = Uuid::from_u128(11);
    let initial =
        TeslaMateLegacyTokenStore::imported(b"old-access".to_vec(), b"old-refresh".to_vec())
            .expect("initial pair")
            .with_credential_generation(old_generation)
            .expect("initial generation");
    store
        .replace_teslamate_legacy_tokens(&initial)
        .expect("initial pair persists");
    store
        .begin_legacy_refresh(old_generation)
        .expect("ambiguous intent persists");
    assert!(matches!(
        store.replace_teslamate_legacy_tokens(&initial),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    let same_refresh_reencrypted = TeslaMateLegacyTokenStore::imported(
        b"different-random-access-envelope".to_vec(),
        b"different-random-refresh-envelope".to_vec(),
    )
    .expect("re-encrypted pair")
    .with_credential_generation(old_generation)
    .expect("same plaintext generation");
    assert!(matches!(
        store.replace_teslamate_legacy_tokens(&same_refresh_reencrypted),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));

    let replacement_generation = Uuid::from_u128(12);
    let replacement =
        TeslaMateLegacyTokenStore::imported(b"fresh-access".to_vec(), b"fresh-refresh".to_vec())
            .expect("explicit replacement pair")
            .with_credential_generation(replacement_generation)
            .expect("replacement generation");
    store
        .replace_teslamate_legacy_tokens(&replacement)
        .expect("new credential generation supersedes ambiguity");
    assert!(
        !store
            .has_unresolved_legacy_refresh()
            .expect("old attempt is terminal")
    );
    assert_eq!(
        store
            .load_teslamate_legacy_tokens()
            .expect("replacement load")
            .expect("replacement")
            .credential_generation(),
        Some(replacement_generation)
    );
    let new_receipt = store
        .begin_legacy_refresh(replacement_generation)
        .expect("replacement may refresh");
    store
        .cancel_unsent_legacy_refresh(new_receipt, replacement_generation)
        .expect("pre-send cancellation remains available");
    assert!(matches!(
        store.begin_legacy_refresh(old_generation),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
}

#[test]
fn legacy_refresh_rejects_successor_generation_already_consumed() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let first_generation = Uuid::from_u128(41);
    let first =
        TeslaMateLegacyTokenStore::imported(b"first-access".to_vec(), b"first-refresh".to_vec())
            .expect("first tokens")
            .with_credential_generation(first_generation)
            .expect("first generation");
    store
        .replace_teslamate_legacy_tokens(&first)
        .expect("first tokens persist");

    let first_receipt = store
        .begin_legacy_refresh(first_generation)
        .expect("first refresh begins");
    let second_generation = Uuid::from_u128(42);
    let second = TeslaMateLegacyTokenStore::refreshed(
        b"second-access".to_vec(),
        b"second-refresh".to_vec(),
        2_000,
        1_750,
    )
    .expect("second tokens")
    .with_credential_generation(second_generation)
    .expect("second generation");
    store
        .complete_legacy_refresh(first_receipt, first_generation, second_generation, &second)
        .expect("first refresh completes");

    let second_receipt = store
        .begin_legacy_refresh(second_generation)
        .expect("second refresh begins");
    let consumed_successor = TeslaMateLegacyTokenStore::refreshed(
        b"cycled-access".to_vec(),
        b"cycled-refresh".to_vec(),
        3_000,
        2_750,
    )
    .expect("cycled tokens")
    .with_credential_generation(first_generation)
    .expect("consumed generation");
    assert!(matches!(
        store.complete_legacy_refresh(
            second_receipt,
            second_generation,
            first_generation,
            &consumed_successor,
        ),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    assert_eq!(
        store
            .load_teslamate_legacy_tokens()
            .expect("tokens load")
            .expect("tokens remain")
            .credential_generation(),
        Some(second_generation)
    );
    assert!(
        store
            .has_unresolved_legacy_refresh()
            .expect("rejected successor leaves receipt unresolved")
    );
}

#[test]
fn explicit_replacement_rejects_previously_consumed_generation_after_successor_commit() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let input_generation = Uuid::from_u128(21);
    let input =
        TeslaMateLegacyTokenStore::imported(b"old-access".to_vec(), b"old-refresh".to_vec())
            .expect("input tokens")
            .with_credential_generation(input_generation)
            .expect("input generation");
    store
        .replace_teslamate_legacy_tokens(&input)
        .expect("input persists");
    let receipt = store
        .begin_legacy_refresh(input_generation)
        .expect("refresh begins");

    let output_generation = Uuid::from_u128(22);
    let output = TeslaMateLegacyTokenStore::refreshed(
        b"new-access".to_vec(),
        b"new-refresh".to_vec(),
        2_000,
        1_750,
    )
    .expect("output tokens")
    .with_credential_generation(output_generation)
    .expect("output generation");
    store
        .complete_legacy_refresh(receipt, input_generation, output_generation, &output)
        .expect("refresh completes");

    assert!(matches!(
        store.replace_teslamate_legacy_tokens(&input),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    let current = store
        .load_teslamate_legacy_tokens()
        .expect("current tokens load")
        .expect("current tokens remain");
    assert_eq!(current.credential_generation(), Some(output_generation));
    assert_eq!(current.access(), output.access());
}

#[test]
fn fleet_refresh_intent_fences_restart_and_commits_successor_atomically() {
    let temporary = crate::private_tempdir().expect("temporary database");
    let store = HubStore::initialize(temporary.path()).expect("store initializes");
    let input_generation = Uuid::from_u128(101);
    let input = FleetTokenStore::new(
        b"old-access-envelope".to_vec(),
        b"old-refresh-envelope".to_vec(),
        "owner-client".to_owned(),
        "eu".to_owned(),
        2_000,
        1_750,
        Some(input_generation),
    )
    .expect("input Fleet tokens");
    store
        .replace_fleet_tokens(&input)
        .expect("input Fleet tokens persist");

    assert!(matches!(
        store.begin_fleet_refresh(Uuid::new_v4()),
        Err(StoreError::FleetRefreshOutcomeUnknown)
    ));
    let first_receipt = store
        .begin_fleet_refresh(input_generation)
        .expect("Fleet refresh intent persists");
    assert!(matches!(
        store.complete_outbound_request(
            first_receipt,
            &OutboundRequestCompletion {
                outcome: OutboundRequestOutcome::Cancelled,
                http_status: None,
                retry_after_seconds: None,
            },
        ),
        Err(StoreError::ReservedLegacyRefreshReceipt)
    ));
    drop(store);

    let restarted = HubStore::open_existing(temporary.path()).expect("restart opens");
    assert!(
        restarted
            .has_unresolved_fleet_refresh()
            .expect("restart retains Fleet refresh intent")
    );
    assert!(matches!(
        restarted.begin_fleet_refresh(input_generation),
        Err(StoreError::FleetRefreshOutcomeUnknown)
    ));
    restarted
        .cancel_unsent_fleet_refresh(first_receipt, input_generation)
        .expect("definitely-unsent Fleet refresh cancels");

    let receipt = restarted
        .begin_fleet_refresh(input_generation)
        .expect("Fleet refresh restarts after pre-send cancellation");
    let output_generation = Uuid::from_u128(102);
    let output = FleetTokenStore::new(
        b"new-access-envelope".to_vec(),
        b"new-refresh-envelope".to_vec(),
        "owner-client".to_owned(),
        "eu".to_owned(),
        3_000,
        2_750,
        Some(output_generation),
    )
    .expect("output Fleet tokens");
    restarted
        .complete_fleet_refresh(receipt, input_generation, output_generation, &output)
        .expect("Fleet successor commits atomically");
    assert!(
        !restarted
            .has_unresolved_fleet_refresh()
            .expect("Fleet intent completed")
    );
    let persisted = restarted
        .load_fleet_tokens()
        .expect("Fleet tokens load")
        .expect("Fleet tokens remain");
    assert_eq!(persisted.access(), output.access());
    assert_eq!(persisted.credential_generation(), Some(output_generation));
    assert!(matches!(
        restarted.replace_fleet_tokens(&input),
        Err(StoreError::FleetRefreshOutcomeUnknown)
    ));

    restarted
        .begin_fleet_refresh(output_generation)
        .expect("next Fleet refresh intent persists");
    assert!(matches!(
        restarted.replace_fleet_tokens(&output),
        Err(StoreError::FleetRefreshOutcomeUnknown)
    ));
    let replacement_generation = Uuid::from_u128(103);
    let replacement = FleetTokenStore::new(
        b"operator-access-envelope".to_vec(),
        b"operator-refresh-envelope".to_vec(),
        "owner-client".to_owned(),
        "eu".to_owned(),
        4_000,
        3_750,
        Some(replacement_generation),
    )
    .expect("operator replacement Fleet tokens");
    restarted
        .replace_fleet_tokens(&replacement)
        .expect("different operator credentials recover ambiguity");
    assert!(
        !restarted
            .has_unresolved_fleet_refresh()
            .expect("operator replacement terminalizes old intent")
    );
}
