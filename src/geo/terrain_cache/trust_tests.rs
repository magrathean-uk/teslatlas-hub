// SPDX-License-Identifier: AGPL-3.0-only

//! G2-3f: the cache root must be a trusted private directory, and a cached
//! tile is used only when its digest matches the one recorded at download.
//! Every root here is a temporary directory; nothing reads or writes the real
//! user's cache.

use std::os::unix::fs::{PermissionsExt, symlink};

use super::*;

/// An HGT whose cell at (51.5, -0.1) holds `elevation` metres.
fn tile_with_elevation(elevation: i16) -> Vec<u8> {
    let mut bytes = hgt_bytes();
    let row = 600_usize;
    let column = 1_080_usize;
    let offset = (row * SRTM3_SIDE + column) * 2;
    bytes[offset..offset + 2].copy_from_slice(&elevation.to_be_bytes());
    bytes
}

fn zip_of(bytes: &[u8]) -> Vec<u8> {
    let mut output = std::io::Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(&mut output);
    archive
        .start_file("N51W001.hgt", zip::write::SimpleFileOptions::default())
        .unwrap();
    archive.write_all(bytes).unwrap();
    archive.finish().unwrap();
    output.into_inner()
}

/// A provider that has no AWS copy and serves `bytes` from ESA, counting
/// every request it receives.
async fn serving(bytes: Vec<u8>) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    let archive = zip_of(&bytes);
    let (endpoint, task) = server(move |uri| {
        counted.fetch_add(1, Ordering::SeqCst);
        if uri.path().ends_with(".gz") {
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap()
        } else {
            Response::new(Body::from(archive.clone()))
        }
    })
    .await;
    (endpoint, requests, task)
}

