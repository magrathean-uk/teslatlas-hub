// SPDX-License-Identifier: AGPL-3.0-only

//! M08-T29: the storage census counts pack files by class and the bytes no
//! reference, download, recovery path or backup pin needs, and reclamation
//! removes exactly those bytes, resumably, leaving every kept pack unchanged.

use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const SOURCE: &str = "storage-census";
const GRACE: i64 = RETIRED_LINEAGE_PACK_DELETE_GRACE_MS;
const RETENTION: i64 = RETIRED_LINEAGE_PACK_RETENTION_MS;

fn pack_object_hashes(store: &HubStore) -> BTreeMap<String, String> {
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

fn head_candidate(
    temporary: &std::path::Path,
    store: &HubStore,
    name: &str,
    chunks: u32,
    head_sequence: u64,
) -> (
    ProjectionBinding,
    crate::sync::physical_fragments::StagedPhysicalProjectionV3,
) {
    crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
        &temporary.join(name),
        store,
        &CursorKey::from_bytes([29; 32]),
        chunks,
        SOURCE,
        head_sequence,
    )
}

fn chunk_files(
    store: &HubStore,
    admission: &PendingPhysicalV3Admission,
) -> Vec<(std::path::PathBuf, u64)> {
    admission
        .manifest
        .chunks
        .iter()
        .map(|chunk| {
            (
                store
                    .packs_dir()
                    .join("sha256")
                    .join(format!("{}.sqlite.zst", chunk.sha256)),
                chunk.compressed_bytes,
            )
        })
        .collect()
}

fn write_unreferenced_pack(store: &HubStore, fill: u8, bytes: usize) -> String {
    let digest = hex::encode([fill; 32]);
    std::fs::write(
        store
            .packs_dir()
            .join("sha256")
            .join(format!("{digest}.sqlite.zst")),
        vec![fill; bytes],
    )
    .expect("unreferenced pack");
    digest
}

fn retained_receipts(store: &HubStore) -> Vec<String> {
    store
        .open()
        .expect("catalogue")
        .prepare("SELECT receipt_id FROM retained_physical_v3_admissions ORDER BY head_sequence")
        .expect("retained query")
        .query_map([], |row| row.get(0))
        .expect("retained rows")
        .collect::<Result<_, _>>()
        .expect("retained receipts")
}

/// Three public heads: the first expired past its delete grace, the second
/// inside its retention, the third current; the vehicle's kept import stage
/// and source sessions; one orphan pack, one orphan pack a live backup pins,
/// and one stage copy an interrupted hand-back left.
struct PublicFixture {
    _temporary: tempfile::TempDir,
    store: HubStore,
    expired: PendingPhysicalV3Admission,
    retained: PendingPhysicalV3Admission,
    current: PendingPhysicalV3Admission,
    orphan_bytes: u64,
    pinned_bytes: u64,
    kept_paths: Vec<std::path::PathBuf>,
    stray_copy: std::path::PathBuf,
    now_ms: i64,
}

const KEPT_STAGE_BYTES: u64 = 500;
const KEPT_SOURCE_BYTES: u64 = 50;
const STRAY_COPY_BYTES: u64 = 900;

/// The kept stage and source sessions of `vehicle_id`, a stray stage copy,
/// and one file of another shape that the census does not count.
fn write_kept_stages(
    store: &HubStore,
    vehicle_id: Uuid,
) -> (Vec<std::path::PathBuf>, std::path::PathBuf) {
    let kept = store.packs_dir().join("physical-kept");
    let staging = kept.join(".staging");
    std::fs::create_dir_all(&staging).expect("kept stages");
    let stage = staging.join(format!("{vehicle_id}.sqlite"));
    let source = kept.join(format!("{vehicle_id}.source.json"));
    let stray = staging.join(format!("{}.sqlite", Uuid::from_u128(0x2929)));
    std::fs::write(&stage, vec![1; KEPT_STAGE_BYTES as usize]).expect("kept stage");
    std::fs::write(&source, vec![2; KEPT_SOURCE_BYTES as usize]).expect("kept source");
    std::fs::write(&stray, vec![3; STRAY_COPY_BYTES as usize]).expect("stray copy");
    std::fs::write(staging.join("other-shape.txt"), b"not counted").expect("other file");
    (vec![stage, source], stray)
}

