// SPDX-License-Identifier: AGPL-3.0-only

// The process-wide verified lineage-digest memory. Real SQLite stores and
// real pack files; the store's hash log records every full hash, so a test
// states exactly which packs were read from disk. Synthetic data only.

/// An imported base plus one committed collector delta: two real packs.
fn lineage_digest_fixture(store: &HubStore) -> (Uuid, TransportPack, TransportPack) {
    let (vehicle, binding, base) = imported_v2_base(store);
    let (claim, delta) = claimed_collector_delta(store, vehicle.vehicle_id, &binding);
    store
        .commit_v2_delta_claim(
            &claim,
            &delta,
            &import_delta_test_cursor_key(),
            &import_delta_test_cursor(&binding, delta.to_sequence),
        )
        .expect("commit the fixture delta");
    (vehicle.vehicle_id, base.base.packs[0].clone(), delta.pack)
}

fn lineage_digest_pack_path(store: &HubStore, pack: &TransportPack) -> PathBuf {
    store
        .packs_dir()
        .join("sha256")
        .join(format!("{}.sqlite.zst", pack.sha256))
}

fn lineage_digest_read(store: &HubStore, vehicle_id: Uuid) -> LineageManifestV2 {
    store
        .lineage_manifest_for_vehicle(vehicle_id)
        .expect("lineage read")
        .expect("published lineage")
}

fn lineage_digest_hashes_of(store: &HubStore, digest: Sha256Digest) -> usize {
    store
        .lineage_pack_hash_log()
        .iter()
        .filter(|hashed| **hashed == digest)
        .count()
}

fn lineage_digest_test_key(inode: u64) -> LineageDigestKey {
    LineageDigestKey {
        device: 1,
        inode,
        length: 10,
        modified_seconds: 2,
        modified_nanoseconds: 3,
        changed_seconds: 4,
        changed_nanoseconds: 5,
        expected: Sha256Digest::of_bytes(b"pack"),
    }
}

#[test]
fn stable_lineage_packs_are_hashed_once_per_process() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle_id, base, delta) = lineage_digest_fixture(&store);

    let first = lineage_digest_read(&store, vehicle_id);
    assert_eq!(first.base.packs, vec![base.clone()]);
    assert_eq!(
        first
            .deltas
            .iter()
            .map(|d| d.pack.clone())
            .collect::<Vec<_>>(),
        vec![delta.clone()]
    );
    let hashed_up_to_first_read = store.lineage_pack_hash_log();
    assert_eq!(
        hashed_up_to_first_read,
        vec![base.sha256, delta.sha256],
        "each pack is hashed once, base then delta, however it got verified first"
    );

    for _ in 0..3 {
        assert_eq!(lineage_digest_read(&store, vehicle_id), first);
    }
    assert_eq!(
        store.v2_lineage_pack_count(vehicle_id).expect("pack count"),
        2
    );
    assert_eq!(
        store.lineage_pack_hash_log(),
        hashed_up_to_first_read,
        "later reads of stable packs hash nothing"
    );
}

#[test]
fn a_modified_lineage_pack_is_verified_again_and_corruption_still_fails() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle_id, base, delta) = lineage_digest_fixture(&store);
    let published = lineage_digest_read(&store, vehicle_id);
    let base_path = lineage_digest_pack_path(&store, &base);
    let original = fs::read(&base_path).expect("base pack bytes");
    let warmed = store.lineage_pack_hash_log();
    assert_eq!(warmed, vec![base.sha256, delta.sha256]);

    let mut corrupt = original.clone();
    corrupt[0] ^= 0xff;
    fs::write(&base_path, &corrupt).expect("same-length corruption");
    for attempt in 1..=2 {
        assert!(matches!(
            store.lineage_manifest_for_vehicle(vehicle_id),
            Err(StoreError::LineagePackDigestMismatch)
        ));
        assert_eq!(
            lineage_digest_hashes_of(&store, base.sha256),
            1 + attempt,
            "a corrupt pack is hashed on every read and never remembered"
        );
    }
    assert_eq!(lineage_digest_hashes_of(&store, delta.sha256), 1);

    fs::write(&base_path, &original).expect("restore the pack");
    assert_eq!(lineage_digest_read(&store, vehicle_id), published);
    assert_eq!(lineage_digest_hashes_of(&store, base.sha256), 4);
    assert_eq!(
        lineage_digest_hashes_of(&store, delta.sha256),
        1,
        "the untouched delta keeps its remembered verdict"
    );
    assert_eq!(lineage_digest_read(&store, vehicle_id), published);
    assert_eq!(
        store.lineage_pack_hash_log().len(),
        5,
        "base and delta once each, then the base three more times"
    );
}

