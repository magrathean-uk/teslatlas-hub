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
    assert_eq!(
        store
            .pending_physical_v3_control_admission_for_vehicle(binding.vehicle_id)
            .expect("public control head")
            .expect("rotated head"),
        changed.admission
    );
    assert!(!changed_path.exists());
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