fn public_fixture() -> PublicFixture {
    let (temporary, store) = try_test_store().expect("store");
    let first_retained_at = 1_000;
    let second_retained_at = first_retained_at + RETENTION + GRACE + 10;
    let now_ms = second_retained_at + 10;
    let (binding, first) = head_candidate(temporary.path(), &store, "first", 2, 1);
    let gate = store.try_acquire_publication_gate().expect("gate");
    let expired = store
        .stage_pending_physical_v3_admission(&gate, first)
        .expect("first head");
    let (second_binding, second) = head_candidate(temporary.path(), &store, "second", 3, 2);
    assert_eq!(second_binding, binding);
    let retained = store
        .rotate_and_activate_physical_v3_admission_with_delta_at(
            &gate,
            second,
            None,
            first_retained_at,
        )
        .expect("second head");
    let (_, third) = head_candidate(temporary.path(), &store, "third", 4, 3);
    let current = store
        .rotate_and_activate_physical_v3_admission_with_delta_at(
            &gate,
            third,
            None,
            second_retained_at,
        )
        .expect("third head");
    drop(gate);
    let (kept_paths, stray_copy) = write_kept_stages(&store, binding.vehicle_id);
    write_unreferenced_pack(&store, 0x0a, 700);
    let pinned = write_unreferenced_pack(&store, 0x0b, 300);
    store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO backup_pack_pins (pin_id, sha256, expires_at_ms) VALUES (?1, ?2, ?3)",
            params![Uuid::from_u128(29).to_string(), pinned, now_ms + 60_000],
        )
        .expect("backup pin");
    PublicFixture {
        _temporary: temporary,
        store,
        expired,
        retained,
        current,
        orphan_bytes: 700,
        pinned_bytes: 300,
        kept_paths,
        stray_copy,
        now_ms,
    }
}

fn bytes_of(files: &[(std::path::PathBuf, u64)]) -> u64 {
    files.iter().map(|(_, bytes)| bytes).sum()
}

#[test]
fn storage_census_counts_each_class_and_only_unneeded_bytes_as_reclaimable() {
    let fixture = public_fixture();
    let store = &fixture.store;
    let current = chunk_files(store, &fixture.current);
    let retained = chunk_files(store, &fixture.retained);
    let expired = chunk_files(store, &fixture.expired);
    let hashes_before = pack_object_hashes(store);

    let census = store.storage_census_at(fixture.now_ms).expect("census");

    assert_eq!(
        census.current_heads,
        StorageCensusClass {
            files: 4,
            bytes: bytes_of(&current),
            reclaimable_files: 0,
            reclaimable_bytes: 0,
        }
    );
    assert_eq!(
        census.retained_prior_heads,
        StorageCensusClass {
            files: 5,
            bytes: bytes_of(&retained) + bytes_of(&expired),
            reclaimable_files: 2,
            reclaimable_bytes: bytes_of(&expired),
        }
    );
    assert_eq!(
        census.import_bases,
        StorageCensusClass {
            files: 2,
            bytes: KEPT_STAGE_BYTES + KEPT_SOURCE_BYTES,
            reclaimable_files: 0,
            reclaimable_bytes: 0,
        }
    );
    assert_eq!(
        census.orphans,
        StorageCensusClass {
            files: 3,
            bytes: fixture.orphan_bytes + fixture.pinned_bytes + STRAY_COPY_BYTES,
            reclaimable_files: 2,
            reclaimable_bytes: fixture.orphan_bytes + STRAY_COPY_BYTES,
        }
    );
    assert_eq!(census.deltas, StorageCensusClass::default());
    assert_eq!(census.prepared_maps, StorageCensusClass::default());
    assert_eq!(census.total.files, 14);
    assert_eq!(
        census.total.reclaimable_bytes,
        bytes_of(&expired) + fixture.orphan_bytes + STRAY_COPY_BYTES
    );
    // Inside the delete grace an expired head is still needed: a download
    // that began before expiry may still be reading it.
    let inside_grace = store
        .storage_census_at(1_000 + RETENTION + GRACE - 1)
        .expect("census inside the grace");
    assert_eq!(inside_grace.retained_prior_heads.reclaimable_files, 0);

    // Counts and sizes only: no digest, path or identifier in the output.
    let printed = serde_json::to_string(&census).expect("census JSON");
    assert!(!printed.contains('/'));
    for name in hashes_before.keys() {
        assert!(!printed.contains(name.trim_end_matches(".sqlite.zst")));
    }
    assert!(!printed.contains(&fixture.current.vehicle_id.to_string()));
    // Read-only: nothing is removed or rewritten.
    assert_eq!(pack_object_hashes(store), hashes_before);
    assert_eq!(retained_receipts(store).len(), 2);
}

