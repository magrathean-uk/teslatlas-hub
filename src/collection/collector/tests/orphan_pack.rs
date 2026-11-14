// SPDX-License-Identifier: AGPL-3.0-only

//! M04-T06: a full-snapshot publication that fails after its pack write removes the
//! pack it created, under the publication gate, unless a catalogue row (or an earlier
//! publication of the same digest) retains it; the original error is returned.
//! M04-T07: the same for the v2 delta and compaction packs.

use super::*;
use crate::durability_fault::{DurabilityFaultPoint, inject};
use crate::hub_pack::{
    ProjectionBinding, ProjectionPackOwnership, ProjectionPackRequest, ProjectionPackWriter,
    ProjectionSnapshot,
};

const COLLECTED_AT_MS: i64 = 1_800_000_000_000;

fn orphan_pack_collection(source_vehicle_id: u64, vin: &str) -> ManualCollection {
    ManualCollection {
        vehicles: vec![Vehicle::for_test(source_vehicle_id, vin, "online")],
        snapshots: vec![VehicleData::for_test(
            source_vehicle_id,
            json!({
                "drive_state": {"timestamp": COLLECTED_AT_MS - 1},
                "vehicle_config": {"car_type": "model3"},
                "vehicle_state": {"car_version": "2026.20"}
            }),
        )],
        failures: vec![],
    }
}

fn orphan_pack_publish(
    store: &HubStore,
    collection: &ManualCollection,
) -> Result<usize, CollectorError> {
    persist_collection(store, collection, COLLECTED_AT_MS).expect("raw observation");
    materialise_lifecycle_for_collection(store, collection, COLLECTED_AT_MS).expect("lifecycle");
    let publication_gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    publish_compatibility_snapshots(
        store,
        &publication_gate,
        &CursorKey::from_bytes([7; 32]),
        collection,
        COLLECTED_AT_MS,
    )
}

/// Every content-addressed pack file, with whether a catalogue row retains it.
fn orphan_pack_files(store: &HubStore) -> Vec<(String, bool)> {
    let directory = store.packs_dir().join("sha256");
    let mut files: Vec<(String, bool)> = std::fs::read_dir(&directory)
        .expect("pack directory")
        .map(|entry| entry.expect("pack entry").file_name())
        .filter_map(|name| {
            name.to_str()
                .and_then(|name| name.strip_suffix(".sqlite.zst"))
                .map(str::to_owned)
        })
        .map(|digest| {
            let retained = store.pack_sha256_is_retained(&digest).expect("retention");
            (digest, retained)
        })
        .collect();
    files.sort();
    files
}

/// The modification time of the content directory. The writer stages elsewhere, so it
/// changes only when a pack file is linked in or removed: a changed time over an
/// unchanged listing shows that the failed call wrote a pack before the fault and
/// removed it again, so the emptiness checks cannot pass vacuously (REV-M04-T06
/// finding 2).
fn orphan_pack_directory_time(store: &HubStore) -> std::time::SystemTime {
    std::fs::metadata(store.packs_dir().join("sha256"))
        .expect("pack directory")
        .modified()
        .expect("modification time")
}

fn assert_orphan_pack_written_before_the_fault(store: &HubStore, before: std::time::SystemTime) {
    assert_ne!(
        orphan_pack_directory_time(store),
        before,
        "the failed call wrote a pack before the fault (and removed it)"
    );
}

fn assert_orphan_pack_original_error(result: Result<usize, CollectorError>) {
    match result {
        Err(CollectorError::Store(crate::db::StoreError::CatalogueDurability(source))) => {
            assert_eq!(
                source.to_string(),
                "injected durability fault at catalogue_before_commit",
                "the catalogue failure is returned unchanged"
            );
        }
        other => panic!("expected the original catalogue failure, got {other:?}"),
    }
}