fn tile_id() -> TileId {
    TileId::from_coordinates(51.5, -0.1).unwrap()
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn unusable(root: &Path, store: &HubStore) -> TerrainCacheError {
    match TerrainCache::new(options(root, "http://127.0.0.1:1/", store)) {
        Ok(_) => panic!("the cache root {} must be rejected", root.display()),
        Err(error) => error,
    }
}

// ---- cache root trust ----------------------------------------------------

#[test]
fn a_symlinked_root_is_rejected_and_nothing_is_created_through_it() {
    let base = tempdir().unwrap();
    let (_data, store) = test_store();
    let real = base.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    let link = base.path().join("link");
    symlink(&real, &link).unwrap();

    let error = unusable(&link, &store);

    assert!(
        matches!(&error, TerrainCacheError::UntrustedRoot(PrivateDirectoryError::Symlink(path))
            if path.ends_with("link")),
        "{error:?}"
    );
    assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
}

#[test]
fn a_dangling_symlink_as_the_root_is_rejected_and_its_target_is_not_created() {
    let base = tempdir().unwrap();
    let (_data, store) = test_store();
    let target = base.path().join("target");
    let link = base.path().join("link");
    symlink(&target, &link).unwrap();

    let error = unusable(&link, &store);

    assert!(
        matches!(
            error,
            TerrainCacheError::UntrustedRoot(PrivateDirectoryError::Symlink(_))
        ),
        "{error:?}"
    );
    assert!(!target.exists());
}

#[test]
fn a_root_that_is_a_file_is_rejected() {
    let base = tempdir().unwrap();
    let (_data, store) = test_store();
    let file = base.path().join("terrain");
    fs::write(&file, b"not a directory").unwrap();

    assert!(matches!(
        unusable(&file, &store),
        TerrainCacheError::UntrustedRoot(PrivateDirectoryError::NotDirectory(_))
    ));
    assert_eq!(fs::read(&file).unwrap(), b"not a directory");
}

#[test]
fn a_relative_root_is_rejected_without_creating_anything() {
    let (_data, store) = test_store();
    let relative = Path::new("teslatlas-terrain-relative-root-test/terrain");

    assert!(matches!(
        unusable(relative, &store),
        TerrainCacheError::UntrustedRoot(PrivateDirectoryError::InvalidPath)
    ));
    assert!(!Path::new("teslatlas-terrain-relative-root-test").exists());
}

#[test]
fn a_root_with_a_parent_component_is_rejected() {
    let base = tempdir().unwrap();
    let (_data, store) = test_store();
    let escaping = base.path().join("a/../terrain");

    assert!(matches!(
        unusable(&escaping, &store),
        TerrainCacheError::UntrustedRoot(PrivateDirectoryError::InvalidPath)
    ));
    assert!(!base.path().join("terrain").exists());
}

/// The ACL entries `ls -le` shows for `path`.
#[cfg(target_os = "macos")]
fn acl_entries(path: &Path) -> Vec<String> {
    let output = std::process::Command::new("/bin/ls")
        .arg("-lde")
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .skip(1)
        .map(|line| line.trim().to_owned())
        .collect()
}

#[cfg(target_os = "macos")]
#[test]
fn a_root_whose_acl_admits_others_is_cleared_when_the_cache_opens() {
    let base = tempdir().unwrap();
    let (_data, store) = test_store();
    let root = base.path().join("terrain");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let status = std::process::Command::new("/bin/chmod")
        .arg("+a")
        .arg("group:everyone allow list,search,add_file,delete_child")
        .arg(&root)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(acl_entries(&root).len(), 1, "the fixture ACL is in place");

    TerrainCache::new(options(&root, "http://127.0.0.1:1/", &store))
        .expect("an owner-only root with a stray ACL is cleaned, not refused");

    assert!(acl_entries(&root).is_empty());
    assert_eq!(mode_of(&root), 0o700);
}

#[test]
fn a_wide_root_owned_by_the_user_is_tightened_to_0700_and_works() {
    for wide in [0o775_u32, 0o770, 0o777, 0o755] {
        let base = tempdir().unwrap();
        let (_data, store) = test_store();
        let root = base.path().join("terrain");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(wide)).unwrap();

        let cache = TerrainCache::new(options(&root, "http://127.0.0.1:1/", &store))
            .unwrap_or_else(|error| panic!("mode {wide:o} must be tightened: {error:?}"));

        assert_eq!(mode_of(&root), 0o700, "mode {wide:o}");
        assert_eq!(cache.root_path().file_name().unwrap(), "terrain");
    }
}

#[test]
fn a_missing_root_is_created_private_below_an_existing_ancestor() {
    // The default roots have two missing levels: `TeslatlasHub/terrain` below
    // the user cache directory and `cache/terrain` below the data directory.
    let base = tempdir().unwrap();
    let (_data, store) = test_store();
    let root = base.path().join("TeslatlasHub/terrain");

    let cache = TerrainCache::new(options(&root, "http://127.0.0.1:1/", &store)).unwrap();

    assert_eq!(mode_of(&base.path().join("TeslatlasHub")), 0o700);
    assert_eq!(mode_of(&root), 0o700);
    assert!(cache.root_path().ends_with("TeslatlasHub/terrain"));
}

#[test]
fn a_group_writable_ancestor_owned_by_the_user_is_accepted() {
    let base = tempdir().unwrap();
    let (_data, store) = test_store();
    let ancestor = base.path().join("shared");
    fs::create_dir(&ancestor).unwrap();
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o775)).unwrap();

    TerrainCache::new(options(
        &ancestor.join("terrain"),
        "http://127.0.0.1:1/",
        &store,
    ))
    .unwrap();

    assert_eq!(mode_of(&ancestor), 0o775);
    assert_eq!(mode_of(&ancestor.join("terrain")), 0o700);
}

#[test]
fn a_root_below_a_symlinked_ancestor_is_validated_where_the_link_leads() {
    let base = tempdir().unwrap();
    let (_data, store) = test_store();
    let real = base.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::set_permissions(&real, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&real, base.path().join("link")).unwrap();

    let cache = TerrainCache::new(options(
        &base.path().join("link/terrain"),
        "http://127.0.0.1:1/",
        &store,
    ))
    .unwrap();

    assert!(real.join("terrain").is_dir());
    assert!(cache.root_path().starts_with(real.canonicalize().unwrap()));
}

