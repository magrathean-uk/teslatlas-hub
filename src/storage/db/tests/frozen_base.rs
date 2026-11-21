// SPDX-License-Identifier: AGPL-3.0-only

//! STORAGE-T3a: a vehicle's frozen physical base names the pack objects of one
//! verified, published head, and every sweep keeps them after the head stops
//! being current. A damaged record owns nothing and deletes nothing.

use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const SOURCE: &str = "frozen-base";
const GRACE: i64 = RETIRED_LINEAGE_PACK_DELETE_GRACE_MS;
const RETENTION: i64 = RETIRED_LINEAGE_PACK_RETENTION_MS;

fn candidate(
    temporary: &std::path::Path,
    store: &HubStore,
    name: &str,
    chunks: u32,
    head_sequence: u64,
) -> crate::sync::physical_fragments::StagedPhysicalProjectionV3 {
    crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
        &temporary.join(name),
        store,
        &CursorKey::from_bytes([31; 32]),
        chunks,
        SOURCE,
        head_sequence,
    )
    .1
}

fn pack_files(store: &HubStore) -> BTreeMap<String, String> {
    let mut hashes = BTreeMap::new();
    for entry in std::fs::read_dir(store.packs_dir().join("sha256")).expect("pack objects") {
        let entry = entry.expect("pack object entry");
        if entry.file_type().expect("pack object type").is_file() {
            let bytes = std::fs::read(entry.path()).expect("pack object bytes");
            hashes.insert(
                entry.file_name().to_string_lossy().into_owned(),
                hex::encode(Sha256::digest(&bytes)),
            );
        }
    }
    hashes
}

fn chunk_names(admission: &PendingPhysicalV3Admission) -> Vec<String> {
    admission
        .manifest
        .chunks
        .iter()
        .map(|chunk| format!("{}.sqlite.zst", chunk.sha256))
        .collect()
}

fn frozen_value(store: &HubStore, vehicle_id: Uuid) -> Option<String> {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT value FROM hub_metadata WHERE key = ?1",
            [format!("physical_v3_frozen_base:{vehicle_id}")],
            |row| row.get(0),
        )
        .optional()
        .expect("frozen value")
}

fn put_metadata(store: &HubStore, key: &str, value: &str) {
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO hub_metadata(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )
        .expect("metadata row");
}

/// A frozen first head, three successor heads (the first two expired well
/// past their grace, the third current), and one orphan pack.
struct Frozen {
    _temporary: tempfile::TempDir,
    store: HubStore,
    first: PendingPhysicalV3Admission,
    successors: Vec<PendingPhysicalV3Admission>,
    base: FrozenPhysicalBase,
    logical_at_freeze: String,
    now_ms: i64,
}

fn frozen_fixture() -> Frozen {
    let (temporary, store) = try_test_store().expect("store");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let first = store
        .stage_pending_physical_v3_admission(&gate, candidate(temporary.path(), &store, "h1", 2, 1))
        .expect("first head");
    let binding = ProjectionBinding {
        installation_id: first.installation_id,
        account_id: first.account_id,
        vehicle_id: first.vehicle_id,
        generation: first.manifest.generation,
        selected_car_id: first.selected_car_id,
    };
    let logical_at_freeze = frozen_physical_base_scan(store.packs_dir(), &first.manifest, &binding)
        .expect("scan at freeze")
        .logical;
    let base = store
        .freeze_physical_v3_base(&gate, first.vehicle_id, 5)
        .expect("freeze the first head");
    let mut successors = Vec::new();
    let mut retained_at = 1_000;
    for (sequence, chunks) in [(2, 3), (3, 4), (4, 2)] {
        successors.push(
            store
                .rotate_and_activate_physical_v3_admission_with_delta_at(
                    &gate,
                    candidate(temporary.path(), &store, &format!("h{sequence}"), chunks, sequence),
                    None,
                    retained_at,
                )
                .expect("successor head"),
        );
        retained_at += RETENTION + GRACE + 10;
    }
    drop(gate);
    let orphan = hex::encode([0x3a; 32]);
    std::fs::write(
        store.packs_dir().join("sha256").join(format!("{orphan}.sqlite.zst")),
        vec![0x3a; 640],
    )
    .expect("orphan pack");
    Frozen {
        _temporary: temporary,
        store,
        first,
        successors,
        base,
        logical_at_freeze,
        now_ms: retained_at + RETENTION + GRACE + 10,
    }
}