#[test]
fn orphan_pack_a_failed_publication_leaves_no_unretained_file() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let collection = orphan_pack_collection(9, "5YJ3E1EA7KF000001");
    // The writer makes the content directory the same way; made here, its time
    // before the call can be compared.
    std::fs::create_dir_all(store.packs_dir().join("sha256")).expect("pack directory");
    let before = orphan_pack_directory_time(&store);

    let result = {
        // The catalogue commit is the step after the pack write.
        let _fault = inject(DurabilityFaultPoint::CatalogueBeforeCommit);
        orphan_pack_publish(&store, &collection)
    };

    assert_orphan_pack_original_error(result);
    assert_orphan_pack_written_before_the_fault(&store, before);
    assert_eq!(
        orphan_pack_files(&store),
        Vec::<(String, bool)>::new(),
        "the failed publication left no pack file behind"
    );
}

#[test]
fn orphan_pack_an_earlier_publication_survives_a_later_failure() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    orphan_pack_publish(&store, &orphan_pack_collection(9, "5YJ3E1EA7KF000001"))
        .expect("the earlier publication");
    let earlier = orphan_pack_files(&store);
    assert_eq!(earlier.len(), 1, "the earlier publication wrote one pack");
    assert!(earlier[0].1, "a catalogue row retains the earlier pack");
    let before = orphan_pack_directory_time(&store);

    let result = {
        let _fault = inject(DurabilityFaultPoint::CatalogueBeforeCommit);
        orphan_pack_publish(&store, &orphan_pack_collection(10, "5YJ3E1EA7KF000002"))
    };

    assert_orphan_pack_original_error(result);
    assert_orphan_pack_written_before_the_fault(&store, before);
    assert_eq!(
        orphan_pack_files(&store),
        earlier,
        "the earlier pack survives and the failed one is gone"
    );
}

fn orphan_pack_snapshot() -> ProjectionSnapshot {
    ProjectionSnapshot {
        cars: vec![compatibility_car(
            &Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online"),
            None,
            1,
        )],
        drives: vec![],
        positions: vec![],
        charges: vec![],
        charge_samples: vec![],
    }
}

fn orphan_pack_request(snapshot: &ProjectionSnapshot) -> ProjectionPackRequest<'_> {
    ProjectionPackRequest {
        pack_id: Uuid::from_u128(4),
        snapshot_id: Uuid::from_u128(5),
        ordinal: 0,
        binding: ProjectionBinding {
            installation_id: Uuid::from_u128(1),
            account_id: Uuid::from_u128(2),
            vehicle_id: Uuid::from_u128(3),
            generation: 1,
            selected_car_id: 1,
        },
        sequence: SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
        snapshot,
    }
}

fn orphan_pack_failure() -> CollectorError {
    crate::db::StoreError::CatalogueDurability(std::io::Error::other("catalogue failure")).into()
}

#[test]
fn orphan_pack_a_retained_pack_with_the_same_digest_survives_the_failure() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let snapshot = orphan_pack_snapshot();
    let request = orphan_pack_request(&snapshot);
    let writer = ProjectionPackWriter::new(store.packs_dir());
    let cursor_key = CursorKey::from_bytes([46; 32]);

    // The pack this publication created is named by a catalogue row by the time the
    // failure is handled (an earlier commit of the same digest): it stays.
    let created = writer.write_full_snapshot(&request).expect("created pack");
    assert_eq!(created.ownership(), ProjectionPackOwnership::Created);
    let manifest = request
        .signed_manifest(&created, &cursor_key)
        .expect("manifest");
    store.publish_manifest(&manifest).expect("catalogue row");
    let publication_gate = store.try_acquire_publication_gate().expect("gate");
    discard_failed_publication_pack(
        &store,
        &publication_gate,
        &created,
        &orphan_pack_failure(),
        None,
    );
    assert!(created.path.is_file(), "the retained pack survives");

    // A later write of the same digest reuses the file: it never removes it.
    let reused = writer.write_full_snapshot(&request).expect("reused pack");
    assert_eq!(reused.ownership(), ProjectionPackOwnership::ReusedExisting);
    discard_failed_publication_pack(
        &store,
        &publication_gate,
        &reused,
        &orphan_pack_failure(),
        None,
    );
    assert!(reused.path.is_file(), "the reused pack survives");
    store.catalogue_check().expect("catalogue still valid");
}

