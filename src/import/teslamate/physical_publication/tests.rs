// SPDX-License-Identifier: AGPL-3.0-only

use serde_json::json;

use super::*;
use crate::{
    db::HubStore,
    teslamate_physical_fragments::tests::{
        registered_admission_binding, seed_roots, seed_updates, stage_limits,
    },
    teslamate_stage::{TeslaMateStageLimits, TeslaMateStageTable},
};

fn physical_stage(root: &std::path::Path, name: &str, updates: &[i32]) -> TeslaMateStage {
    let mut stage = TeslaMateStage::create_physical_v3(root.join(name), stage_limits())
        .expect("physical stage");
    seed_roots(&mut stage);
    seed_updates(&mut stage, updates);
    stage.seal().expect("seal physical stage");
    stage
}

#[tokio::test]
async fn production_caller_publishes_noops_and_rotates_under_one_gate() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let binding = registered_admission_binding(&store);
    let cursor_key = CursorKey::from_bytes([0x51; 32]);

    let first_stage = physical_stage(temporary.path(), "first", &[10]);
    let first_path = first_stage.path().to_path_buf();
    let first = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        first_stage,
        1_000,
    )
    .await
    .expect("first public head");
    assert_eq!(first.kind, PhysicalV3PublicationKind::FirstHead);
    assert_eq!(first.admission.head_sequence, 1);
    assert!(!first_path.exists());

    let unchanged_stage = physical_stage(temporary.path(), "unchanged", &[10]);
    let unchanged_path = unchanged_stage.path().to_path_buf();
    let unchanged = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        unchanged_stage,
        1_001,
    )
    .await
    .expect("unchanged public head");
    assert_eq!(unchanged.kind, PhysicalV3PublicationKind::Unchanged);
    assert_eq!(unchanged.admission, first.admission);
    assert!(!unchanged_path.exists());

    let changed_stage = physical_stage(temporary.path(), "changed", &[10, 11]);
    let changed_path = changed_stage.path().to_path_buf();
    let changed = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        changed_stage,
        2_000,
    )
    .await
    .expect("rotated public head");
    assert_eq!(changed.kind, PhysicalV3PublicationKind::Rotation);
    assert_eq!(changed.admission.head_sequence, 2);
    assert_ne!(changed.admission.snapshot_id, first.admission.snapshot_id);
    let (_, _, delta_json) = store
        .physical_v3_delta_receipt_for_base_at(
            binding.vehicle_id,
            &first.admission.receipt_id,
            &changed.admission.receipt_id,
            2_001,
        )
        .expect("delta catalogue")
        .expect("immediate changed-set receipt");
    let delta: serde_json::Value = serde_json::from_slice(&delta_json).expect("signed delta");
    assert_eq!(delta["kind"], "physical_changed_set");
    assert!(delta["total_rows"].as_u64().expect("typed rows") >= 2);
    assert_eq!(delta["target"]["sequence"], 2);
    let signing = crate::manifest_signing::ManifestSigning::from_cursor_key(&cursor_key);
    assert_eq!(
        delta["base"]["manifest_sha256"],
        Sha256Digest::of_bytes(
            &signing
                .signed_physical_manifest_document(&first.admission)
                .expect("base manifest")
        )
        .to_string()
    );
    let pack_digest: Sha256Digest = delta["chunks"][0]["pack"]["sha256"]
        .as_str()
        .expect("pack digest")
        .parse()
        .expect("valid digest");
    let stored = store
        .physical_v3_delta_pack_for_digest_at(pack_digest, 2_001)
        .expect("delta pack catalogue")
        .expect("published delta pack");
    if let Some(output) = std::env::var_os("TESLATLAS_HUB_SYNTHETIC_DELTA_FIXTURE_DIR") {
        let output = std::path::PathBuf::from(output);
        std::fs::create_dir_all(&output).expect("synthetic fixture output directory");
        std::fs::write(output.join("receipt.json"), &delta_json).expect("synthetic signed receipt");
        std::fs::copy(&stored.path, output.join("pack.sqlite.zst"))
            .expect("synthetic immutable delta pack");
    }
    let decoded = zstd::stream::decode_all(std::fs::File::open(&stored.path).expect("pack file"))
        .expect("decode delta pack");
    let sqlite = temporary.path().join("physical-changed-set.sqlite");
    std::fs::write(&sqlite, &decoded).expect("private synthetic pack");
    let connection =
        rusqlite::Connection::open_with_flags(&sqlite, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("delta SQLite");
    let expected_schema = rusqlite::Connection::open_in_memory().expect("Protocol schema memory");
    expected_schema
        .execute_batch(include_str!("../physical_delta_pack_v1.sql"))
        .expect("frozen Protocol SQL");
    let tables = |db: &rusqlite::Connection| -> Vec<(String, String)> {
        db.prepare("SELECT name,sql FROM sqlite_schema WHERE type='table' ORDER BY name")
            .expect("schema query")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("schema rows")
            .collect::<Result<_, _>>()
            .expect("schema values")
    };
    assert_eq!(tables(&connection), tables(&expected_schema));
    let changed_update: (i64, String) = connection
        .query_row(
            "SELECT entity_id,role FROM row_roles WHERE table_name='updates' AND entity_id=11",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("changed update");
    assert_eq!(changed_update, (11, "changed".to_owned()));
    let third = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        physical_stage(temporary.path(), "third-reverts-to-first", &[10]),
        3_000,
    )
    .await
    .expect("third public head");
    assert_eq!(third.kind, PhysicalV3PublicationKind::Rotation);
    assert_eq!(third.admission.head_sequence, 3);
    assert_eq!(third.admission.snapshot_id, first.admission.snapshot_id);
    assert_ne!(third.admission.receipt_id, first.admission.receipt_id);
    let fourth = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        physical_stage(temporary.path(), "fourth", &[10, 11, 12]),
        4_000,
    )
    .await
    .expect("fourth public head");
    assert_eq!(fourth.kind, PhysicalV3PublicationKind::Rotation);
    assert_eq!(fourth.admission.head_sequence, 4);
    for prior in [&first.admission, &changed.admission, &third.admission] {
        assert_eq!(
            store
                .retained_physical_v3_admission_for_receipt_at(
                    binding.vehicle_id,
                    &prior.receipt_id,
                    4_001,
                    true,
                )
                .expect("retained lookup")
                .expect("unexpired prior")
                .admission,
            *prior
        );
    }
    assert_eq!(
        store
            .pending_physical_v3_control_admission_for_vehicle(binding.vehicle_id)
            .expect("public control head")
            .expect("rotated head"),
        fourth.admission
    );
    assert!(!changed_path.exists());
}

