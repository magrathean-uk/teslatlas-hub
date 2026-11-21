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
    let recorded = store
        .record_pack_offers_at(device, binding.vehicle_id, &offered.receipt_id, &offers, now_ms)
        .expect("offer leases");
    assert_eq!(recorded, PackOfferRecord::Recorded);
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
    // The changed set into the superseded head: that head moved, no lease.
    assert!(matches!(
        fixture.store.record_pack_offers_at(
            fixture.other_device,
            fixture.binding.vehicle_id,
            &fixture.offered.receipt_id,
            &stale_delta,
            at,
        ),
        Ok(PackOfferRecord::HeadMoved)
    ));
    // Bound to the current head, the old changed set is not catalogued.
    assert!(matches!(
        fixture.store.record_pack_offers_at(
            fixture.other_device,
            fixture.binding.vehicle_id,
            &fixture.current.receipt_id,
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
    // A generation change since selection moves the head as well.
    bump_generation(&fixture, 1);
    assert!(matches!(
        fixture.store.record_pack_offers_at(
            fixture.other_device,
            fixture.binding.vehicle_id,
            &fixture.current.receipt_id,
            &current,
            at,
        ),
        Ok(PackOfferRecord::HeadMoved)
    ));
    bump_generation(&fixture, -1);
    assert!(!leased(&fixture, fixture.other_device, fixture.delta_digests[0], at));
    fixture
        .store
        .record_pack_offers_at(
            fixture.other_device,
            fixture.binding.vehicle_id,
            &fixture.current.receipt_id,
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
            &fixture.current.receipt_id,
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
            &fixture.current.receipt_id,
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
            &fixture.current.receipt_id,
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

fn bump_generation(fixture: &Rotated, by: i64) {
    fixture
        .store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE sources SET generation = generation + ?2 WHERE source_id =
               (SELECT source_id FROM vehicles WHERE vehicle_id = ?1)",
            params![fixture.binding.vehicle_id.to_string(), by],
        )
        .expect("move generation");
}

#[test]
fn design_later_98_current_delta_lookup_requires_the_source_generation() {
    let fixture = rotated(0);
    let digest = fixture.delta_digests[0];
    let current = |fixture: &Rotated| {
        fixture
            .store
            .physical_v3_delta_pack_for_digest_at(
                digest,
                fixture.now_ms + 10,
                VehicleScope::Device(fixture.device),
            )
            .expect("current delta lookup")
            .is_some()
    };
    assert!(current(&fixture));
    // The target and base rows stay, but the source moved on: no delta.
    bump_generation(&fixture, 1);
    assert!(!current(&fixture));
    assert!(!leased(&fixture, fixture.device, digest, fixture.now_ms + 10));
    bump_generation(&fixture, -1);
    assert!(current(&fixture));
}

#[test]
fn design_later_98_offer_write_binds_the_selected_head_and_import_revision() {
    let fixture = rotated(0);
    let store = &fixture.store;
    let vehicle_id = fixture.binding.vehicle_id;
    let head = &fixture.current;
    let offers = [PackOffer { digest: head.manifest.chunks[0].sha256, kind: PackOfferKind::Physical }];
    let record = |at: i64| {
        store
            .record_pack_offers_at(fixture.other_device, vehicle_id, &head.receipt_id, &offers, at)
            .expect("offer write")
    };
    // An issue #60 import commits a revision with no head after the
    // selection: the selected head is stale and nothing is leased.
    let connection = store.open().expect("catalogue");
    let revision = |head: Option<Uuid>| PhysicalV3ImportRevision {
        run_id: Uuid::from_u128(0x60),
        kept: None,
        head,
    };
    put_physical_v3_import_revision_in(&connection, vehicle_id, &revision(None))
        .expect("import revision");
    assert_eq!(record(fixture.now_ms + 10), PackOfferRecord::HeadMoved);
    assert!(!leased(&fixture, fixture.other_device, offers[0].digest, fixture.now_ms + 10));
    // The revision publishes this head: the same offer is leased.
    put_physical_v3_import_revision_in(&connection, vehicle_id, &revision(Some(head.snapshot_id)))
        .expect("published revision");
    assert_eq!(record(fixture.now_ms + 10), PackOfferRecord::Recorded);
    assert!(leased(&fixture, fixture.other_device, offers[0].digest, fixture.now_ms + 10));
    // A rotation after selection: the old head is refused, the issued
    // lease stays.
    connection
        .execute(
            "DELETE FROM hub_metadata WHERE key = ?1",
            [physical_v3_import_revision_key(vehicle_id)],
        )
        .expect("no import revision");
    let gate = store.try_acquire_publication_gate().expect("gate");
    let (_, next) = candidate(fixture._temporary.path(), store, "moved", 1, head.head_sequence + 1);
    let next = store
        .rotate_pending_physical_v3_admission_at(&gate, next, fixture.now_ms + 11)
        .expect("rotation");
    store
        .activate_pending_physical_v3_rotation_at(&gate, vehicle_id, &head.receipt_id, fixture.now_ms + 11)
        .expect("rotation public");
    drop(gate);
    assert_ne!(next.receipt_id, head.receipt_id);
    assert_eq!(record(fixture.now_ms + 12), PackOfferRecord::HeadMoved);
    assert!(leased(&fixture, fixture.other_device, offers[0].digest, fixture.now_ms + 12));
}

#[test]
fn design_later_98_read_only_inventory_reads_a_v83_store_without_the_lease_table() {
    let temporary = crate::private_tempdir().expect("private test root");
    let data_dir = temporary.path().join("store");
    let store = HubStore::initialize(&data_dir).expect("store");
    // A base v83 catalogue before its first upgraded writable start.
    store
        .open()
        .expect("catalogue")
        .execute_batch("DROP TABLE device_pack_offers")
        .expect("base v83 shape");
    drop(store);
    let reader = HubStore::open_read_only(&data_dir).expect("read-only store");
    let inventory = reader.catalogue_inventory().expect("doctor inventory");
    assert_eq!(inventory.referenced_packs, 0);
    reader.storage_census().expect("census");
    let present: bool = reader
        .open_read_only_connection()
        .expect("reader")
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'device_pack_offers')",
            [],
            |row| row.get(0),
        )
        .expect("schema");
    assert!(!present, "the read-only path creates nothing");
}

fn catalogued_bytes(store: &HubStore, digest: Sha256Digest) -> i64 {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT compressed_bytes FROM pending_physical_v3_packs WHERE sha256 = ?1
             UNION ALL SELECT compressed_bytes FROM physical_v3_delta_packs WHERE sha256 = ?1
             LIMIT 1",
            [digest.to_string()],
            |row| row.get(0),
        )
        .expect("catalogued size")
}