/// Every frozen pack is present and the frozen base decodes, with the
/// authenticated decoder, to the logical digest recorded at freeze.
fn assert_frozen_packs_intact(store: &HubStore, fixture_base: &FrozenPhysicalBase, logical: &str) {
    let files = pack_files(store);
    for chunk in &fixture_base.manifest.chunks {
        let name = format!("{}.sqlite.zst", chunk.sha256);
        assert_eq!(files.get(&name), Some(&chunk.sha256.to_string()), "{name}");
    }
    crate::hub_pack::take_verifications();
    let verified = store
        .verify_frozen_physical_v3_base(fixture_base.vehicle_id)
        .expect("the frozen base verifies");
    assert_eq!(&verified, fixture_base);
    assert_eq!(verified.logical, logical);
    let decoded = crate::hub_pack::take_verifications()
        .into_iter()
        .filter(|check| check.place == "pack-decode" && check.ok)
        .count();
    assert_eq!(decoded, fixture_base.manifest.chunks.len());
}

#[test]
fn frozen_base_freeze_reads_back_and_records_the_verified_head() {
    let (temporary, store) = try_test_store().expect("store");
    let gate = store.try_acquire_publication_gate().expect("gate");
    assert!(matches!(
        store.freeze_physical_v3_base(&gate, Uuid::from_u128(7), 1),
        Err(FrozenPhysicalBaseRefusal::NoHead(_))
    ));
    let head = store
        .stage_pending_physical_v3_admission(&gate, candidate(temporary.path(), &store, "h1", 3, 1))
        .expect("head");
    let base = store
        .freeze_physical_v3_base(&gate, head.vehicle_id, 42)
        .expect("freeze");

    assert_eq!(store.frozen_physical_v3_base(head.vehicle_id).expect("read"), Some(base.clone()));
    assert_eq!(base.snapshot_id, head.snapshot_id);
    assert_eq!(base.receipt_id, head.receipt_id);
    assert_eq!(base.manifest, head.manifest);
    assert_eq!(base.binding.vehicle_id, head.vehicle_id);
    assert_eq!(base.frozen_at_ms, 42);
    assert_eq!(base.rows, head.manifest.chunks.iter().map(|chunk| chunk.row_count).sum::<u64>());
    assert_eq!(base.provenance.rule, FROZEN_PHYSICAL_BASE_PROVENANCE_RULE);
    assert_eq!(base.provenance.tables.len(), crate::hub_pack::PHYSICAL_TABLES_2_2.len());
    // The fixture's rows are all source rows: no id in the Hub's range.
    assert!(base.provenance.tables.iter().all(|table| table.hub_rows == 0));
    assert_eq!(base.provenance.import_run, None);
    // The stored value is the canonical serialisation of the record.
    assert_eq!(
        frozen_value(&store, head.vehicle_id),
        Some(serde_json::to_string(&base).expect("serialise"))
    );
    // Freezing the frozen head again is the same record.
    assert_eq!(
        store.freeze_physical_v3_base(&gate, head.vehicle_id, 99).expect("again"),
        base
    );
    // Another head is refused while a base is frozen.
    store
        .rotate_and_activate_physical_v3_admission_with_delta_at(
            &gate,
            candidate(temporary.path(), &store, "h2", 2, 2),
            None,
            1_000,
        )
        .expect("successor");
    match store.freeze_physical_v3_base(&gate, head.vehicle_id, 100) {
        Err(FrozenPhysicalBaseRefusal::AlreadyFrozen { snapshot_id, .. }) => {
            assert_eq!(snapshot_id, head.snapshot_id);
        }
        other => panic!("expected AlreadyFrozen, got {other:?}"),
    }
    assert_eq!(store.frozen_physical_v3_base(head.vehicle_id).expect("read"), Some(base));
}