#[tokio::test]
async fn physical_delta_pack_is_backed_up_and_expires_with_its_retained_base() {
    let temporary = crate::private_tempdir().expect("private store root");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let binding = registered_admission_binding(&store);
    let key = CursorKey::from_bytes([0x52; 32]);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("wall clock")
            .as_millis(),
    )
    .expect("bounded clock");
    let first = publish_sealed_physical_v3_stage_at(
        &store,
        &key,
        binding.clone(),
        physical_stage(temporary.path(), "backup-first", &[10]),
        now,
    )
    .await
    .expect("first head");
    let second = publish_sealed_physical_v3_stage_at(
        &store,
        &key,
        binding.clone(),
        physical_stage(temporary.path(), "backup-second", &[10, 11]),
        now + 1,
    )
    .await
    .expect("changed head");
    let (_, _, receipt) = store
        .physical_v3_delta_receipt_for_base_at(
            binding.vehicle_id,
            &first.admission.receipt_id,
            &second.admission.receipt_id,
            now + 2,
        )
        .expect("delta lookup")
        .expect("active changed-set receipt");
    let receipt: serde_json::Value = serde_json::from_slice(&receipt).expect("signed receipt");
    let digest: Sha256Digest = receipt["chunks"][0]["pack"]["sha256"]
        .as_str()
        .expect("pack digest")
        .parse()
        .expect("valid digest");
    let original_pack = store
        .physical_v3_delta_pack_for_digest_at(digest, now + 2)
        .expect("pack lookup")
        .expect("current pack");
    let backup = temporary.path().join("backup");
    store
        .backup_to(&backup)
        .expect("referenced delta pack backup");
    let restored = HubStore::initialize(&backup).expect("restore complete backup");
    restored.quick_check().expect("restored integrity");
    let restored_pack = restored
        .physical_v3_delta_pack_for_digest_at(digest, now + 2)
        .expect("restored pack lookup")
        .expect("restored current pack");
    assert_eq!(
        Sha256Digest::of_bytes(&std::fs::read(&original_pack.path).expect("original bytes")),
        Sha256Digest::of_bytes(&std::fs::read(&restored_pack.path).expect("backup bytes"))
    );

    // Expiration removes the base checkpoint, transition and pack reference
    // together; repair may then collect the now-orphaned immutable object.
    let expired = now - crate::db::RETIRED_LINEAGE_PACK_RETENTION_MS;
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE retained_physical_v3_admissions
             SET retained_at_ms=?1, expires_at_ms=?2 WHERE receipt_id=?3",
            rusqlite::params![expired - 1, expired, first.admission.receipt_id],
        )
        .expect("expire synthetic retained base");
    store.repair().expect("expired delta repair");
    assert!(
        store
            .physical_v3_delta_pack_for_digest_at(digest, now + 2)
            .expect("expired pack lookup")
            .is_none()
    );
    assert!(
        !original_pack.path.exists(),
        "expired delta object collected"
    );
    let count: i64 = store
        .open()
        .expect("catalogue after repair")
        .query_row(
            "SELECT COUNT(*) FROM physical_v3_delta_transitions",
            [],
            |row| row.get(0),
        )
        .expect("transition count");
    assert_eq!(count, 0);
}