#[test]
fn a_pack_with_a_new_length_is_refused_without_hashing_and_verified_again_once_restored() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle_id, base, _delta) = lineage_digest_fixture(&store);
    let published = lineage_digest_read(&store, vehicle_id);
    let base_path = lineage_digest_pack_path(&store, &base);
    let original = fs::read(&base_path).expect("base pack bytes");
    let hashed = store.lineage_pack_hash_log();

    let mut longer = original.clone();
    longer.push(0);
    fs::write(&base_path, &longer).expect("append one byte");
    assert!(matches!(
        store.lineage_manifest_for_vehicle(vehicle_id),
        Err(StoreError::LineagePackNotReady)
    ));
    assert_eq!(
        store.lineage_pack_hash_log(),
        hashed,
        "the length check needs no hash"
    );

    fs::write(&base_path, &original).expect("restore the pack");
    assert_eq!(lineage_digest_read(&store, vehicle_id), published);
    assert_eq!(lineage_digest_hashes_of(&store, base.sha256), 2);
}

#[test]
fn an_in_place_rewrite_with_the_modification_time_put_back_is_detected() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle_id, base, _delta) = lineage_digest_fixture(&store);
    lineage_digest_read(&store, vehicle_id);
    let base_path = lineage_digest_pack_path(&store, &base);
    let before = fs::metadata(&base_path).expect("pack metadata");
    let hashed = lineage_digest_hashes_of(&store, base.sha256);

    let mut corrupt = fs::read(&base_path).expect("base pack bytes");
    corrupt[0] ^= 0xff;
    fs::write(&base_path, &corrupt).expect("same-length corruption");
    OpenOptions::new()
        .write(true)
        .open(&base_path)
        .expect("reopen pack")
        .set_modified(before.modified().expect("modification time"))
        .expect("put the modification time back");
    let after = fs::metadata(&base_path).expect("pack metadata after rewrite");
    assert_eq!(after.len(), before.len());
    assert_eq!(
        (after.mtime(), after.mtime_nsec()),
        (before.mtime(), before.mtime_nsec()),
        "the rewrite looks untouched by its modification time"
    );
    assert_ne!(
        (after.ctime(), after.ctime_nsec()),
        (before.ctime(), before.ctime_nsec()),
        "the filesystem still records the change"
    );

    assert!(matches!(
        store.lineage_manifest_for_vehicle(vehicle_id),
        Err(StoreError::LineagePackDigestMismatch)
    ));
    assert_eq!(lineage_digest_hashes_of(&store, base.sha256), hashed + 1);
}

#[test]
fn a_pack_rewritten_while_it_is_hashed_is_not_remembered() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle_id, base, delta) = lineage_digest_fixture(&store);
    let published = lineage_digest_read(&store, vehicle_id);
    let base_path = lineage_digest_pack_path(&store, &base);
    let hashed_before = lineage_digest_hashes_of(&store, base.sha256);
    let moved_modification_time = |offset: i64| {
        let when = if offset < 0 {
            std::time::SystemTime::now() - std::time::Duration::from_secs(offset.unsigned_abs())
        } else {
            std::time::SystemTime::now() + std::time::Duration::from_secs(offset.unsigned_abs())
        };
        move |path: &Path| {
            fs::OpenOptions::new()
                .write(true)
                .open(path)
                .expect("open the pack")
                .set_modified(when)
                .expect("move the modification time");
        }
    };

    // A changed modification time makes the next read hash the base pack. The
    // seam moves it again right after the bytes were hashed: nothing about
    // the bytes or the length changed, yet the file is no longer the one that
    // was opened, so the verdict must not be kept.
    moved_modification_time(-120)(&base_path);
    AFTER_LINEAGE_HASH
        .with(|slot| *slot.borrow_mut() = Some(Box::new(moved_modification_time(60))));
    assert!(matches!(
        store.lineage_manifest_for_vehicle(vehicle_id),
        Err(StoreError::LineagePackNotReady)
    ));
    assert!(
        AFTER_LINEAGE_HASH.with(|slot| slot.borrow().is_none()),
        "the seam ran"
    );
    assert_eq!(
        lineage_digest_hashes_of(&store, base.sha256),
        hashed_before + 1
    );

    // The identity it ended with was never remembered, so the next read
    // hashes the pack again, and only then is the verdict kept.
    assert_eq!(lineage_digest_read(&store, vehicle_id), published);
    assert_eq!(
        lineage_digest_hashes_of(&store, base.sha256),
        hashed_before + 2
    );
    assert_eq!(lineage_digest_read(&store, vehicle_id), published);
    assert_eq!(
        lineage_digest_hashes_of(&store, base.sha256),
        hashed_before + 2
    );
    assert_eq!(
        lineage_digest_hashes_of(&store, delta.sha256),
        1,
        "the untouched delta keeps its remembered verdict"
    );
}