#[test]
fn frozen_base_freeze_refuses_withheld_pending_unverified_and_blocked_heads() {
    // Withheld (a committed import that published no head) and restating (a
    // committed import whose head is another) are both unserved heads.
    for head_field in ["null", "\"00000000-0000-4000-8000-000000000001\""] {
        let (temporary, store) = try_test_store().expect("store");
        let gate = store.try_acquire_publication_gate().expect("gate");
        let head = store
            .stage_pending_physical_v3_admission(&gate, candidate(temporary.path(), &store, "h", 2, 1))
            .expect("head");
        put_metadata(
            &store,
            &format!("physical_v3_import_revision:{}", head.vehicle_id),
            &format!(
                "{{\"run_id\":\"00000000-0000-4000-8000-0000000000aa\",\"kept\":null,\"head\":{head_field}}}"
            ),
        );
        assert!(matches!(
            store.freeze_physical_v3_base(&gate, head.vehicle_id, 1),
            Err(FrozenPhysicalBaseRefusal::NotServed(_))
        ));
        assert_eq!(frozen_value(&store, head.vehicle_id), None);
    }

    // A pending import publication intent (one that does not read back
    // counts as pending).
    let (temporary, store) = try_test_store().expect("store");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let head = store
        .stage_pending_physical_v3_admission(&gate, candidate(temporary.path(), &store, "h", 2, 1))
        .expect("head");
    put_metadata(&store, &format!("import_publication_intent:{}", head.vehicle_id), "{}");
    assert!(matches!(
        store.freeze_physical_v3_base(&gate, head.vehicle_id, 1),
        Err(FrozenPhysicalBaseRefusal::ImportPublicationPending(_))
    ));
    assert_eq!(frozen_value(&store, head.vehicle_id), None);

    // A pack whose bytes are not the manifest's.
    let (temporary, store) = try_test_store().expect("store");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let head = store
        .stage_pending_physical_v3_admission(&gate, candidate(temporary.path(), &store, "h", 2, 1))
        .expect("head");
    let path = store.packs_dir().join("sha256").join(&chunk_names(&head)[1]);
    let length = std::fs::metadata(&path).expect("pack").len() as usize;
    std::fs::remove_file(&path).expect("remove pack");
    std::fs::write(&path, vec![0x55; length]).expect("tampered pack");
    assert!(matches!(
        store.freeze_physical_v3_base(&gate, head.vehicle_id, 1),
        Err(FrozenPhysicalBaseRefusal::Unverified { .. })
    ));
    assert_eq!(frozen_value(&store, head.vehicle_id), None);

    // A blocked rotation is not a public head.
    let (temporary, store) = try_test_store().expect("store");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let head = store
        .stage_pending_physical_v3_admission(&gate, candidate(temporary.path(), &store, "h1", 2, 1))
        .expect("head");
    store
        .rotate_pending_physical_v3_admission_at(&gate, candidate(temporary.path(), &store, "h2", 2, 2), 1_000)
        .expect("blocked rotation");
    assert!(matches!(
        store.freeze_physical_v3_base(&gate, head.vehicle_id, 1),
        Err(FrozenPhysicalBaseRefusal::NotPublic(_))
    ));
    assert_eq!(frozen_value(&store, head.vehicle_id), None);
}

#[test]
fn frozen_base_packs_survive_successor_heads_and_pack_collection() {
    let fixture = frozen_fixture();
    let store = &fixture.store;
    assert_eq!(fixture.base.snapshot_id, fixture.first.snapshot_id);
    assert_eq!(fixture.base.logical, fixture.logical_at_freeze);
    let expired = chunk_names(&fixture.successors[0]);

    let repaired = store.repair_at(fixture.now_ms).expect("pack collection");
    assert!(repaired.orphaned_packs_removed > 0, "{repaired:?}");
    store.reclaim_storage_at(fixture.now_ms).expect("reclaim");

    let files = pack_files(store);
    assert!(expired.iter().all(|name| !files.contains_key(name)), "the expired head went");
    assert!(!files.contains_key(&format!("{}.sqlite.zst", hex::encode([0x3a; 32]))));
    assert_frozen_packs_intact(store, &fixture.base, &fixture.logical_at_freeze);
    // The current head is unchanged and still served.
    let current = fixture.successors.last().expect("current");
    assert_eq!(
        store
            .pending_physical_v3_admission_for_vehicle(current.vehicle_id)
            .expect("current head"),
        Some(current.clone())
    );
}