#[test]
fn storage_reclaim_removes_exactly_the_census_bytes_and_keeps_every_needed_pack() {
    let fixture = public_fixture();
    let store = &fixture.store;
    let expired = chunk_files(store, &fixture.expired);
    let census = store.storage_census_at(fixture.now_ms).expect("census");
    let mut kept = pack_object_hashes(store);
    for (path, _) in &expired {
        kept.remove(path.file_name().unwrap().to_str().unwrap());
    }
    kept.remove(&format!("{}.sqlite.zst", hex::encode([0x0a; 32])));
    let kept_stage_bytes = fixture
        .kept_paths
        .iter()
        .map(|path| std::fs::read(path).expect("kept file"))
        .collect::<Vec<_>>();

    let report = store.reclaim_storage_at(fixture.now_ms).expect("reclaim");

    assert_eq!(report.before, census);
    assert_eq!(report.reclaimed_files, census.total.reclaimable_files);
    assert_eq!(report.reclaimed_bytes, census.total.reclaimable_bytes);
    assert_eq!(report.after.total.reclaimable_bytes, 0);
    assert_eq!(report.after.total.bytes, census.total.bytes - report.reclaimed_bytes);
    // Every kept pack is byte for byte what it was.
    assert_eq!(pack_object_hashes(store), kept);
    assert_eq!(
        fixture
            .kept_paths
            .iter()
            .map(|path| std::fs::read(path).expect("kept file after reclaim"))
            .collect::<Vec<_>>(),
        kept_stage_bytes
    );
    assert!(!fixture.stray_copy.exists());
    assert!(
        store
            .packs_dir()
            .join("physical-kept/.staging/other-shape.txt")
            .is_file()
    );
    assert_eq!(retained_receipts(store), vec![fixture.retained.receipt_id.clone()]);
    assert_eq!(
        store
            .pending_physical_v3_admission_for_vehicle(fixture.current.vehicle_id)
            .expect("current head after reclaim"),
        Some(fixture.current.clone())
    );
    assert!(
        store
            .retained_physical_v3_admission_for_receipt_at(
                fixture.current.vehicle_id,
                &fixture.retained.receipt_id,
                fixture.now_ms,
                true,
            )
            .expect("retained head after reclaim")
            .is_some()
    );
    // A second run finds nothing left.
    let again = store.reclaim_storage_at(fixture.now_ms).expect("second reclaim");
    assert_eq!((again.reclaimed_files, again.reclaimed_bytes), (0, 0));
    assert_eq!(again.after, report.after);
}