#[test]
fn a_remembered_digest_does_not_excuse_a_length_the_catalogue_does_not_record() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (vehicle_id, base, _delta) = lineage_digest_fixture(&store);
    lineage_digest_read(&store, vehicle_id);
    let hashed = store.lineage_pack_hash_log();
    store
        .verify_lineage_pack(&base)
        .expect("the remembered pack still verifies");

    for compressed_bytes in [base.compressed_bytes + 1, base.compressed_bytes - 1] {
        let recorded = TransportPack {
            compressed_bytes,
            ..base.clone()
        };
        assert!(
            matches!(
                store.verify_lineage_pack(&recorded),
                Err(StoreError::LineagePackNotReady)
            ),
            "recorded length {compressed_bytes} differs from the file"
        );
    }
    let renamed = TransportPack {
        relative_path: "packs/sha256/other.sqlite.zst".to_owned(),
        ..base.clone()
    };
    assert!(matches!(
        store.verify_lineage_pack(&renamed),
        Err(StoreError::LineagePackNotReady)
    ));
    assert_eq!(store.lineage_pack_hash_log(), hashed);
}

#[test]
fn a_remembered_verdict_is_bound_to_the_digest_it_was_verified_against() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (_vehicle_id, base, _delta) = lineage_digest_fixture(&store);
    let wrong_digest = Sha256Digest::of_bytes(b"a digest the pack bytes do not have");
    let wrong_name = TransportPack {
        pack_id: Uuid::new_v4(),
        sha256: wrong_digest,
        relative_path: TransportPack::canonical_relative_path(wrong_digest),
        ..base.clone()
    };
    // The same inode, with the same length and times, under another name.
    fs::hard_link(
        lineage_digest_pack_path(&store, &base),
        lineage_digest_pack_path(&store, &wrong_name),
    )
    .expect("second name for the pack file");

    store
        .verify_lineage_pack(&base)
        .expect("the pack verifies under its own name");
    let hashed = store.lineage_pack_hash_log().len();
    assert!(matches!(
        store.verify_lineage_pack(&wrong_name),
        Err(StoreError::LineagePackDigestMismatch)
    ));
    assert_eq!(
        store.lineage_pack_hash_log()[hashed..],
        [wrong_digest],
        "the other digest was checked against the bytes, not served from memory"
    );
    store
        .verify_lineage_pack(&base)
        .expect("the real name is still remembered");
    assert_eq!(store.lineage_pack_hash_log().len(), hashed + 1);
}

#[test]
fn hashing_a_lineage_pack_refuses_a_link_and_a_length_the_catalogue_does_not_record() {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let (_vehicle_id, base, _delta) = lineage_digest_fixture(&store);
    let real = lineage_digest_pack_path(&store, &base);
    let link = temporary.path().join("pack-link");
    std::os::unix::fs::symlink(&real, &link).expect("link to the pack");
    let hashed = store.lineage_pack_hash_log().len();

    assert!(matches!(
        store.hash_lineage_pack(&base, &link),
        Err(StoreError::OpenBackupPack(_))
    ));
    let longer = TransportPack {
        compressed_bytes: base.compressed_bytes + 1,
        ..base.clone()
    };
    assert!(matches!(
        store.hash_lineage_pack(&longer, &real),
        Err(StoreError::LineagePackNotReady)
    ));
    assert_eq!(
        store.lineage_pack_hash_log().len(),
        hashed,
        "neither file was read"
    );
    store
        .hash_lineage_pack(&base, &real)
        .expect("the real file hashes");
    assert_eq!(store.lineage_pack_hash_log().len(), hashed + 1);
}