#[test]
fn frozen_base_census_reclaim_keeps_and_counts_frozen_packs_twice_identically() {
    let fixture = frozen_fixture();
    let store = &fixture.store;
    let frozen_bytes: u64 = fixture
        .first
        .manifest
        .chunks
        .iter()
        .map(|chunk| chunk.compressed_bytes)
        .sum();
    let census = store.storage_census_at(fixture.now_ms).expect("census");
    let frozen_class = StorageCensusClass {
        files: fixture.first.manifest.chunks.len() as u64,
        bytes: frozen_bytes,
        reclaimable_files: 0,
        reclaimable_bytes: 0,
    };
    assert_eq!(census.frozen_bases, Some(frozen_class));
    assert!(census.total.reclaimable_files > 0);

    let first = store.reclaim_storage_at(fixture.now_ms).expect("first reclaim");
    let second = store.reclaim_storage_at(fixture.now_ms).expect("second reclaim");
    let third = store.reclaim_storage_at(fixture.now_ms).expect("third reclaim");

    assert_eq!(first.before, census);
    assert!(first.reclaimed_files > 0);
    for report in [&first, &second, &third] {
        assert_eq!(report.before.frozen_bases, Some(frozen_class));
        assert_eq!(report.after.frozen_bases, Some(frozen_class));
    }
    assert_eq!(second.before, first.after);
    assert_eq!((second.reclaimed_files, second.reclaimed_bytes), (0, 0));
    assert_eq!(second, third, "two runs in a row give identical receipts");
    assert_frozen_packs_intact(store, &fixture.base, &fixture.logical_at_freeze);
}

#[test]
fn frozen_base_backup_and_restore_keep_the_frozen_packs() {
    let fixture = frozen_fixture();
    let store = &fixture.store;
    // Backup and restore live outside the store's data directory.
    let outside = crate::private_tempdir().expect("backup parent");
    let backup = outside.path().join("backup");
    crate::data_recovery::create_data_backup(store, &backup).expect("backup");
    let backed_up = backup.join("data").join("packs").join("sha256");
    for name in chunk_names(&fixture.first) {
        assert!(backed_up.join(&name).is_file(), "{name} is a backup member");
    }
    let restored = outside.path().join("restored");
    crate::data_recovery::restore_data_backup(&backup, &restored).expect("restore");
    let restored_store = HubStore::open_existing(&restored).expect("restored store");
    assert_eq!(
        restored_store
            .frozen_physical_v3_base(fixture.base.vehicle_id)
            .expect("restored record"),
        Some(fixture.base.clone())
    );
    assert_frozen_packs_intact(&restored_store, &fixture.base, &fixture.logical_at_freeze);

    restored_store.repair_at(fixture.now_ms).expect("restored pack collection");
    let first = restored_store.reclaim_storage_at(fixture.now_ms).expect("restored reclaim");
    let second = restored_store.reclaim_storage_at(fixture.now_ms).expect("restored reclaim again");
    assert_eq!(first.after.frozen_bases.expect("frozen class").files, fixture.first.manifest.chunks.len() as u64);
    assert_eq!(first.after, second.after);
    assert_frozen_packs_intact(&restored_store, &fixture.base, &fixture.logical_at_freeze);
}