#[test]
fn defaults_work_from_an_explicit_temporary_cache_directory() {
    let base = tempdir().unwrap();
    let (data, store) = test_store();
    let config = TerrainConfig {
        enabled: true,
        cache_dir: Some(base.path().join("cache/terrain")),
        ..TerrainConfig::default()
    };

    let options = TerrainCacheOptions::from_config(&config, data.path(), store).unwrap();
    let cache = TerrainCache::new(options).unwrap();

    assert!(base.path().join("cache/terrain").is_dir());
    assert_eq!(mode_of(&base.path().join("cache/terrain")), 0o700);
    assert!(cache.root_path().ends_with("cache/terrain"));
}

// ---- tile admission by recorded digest -------------------------------------

#[tokio::test]
async fn a_valid_length_unrecorded_tile_is_not_used_and_is_fetched_again() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let path = dir.path().join(format!("{}.hgt", tile.name()));
    fs::write(&path, hgt_bytes()).unwrap();
    let served = tile_with_elevation(123);
    let (endpoint, requests, task) = serving(served.clone()).await;
    let cache = TerrainCache::new(options(dir.path(), &endpoint, &store)).unwrap();

    let result = cache
        .lookup(51.5, -0.1, Duration::from_secs(5))
        .await
        .unwrap();

    // The planted file reads 0 m; the answer is the provider's tile.
    assert_eq!(result.elevation_m, Some(123));
    assert_eq!(result.tile_hash, sha256_hex(&served));
    assert_eq!(fs::read(&path).unwrap(), served);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    let recorded = store.terrain_tile_digest(&tile.name()).unwrap().unwrap();
    assert_eq!(recorded.sha256, sha256_hex(&served));
    task.abort();
}

#[tokio::test]
async fn an_unrecorded_tile_is_never_used_when_no_provider_is_reachable() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let path = dir.path().join(format!("{}.hgt", tile.name()));
    fs::write(&path, hgt_bytes()).unwrap();
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();

    assert!(cache.get(tile).await.is_err());
    assert!(
        cache
            .lookup(51.5, -0.1, Duration::from_secs(1))
            .await
            .is_err()
    );

    assert!(!path.exists(), "the unproven file is discarded");
    assert!(store.terrain_tile_digest(&tile.name()).unwrap().is_none());
}

#[tokio::test]
async fn a_recorded_tile_is_used_without_the_network() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let bytes = tile_with_elevation(77);
    fs::write(dir.path().join(format!("{}.hgt", tile.name())), &bytes).unwrap();
    fs::write(dir.path().join(format!("{}.source", tile.name())), "esa").unwrap();
    record_digest(&store, tile, &bytes);
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();

    let result = cache
        .lookup(51.5, -0.1, Duration::from_secs(1))
        .await
        .unwrap();

    assert_eq!(result.elevation_m, Some(77));
    assert_eq!(result.tile_hash, sha256_hex(&bytes));
    assert_eq!(result.dataset_source, "esa");
}

#[tokio::test]
async fn a_tile_that_does_not_match_its_record_is_discarded_and_fetched_again() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let path = dir.path().join(format!("{}.hgt", tile.name()));
    fs::write(&path, hgt_bytes()).unwrap();
    // The record belongs to some other content.
    record_digest(&store, tile, &tile_with_elevation(5));
    let served = tile_with_elevation(200);
    let (endpoint, requests, task) = serving(served.clone()).await;
    let cache = TerrainCache::new(options(dir.path(), &endpoint, &store)).unwrap();

    let result = cache
        .lookup(51.5, -0.1, Duration::from_secs(5))
        .await
        .unwrap();

    assert_eq!(result.elevation_m, Some(200));
    assert_eq!(fs::read(&path).unwrap(), served);
    assert_eq!(requests.load(Ordering::SeqCst), 2);
    let recorded = store.terrain_tile_digest(&tile.name()).unwrap().unwrap();
    assert_eq!(
        recorded.sha256,
        sha256_hex(&served),
        "the new download replaced the stale record"
    );
    task.abort();
}