#[test]
fn the_lineage_digest_key_covers_the_whole_file_identity_and_the_expected_digest() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let path = temporary.path().join("pack");
    fs::write(&path, b"identity").expect("pack file");
    OpenOptions::new()
        .write(true)
        .open(&path)
        .expect("reopen pack")
        .set_modified(UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789))
        .expect("set modification time");
    let metadata = fs::symlink_metadata(&path).expect("pack metadata");
    let digest = Sha256Digest::of_bytes(b"identity");

    let key = LineageDigestKey::new(&metadata, digest);
    assert_eq!(
        key,
        LineageDigestKey {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: 8,
            modified_seconds: 1_700_000_000,
            modified_nanoseconds: 123_456_789,
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
            expected: digest,
        }
    );

    let mut verified = VerifiedLineageDigests::new(16);
    verified.insert(key);
    assert!(verified.contains(&key));
    for (field, variant) in [
        (
            "device",
            LineageDigestKey {
                device: key.device + 1,
                ..key
            },
        ),
        (
            "inode",
            LineageDigestKey {
                inode: key.inode + 1,
                ..key
            },
        ),
        (
            "length",
            LineageDigestKey {
                length: key.length + 1,
                ..key
            },
        ),
        (
            "modification seconds",
            LineageDigestKey {
                modified_seconds: key.modified_seconds + 1,
                ..key
            },
        ),
        (
            "modification nanoseconds",
            LineageDigestKey {
                modified_nanoseconds: key.modified_nanoseconds + 1,
                ..key
            },
        ),
        (
            "status-change seconds",
            LineageDigestKey {
                changed_seconds: key.changed_seconds + 1,
                ..key
            },
        ),
        (
            "status-change nanoseconds",
            LineageDigestKey {
                changed_nanoseconds: key.changed_nanoseconds + 1,
                ..key
            },
        ),
        (
            "expected digest",
            LineageDigestKey {
                expected: Sha256Digest::of_bytes(b"other"),
                ..key
            },
        ),
    ] {
        assert_ne!(variant, key, "{field} is part of the key");
        assert!(!verified.contains(&variant), "a new {field} is a miss");
    }
}

#[test]
fn the_verified_lineage_digest_set_evicts_the_least_recently_used() {
    let mut verified = VerifiedLineageDigests::new(3);
    for inode in 1..=3 {
        verified.insert(lineage_digest_test_key(inode));
    }
    assert!(
        verified.contains(&lineage_digest_test_key(1)),
        "a hit refreshes"
    );

    verified.insert(lineage_digest_test_key(4));
    assert_eq!(verified.len(), 3);
    assert!(
        !verified.contains(&lineage_digest_test_key(2)),
        "the oldest went"
    );
    assert!(verified.contains(&lineage_digest_test_key(1)));
    assert!(verified.contains(&lineage_digest_test_key(3)));
    assert!(verified.contains(&lineage_digest_test_key(4)));

    verified.insert(lineage_digest_test_key(4));
    assert_eq!(verified.len(), 3, "remembering a key twice keeps one entry");
    verified.insert(lineage_digest_test_key(5));
    assert_eq!(verified.len(), 3);
    assert!(!verified.contains(&lineage_digest_test_key(1)));
    assert!(verified.contains(&lineage_digest_test_key(5)));
}

#[test]
fn the_verified_lineage_digest_set_never_exceeds_its_capacity() {
    let mut verified = VerifiedLineageDigests::new(16);
    for inode in 0..1_000 {
        verified.insert(lineage_digest_test_key(inode));
        assert!(verified.len() <= verified.capacity());
        assert_eq!(verified.recency.len(), verified.by_recency.len());
    }
    assert_eq!(verified.len(), 16);
    for inode in 0..984 {
        assert!(!verified.contains(&lineage_digest_test_key(inode)));
    }
    for inode in 984..1_000 {
        assert!(verified.contains(&lineage_digest_test_key(inode)));
    }
    assert_eq!(VerifiedLineageDigests::new(0).capacity(), 1);
}

#[test]
fn the_process_wide_lineage_digest_set_is_sized_above_one_whole_lineage() {
    assert_eq!(LINEAGE_DIGEST_CACHE_CAPACITY, 2_048);
    assert!(LINEAGE_DIGEST_CACHE_CAPACITY > ProtocolLimits::default().max_chunks);
    let shared = verified_lineage_digests().lock().expect("shared set");
    assert_eq!(shared.capacity(), LINEAGE_DIGEST_CACHE_CAPACITY);
    assert!(shared.len() <= LINEAGE_DIGEST_CACHE_CAPACITY);
}