#[test]
fn frozen_base_damaged_record_refuses_every_sweep_and_deletes_nothing() {
    let fixture = frozen_fixture();
    let store = &fixture.store;
    let vehicle_id = fixture.base.vehicle_id;
    let key = format!("physical_v3_frozen_base:{vehicle_id}");
    let valid = frozen_value(store, vehicle_id).expect("record");
    let pretty = serde_json::to_string_pretty(&fixture.base).expect("pretty");
    let mut wrong_logical = fixture.base.clone();
    wrong_logical.logical = wrong_logical.logical.to_uppercase();
    let mut wrong_receipt = fixture.base.clone();
    wrong_receipt.receipt_id = "pv3_00".into();
    let damaged: Vec<(String, String)> = vec![
        (key.clone(), "{not json".into()),
        (key.clone(), pretty),
        (key.clone(), serde_json::to_string(&wrong_logical).expect("value")),
        (key.clone(), serde_json::to_string(&wrong_receipt).expect("value")),
        (key.clone(), valid.replacen("\"format\":1", "\"format\":2", 1)),
        (key.to_uppercase(), valid.clone()),
        (format!("physical_v3_frozen_base:{}", vehicle_id.to_string().to_uppercase()), valid.clone()),
    ];
    let outside = crate::private_tempdir().expect("backup parent");
    let files = pack_files(store);
    let kept_rows: i64 = store
        .open()
        .expect("catalogue")
        .query_row("SELECT COUNT(*) FROM retained_physical_v3_admissions", [], |row| row.get(0))
        .expect("retained rows");
    for (index, (damaged_key, value)) in damaged.into_iter().enumerate() {
        put_metadata(store, &damaged_key, &value);

        assert!(matches!(
            store.reclaim_storage_at(fixture.now_ms),
            Err(StoreError::FrozenPhysicalBaseInvalid { .. })
        ));
        assert!(matches!(
            store.storage_census_at(fixture.now_ms),
            Err(StoreError::FrozenPhysicalBaseInvalid { .. })
        ));
        assert!(matches!(
            store.repair_at(fixture.now_ms),
            Err(StoreError::FrozenPhysicalBaseInvalid { .. })
        ));
        let refused_backup = outside.path().join(format!("refused-backup-{index}"));
        let error = crate::data_recovery::create_data_backup(store, &refused_backup)
            .expect_err("a damaged record refuses the backup")
            .to_string();
        assert!(error.contains("frozen physical base"), "{damaged_key}: {error}");
        assert!(!refused_backup.exists());
        assert_eq!(pack_files(store), files, "{damaged_key}: nothing was deleted");
        let rows: i64 = store
            .open()
            .expect("catalogue")
            .query_row("SELECT COUNT(*) FROM retained_physical_v3_admissions", [], |row| row.get(0))
            .expect("retained rows");
        assert_eq!(rows, kept_rows, "{damaged_key}: no retention row went");

        // Put the valid record back alone.
        let connection = store.open().expect("catalogue");
        connection
            .execute("DELETE FROM hub_metadata WHERE key = ?1", [&damaged_key])
            .expect("remove damaged row");
        connection
            .execute(
                "INSERT INTO hub_metadata(key, value) VALUES(?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, valid],
            )
            .expect("valid record");
    }
    // With the record whole again, the sweep runs and keeps the frozen packs.
    store.reclaim_storage_at(fixture.now_ms).expect("reclaim");
    assert_frozen_packs_intact(store, &fixture.base, &fixture.logical_at_freeze);
}

#[test]
fn frozen_base_key_with_a_damaged_separator_still_owns_its_packs() {
    let fixture = frozen_fixture();
    let store = &fixture.store;
    let vehicle_id = fixture.base.vehicle_id;
    let key = format!("physical_v3_frozen_base:{vehicle_id}");
    let valid = frozen_value(store, vehicle_id).expect("record");
    let outside = crate::private_tempdir().expect("backup parent");
    let files = pack_files(store);
    let damaged_keys = [
        format!("physical_v3_frozen_base!{vehicle_id}"),
        format!("physical_v3_frozen_base{vehicle_id}"),
        format!("physical_v3_frozen_base:{vehicle_id}x"),
        "physical_v3_frozen_base".to_owned(),
    ];
    for (index, damaged_key) in damaged_keys.into_iter().enumerate() {
        // The value is intact and still names the packs; only the key is
        // damaged, and the older head's retention has long expired.
        let connection = store.open().expect("catalogue");
        connection.execute("DELETE FROM hub_metadata WHERE key = ?1", [&key]).expect("remove key");
        drop(connection);
        put_metadata(store, &damaged_key, &valid);

        assert!(matches!(
            store.reclaim_storage_at(fixture.now_ms),
            Err(StoreError::FrozenPhysicalBaseInvalid { .. })
        ));
        assert!(matches!(
            store.repair_at(fixture.now_ms),
            Err(StoreError::FrozenPhysicalBaseInvalid { .. })
        ));
        let refused = outside.path().join(format!("refused-{index}"));
        crate::data_recovery::create_data_backup(store, &refused)
            .expect_err("a damaged key refuses the backup");
        assert!(!refused.exists());
        assert_eq!(pack_files(store), files, "{damaged_key}: nothing was unlinked");

        let connection = store.open().expect("catalogue");
        connection.execute("DELETE FROM hub_metadata WHERE key = ?1", [&damaged_key]).expect("remove");
        drop(connection);
        put_metadata(store, &key, &valid);
    }
    // Whole again, the same backup carries the frozen packs.
    let backup = outside.path().join("whole");
    crate::data_recovery::create_data_backup(store, &backup).expect("backup");
    for name in chunk_names(&fixture.first) {
        assert!(backup.join("data").join("packs").join("sha256").join(&name).is_file(), "{name}");
    }
}

