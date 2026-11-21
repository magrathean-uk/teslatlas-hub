// SPDX-License-Identifier: AGPL-3.0-only

//! Issues #98 and #109: an offer lease keeps exactly the offered digest
//! fetchable by the device it was offered to, across rotations, until its
//! finite expiry; reclaim and backup keep the leased object exactly as long.

use super::*;

const SOURCE: &str = "pack-offers";
const RETENTION: i64 = RETIRED_LINEAGE_PACK_RETENTION_MS;
const GRACE: i64 = RETIRED_LINEAGE_PACK_DELETE_GRACE_MS;

struct Rotated {
    _temporary: tempfile::TempDir,
    store: HubStore,
    binding: ProjectionBinding,
    device: Uuid,
    other_device: Uuid,
    /// The head whose full manifest and A-to-B changed set were offered.
    offered: PendingPhysicalV3Admission,
    delta_digests: Vec<Sha256Digest>,
    current: PendingPhysicalV3Admission,
    now_ms: i64,
}

fn pair(store: &HubStore, name: &str, now_ms: i64) -> Uuid {
    let invitation = store
        .create_pairing(name, now_ms - 1, i64::MAX)
        .expect("invitation");
    store
        .claim_pairing(invitation.pairing_id, invitation.secret(), name, now_ms)
        .expect("paired device")
        .device_id
}

fn candidate(
    temporary: &std::path::Path,
    store: &HubStore,
    name: &str,
    updates: u32,
    sequence: u64,
) -> (
    ProjectionBinding,
    crate::sync::physical_fragments::StagedPhysicalProjectionV3,
) {
    crate::sync::physical_fragments::tests::public_admission_candidate_fixture_for_source_and_sequence(
        &temporary.join(name),
        store,
        &CursorKey::from_bytes([0x98; 32]),
        updates,
        SOURCE,
        sequence,
    )
}

/// Head A, then B with an A-to-B changed set; B's chunks and the changed
/// set are offered to `device`; then `rotations` more heads replace B.
fn rotated(rotations: u64) -> Rotated {
    let temporary = crate::private_tempdir().expect("private test root");
    let store = HubStore::initialize(temporary.path().join("store")).expect("store");
    let key = CursorKey::from_bytes([0x98; 32]);
    let now_ms = 1_800_000_000_000;
    let gate = store.try_acquire_publication_gate().expect("gate");
    let (binding, first) = candidate(temporary.path(), &store, "a", 1, 1);
    let first = store
        .stage_pending_physical_v3_admission(&gate, first)
        .expect("head A");
    let (_, second) = candidate(temporary.path(), &store, "b", 3, 2);
    let preview =
        crate::db::physical_v3_admission_from_manifest(&second.manifest, binding.selected_car_id)
            .expect("B identity");
    let delta = crate::import::source_db::physical_delta_pack::prepare_changed_set(
        &first,
        &preview,
        &binding,
        &key,
        Sha256Digest::of_bytes(b"pack offers target"),
        store.packs_dir(),
        0,
    )
    .expect("changed set");
    let delta_digests = delta.packs.iter().map(|pack| pack.sha256).collect::<Vec<_>>();
    let offered = store
        .rotate_pending_physical_v3_admission_with_delta_at(&gate, second, Some(delta), now_ms)
        .expect("head B");
    store
        .activate_pending_physical_v3_rotation_at(&gate, binding.vehicle_id, &first.receipt_id, now_ms)
        .expect("B public");
    drop(gate);
    let device = pair(&store, "offered phone", now_ms);
    let other_device = pair(&store, "other phone", now_ms);
    let offers = offered
        .manifest
        .chunks
        .iter()
        .map(|chunk| PackOffer { digest: chunk.sha256, kind: PackOfferKind::Physical })
        .chain(delta_digests.iter().map(|digest| PackOffer {
            digest: *digest,
            kind: PackOfferKind::Delta,
        }))
        .collect::<Vec<_>>();
    store
        .record_pack_offers_at(device, binding.vehicle_id, binding.generation, &offers, now_ms)
        .expect("offer leases");
    let mut current = offered.clone();
    for rotation in 0..rotations {
        let gate = store.try_acquire_publication_gate().expect("rotation gate");
        let at = now_ms + 1 + i64::try_from(rotation).unwrap();
        let (_, next) = candidate(
            temporary.path(),
            &store,
            &format!("r{rotation}"),
            1,
            current.head_sequence + 1,
        );
        let next = store
            .rotate_pending_physical_v3_admission_at(&gate, next, at)
            .expect("rotation");
        store
            .activate_pending_physical_v3_rotation_at(&gate, binding.vehicle_id, &current.receipt_id, at)
            .expect("rotation public");
        current = next;
    }
    Rotated {
        _temporary: temporary,
        store,
        binding,
        device,
        other_device,
        offered,
        delta_digests,
        current,
        now_ms,
    }
}