#[test]
fn physical_delta_multiple_packs_are_all_referenced_and_backed_up() {
    use crate::import::teslamate::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence;

    let temporary = crate::private_tempdir().expect("private store root");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let key = CursorKey::from_bytes([0x53; 32]);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("wall clock")
            .as_millis(),
    )
    .expect("bounded clock");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let (binding, first_candidate) = public_admission_candidate_fixture_for_source_and_sequence(
        &temporary.path().join("first-candidate"),
        &store,
        &key,
        1,
        "multi-pack",
        1,
    );
    let first = store
        .stage_pending_physical_v3_admission(&gate, first_candidate)
        .expect("first public head");
    let (same_binding, target_candidate) =
        public_admission_candidate_fixture_for_source_and_sequence(
            &temporary.path().join("target-candidate"),
            &store,
            &key,
            384,
            "multi-pack",
            2,
        );
    assert_eq!(same_binding, binding);
    let preview = crate::db::physical_v3_admission_from_manifest(
        &target_candidate.manifest,
        binding.selected_car_id,
    )
    .expect("target preview");
    let delta =
        crate::import::teslamate::physical_delta_pack::prepare_changed_set_with_chunk_target(
            &first,
            &preview,
            &binding,
            &key,
            Sha256Digest::of_bytes(b"synthetic target raw"),
            store.packs_dir(),
            0,
            4096,
        )
        .expect("bounded multi-pack change");
    assert!(delta.packs.len() > 1);
    let digests = delta
        .packs
        .iter()
        .map(|pack| pack.sha256)
        .collect::<Vec<_>>();
    let target = store
        .rotate_pending_physical_v3_admission_with_delta_at(
            &gate,
            target_candidate,
            Some(delta),
            now,
        )
        .expect("atomic multi-pack rotation");
    store
        .activate_pending_physical_v3_rotation_at(&gate, binding.vehicle_id, &first.receipt_id, now)
        .expect("target public");
    drop(gate);
    assert_eq!(target.head_sequence, 2);
    let backup = temporary.path().join("backup");
    store
        .backup_to(&backup)
        .expect("all referenced packs copied");
    let restored = HubStore::initialize(&backup).expect("restored multi-pack store");
    restored.quick_check().expect("restored integrity");
    for digest in digests {
        let source = store
            .physical_v3_delta_pack_for_digest_at(digest, now + 1)
            .expect("source pack lookup")
            .expect("source pack");
        let copied = restored
            .physical_v3_delta_pack_for_digest_at(digest, now + 1)
            .expect("restored pack lookup")
            .expect("restored pack");
        assert_eq!(
            Sha256Digest::of_bytes(&std::fs::read(source.path).expect("source bytes")),
            Sha256Digest::of_bytes(&std::fs::read(copied.path).expect("restored bytes"))
        );
    }
}

