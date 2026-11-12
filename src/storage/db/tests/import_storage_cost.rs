// SPDX-License-Identifier: AGPL-3.0-only

//! M03-T03: storage cost repeats on the import finalizing path. Each test fails
//! if a removed repeat comes back, and shows that the stored output is unchanged.

use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;

/// The content-addressed pack objects under `packs/sha256`, by file name, with
/// the SHA-256 of their bytes.
fn pack_object_hashes(store: &HubStore) -> BTreeMap<String, String> {
    let directory = store.packs_dir().join("sha256");
    let mut hashes = BTreeMap::new();
    for entry in std::fs::read_dir(&directory).expect("pack objects") {
        let entry = entry.expect("pack object entry");
        if !entry.file_type().expect("pack object type").is_file() {
            continue;
        }
        let bytes = std::fs::read(entry.path()).expect("pack object bytes");
        hashes.insert(
            entry.file_name().to_string_lossy().into_owned(),
            hex::encode(Sha256::digest(&bytes)),
        );
    }
    hashes
}

fn physical_candidate(
    temporary: &std::path::Path,
    store: &HubStore,
    chunks: u32,
) -> (
    ProjectionBinding,
    crate::sync::physical_fragments::StagedPhysicalProjectionV3,
) {
    crate::sync::physical_fragments::tests::public_admission_candidate_fixture(
        &temporary.join("physical-fixture"),
        store,
        &CursorKey::from_bytes([23; 32]),
        chunks,
    )
}

/// Item (a): physical v3 admission verifies every pack once, and the admitted
/// catalogue row and pack objects are exactly what the candidate carried.
#[test]
fn storage_import_cost_physical_admission_verifies_each_pack_once_and_keeps_bytes() {
    let (temporary, store) = try_test_store().expect("store");
    let (binding, candidate) = physical_candidate(temporary.path(), &store, 3);
    let chunks = candidate.chunks.len();
    assert_eq!(chunks, 3);
    let expected = physical_v3_admission_from_manifest(&candidate.manifest, binding.selected_car_id)
        .expect("admission derived from the manifest alone");
    let packs_before = pack_object_hashes(&store);
    assert!(packs_before.len() >= chunks);
    let verifications_before = store.instrumentation().physical_v3_pack_verifications();

    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    let admitted = store
        .stage_pending_physical_v3_admission(&gate, candidate)
        .expect("physical admission");

    assert_eq!(
        store.instrumentation().physical_v3_pack_verifications() - verifications_before,
        chunks,
        "one full verification per physical pack"
    );
    assert_eq!(admitted, expected);
    assert_eq!(
        store
            .pending_physical_v3_admission_for_vehicle(binding.vehicle_id)
            .expect("pending admission read"),
        Some(expected)
    );
    assert_eq!(
        pack_object_hashes(&store),
        packs_before,
        "admission leaves every pack object byte-identical"
    );
}

/// Item (a), safety kept: the single verification still rejects a pack whose
/// bytes changed after it was built, and nothing is admitted.
#[test]
fn storage_import_cost_physical_admission_single_verification_rejects_tampering() {
    let (temporary, store) = try_test_store().expect("store");
    let (binding, candidate) = physical_candidate(temporary.path(), &store, 2);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&candidate.chunks[0].path)
        .expect("pack object")
        .write_all(b"tamper")
        .expect("append to pack");
    let gate = store
        .try_acquire_publication_gate()
        .expect("publication gate");
    assert!(matches!(
        store.stage_pending_physical_v3_admission(&gate, candidate),
        Err(StoreError::PhysicalV3Pack(_))
    ));
    assert!(
        store
            .pending_physical_v3_admission_for_vehicle(binding.vehicle_id)
            .expect("pending admission read")
            .is_none()
    );
}

/// Item (a), static guard: the admission check calls exactly one full pack
/// verification. A second call (such as the former profile verification ahead
/// of the purpose check) fails this test.
#[test]
fn storage_import_cost_physical_admission_source_has_one_pack_verification() {
    let source = include_str!("../physical_v3_admission.rs");
    let start = source
        .find("fn validate_physical_v3_candidate(")
        .expect("admission validation function");
    let rest = &source[start..];
    let end = rest[1..]
        .find("\n    fn ")
        .map(|offset| offset + 1)
        .expect("next method");
    let body = &rest[..end];
    assert_eq!(body.matches(".verify_with_limits(").count(), 0);
    assert_eq!(body.matches(".verify_reader(").count(), 0);
    assert_eq!(
        body.matches(".verify_hub_sync_v1_1_3_physical_purpose(").count(),
        1
    );
}