fn offered_digests(fixture: &Rotated) -> Vec<Sha256Digest> {
    fixture
        .offered
        .manifest
        .chunks
        .iter()
        .map(|chunk| chunk.sha256)
        .chain(fixture.delta_digests.iter().copied())
        .collect()
}

fn leased(fixture: &Rotated, device: Uuid, digest: Sha256Digest, at: i64) -> bool {
    fixture
        .store
        .offered_pack_for_digest_at(device, digest, at)
        .expect("lease lookup")
        .is_some()
}

fn object(store: &HubStore, digest: Sha256Digest) -> std::path::PathBuf {
    store.packs_dir().join("sha256").join(format!("{digest}.sqlite.zst"))
}

#[test]
fn design_later_98_offer_survives_one_and_two_rotations_until_expiry() {
    for rotations in [1, 2] {
        let fixture = rotated(rotations);
        assert_ne!(fixture.current.receipt_id, fixture.offered.receipt_id);
        // The changed-set rows went with the rotation; only the lease names them.
        assert!(
            fixture
                .store
                .physical_v3_delta_pack_for_digest_at(
                    fixture.delta_digests[0],
                    fixture.now_ms + 10,
                    VehicleScope::Unrestricted,
                )
                .expect("current delta lookup")
                .is_none()
        );
        for digest in offered_digests(&fixture) {
            let (pack, kind) = fixture
                .store
                .offered_pack_for_digest_at(fixture.device, digest, fixture.now_ms + 10)
                .expect("lease lookup")
                .expect("offered digest stays fetchable");
            assert_eq!(pack.digest, digest);
            assert_eq!(
                Sha256Digest::of_bytes(&std::fs::read(&pack.path).expect("leased bytes")),
                digest
            );
            assert_eq!(kind == PackOfferKind::Delta, fixture.delta_digests.contains(&digest));
            assert!(leased(&fixture, fixture.device, digest, fixture.now_ms + RETENTION - 1));
            assert!(!leased(&fixture, fixture.device, digest, fixture.now_ms + RETENTION));
            // A clock behind the offer is not inside the lease either.
            assert!(!leased(&fixture, fixture.device, digest, fixture.now_ms - 1));
            assert!(!leased(&fixture, fixture.other_device, digest, fixture.now_ms + 10));
        }
        // A digest never offered to the device is not leased, current or not.
        let offered = offered_digests(&fixture);
        for chunk in &fixture.current.manifest.chunks {
            if !offered.contains(&chunk.sha256) {
                assert!(!leased(&fixture, fixture.device, chunk.sha256, fixture.now_ms + 10));
            }
        }
        assert!(!leased(
            &fixture,
            fixture.device,
            Sha256Digest::of_bytes(b"never offered"),
            fixture.now_ms + 10
        ));
    }
}

#[test]
fn design_later_98_revocation_grant_and_generation_end_the_lease_at_once() {
    let fixture = rotated(1);
    let digest = fixture.delta_digests[0];
    let at = fixture.now_ms + 10;
    assert!(leased(&fixture, fixture.device, digest, at));
    let connection = fixture.store.open().expect("catalogue");
    connection
        .execute(
            "DELETE FROM device_vehicle_grants WHERE device_id = ?1",
            [fixture.device.to_string()],
        )
        .expect("remove grant");
    assert!(!leased(&fixture, fixture.device, digest, at));
    connection
        .execute(
            "INSERT INTO device_vehicle_grants(device_id, vehicle_id, granted_at_ms)
             VALUES (?1, ?2, ?3)",
            params![fixture.device.to_string(), fixture.binding.vehicle_id.to_string(), at],
        )
        .expect("grant again");
    assert!(leased(&fixture, fixture.device, digest, at));
    connection
        .execute(
            "UPDATE sources SET generation = generation + 1 WHERE source_id =
               (SELECT source_id FROM vehicles WHERE vehicle_id = ?1)",
            [fixture.binding.vehicle_id.to_string()],
        )
        .expect("bump generation");
    assert!(!leased(&fixture, fixture.device, digest, at));
    connection
        .execute(
            "UPDATE sources SET generation = generation - 1 WHERE source_id =
               (SELECT source_id FROM vehicles WHERE vehicle_id = ?1)",
            [fixture.binding.vehicle_id.to_string()],
        )
        .expect("restore generation");
    assert!(leased(&fixture, fixture.device, digest, at));
    fixture.store.revoke_device_at(fixture.device, at).expect("revoke");
    assert!(!leased(&fixture, fixture.device, digest, at));
}