#[tokio::test]
async fn a_download_records_the_digest_of_the_stored_bytes_and_is_then_used_offline() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let served = tile_with_elevation(321);
    let (endpoint, requests, task) = serving(served.clone()).await;
    let cache = TerrainCache::new(options(dir.path(), &endpoint, &store)).unwrap();
    assert!(store.terrain_tile_digest(&tile.name()).unwrap().is_none());

    let first = cache
        .lookup(51.5, -0.1, Duration::from_secs(5))
        .await
        .unwrap();

    let stored = fs::read(dir.path().join(format!("{}.hgt", tile.name()))).unwrap();
    assert_eq!(stored, served);
    let recorded = store.terrain_tile_digest(&tile.name()).unwrap().unwrap();
    assert_eq!(recorded.sha256, sha256_hex(&stored));
    assert!(recorded.recorded_at_ms > 0);
    assert_eq!(first.tile_hash, recorded.sha256);
    assert_eq!(first.dataset_source, "esa");
    assert_eq!(requests.load(Ordering::SeqCst), 2);

    // A second cache over the same root and store (a restart) trusts the record
    // and needs no provider.
    task.abort();
    let restarted = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();
    let second = restarted
        .lookup(51.5, -0.1, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(second.elevation_m, Some(321));
    assert_eq!(second.tile_hash, recorded.sha256);
}

#[tokio::test]
async fn a_recorded_tile_rewritten_in_place_is_unproven_even_with_its_mtime_restored() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let path = dir.path().join(format!("{}.hgt", tile.name()));
    fs::write(&path, hgt_bytes()).unwrap();
    record_digest(&store, tile, &hgt_bytes());
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();
    cache
        .lookup(51.5, -0.1, Duration::from_secs(1))
        .await
        .unwrap();

    // Same inode, same length, same modification time: only the status-change
    // time moves. (Wait out a coarse filesystem clock tick so it can.)
    std::thread::sleep(Duration::from_millis(30));
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let file = File::options().write(true).open(&path).unwrap();
    std::os::unix::fs::FileExt::write_all_at(&file, &[0x7f, 0x7f], 0).unwrap();
    file.set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    drop(file);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);

    assert!(
        cache
            .lookup(51.5, -0.1, Duration::from_secs(1))
            .await
            .is_err(),
        "the cached digest must not vouch for a rewritten file"
    );
    assert!(!path.exists());
}

#[tokio::test]
async fn sampling_uses_the_descriptor_that_was_admitted() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let path = dir.path().join(format!("{}.hgt", tile.name()));
    let original = tile_with_elevation(88);
    fs::write(&path, &original).unwrap();
    record_digest(&store, tile, &original);
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();

    let admitted = cache.get(tile).await.unwrap();
    let replacement = dir.path().join("replacement.hgt");
    fs::write(&replacement, tile_with_elevation(99)).unwrap();
    fs::rename(&replacement, &path).unwrap();

    assert_eq!(admitted.elevation_at(51.5, -0.1).unwrap(), Some(88));
    assert_eq!(admitted.sha256_hex().unwrap(), sha256_hex(&original));
}