#[test]
fn design_later_98_offer_byte_budgets_cap_each_device_and_the_store() {
    let fixture = rotated(0);
    let store = &fixture.store;
    let vehicle_id = fixture.binding.vehicle_id;
    let head = fixture.current.receipt_id.clone();
    // A fresh device: the fixture's own offer would count against the store.
    store.revoke_device_at(fixture.device, fixture.now_ms).expect("revoke fixture device");
    let objects = fixture
        .current
        .manifest
        .chunks
        .iter()
        .map(|chunk| PackOffer { digest: chunk.sha256, kind: PackOfferKind::Physical })
        .chain(fixture.delta_digests.iter().map(|digest| PackOffer {
            digest: *digest,
            kind: PackOfferKind::Delta,
        }))
        .collect::<Vec<_>>();
    assert!(objects.len() >= 3, "the fixture offers at least three objects");
    let size = |index: usize| catalogued_bytes(store, objects[index].digest);
    let at = fixture.now_ms + 10;
    let offer = |device: Uuid, budgets: PackOfferBudgets| {
        store
            .record_pack_offers_with_budgets_at(device, vehicle_id, &head, &objects, at, budgets)
            .expect("offer is served")
    };
    // Device cap: only the first object fits; the response is still served.
    let device_cap = PackOfferBudgets { device_bytes: size(0), store_bytes: i64::MAX };
    assert_eq!(offer(fixture.other_device, device_cap), PackOfferRecord::Recorded);
    assert!(leased(&fixture, fixture.other_device, objects[0].digest, at));
    for later in &objects[1..] {
        assert!(!leased(&fixture, fixture.other_device, later.digest, at));
    }
    // A re-offer of the leased object is not charged again.
    assert_eq!(offer(fixture.other_device, device_cap), PackOfferRecord::Recorded);
    assert!(leased(&fixture, fixture.other_device, objects[0].digest, at));
    // Store cap: the first object is already held across the store, so a
    // third device fits it and the second object, and nothing after.
    let third = pair(store, "third phone", at);
    let store_cap = PackOfferBudgets { device_bytes: i64::MAX, store_bytes: size(0) + size(1) };
    assert_eq!(offer(third, store_cap), PackOfferRecord::Recorded);
    assert!(leased(&fixture, third, objects[0].digest, at));
    assert!(leased(&fixture, third, objects[1].digest, at));
    for later in &objects[2..] {
        if objects[..2].iter().all(|held| held.digest != later.digest) {
            assert!(!leased(&fixture, third, later.digest, at));
        }
    }
    // The shipped budgets are 1 GiB per device and 4 GiB per store.
    assert_eq!((MAX_LEASED_BYTES_PER_DEVICE, MAX_LEASED_BYTES_PER_STORE), (1 << 30, 4 << 30));
}