#[test]
fn storage_reclaim_resumes_after_an_interrupted_run() {
    let fixture = public_fixture();
    let store = &fixture.store;
    let expired = chunk_files(store, &fixture.expired);
    let census = store.storage_census_at(fixture.now_ms).expect("census");
    // A run that stopped after its first removal, before the rows went.
    std::fs::remove_file(&expired[0].0).expect("first removal");
    let partial = store.storage_census_at(fixture.now_ms).expect("partial census");
    assert_eq!(
        partial.total.reclaimable_bytes,
        census.total.reclaimable_bytes - expired[0].1
    );
    assert_eq!(retained_receipts(store).len(), 2);

    let report = store.reclaim_storage_at(fixture.now_ms).expect("resumed reclaim");

    assert_eq!(report.reclaimed_files, census.total.reclaimable_files - 1);
    assert_eq!(
        report.reclaimed_bytes,
        census.total.reclaimable_bytes - expired[0].1
    );
    assert_eq!(report.after.total.reclaimable_files, 0);
    assert_eq!(retained_receipts(store), vec![fixture.retained.receipt_id.clone()]);
    assert!(expired.iter().all(|(path, _)| !path.exists()));
}

#[test]
fn storage_reclaim_keeps_a_blocked_rotations_predecessor_for_its_recovery() {
    let (temporary, store) = try_test_store().expect("store");
    let (_, first) = head_candidate(temporary.path(), &store, "first", 2, 1);
    let gate = store.try_acquire_publication_gate().expect("gate");
    let predecessor = store
        .stage_pending_physical_v3_admission(&gate, first)
        .expect("first head");
    let (_, second) = head_candidate(temporary.path(), &store, "second", 3, 2);
    let blocked = store
        .rotate_pending_physical_v3_admission_at(&gate, second, 1_000)
        .expect("blocked rotation");
    drop(gate);
    // The last moment of the predecessor's 24-hour retention.
    let now_ms = 1_000 + RETENTION - 1;
    let hashes = pack_object_hashes(&store);
    let census = store.storage_census_at(now_ms).expect("census");
    assert_eq!(census.current_heads.files, 3);
    assert_eq!(census.retained_prior_heads.files, 2);
    assert_eq!(census.total.reclaimable_files, 0);

    let report = store.reclaim_storage_at(now_ms).expect("reclaim");

    assert_eq!((report.reclaimed_files, report.reclaimed_bytes), (0, 0));
    assert_eq!(pack_object_hashes(&store), hashes);
    assert_eq!(retained_receipts(&store), vec![predecessor.receipt_id.clone()]);
    match store
        .physical_v3_publication_state_for_vehicle_at(blocked.vehicle_id, now_ms)
        .expect("recovery state after reclaim")
    {
        PhysicalV3PublicationState::Blocked { current, retained } => {
            assert_eq!(current, blocked);
            assert_eq!(retained.admission, predecessor);
        }
        _ => panic!("the blocked rotation is no longer recoverable"),
    }
}

/// Lab run: the census of a store copy named by `TESATLAS_HUB_CENSUS_STORE`,
/// opened read-only and printed as counts and sizes only.
#[test]
#[ignore = "lab: needs a store copy"]
fn storage_census_of_a_lab_store_copy() {
    let Some(data_dir) = std::env::var_os("TESATLAS_HUB_CENSUS_STORE") else {
        return;
    };
    let store = HubStore::open_read_only(&data_dir).expect("store copy");
    let census = store.storage_census().expect("census");
    println!("{}", serde_json::to_string(&census).expect("census JSON"));
    // With TESATLAS_HUB_CENSUS_RECLAIM set, the copy is then reclaimed and
    // the receipt printed.
    if std::env::var_os("TESATLAS_HUB_CENSUS_RECLAIM").is_some() {
        drop(store);
        let store = HubStore::initialize(&data_dir).expect("writable store copy");
        let report = store.reclaim_storage().expect("reclaim");
        println!("{}", serde_json::to_string(&report).expect("reclaim JSON"));
    }
}