#[tokio::test]
async fn a_record_of_another_tile_does_not_admit_this_one() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let neighbour = TileId::from_coordinates(52.5, -0.1).unwrap();
    let path = dir.path().join(format!("{}.hgt", tile.name()));
    fs::write(&path, hgt_bytes()).unwrap();
    // The bytes are right, but the record is for the tile next door.
    record_digest(&store, neighbour, &hgt_bytes());
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();

    assert!(cache.get(tile).await.is_err());

    assert!(!path.exists());
    assert!(store.terrain_tile_digest(&tile.name()).unwrap().is_none());
    assert!(
        store
            .terrain_tile_digest(&neighbour.name())
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn an_unreadable_digest_store_fails_closed_and_keeps_the_tile() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let path = dir.path().join(format!("{}.hgt", tile.name()));
    fs::write(&path, hgt_bytes()).unwrap();
    record_digest(&store, tile, &hgt_bytes());
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();
    store
        .open()
        .unwrap()
        .execute_batch("DROP TABLE terrain_tile_digests")
        .unwrap();

    let error = cache.get(tile).await.err().expect("no digest, no tile");

    assert!(matches!(error, TerrainCacheError::Store(_)), "{error:?}");
    assert!(path.exists(), "a store failure does not delete the tile");
}

#[tokio::test]
async fn a_tile_rewritten_after_admission_fails_the_final_identity_check() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let path = dir.path().join(format!("{}.hgt", tile.name()));
    fs::write(&path, hgt_bytes()).unwrap();
    record_digest(&store, tile, &hgt_bytes());
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();
    let admitted = cache.admit(tile).unwrap().expect("recorded tile");
    admitted.confirm_unchanged().unwrap();

    std::thread::sleep(Duration::from_millis(30));
    std::os::unix::fs::FileExt::write_all_at(
        &File::options().write(true).open(&path).unwrap(),
        &[0x01, 0x01],
        0,
    )
    .unwrap();

    assert!(matches!(
        admitted.confirm_unchanged(),
        Err(TerrainCacheError::InvalidTile(_))
    ));
}

#[tokio::test]
async fn a_symlinked_tile_is_discarded_and_never_followed() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let elsewhere = outside.path().join("elsewhere.hgt");
    fs::write(&elsewhere, hgt_bytes()).unwrap();
    record_digest(&store, tile, &hgt_bytes());
    let path = dir.path().join(format!("{}.hgt", tile.name()));
    symlink(&elsewhere, &path).unwrap();
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();

    assert!(cache.get(tile).await.is_err());

    assert!(path.symlink_metadata().is_err(), "the link was removed");
    assert_eq!(
        fs::read(&elsewhere).unwrap(),
        hgt_bytes(),
        "the link's target is untouched"
    );
}

// ---- operations stay inside the validated root -----------------------------

#[tokio::test]
async fn downloads_stay_inside_the_validated_directory_when_its_path_is_swapped() {
    let base = tempdir().unwrap();
    let (_data, store) = test_store();
    let root = base.path().join("cache");
    let served = tile_with_elevation(42);
    let (endpoint, _requests, task) = serving(served.clone()).await;
    let cache = TerrainCache::new(options(&root, &endpoint, &store)).unwrap();

    // After validation the path is moved aside and a symlink to another
    // directory takes its place.
    let held = base.path().join("held");
    let elsewhere = base.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::rename(&root, &held).unwrap();
    symlink(&elsewhere, &root).unwrap();

    let result = cache
        .lookup(51.5, -0.1, Duration::from_secs(5))
        .await
        .unwrap();

    assert_eq!(result.elevation_m, Some(42));
    assert_eq!(
        fs::read(held.join("N51W001.hgt")).unwrap(),
        served,
        "the tile is written to the directory that was validated"
    );
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    task.abort();
}

#[tokio::test]
async fn a_completed_download_leaves_only_the_tile_and_its_source_marker() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let (endpoint, _requests, task) = serving(tile_with_elevation(1)).await;
    let cache = TerrainCache::new(options(dir.path(), &endpoint, &store)).unwrap();

    cache.get(tile_id()).await.unwrap();

    let mut names: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["N51W001.hgt", "N51W001.source"]);
    task.abort();
}

#[test]
fn startup_cleanup_does_not_follow_a_symlinked_temporary_name() {
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let (_data, store) = test_store();
    let target = outside.path().join("precious");
    fs::write(&target, b"keep").unwrap();
    let stale = dir.path().join(".archive.123.0.tmp");
    symlink(&target, &stale).unwrap();

    TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"keep");
    assert!(
        stale.symlink_metadata().is_ok(),
        "only owned regular files are cleaned"
    );
}