#[test]
fn design_later_98_observed_expiry_survives_a_clock_moved_back_and_a_restart() {
    let fixture = rotated(1);
    let digest = fixture.delta_digests[0];
    let inside = fixture.now_ms + 10;
    assert!(leased(&fixture, fixture.device, digest, inside));
    // The lease is seen expired once...
    assert!(!leased(&fixture, fixture.device, digest, fixture.now_ms + RETENTION));
    // ...so a clock moved back into the window does not revive it. The row
    // stays as a tombstone until the grace and reclaim remove it.
    assert!(!leased(&fixture, fixture.device, digest, inside));
    let tombstone: (i64, i64) = fixture
        .store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT count(*), COALESCE(min(expired), 0) FROM device_pack_offers
              WHERE device_id = ?1 AND sha256 = ?2",
            rusqlite::params![fixture.device.to_string(), digest.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("tombstone");
    assert_eq!(tombstone, (1, 1));
    let reopened = HubStore::initialize(fixture._temporary.path().join("store")).expect("restart");
    assert!(
        reopened
            .offered_pack_for_digest_at(fixture.device, digest, inside)
            .expect("lease lookup")
            .is_none()
    );
    // The device's other leases are untouched.
    assert!(leased(&fixture, fixture.device, fixture.offered.manifest.chunks[0].sha256, inside));
}

#[test]
fn design_later_98_byte_budgets_count_expired_leases_inside_their_grace() {
    let fixture = rotated(0);
    let store = &fixture.store;
    let vehicle_id = fixture.binding.vehicle_id;
    let head = fixture.current.receipt_id.clone();
    store.revoke_device_at(fixture.device, fixture.now_ms).expect("revoke fixture device");
    let objects = fixture
        .current
        .manifest
        .chunks
        .iter()
        .map(|chunk| PackOffer { digest: chunk.sha256, kind: PackOfferKind::Physical })
        .collect::<Vec<_>>();
    assert!(objects.len() >= 2, "the fixture head has at least two chunks");
    // Each object fits alone; no two fit together.
    let cap = catalogued_bytes(store, objects[0].digest)
        .max(catalogued_bytes(store, objects[1].digest));
    let device_cap = PackOfferBudgets { device_bytes: cap, store_bytes: i64::MAX };
    let store_cap = PackOfferBudgets { device_bytes: i64::MAX, store_bytes: cap };
    let offer = |device: Uuid, object: usize, at: i64, budgets: PackOfferBudgets| {
        store
            .record_pack_offers_with_budgets_at(
                device,
                vehicle_id,
                &head,
                &objects[object..=object],
                at,
                budgets,
            )
            .expect("offer is served")
    };
    let rows = |object: usize| -> i64 {
        store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT count(*) FROM device_pack_offers WHERE sha256 = ?1",
                [objects[object].digest.to_string()],
                |row| row.get(0),
            )
            .expect("lease rows")
    };
    let start = fixture.now_ms + 10;
    assert_eq!(offer(fixture.other_device, 0, start, device_cap), PackOfferRecord::Recorded);
    assert!(leased(&fixture, fixture.other_device, objects[0].digest, start));
    // Expired, but inside its grace the old row still counts for the device
    // and for the store.
    let in_grace = start + RETENTION + 1;
    assert_eq!(offer(fixture.other_device, 1, in_grace, device_cap), PackOfferRecord::Recorded);
    let third = pair(store, "third phone", in_grace);
    assert_eq!(offer(third, 1, in_grace, store_cap), PackOfferRecord::Recorded);
    assert_eq!(rows(1), 0, "no new row joins the old one inside its grace");
    // A clock stepped back across the old expiry revives only the old row.
    assert!(leased(&fixture, fixture.other_device, objects[0].digest, start));
    assert_eq!((rows(0), rows(1)), (1, 0));
    // Past the grace the old row is gone and the next object fits.
    let after = start + RETENTION + GRACE + 1;
    assert_eq!(offer(fixture.other_device, 1, after, device_cap), PackOfferRecord::Recorded);
    assert_eq!((rows(0), rows(1)), (0, 1));
}