#[test]
fn design_later_98_offer_is_refused_for_an_uncatalogued_digest_or_a_stale_binding() {
    let fixture = rotated(1);
    let at = fixture.now_ms + 10;
    let stale_delta = [PackOffer { digest: fixture.delta_digests[0], kind: PackOfferKind::Delta }];
    // The changed set left the catalogue with the rotation: no new lease.
    assert!(matches!(
        fixture.store.record_pack_offers_at(
            fixture.other_device,
            fixture.binding.vehicle_id,
            fixture.binding.generation,
            &stale_delta,
            at,
        ),
        Err(StoreError::PhysicalV3AdmissionConflict)
    ));
    let current = fixture
        .current
        .manifest
        .chunks
        .iter()
        .map(|chunk| PackOffer { digest: chunk.sha256, kind: PackOfferKind::Physical })
        .collect::<Vec<_>>();
    assert!(matches!(
        fixture.store.record_pack_offers_at(
            fixture.other_device,
            fixture.binding.vehicle_id,
            fixture.binding.generation + 1,
            &current,
            at,
        ),
        Err(StoreError::PhysicalV3AdmissionConflict)
    ));
    assert!(!leased(&fixture, fixture.other_device, fixture.delta_digests[0], at));
    fixture
        .store
        .record_pack_offers_at(
            fixture.other_device,
            fixture.binding.vehicle_id,
            fixture.binding.generation,
            &current,
            at,
        )
        .expect("current offer");
    // Re-offering keeps the later deadline; fetching renews nothing.
    fixture
        .store
        .record_pack_offers_at(
            fixture.other_device,
            fixture.binding.vehicle_id,
            fixture.binding.generation,
            &current,
            at - 5,
        )
        .expect("earlier re-offer");
    let digest = current[0].digest;
    assert!(leased(&fixture, fixture.other_device, digest, at + RETENTION - 1));
    assert!(!leased(&fixture, fixture.other_device, digest, at + RETENTION));
}

#[test]
fn design_later_98_reclaim_and_backup_keep_a_leased_object_until_expiry() {
    let fixture = rotated(2);
    let store = &fixture.store;
    let delta = fixture.delta_digests[0];
    // Inside the lease: reclaim keeps the changed-set object no row but the
    // lease names, and a backup carries it.
    let report = store.reclaim_storage_at(fixture.now_ms + 10).expect("reclaim in lease");
    assert!(report.before.deltas.files >= 1);
    assert!(object(store, delta).is_file());
    let gate = store.try_acquire_publication_gate().expect("backup gate");
    let backup = fixture._temporary.path().join("backup-in-lease");
    store
        .backup_to_with_gate(&backup, &gate, fixture.now_ms + 10)
        .expect("backup in lease");
    drop(gate);
    assert!(
        backup
            .join("packs/sha256")
            .join(format!("{delta}.sqlite.zst"))
            .is_file()
    );
    // After expiry and the delete grace the object and the lease go.
    let after = fixture.now_ms + RETENTION + GRACE + 10;
    let gate = store.try_acquire_publication_gate().expect("expired backup gate");
    let expired_backup = fixture._temporary.path().join("backup-expired");
    store
        .backup_to_with_gate(&expired_backup, &gate, after)
        .expect("backup after expiry");
    drop(gate);
    assert!(
        !expired_backup
            .join("packs/sha256")
            .join(format!("{delta}.sqlite.zst"))
            .exists()
    );
    store.reclaim_storage_at(after).expect("reclaim after expiry");
    assert!(!object(store, delta).exists());
    let leases: i64 = store
        .open()
        .expect("catalogue")
        .query_row("SELECT count(*) FROM device_pack_offers", [], |row| row.get(0))
        .expect("lease count");
    assert_eq!(leases, 0);
    // The current head is untouched.
    for chunk in &fixture.current.manifest.chunks {
        assert!(object(store, chunk.sha256).is_file());
    }
}