#[test]
fn orphan_pack_an_ambiguous_commit_or_a_refused_guard_keeps_the_pack() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let snapshot = orphan_pack_snapshot();
    let request = orphan_pack_request(&snapshot);
    let publication_gate = store.try_acquire_publication_gate().expect("gate");
    let created = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot(&request)
        .expect("created pack");

    // The commit may have landed: startup repair owns the evidence.
    let ambiguous: CollectorError = crate::db::StoreError::AmbiguousCatalogueCommit {
        vehicle_id: Uuid::from_u128(3),
        snapshot_id: Uuid::from_u128(5),
    }
    .into();
    discard_failed_publication_pack(&store, &publication_gate, &created, &ambiguous, None);
    assert!(created.path.is_file(), "an ambiguous commit keeps the pack");

    // The run may no longer write: the removal is a write too.
    let fence = AdmissionFence::default();
    fence.set();
    let guard = RunWriteGuard::from(fence);
    discard_failed_publication_pack(
        &store,
        &publication_gate,
        &created,
        &orphan_pack_failure(),
        Some(&guard),
    );
    assert!(created.path.is_file(), "a refused guard keeps the pack");

    // With neither, the unretained pack this publication created is removed.
    discard_failed_publication_pack(
        &store,
        &publication_gate,
        &created,
        &orphan_pack_failure(),
        None,
    );
    assert!(!created.path.exists(), "the unretained pack is removed");
}

#[test]
fn orphan_pack_a_failed_removal_is_logged_and_does_not_panic() {
    use std::os::unix::fs::PermissionsExt;
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let snapshot = orphan_pack_snapshot();
    let request = orphan_pack_request(&snapshot);
    let publication_gate = store.try_acquire_publication_gate().expect("gate");
    let created = ProjectionPackWriter::new(store.packs_dir())
        .write_full_snapshot(&request)
        .expect("created pack");
    // A mode the removal helper refuses to unlink makes the removal fail.
    std::fs::set_permissions(&created.path, std::fs::Permissions::from_mode(0o600))
        .expect("mode");

    discard_failed_publication_pack(
        &store,
        &publication_gate,
        &created,
        &orphan_pack_failure(),
        None,
    );
    assert!(created.path.is_file(), "the failed removal left the file");
}

// ---- M04-T07: the v2 delta and compaction packs ----------------------------------------

const ORPHAN_PACK_CATALOGUE_FAULT: &str = "orphan pack injected catalogue fault";

/// Every later lineage commit fails in its transaction, after the pack write: the
/// delta and the compaction commits both insert a `sync_deltas` row. The failure
/// names the digest of the pack the commit carried, which exists only once the
/// writer has published the file.
fn orphan_pack_fail_lineage_commits(store: &HubStore) {
    store
        .open()
        .expect("database")
        .execute_batch(&format!(
            "CREATE TRIGGER orphan_pack_lineage_fault BEFORE INSERT ON sync_deltas
             BEGIN SELECT RAISE(ABORT, '{ORPHAN_PACK_CATALOGUE_FAULT} pack='
                 || NEW.pack_digest); END;"
        ))
        .expect("lineage fault trigger");
}