/// Closes one more window with coverage of every vehicle, which changes the
/// closed-coverage digest the planned head was bound to.
fn close_window(store: &HubStore, seq: i64) {
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO collection_coverage(seq, grant_digest, opened_at_ms, closed_at_ms,
                                             gaps_json, vehicle_ends_json, sealed)
             VALUES(?1, 'grant', ?2, ?3, '[]', NULL, 'handback')",
            params![seq, seq * 10_000, seq * 10_000 + 5_000],
        )
        .expect("closed window");
}

/// A head planned with the closed coverage the store holds now: served.
fn planned_head(
    temporary: &std::path::Path,
    store: &HubStore,
    gate: &PublicationGate,
) -> PendingPhysicalV3Admission {
    close_window(store, 1);
    let head = store
        .stage_pending_physical_v3_admission(gate, candidate(temporary, store, "h", 2, 1))
        .expect("head");
    store
        .record_physical_v3_seam_planned(gate, head.vehicle_id, head.snapshot_id)
        .expect("planned with the coverage");
    assert!(store.physical_v3_admission_served(head.vehicle_id, head.snapshot_id).expect("served"));
    head
}

#[test]
fn frozen_base_freeze_refuses_coverage_that_closes_between_scan_and_insert() {
    let (temporary, store) = try_test_store().expect("store");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let head = planned_head(temporary.path(), &store, &gate);
    let store_for_hook = store.clone();
    FREEZE_AFTER_SCAN.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || close_window(&store_for_hook, 2)));
    });
    assert!(matches!(
        store.freeze_physical_v3_base(&gate, head.vehicle_id, 1),
        Err(FrozenPhysicalBaseRefusal::NotServed(_))
    ));
    assert_eq!(frozen_value(&store, head.vehicle_id), None);
    assert!(!store.physical_v3_admission_served(head.vehicle_id, head.snapshot_id).expect("served"));
}

#[test]
fn frozen_base_retry_is_answered_only_while_the_head_is_served() {
    let (temporary, store) = try_test_store().expect("store");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let head = planned_head(temporary.path(), &store, &gate);
    let base = store.freeze_physical_v3_base(&gate, head.vehicle_id, 1).expect("freeze");
    assert_eq!(store.freeze_physical_v3_base(&gate, head.vehicle_id, 2).expect("retry"), base);
    close_window(&store, 2);
    assert!(matches!(
        store.freeze_physical_v3_base(&gate, head.vehicle_id, 3),
        Err(FrozenPhysicalBaseRefusal::NotServed(_))
    ));
    // The record itself is unchanged.
    assert_eq!(store.frozen_physical_v3_base(head.vehicle_id).expect("read"), Some(base));
}

#[test]
fn frozen_base_ownership_is_per_table() {
    let inside = [hub_ids::HUB_ID_BASE, hub_ids::HUB_ID_MAX];
    let outside = [0, 1, hub_ids::HUB_ID_BASE - 1, hub_ids::HUB_ID_MAX + 1];
    let tables = crate::hub_pack::PHYSICAL_TABLES_2_2;
    assert_eq!(FROZEN_PHYSICAL_BASE_PROVENANCE_RULE, "hub-id-range/v2");
    for table in tables {
        let dimension = matches!(
            table,
            "global_settings" | "car_settings" | "cars" | "addresses" | "geofences"
        );
        for id in inside {
            // An imported address whose source id lies in the Hub range is
            // still a source row; a drive in that range is the Hub's.
            assert_eq!(frozen_row_is_hub_owned(table, id), !dimension, "{table} {id}");
        }
        for id in outside {
            assert!(!frozen_row_is_hub_owned(table, id), "{table} {id}");
        }
    }
    assert!(!frozen_row_is_hub_owned("addresses", hub_ids::HUB_ID_BASE));
    assert!(frozen_row_is_hub_owned("drives", hub_ids::HUB_ID_BASE));
}