#[test]
fn design_later_98_revoked_lease_frees_its_object_at_the_next_reclaim() {
    let fixture = rotated(1);
    let store = &fixture.store;
    let delta = fixture.delta_digests[0];
    store.reclaim_storage_at(fixture.now_ms + 10).expect("reclaim in lease");
    assert!(object(store, delta).is_file());
    store
        .revoke_device_at(fixture.device, fixture.now_ms + 11)
        .expect("revoke");
    store.reclaim_storage_at(fixture.now_ms + 12).expect("reclaim after revoke");
    assert!(!object(store, delta).exists());
}

/// A 100-byte prepared pack object and its catalogue row for the current
/// head of `fixture`, as publication would leave them.
fn prepared_object(fixture: &Rotated, fill: u8) -> Sha256Digest {
    let bytes = vec![fill; 100];
    let digest = Sha256Digest::of_bytes(&bytes);
    std::fs::write(object(&fixture.store, digest), &bytes).expect("prepared object");
    fixture
        .store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO prepared_map_months
             (artifact_id, vehicle_id, month, input_manifest_id, input_receipt_id,
              input_sequence, source_digest, pack_sha256, compressed_bytes, receipt_json)
             VALUES (?1, ?2, ?8, ?3, ?4, ?5, ?6, ?7, 100, X'7B7D')",
            params![
                format!("map-month-v1.{}", hex::encode([fill; 32])),
                fixture.binding.vehicle_id.to_string(),
                fixture.current.snapshot_id.to_string(),
                fixture.current.receipt_id,
                i64::try_from(fixture.current.head_sequence).unwrap(),
                "b".repeat(64),
                digest.to_string(),
                format!("2026-{:02}", fill % 12 + 1),
            ],
        )
        .expect("prepared row");
    digest
}

#[test]
fn design_later_109_prepared_lease_outlives_its_catalogue_row_until_expiry() {
    let fixture = rotated(0);
    let store = &fixture.store;
    let digest = prepared_object(&fixture, 0x61);
    let never = prepared_object(&fixture, 0x62);
    store
        .record_pack_offers_at(
            fixture.device,
            fixture.binding.vehicle_id,
            fixture.binding.generation,
            &[PackOffer { digest, kind: PackOfferKind::Prepared }],
            fixture.now_ms,
        )
        .expect("prepared lease");
    // Publication prunes the month once its head is no longer retained.
    store
        .open()
        .expect("catalogue")
        .execute("DELETE FROM prepared_map_months", [])
        .expect("prune months");
    let (pack, kind) = store
        .offered_pack_for_digest_at(fixture.device, digest, fixture.now_ms + 10)
        .expect("lease lookup")
        .expect("issued prepared pack stays fetchable");
    assert_eq!((pack.digest, pack.compressed_bytes, kind), (digest, 100, PackOfferKind::Prepared));
    assert!(!leased(&fixture, fixture.other_device, digest, fixture.now_ms + 10));
    assert!(!leased(&fixture, fixture.device, never, fixture.now_ms + 10));
    // A digest no catalogue row names can no longer be offered.
    assert!(matches!(
        store.record_pack_offers_at(
            fixture.other_device,
            fixture.binding.vehicle_id,
            fixture.binding.generation,
            &[PackOffer { digest, kind: PackOfferKind::Prepared }],
            fixture.now_ms + 10,
        ),
        Err(StoreError::PhysicalV3AdmissionConflict)
    ));
    store.reclaim_storage_at(fixture.now_ms + 10).expect("reclaim in lease");
    assert!(object(store, digest).is_file());
    assert!(!object(store, never).exists());
    let gate = store.try_acquire_publication_gate().expect("backup gate");
    let backup = fixture._temporary.path().join("prepared-backup");
    store
        .backup_to_with_gate(&backup, &gate, fixture.now_ms + 10)
        .expect("backup in lease");
    drop(gate);
    assert!(backup.join("packs/sha256").join(format!("{digest}.sqlite.zst")).is_file());
    assert!(!leased(&fixture, fixture.device, digest, fixture.now_ms + RETENTION));
    store
        .reclaim_storage_at(fixture.now_ms + RETENTION + GRACE)
        .expect("reclaim after expiry");
    assert!(!object(store, digest).exists());
}