// ---- the archive is read through the descriptor it was written through ----

#[test]
fn decompression_reads_the_archive_it_was_handed_not_the_entry_name() {
    let base = tempdir().unwrap();
    let root =
        Arc::new(crate::platform::private_directory::open_validated(base.path(), true).unwrap());
    let written = tile_with_elevation(100);
    let substitute = tile_with_elevation(200);

    // The archive as the downloader leaves it: written, with the offset at the
    // end of the file.
    let (mut archive, name) = create_private_temp(&root, "archive").unwrap();
    archive.write_all(&zip_of(&written)).unwrap();
    // Someone replaces the entry by name with a valid archive of other bytes.
    let (mut other, other_name) = create_private_temp(&root, "archive").unwrap();
    other.write_all(&zip_of(&substitute)).unwrap();
    drop(other);
    root.rename(&other_name, &name).unwrap();

    let staged = decompress_to_hgt(&root, archive, false, "N51W001").unwrap();

    assert_eq!(staged.sha256, sha256_hex(&written));
    assert_ne!(staged.sha256, sha256_hex(&substitute));
}

/// `download` cannot be paused between writing the archive and decompressing
/// it, so no test can swap the entry at that point. The wiring is pinned
/// instead: the descriptor the archive was written through is the one handed
/// to decompression (`decompress_to_hgt` takes a `File`, not a name).
#[test]
fn the_downloader_hands_the_written_descriptor_to_decompression() {
    let source = include_str!("../terrain_cache.rs");
    for call in [
        "let archive = output.into_std().await;",
        "decompress_to_hgt(&self.root, archive, aws, &name)",
    ] {
        assert!(
            source.contains(call),
            "terrain_cache.rs no longer contains `{call}`"
        );
    }
}

// ---- the gzip (AWS) archive, read through the same retained descriptor -------

fn gzip_of(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// The downloader keeps the descriptor it wrote the archive through, so that
/// descriptor is left at the end of the file. A gzip stream has no central
/// directory to seek to (a zip archive finds its own), so only the explicit
/// rewind in `decompress_to_hgt` lets it be read.
#[test]
fn a_gzip_archive_is_read_from_its_start_though_it_was_left_at_its_end() {
    let base = tempdir().unwrap();
    let root =
        Arc::new(crate::platform::private_directory::open_validated(base.path(), true).unwrap());
    let written = tile_with_elevation(150);
    let (mut archive, _name) = create_private_temp(&root, "archive").unwrap();
    archive.write_all(&gzip_of(&written)).unwrap();

    let staged = decompress_to_hgt(&root, archive, true, "N51W001").unwrap();

    assert_eq!(staged.sha256, sha256_hex(&written));
}

#[tokio::test]
async fn an_aws_gzip_download_is_stored_recorded_and_sourced_as_aws() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = tile_id();
    let served = tile_with_elevation(150);
    let archive = gzip_of(&served);
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    let (endpoint, task) = server(move |uri| {
        counted.fetch_add(1, Ordering::SeqCst);
        if uri.path().ends_with(".gz") {
            Response::new(Body::from(archive.clone()))
        } else {
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap()
        }
    })
    .await;
    let cache = TerrainCache::new(options(dir.path(), &endpoint, &store)).unwrap();

    let result = cache
        .lookup(51.5, -0.1, Duration::from_secs(5))
        .await
        .unwrap();

    assert_eq!(result.elevation_m, Some(150));
    assert_eq!(result.dataset_source, "aws");
    assert_eq!(requests.load(Ordering::SeqCst), 1, "no ESA fallback");
    let stored = fs::read(dir.path().join(format!("{}.hgt", tile.name()))).unwrap();
    assert_eq!(stored, served);
    let recorded = store.terrain_tile_digest(&tile.name()).unwrap().unwrap();
    assert_eq!(recorded.sha256, sha256_hex(&served));
    task.abort();
}