#[test]
fn frozen_base_record_claiming_a_hub_dimension_row_is_refused() {
    let fixture = frozen_fixture();
    let mut forged = fixture.base.clone();
    let addresses = forged
        .provenance
        .tables
        .iter_mut()
        .find(|table| table.table == "addresses")
        .expect("addresses");
    addresses.hub_rows += 1;
    forged.rows += 1;
    // Sealed again, so only the ownership rule can refuse it.
    forged.integrity = frozen_physical_base_integrity(&forged).expect("integrity");
    let key = format!("physical_v3_frozen_base:{}", forged.vehicle_id);
    let value = serde_json::to_string(&forged).expect("value");
    assert!(matches!(
        frozen_physical_base_from_row(&key, &value),
        Err(StoreError::FrozenPhysicalBaseInvalid { .. })
    ));
}

#[test]
fn frozen_base_canonical_one_digit_change_is_refused_at_every_gate() {
    let fixture = frozen_fixture();
    let store = &fixture.store;
    let vehicle_id = fixture.base.vehicle_id;
    let key = format!("physical_v3_frozen_base:{vehicle_id}");
    let valid = frozen_value(store, vehicle_id).expect("record");
    // Flip the last hex digit of `logical`: still lower-case hex, still the
    // canonical serialisation, every pack body still intact.
    let mut flipped = fixture.base.clone();
    let last = flipped.logical.pop().expect("digit");
    flipped.logical.push(if last == '0' { '1' } else { '0' });
    let damaged = serde_json::to_string(&flipped).expect("value");
    assert!(matches!(
        frozen_physical_base_from_row(&key, &damaged),
        Err(StoreError::FrozenPhysicalBaseInvalid { .. })
    ));
    // A change to another field (the freeze time) is refused as well.
    let mut later = fixture.base.clone();
    later.frozen_at_ms += 1;
    assert!(frozen_physical_base_from_row(&key, &serde_json::to_string(&later).expect("value")).is_err());
    assert!(frozen_physical_base_from_row(&key, &valid).is_ok());

    put_metadata(store, &key, &damaged);
    // Reference selection (reclaim) and the directory backup refuse.
    assert!(matches!(
        store.reclaim_storage_at(fixture.now_ms),
        Err(StoreError::FrozenPhysicalBaseInvalid { .. })
    ));
    let outside = crate::private_tempdir().expect("backup parent");
    let refused = outside.path().join("refused");
    crate::data_recovery::create_data_backup(store, &refused).expect_err("backup refuses");
    // Restore opens the restored catalogue and runs this same check.
    assert!(matches!(
        store.catalogue_check(),
        Err(StoreError::FrozenPhysicalBaseInvalid { .. })
    ));
    put_metadata(store, &key, &valid);
    store.catalogue_check().expect("whole again");
}

#[test]
fn frozen_base_census_json_has_no_frozen_class_until_a_record_exists() {
    let (temporary, store) = try_test_store().expect("store");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let head = store
        .stage_pending_physical_v3_admission(&gate, candidate(temporary.path(), &store, "h", 2, 1))
        .expect("head");
    let keys = |census: &StorageCensus| -> Vec<String> {
        serde_json::to_value(census)
            .expect("census json")
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect()
    };
    let census = store.storage_census_at(1).expect("census");
    assert_eq!(census.frozen_bases, None);
    assert!(!keys(&census).contains(&"frozen_bases".to_owned()));
    // Reclaim takes the publication gate itself, so the test releases its own first.
    drop(gate);
    let report = store.reclaim_storage_at(1).expect("reclaim");
    for side in [&report.before, &report.after] {
        assert!(!keys(side).contains(&"frozen_bases".to_owned()));
    }
    let gate = store.try_acquire_publication_gate().expect("gate again");
    // Once frozen, the class is present (zero while the head is current).
    store.freeze_physical_v3_base(&gate, head.vehicle_id, 1).expect("freeze");
    let census = store.storage_census_at(1).expect("census");
    assert_eq!(census.frozen_bases, Some(StorageCensusClass::default()));
    assert!(keys(&census).contains(&"frozen_bases".to_owned()));
}