/// The original lineage commit failure, and the digest of the pack it carried.
fn assert_orphan_pack_lineage_error<T: std::fmt::Debug>(
    result: Result<T, CollectorError>,
) -> String {
    match result {
        Err(CollectorError::Store(crate::db::StoreError::LineageCatalog(source))) => {
            let message = source.to_string();
            assert!(
                message.contains(ORPHAN_PACK_CATALOGUE_FAULT),
                "the lineage commit failure is returned unchanged: {message}"
            );
            let digest = message
                .split_once(" pack=")
                .map(|(_, digest)| digest.chars().take(64).collect::<String>())
                .unwrap_or_default();
            assert!(
                digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()),
                "the failed commit names the pack it carried: {message}"
            );
            digest
        }
        other => panic!("expected the original lineage commit failure, got {other:?}"),
    }
}

/// The failed commit carried a pack the writer published, and that file is gone.
fn assert_orphan_pack_failed_pack_removed(files: &[(String, bool)], failed_digest: &str) {
    assert!(
        files.iter().all(|(digest, _)| digest != failed_digest),
        "the pack of the failed commit ({failed_digest}) is removed: {files:?}"
    );
}

fn assert_orphan_pack_all_retained(files: &[(String, bool)]) {
    assert!(
        files.iter().all(|(_, retained)| *retained),
        "no unretained pack file: {files:?}"
    );
}

#[test]
fn orphan_pack_delta_a_failed_replay_leaves_no_unretained_file() {
    let rig = ExportRig::with_base();
    // An earlier live delta, published through the same replay: its pack is retained.
    guard_block_on(replay_export_outbox(
        &rig.store,
        &rig.cursor_key,
        std::slice::from_ref(&rig.vehicle),
        rig.now + 1,
    ))
    .expect("the earlier delta");
    let earlier = orphan_pack_files(&rig.store);
    assert!(earlier.len() >= 2, "the base and the earlier delta pack");
    assert_orphan_pack_all_retained(&earlier);

    // A second, different settings change and its outbox row.
    rig.store
        .upsert_car_settings(
            rig.vehicle_id,
            9,
            &crate::hub_pack::ProjectionCarSettings {
                enabled: true,
                ..crate::hub_pack::ProjectionCarSettings::default()
            },
        )
        .expect("settings mutation");
    rig.store
        .open()
        .expect("database")
        .execute(
            "INSERT OR IGNORE INTO export_outbox(
                vehicle_id, dirty_revision, attempts, next_attempt_ms,
                claimed_until_ms, last_error
             ) VALUES (?1, 1, 0, 0, 0, NULL)",
            rusqlite::params![rig.vehicle_id.to_string()],
        )
        .expect("outbox row");
    assert!(
        rig.store
            .has_unpublished_sync_mutations(rig.vehicle_id)
            .expect("pending mutations"),
        "the replay has a delta to publish"
    );
    orphan_pack_fail_lineage_commits(&rig.store);
    let before = orphan_pack_directory_time(&rig.store);
    // The replay holds the publication gate and passes it down to the delta.
    let result = guard_block_on(replay_export_outbox(
        &rig.store,
        &rig.cursor_key,
        std::slice::from_ref(&rig.vehicle),
        rig.now + 2,
    ));

    let failed_digest = assert_orphan_pack_lineage_error(result);
    assert_orphan_pack_written_before_the_fault(&rig.store, before);
    let files = orphan_pack_files(&rig.store);
    assert_orphan_pack_failed_pack_removed(&files, &failed_digest);
    assert_eq!(
        files, earlier,
        "the retained packs survive and the failed delta pack is gone"
    );
}