#[tokio::test]
async fn retry_finishes_the_same_rotation_after_its_catalogue_commit() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let binding = registered_admission_binding(&store);
    let cursor_key = CursorKey::from_bytes([0x52; 32]);
    let first = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        physical_stage(temporary.path(), "first", &[10]),
        1_000,
    )
    .await
    .expect("first public head");

    let retry_stage = physical_stage(temporary.path(), "retry", &[10, 11]);
    let snapshot_id = physical_v3_snapshot_id(
        retry_stage
            .sealed_content_digest()
            .expect("sealed stage digest"),
        &binding,
    );
    let gate = store
        .acquire_publication_gate()
        .await
        .expect("publication gate");
    let candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        &retry_stage,
        &ProjectionPackWriter::with_limits(
            store.packs_dir(),
            ProtocolLimits::hub_sync_v1_1_3_schema_2_2(),
        ),
        binding.clone(),
        snapshot_id,
        SequenceRange {
            from_exclusive: 2,
            to_inclusive: 2,
        },
        &cursor_key,
        TeslaMatePhysicalFragmentLimits::default(),
    )
    .expect("rotation candidate");
    let blocked = store
        .rotate_pending_physical_v3_admission_at(&gate, candidate, 2_000)
        .expect("committed blocked rotation");
    assert_eq!(blocked.snapshot_id, snapshot_id);
    drop(gate);

    let resumed = publish_sealed_physical_v3_stage_at(
        &store,
        &cursor_key,
        binding.clone(),
        retry_stage,
        2_001,
    )
    .await
    .expect("resume blocked rotation");
    assert_eq!(resumed.kind, PhysicalV3PublicationKind::ResumedRotation);
    assert_eq!(resumed.admission, blocked);
    assert_eq!(
        store
            .retained_physical_v3_admission_for_receipt_at(
                binding.vehicle_id,
                &first.admission.receipt_id,
                2_001,
                true,
            )
            .expect("retained receipt")
            .expect("retained prior")
            .admission,
        first.admission
    );

    let third_stage = physical_stage(temporary.path(), "third", &[10, 11, 12]);
    let third_snapshot_id = physical_v3_snapshot_id(
        third_stage
            .sealed_content_digest()
            .expect("third stage digest"),
        &binding,
    );
    let gate = store
        .acquire_publication_gate()
        .await
        .expect("third publication gate");
    let third_candidate = write_staged_physical_updates_snapshot_v3_with_limits(
        &third_stage,
        &ProjectionPackWriter::with_limits(
            store.packs_dir(),
            ProtocolLimits::hub_sync_v1_1_3_schema_2_2(),
        ),
        binding.clone(),
        third_snapshot_id,
        SequenceRange {
            from_exclusive: 3,
            to_inclusive: 3,
        },
        &cursor_key,
        TeslaMatePhysicalFragmentLimits::default(),
    )
    .expect("third candidate");
    let blocked_third = store
        .rotate_pending_physical_v3_admission_at(&gate, third_candidate, 3_000)
        .expect("third blocked head");
    assert!(matches!(
        store.activate_pending_physical_v3_rotation_at(
            &gate,
            binding.vehicle_id,
            &first.admission.receipt_id,
            3_001,
        ),
        Err(StoreError::PhysicalV3AdmissionConflict)
    ));
    drop(gate);
    drop(store);

    let reopened = HubStore::initialize(temporary.path().join("hub")).expect("restart Hub");
    let resumed_third = publish_sealed_physical_v3_stage_at(
        &reopened,
        &cursor_key,
        binding.clone(),
        third_stage,
        3_001,
    )
    .await
    .expect("resume third rotation after restart");
    assert_eq!(
        resumed_third.kind,
        PhysicalV3PublicationKind::ResumedRotation
    );
    assert_eq!(resumed_third.admission, blocked_third);
    for prior in [&first.admission, &resumed.admission] {
        assert!(
            reopened
                .retained_physical_v3_admission_for_receipt_at(
                    binding.vehicle_id,
                    &prior.receipt_id,
                    3_001,
                    true,
                )
                .expect("retained lookup after restart")
                .is_some()
        );
    }
}

