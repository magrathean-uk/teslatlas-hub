// SPDX-License-Identifier: AGPL-3.0-only

use super::tests::month_stage;
use super::*;
use crate::{
    physical_publication::publish_sealed_physical_v3_stage,
    sync::physical_fragments::tests::{registered_admission_binding, seed_roots, stage_limits},
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::Value;
use std::collections::BTreeMap;
use tower::ServiceExt;

async fn request(
    app: &Router,
    bearer: &str,
    profile: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(crate::server::SYNC_PROFILE_HEADER, profile)
        .header(crate::server::SUPPORTED_SCHEMAS_HEADER, "2.1,2.2");
    let body = match body {
        Some(value) => {
            builder = builder
                .method("POST")
                .header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    (
        response.status(),
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}

fn checkpoint(key: &CursorKey, head: &PendingPhysicalV3Admission) -> Value {
    serde_json::from_slice(
        &ManifestSigning::from_cursor_key(key)
            .signed_physical_manifest_document(head)
            .unwrap(),
    )
    .unwrap()
}

fn changes_request(key: &CursorKey, head: &PendingPhysicalV3Admission, physical: bool) -> Value {
    let mut body = json!({
        "base_receipt_id": head.receipt_id,
        "base_manifest_schema": "2.2", "from_sequence": head.head_sequence,
        "schema_version_range": {"minimum": "2.2", "maximum": "2.2"},
    });
    if physical {
        body["base_manifest_id"] = json!(head.snapshot_id.to_string());
        body["base_manifest_sha256"] = json!(
            Sha256Digest::of_bytes(
                &ManifestSigning::from_cursor_key(key)
                    .signed_physical_manifest_document(head)
                    .unwrap()
            )
            .to_string()
        );
        body["source"] = json!({
            "installation_id": head.installation_id, "account_id": head.account_id,
            "vehicle_id": head.vehicle_id, "generation": head.manifest.generation,
            "selected_car_id": head.selected_car_id,
        });
        body["accepted_changed_set_formats"] = json!(["teslatlas-physical-v3-delta-v1"]);
    }
    body
}

async fn prepared(
    app: &Router,
    bearer: &str,
    profile: &str,
    head: &PendingPhysicalV3Admission,
    month: &Month,
) -> Value {
    let id = artifact_id(head, &month.name, month.from_ms, month.to_ms);
    let (status, raw) = request(
        app,
        bearer,
        profile,
        &format!(
            "/v1/vehicles/{}/sync/prepared-artefacts/{id}",
            head.vehicle_id
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_slice(&raw).unwrap()
}

async fn assert_withheld(app: &Router, bearer: &str, profile: &str, receipt: &Value) {
    for uri in [
        format!(
            "/v1/vehicles/{}/sync/prepared-artefacts/{}",
            receipt["vehicle_id"].as_str().unwrap(),
            receipt["artifact_id"].as_str().unwrap()
        ),
        format!(
            "/v1/packs/sha256/{}.sqlite.zst",
            receipt["pack"]["sha256"].as_str().unwrap()
        ),
    ] {
        assert_eq!(
            request(app, bearer, profile, &uri, None).await.0,
            StatusCode::NOT_FOUND
        );
    }
}

// A small reference consumer of the actual served SQLite bytes. Matching
// is unavailable while a candidate is being applied. Activation includes
// raw edits, all prepared invalidations and the new checkpoint together.
#[derive(Default)]
struct Client {
    checkpoint: Value,
    generation: u64,
    positions: BTreeMap<i64, i64>,
    prepared: Vec<Value>,
    applying: bool,
    events: Vec<&'static str>,
}

impl Client {
    fn matches(&mut self, receipt: &Value) -> bool {
        if self.applying {
            return false;
        }
        self.events.push("match");
        receipt["source"]["input_manifest_id"] == self.checkpoint["manifest_id"]
            && receipt["source"]["input_receipt_id"] == self.checkpoint["receipt_id"]
            && receipt["source"]["input_sequence"] == self.checkpoint["sequence"]
            && self.prepared.iter().any(|entry| entry == receipt)
    }

    async fn apply(
        &mut self,
        app: &Router,
        bearer: &str,
        profile: &str,
        root: &Path,
        target: &Value,
        generation: u64,
        replacement: bool,
        affected: &[Value],
    ) {
        self.applying = true;
        self.events.clear();
        let mut positions = if replacement {
            BTreeMap::new()
        } else {
            self.positions.clone()
        };
        for chunk in target["chunks"].as_array().unwrap() {
            let digest = chunk["pack"]["sha256"].as_str().unwrap();
            let (status, compressed) = request(
                app,
                bearer,
                profile,
                &format!("/v1/packs/sha256/{digest}.sqlite.zst"),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(Sha256Digest::of_bytes(&compressed).to_string(), digest);
            let raw = zstd::stream::decode_all(compressed.as_slice()).unwrap();
            let image = NamedTempFile::new_in(root).unwrap();
            fs::write(image.path(), raw).unwrap();
            let db = Connection::open(image.path()).unwrap();
            if replacement {
                let pack_generation: String = db
                    .query_row(
                        "SELECT value FROM hub_pack_metadata WHERE key = 'generation'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(pack_generation.parse::<u64>().unwrap(), generation);
            }
            let mut rows = db.prepare("SELECT id, latitude_e6 FROM positions").unwrap();
            for row in rows
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
                .unwrap()
            {
                let (id, latitude) = row.unwrap();
                positions.insert(id, latitude);
            }
            if !replacement {
                let mut deletes = db
                    .prepare("SELECT entity_id FROM tombstones WHERE table_name = 'positions'")
                    .unwrap();
                for id in deletes.query_map([], |r| r.get::<_, i64>(0)).unwrap() {
                    positions.remove(&id.unwrap());
                }
            }
        }
        self.events.push("data");
        for old in self.prepared.clone() {
            assert!(
                !self.matches(&old),
                "no matching between data and invalidation apply"
            );
        }
        // affected_months invalidates local month results; exact source
        // identity additionally rejects every preparation of an old head.
        self.prepared.retain(|entry| {
            generation == self.generation
                && entry["source"]["input_manifest_id"] == target["manifest_id"]
                && entry["source"]["input_receipt_id"] == target["receipt_id"]
                && entry["source"]["input_sequence"] == target["sequence"]
                && !affected.iter().any(|month| {
                    entry["dirty_spans"]["map_months"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|span| &span["month"] == month)
                })
        });
        self.events.push("invalidations");
        self.positions = positions;
        self.generation = generation;
        self.checkpoint = target.clone();
        self.applying = false;
    }
}

#[tokio::test]
// Regression guard for the existing exact-head binding and apply order.
async fn correction_and_deletion_apply_before_match_on_both_served_profiles() {
    for physical in [false, true] {
        let profile = if physical {
            "hub-sync-v1@1.4.0"
        } else {
            "hub-sync-v1@1.3.0"
        };
        let temporary = crate::private_tempdir().unwrap();
        let store = HubStore::initialize(temporary.path().join("hub")).unwrap();
        let binding = registered_admission_binding(&store);
        let key = CursorKey::from_bytes([0x59; 32]);
        let now = OffsetDateTime::now_utc().unix_timestamp() * 1_000;
        let month = previous_complete_month(now).unwrap().unwrap();
        let mut head = publish_sealed_physical_v3_stage(
            &store,
            &key,
            binding.clone(),
            month_stage(temporary.path(), &month, false, false),
        )
        .await
        .unwrap()
        .admission;
        let invitation = store
            .create_pairing("invalidation fixture", 0, i64::MAX)
            .unwrap();
        let access = store
            .claim_pairing(
                invitation.pairing_id,
                invitation.secret(),
                "reference client",
                now,
            )
            .unwrap();
        let bearer = access.access_token.as_bearer();
        let app = crate::server::paired_router(store.clone(), &key);
        let mut client = Client::default();
        client
            .apply(
                &app,
                bearer,
                profile,
                temporary.path(),
                &checkpoint(&key, &head),
                binding.generation,
                true,
                &[],
            )
            .await;
        assert_eq!(client.positions[&41], 51_001_000);

        for deletion in [false, true] {
            // More than one prepared result must be invalidated. A second
            // month also proves invalidation does not depend on which
            // optional month the successor happens to prepare.
            let stage = month_stage(temporary.path(), &month, deletion, false);
            let earlier = previous_complete_month(month.from_ms).unwrap().unwrap();
            publish_selected_month(&store, &key, &stage, &head, month.from_ms).unwrap();
            stage.discard().unwrap();
            let old = vec![
                prepared(&app, bearer, profile, &head, &month).await,
                prepared(&app, bearer, profile, &head, &earlier).await,
            ];
            client.prepared = old.clone();
            assert!(client.matches(&old[0]));
            assert!(client.matches(&old[1]));
            let stage = if deletion {
                let mut stage = SourceDbStage::create_physical_v3(
                    temporary.path().join("deleted-stage"),
                    stage_limits(),
                )
                .unwrap();
                seed_roots(&mut stage);
                stage.seal().unwrap();
                stage
            } else {
                month_stage(temporary.path(), &month, true, false)
            };
            let next = publish_sealed_physical_v3_stage(&store, &key, binding.clone(), stage)
                .await
                .unwrap()
                .admission;
            for receipt in &old {
                assert_withheld(&app, bearer, profile, receipt).await;
            }
            let (status, bytes) = request(
                &app,
                bearer,
                profile,
                &format!("/v1/vehicles/{}/sync/changes-since", binding.vehicle_id),
                Some(changes_request(&key, &head, physical)),
            )
            .await;
            let wire: Value = serde_json::from_slice(&bytes).unwrap();
            let mut target = checkpoint(&key, &next);
            let affected = if physical {
                assert_eq!(status, StatusCode::OK);
                assert_eq!(wire["kind"], "physical_changed_set");
                assert_eq!(wire["affected_months"], json!([month.name]));
                assert_eq!(wire["source"]["generation"], binding.generation);
                assert_eq!(wire["target"]["receipt_id"], next.receipt_id);
                target["chunks"] = wire["chunks"].clone();
                wire["affected_months"].as_array().unwrap().clone()
            } else {
                assert_eq!(status, StatusCode::CONFLICT);
                assert_eq!(wire["kind"], "rebase_required");
                assert_eq!(wire["replacement"]["receipt_id"], next.receipt_id);
                assert_eq!(wire["replacement"]["chunks"], target["chunks"]);
                Vec::new()
            };
            client
                .apply(
                    &app,
                    bearer,
                    profile,
                    temporary.path(),
                    &target,
                    binding.generation,
                    !physical,
                    &affected,
                )
                .await;
            assert!(client.prepared.is_empty());
            for receipt in &old {
                assert!(!client.matches(receipt));
            }
            assert_eq!(&client.events[..3], &["data", "invalidations", "match"]);
            if deletion {
                assert!(client.positions.is_empty());
            } else {
                assert_eq!(client.positions[&41], 51_002_000);
            }
            let fresh = prepared(&app, bearer, profile, &next, &month).await;
            if deletion {
                assert_eq!(
                    fresh["dirty_spans"]["map_months"][0]["resolution"],
                    "readyEmpty"
                );
            }
            client.prepared.push(fresh.clone());
            assert!(client.matches(&fresh));
            head = next;
        }
    }
}

#[tokio::test]
async fn blocked_rotation_returns_404_for_prepared_receipts_on_both_served_profiles() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path().join("hub")).unwrap();
    let binding = registered_admission_binding(&store);
    let key = CursorKey::from_bytes([0x5c; 32]);
    let now = OffsetDateTime::now_utc().unix_timestamp() * 1_000;
    let month = previous_complete_month(now).unwrap().unwrap();
    let head = publish_sealed_physical_v3_stage(
        &store,
        &key,
        binding.clone(),
        month_stage(temporary.path(), &month, false, false),
    )
    .await
    .unwrap()
    .admission;
    let invitation = store
        .create_pairing("blocked rotation fixture", 0, i64::MAX)
        .unwrap();
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "reference client",
            now,
        )
        .unwrap();
    let bearer = access.access_token.as_bearer();
    let app = crate::server::paired_router(store.clone(), &key);
    let receipt = prepared(&app, bearer, "hub-sync-v1@1.3.0", &head, &month).await;
    assert_eq!(
        prepared(&app, bearer, "hub-sync-v1@1.4.0", &head, &month).await,
        receipt
    );
    assert_eq!(
        store
            .open()
            .unwrap()
            .execute(
                "UPDATE pending_physical_v3_admissions SET serve_state = 'blocked_rotation'
                 WHERE vehicle_id = ?1 AND serve_state = 'public_first'",
                [binding.vehicle_id.to_string()],
            )
            .unwrap(),
        1
    );
    assert!(matches!(
        store.pending_physical_v3_control_admission_for_vehicle(binding.vehicle_id),
        Err(StoreError::PhysicalV3SecondHeadUnsupported(vehicle_id))
            if vehicle_id == binding.vehicle_id
    ));
    for profile in ["hub-sync-v1@1.3.0", "hub-sync-v1@1.4.0"] {
        assert_withheld(&app, bearer, profile, &receipt).await;
    }
}

#[tokio::test]
async fn restore_generation_overrides_a_pinned_lineage_binding() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path().join("hub")).unwrap();
    let binding = registered_admission_binding(&store);
    let key = CursorKey::from_bytes([0x5b; 32]);
    let now = OffsetDateTime::now_utc().unix_timestamp() * 1_000;
    let month = previous_complete_month(now).unwrap().unwrap();
    let head = publish_sealed_physical_v3_stage(
        &store,
        &key,
        binding.clone(),
        month_stage(temporary.path(), &month, false, false),
    )
    .await
    .unwrap()
    .admission;
    let id = artifact_id(&head, &month.name, month.from_ms, month.to_ms);
    let receipt = current_receipt(&store, &key, binding.vehicle_id, &id)
        .unwrap()
        .unwrap();
    let receipt: Value = serde_json::from_slice(&receipt).unwrap();
    let digest = receipt["pack"]["sha256"].as_str().unwrap().parse().unwrap();
    let connection = store.open().unwrap();
    // Only the immutable binding is needed here; no lineage pack is served.
    connection
        .execute(
            "INSERT INTO sync_bases VALUES (?1, ?2, 1, ?3, ?4)",
            params![
                binding.vehicle_id.to_string(),
                head.snapshot_id.to_string(),
                "0".repeat(64),
                b"[]".as_slice()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO v2_base_bindings VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                binding.vehicle_id.to_string(),
                head.snapshot_id.to_string(),
                binding.installation_id.to_string(),
                binding.account_id.to_string(),
                binding.generation as i64,
                binding.selected_car_id
            ],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE sources SET generation = generation + 1 WHERE source_id = ?1",
            [binding.account_id.to_string()],
        )
        .unwrap();
    assert_eq!(
        store.v2_projection_binding(binding.vehicle_id).unwrap(),
        binding
    );
    assert!(
        store
            .pending_physical_v3_control_admission_for_vehicle(binding.vehicle_id)
            .unwrap()
            .is_some()
    );
    assert!(
        current_receipt(&store, &key, binding.vehicle_id, &id)
            .unwrap()
            .is_none()
    );
    assert!(
        current_pack(&store, &key, digest, VehicleScope::Unrestricted)
            .unwrap()
            .is_none()
    );
    assert!(!current_pack_candidate_exists(&store, digest, VehicleScope::Unrestricted).unwrap());
}

#[tokio::test]
async fn restore_generation_invalidates_all_prepared_receipts_and_packs() {
    let temporary = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(temporary.path().join("hub")).unwrap();
    let binding = registered_admission_binding(&store);
    let key = CursorKey::from_bytes([0x5a; 32]);
    let now = OffsetDateTime::now_utc().unix_timestamp() * 1_000;
    let month = previous_complete_month(now).unwrap().unwrap();
    let head = publish_sealed_physical_v3_stage(
        &store,
        &key,
        binding.clone(),
        month_stage(temporary.path(), &month, false, false),
    )
    .await
    .unwrap()
    .admission;
    let stage = month_stage(temporary.path(), &month, false, false);
    let earlier = previous_complete_month(month.from_ms).unwrap().unwrap();
    publish_selected_month(&store, &key, &stage, &head, month.from_ms).unwrap();
    let invitation = store
        .create_pairing("restore fixture", 0, i64::MAX)
        .unwrap();
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "restore client",
            now,
        )
        .unwrap();
    let app = crate::server::paired_router(store.clone(), &key);
    let profile = "hub-sync-v1@1.4.0";
    let old = vec![
        prepared(
            &app,
            access.access_token.as_bearer(),
            profile,
            &head,
            &month,
        )
        .await,
        prepared(
            &app,
            access.access_token.as_bearer(),
            profile,
            &head,
            &earlier,
        )
        .await,
    ];
    let mut client = Client {
        checkpoint: checkpoint(&key, &head),
        generation: binding.generation,
        prepared: old.clone(),
        ..Client::default()
    };
    assert!(client.matches(&old[0]));
    store.open().unwrap().execute(
        "UPDATE sources SET generation = generation + 1 WHERE source_id = (SELECT source_id FROM vehicles WHERE vehicle_id = ?1)",
        [binding.vehicle_id.to_string()],
    ).unwrap();
    // Head and receipt IDs have not changed. The binding alone must
    // close every delivery path, including the no-key pack probe.
    for profile in ["hub-sync-v1@1.3.0", "hub-sync-v1@1.4.0"] {
        for receipt in &old {
            assert_withheld(&app, access.access_token.as_bearer(), profile, receipt).await;
            let digest = receipt["pack"]["sha256"].as_str().unwrap().parse().unwrap();
            assert!(
                !current_pack_candidate_exists(&store, digest, VehicleScope::Unrestricted).unwrap()
            );
        }
    }
    assert!(
        publish_selected_month(&store, &key, &stage, &head, now).is_err(),
        "same-head retry cannot revive a restored preparation"
    );
    stage.discard().unwrap();
    // Represent the completed recovery by admitting a fresh physical head in
    // the new generation. Recovery orchestration is outside this sync test.
    store
        .open()
        .unwrap()
        .execute(
            "DELETE FROM pending_physical_v3_admissions WHERE vehicle_id = ?1",
            [binding.vehicle_id.to_string()],
        )
        .unwrap();
    let restored_binding = store.v2_projection_binding(binding.vehicle_id).unwrap();
    assert_eq!(restored_binding.generation, binding.generation + 1);
    let recovered = publish_sealed_physical_v3_stage(
        &store,
        &key,
        restored_binding.clone(),
        month_stage(temporary.path(), &month, false, false),
    )
    .await
    .unwrap()
    .admission;
    let (status, raw) = request(
        &app,
        access.access_token.as_bearer(),
        profile,
        &format!("/v1/vehicles/{}/sync/manifest", binding.vehicle_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let target: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(target, checkpoint(&key, &recovered));
    client
        .apply(
            &app,
            access.access_token.as_bearer(),
            profile,
            temporary.path(),
            &target,
            restored_binding.generation,
            true,
            &[],
        )
        .await;
    assert!(client.prepared.is_empty());
    for receipt in &old {
        assert!(!client.matches(receipt));
    }
    assert_eq!(&client.events[..3], &["data", "invalidations", "match"]);
    assert_eq!(client.positions[&41], 51_001_000);
    let fresh = prepared(
        &app,
        access.access_token.as_bearer(),
        profile,
        &recovered,
        &month,
    )
    .await;
    client.prepared.push(fresh.clone());
    assert!(client.matches(&fresh));
}