#[test]
fn orphan_pack_compaction_a_failed_commit_leaves_no_unretained_file() {
    let rig = ExportRig::with_base();
    // Two live deltas, as in the compaction proof; the tiny limit makes compaction due.
    for index in 0..2_i64 {
        rig.store
            .upsert_car_settings(
                rig.vehicle_id,
                9,
                &crate::hub_pack::ProjectionCarSettings {
                    enabled: index == 1,
                    ..crate::hub_pack::ProjectionCarSettings::default()
                },
            )
            .expect("settings mutation");
        while let Some(claim) = rig
            .store
            .claim_sync_mutations(rig.vehicle_id, rig.now + 100 + index, 100)
            .expect("claim")
        {
            publish_v2_delta(&rig.store, &rig.cursor_key, &claim).expect("a live delta");
        }
    }
    let earlier = orphan_pack_files(&rig.store);
    assert!(earlier.len() >= 3, "the base and two delta packs");
    assert_orphan_pack_all_retained(&earlier);
    let tiny_limit = ProtocolLimits {
        max_chunks: 4,
        ..ProtocolLimits::default()
    };

    let lineage_packs = rig
        .store
        .v2_lineage_pack_count(rig.vehicle_id)
        .expect("pack count");

    orphan_pack_fail_lineage_commits(&rig.store);
    let before = orphan_pack_directory_time(&rig.store);
    let publication_gate = rig.store.try_acquire_publication_gate().expect("gate");
    let result = compact_v2_lineage_if_needed_with_limits_guarded(
        &rig.store,
        &publication_gate,
        &rig.cursor_key,
        rig.vehicle_id,
        tiny_limit,
        None,
    );

    let failed_digest = assert_orphan_pack_lineage_error(result);
    assert_orphan_pack_written_before_the_fault(&rig.store, before);
    let files = orphan_pack_files(&rig.store);
    assert_orphan_pack_failed_pack_removed(&files, &failed_digest);
    assert_eq!(
        files, earlier,
        "the retained packs survive and the failed compaction pack is gone"
    );
    assert_eq!(
        rig.store
            .v2_lineage_pack_count(rig.vehicle_id)
            .expect("pack count"),
        lineage_packs,
        "the lineage is unchanged"
    );
}

#[test]
fn orphan_pack_drain_a_failed_turn_leaves_no_unretained_file() {
    let rig = ExportRig::with_base();
    let settings_change = |enabled: bool| {
        rig.store
            .upsert_car_settings(
                rig.vehicle_id,
                9,
                &crate::hub_pack::ProjectionCarSettings {
                    enabled,
                    ..crate::hub_pack::ProjectionCarSettings::default()
                },
            )
            .expect("settings mutation");
    };
    let drain_turn = |now_ms: i64| {
        let publication_gate = rig.store.try_acquire_publication_gate().expect("gate");
        publish_unpublished_sync_mutations(
            &rig.store,
            &rig.cursor_key,
            rig.vehicle_id,
            now_ms,
            &publication_gate,
            100,
        )
    };
    // An earlier drain turn publishes a live delta: its pack is retained.
    settings_change(true);
    let earlier_turn = drain_turn(rig.now + 1).expect("the earlier drain turn");
    assert!(earlier_turn.published > 0, "the earlier turn published a delta");
    let earlier = orphan_pack_files(&rig.store);
    assert!(earlier.len() >= 2, "the base and the earlier delta pack");
    assert_orphan_pack_all_retained(&earlier);

    // A second, different settings change; its lineage commit fails after the write.
    settings_change(false);
    assert!(
        rig.store
            .has_unpublished_sync_mutations(rig.vehicle_id)
            .expect("pending mutations"),
        "the drain has a delta to publish"
    );
    orphan_pack_fail_lineage_commits(&rig.store);
    let before = orphan_pack_directory_time(&rig.store);
    let result = drain_turn(rig.now + 2);

    let failed_digest = assert_orphan_pack_lineage_error(result);
    assert_orphan_pack_written_before_the_fault(&rig.store, before);
    let files = orphan_pack_files(&rig.store);
    assert_orphan_pack_failed_pack_removed(&files, &failed_digest);
    assert_eq!(
        files, earlier,
        "the retained packs survive and the failed drain pack is gone"
    );
    assert!(
        rig.store
            .has_unpublished_sync_mutations(rig.vehicle_id)
            .expect("pending mutations"),
        "the failed turn released its claim"
    );
}