#[tokio::test]
async fn caller_rejects_legacy_incomplete_and_foreign_stages_and_cleans_them() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = HubStore::initialize(temporary.path().join("hub")).expect("store");
    let binding = registered_admission_binding(&store);
    let cursor_key = CursorKey::from_bytes([0x53; 32]);

    let mut legacy = TeslaMateStage::create(
        temporary.path().join("legacy"),
        TeslaMateStageLimits {
            max_rows: 8,
            max_stage_bytes: 512 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("legacy stage");
    legacy.seal().expect("seal legacy stage");
    let legacy_path = legacy.path().to_path_buf();
    assert!(matches!(
        publish_sealed_physical_v3_stage_at(&store, &cursor_key, binding.clone(), legacy, 1_000,)
            .await,
        Err(TeslaMatePhysicalPublicationError::WrongStageFormat)
    ));
    assert!(!legacy_path.exists());

    let mut incomplete =
        TeslaMateStage::create_physical_v3(temporary.path().join("incomplete"), stage_limits())
            .expect("incomplete stage");
    incomplete.seal().expect("seal incomplete stage");
    let incomplete_path = incomplete.path().to_path_buf();
    assert!(matches!(
        publish_sealed_physical_v3_stage_at(
            &store,
            &cursor_key,
            binding.clone(),
            incomplete,
            1_000,
        )
        .await,
        Err(TeslaMatePhysicalPublicationError::Fragment(
            TeslaMatePhysicalFragmentError::RootCardinality { .. }
        ))
    ));
    assert!(!incomplete_path.exists());

    let foreign = physical_stage(temporary.path(), "foreign", &[10]);
    let foreign_path = foreign.path().to_path_buf();
    let mut foreign_binding = binding;
    foreign_binding.installation_id = Uuid::from_u128(0xdeadbeef_dead_4eef_8ead_deadbeefdead);
    assert!(matches!(
        publish_sealed_physical_v3_stage_at(&store, &cursor_key, foreign_binding, foreign, 1_000,)
            .await,
        Err(TeslaMatePhysicalPublicationError::Store(
            StoreError::PhysicalV3AdmissionInvalid
        ))
    ));
    assert!(!foreign_path.exists());
    let pack_count = std::fs::read_dir(store.packs_dir().join("sha256"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(pack_count, 0);
}

#[test]
fn sealed_digest_covers_every_physical_table_and_is_order_independent() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let all = TeslaMateStageTable::PHYSICAL_V3_ALL;
    let make_stage = |name: &str, omitted: Option<TeslaMateStageTable>, reverse: bool| {
        let mut stage = TeslaMateStage::create_physical_v3(
            temporary.path().join(name),
            TeslaMateStageLimits {
                max_rows: 32,
                max_stage_bytes: 512 * 1024,
                minimum_free_bytes: 0,
            },
        )
        .expect("digest stage");
        let rows: Box<dyn Iterator<Item = TeslaMateStageTable>> = if reverse {
            Box::new(all.into_iter().rev())
        } else {
            Box::new(all.into_iter())
        };
        for table in rows {
            if Some(table) == omitted {
                continue;
            }
            let index = all
                .iter()
                .position(|candidate| *candidate == table)
                .expect("known physical table");
            stage
                .insert(
                    table,
                    i64::try_from(index + 1).expect("bounded source id"),
                    &json!({"table": table.as_str(), "value": index}),
                )
                .expect("digest row");
        }
        stage.seal().expect("seal digest stage");
        stage
    };

    let complete = make_stage("complete", None, false);
    let complete_digest = complete.sealed_content_digest().expect("complete digest");
    let reverse = make_stage("reverse", None, true);
    assert_eq!(
        complete_digest,
        reverse.sealed_content_digest().expect("reverse digest")
    );
    for (index, table) in all.into_iter().enumerate() {
        let omitted = make_stage(&format!("omitted-{index}"), Some(table), false);
        assert_ne!(
            complete_digest,
            omitted.sealed_content_digest().expect("omitted digest"),
            "{} must contribute to the digest",
            table.as_str()
        );
    }
}
