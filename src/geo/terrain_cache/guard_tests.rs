// SPDX-License-Identifier: AGPL-3.0-only

//! B2 fix round, P3b fix 3 (R-P3bF3-2, R-P3bF3-4): every persistent mutation of the
//! terrain cache is preceded by the run's guard, with no await between: the directory
//! it creates and tightens, the stale files it removes, an unproven tile it discards,
//! the tiles it evicts, and the temporary archive, staged tile, digest, rename and
//! source marker of a download. A guard that has acknowledged a loss refuses every one
//! of them, whatever the admission says afterwards, and after a lock wait too.
//!
//! The proof is counted at the file boundary: `PrivateDirectory::mutation_attempts`
//! counts every creation, rename and removal asked of the cache root (an idempotent
//! removal of an absent entry as well), independently of any guard, and the spy guard
//! below reads it at the moment it refuses. Nothing may be attempted after that moment.
//! Every root is a temporary directory; the data is synthetic.

use std::os::unix::fs::PermissionsExt;

use super::*;

/// A guard shaped like the run's: it refuses once its fence is set, revalidates its
/// admission otherwise and sets the fence when that fails. `allowed` consultations pass
/// at the start (`usize::MAX`: no limit). When it refuses it reads `probe`, the number
/// of mutations the cache had attempted at that very moment.
#[derive(Clone)]
struct SpyGuard {
    fence: Arc<AtomicBool>,
    admission: Arc<AtomicBool>,
    consulted: Arc<AtomicUsize>,
    allowed: usize,
    probe: Arc<dyn Fn() -> usize + Send + Sync>,
    at_first_refusal: Arc<Mutex<Option<usize>>>,
}

impl SpyGuard {
    fn new(allowed: usize) -> Self {
        Self {
            fence: Arc::new(AtomicBool::new(false)),
            admission: Arc::new(AtomicBool::new(true)),
            consulted: Arc::new(AtomicUsize::new(0)),
            allowed,
            probe: Arc::new(|| 0),
            at_first_refusal: Arc::new(Mutex::new(None)),
        }
    }

    /// Read the attempts of `cache`'s root whenever this guard refuses.
    fn watching(mut self, cache: &TerrainCache) -> Self {
        let root = Arc::clone(&cache.root);
        self.probe = Arc::new(move || root.mutation_attempts());
        self
    }

    /// The supervisor observes the loss (the admission is gone, the guard is
    /// consulted and sets its fence), and the admission is then restored.
    fn lose_and_restore_the_admission(&self) {
        self.admission.store(false, Ordering::SeqCst);
        assert!(
            self.assert_egress_allowed().is_err(),
            "the loss is acknowledged"
        );
        assert!(self.fence.load(Ordering::SeqCst), "the fence is set");
        self.admission.store(true, Ordering::SeqCst);
    }

    /// Nothing was attempted at the cache root after the guard first refused.
    fn assert_nothing_attempted_since_the_refusal(&self, cache: &TerrainCache) {
        let at_refusal = self
            .at_first_refusal
            .lock()
            .unwrap()
            .expect("the guard refused");
        assert_eq!(
            cache.root.mutation_attempts(),
            at_refusal,
            "a creation, rename or removal was attempted after the guard refused"
        );
    }
}

impl EgressGuard for SpyGuard {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        let seen = self.consulted.fetch_add(1, Ordering::SeqCst);
        let refused = seen >= self.allowed
            || self.fence.load(Ordering::SeqCst)
            || !self.admission.load(Ordering::SeqCst);
        if refused {
            if !self.admission.load(Ordering::SeqCst) {
                self.fence.store(true, Ordering::SeqCst);
            }
            self.at_first_refusal
                .lock()
                .unwrap()
                .get_or_insert_with(|| (self.probe)());
            return Err(EgressGuardError);
        }
        Ok(())
    }
}

/// A guard that has refused already.
fn a_fenced_guard() -> SpyGuard {
    let guard = SpyGuard::new(usize::MAX);
    guard.fence.store(true, Ordering::SeqCst);
    guard
}

fn stale_temporary(root: &Path) -> PathBuf {
    let stale = root.join(".archive.123.0.tmp");
    fs::write(&stale, b"stale").unwrap();
    File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
        .unwrap();
    stale
}

// ---- construction ------------------------------------------------------------------------------------

#[test]
fn p3bf3_a_refused_guard_creates_no_directory_at_construction() {
    let parent = tempdir().unwrap();
    let (_data, store) = test_store();
    let root = parent.path().join("missing").join("cache");

    let refused = TerrainCache::new_with_guard(
        options(&root, "http://127.0.0.1:1/", &store),
        &a_fenced_guard(),
    );
    assert!(matches!(refused, Err(TerrainCacheError::EgressDenied)));
    assert!(
        !parent.path().join("missing").exists(),
        "no directory was created for a refused guard"
    );

    // The control: the same construction under a guard that allows creates the root.
    TerrainCache::new_with_guard(
        options(&root, "http://127.0.0.1:1/", &store),
        &SpyGuard::new(usize::MAX),
    )
    .unwrap();
    assert!(root.is_dir());
}

#[test]
fn p3bf3_a_refused_guard_tightens_and_removes_nothing_at_construction() {
    let (_data, store) = test_store();

    // The first consultation is refused: the mode stays as it was, the stale file stays.
    let wide = tempdir().unwrap();
    fs::set_permissions(wide.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let stale = stale_temporary(wide.path());
    let refused = TerrainCache::new_with_guard(
        options(wide.path(), "http://127.0.0.1:1/", &store),
        &a_fenced_guard(),
    );
    assert!(matches!(refused, Err(TerrainCacheError::EgressDenied)));
    assert_eq!(
        fs::metadata(wide.path()).unwrap().permissions().mode() & 0o777,
        0o755,
        "the directory was not tightened"
    );
    assert!(stale.exists(), "the stale file was not removed");

    // The second consultation, in front of the removal, is refused on its own.
    let second = tempdir().unwrap();
    fs::set_permissions(second.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let stale = stale_temporary(second.path());
    let refused = TerrainCache::new_with_guard(
        options(second.path(), "http://127.0.0.1:1/", &store),
        &SpyGuard::new(1),
    );
    assert!(matches!(refused, Err(TerrainCacheError::EgressDenied)));
    assert!(stale.exists(), "the stale file was not removed");

    // The control: an allowing guard tightens and removes.
    TerrainCache::new_with_guard(
        options(second.path(), "http://127.0.0.1:1/", &store),
        &SpyGuard::new(usize::MAX),
    )
    .unwrap();
    assert!(!stale.exists());
    assert_eq!(
        fs::metadata(second.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

// ---- a loss met while the lookup waited for the cache's locks ------------------------------------------------

/// Hold the cache's quota lock, start a lookup of `tile` and poll it once: it has taken
/// the tile's lock and waits for the quota lock, so nothing it does later has begun.
macro_rules! park_a_lookup {
    ($cache:expr, $tile:expr, $guard:expr, $held:ident, $lookup:ident) => {
        let $held = $cache.quota_lock.lock().await;
        let mut $lookup = Box::pin($cache.get_with_egress_guard($tile, $guard));
        assert!(
            futures_util::poll!($lookup.as_mut()).is_pending(),
            "the lookup waits for the quota lock"
        );
    };
}

#[tokio::test]
async fn p3bf3_a_cached_tile_is_refused_after_a_loss_met_during_the_lock_wait() {
    // A valid, recorded tile: the lookup would be a hit that reads and evicts nothing.
    // The loss is acknowledged while it waits and the admission comes back before the
    // lock is released: the lookup still ends with the refusal, because the guard
    // stands in front of the work after the lock wait.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    write_cached_tile(dir.path(), tile, 1);
    record_digest(&store, tile, &hgt_bytes());
    let guard = SpyGuard::new(usize::MAX);
    let cache =
        TerrainCache::new_with_guard(options(dir.path(), "http://127.0.0.1:1/", &store), &guard)
            .unwrap();
    let guard = guard.watching(&cache);
    park_a_lookup!(cache, tile, &guard, held, lookup);
    guard.lose_and_restore_the_admission();
    drop(held);
    let result = lookup.await;
    assert!(
        matches!(result, Err(TerrainCacheError::EgressDenied)),
        "{:?}",
        result.map(|_| ())
    );
    guard.assert_nothing_attempted_since_the_refusal(&cache);
    assert!(cache.tile_path(tile).exists());
}

#[tokio::test]
async fn p3bf3_an_unproven_tile_is_not_deleted_after_a_loss_met_during_the_lock_wait() {
    // The cached tile has no recorded digest: admission would discard it. The loss is
    // acknowledged during the lock wait and the admission restored; the file and its
    // source marker stay, and no removal is attempted at all.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    write_cached_tile(dir.path(), tile, 1);
    let guard = SpyGuard::new(usize::MAX);
    let cache =
        TerrainCache::new_with_guard(options(dir.path(), "http://127.0.0.1:1/", &store), &guard)
            .unwrap();
    let guard = guard.watching(&cache);
    let before = cache.root.mutation_attempts();
    park_a_lookup!(cache, tile, &guard, held, lookup);
    guard.lose_and_restore_the_admission();
    drop(held);
    let result = lookup.await;
    assert!(matches!(result, Err(TerrainCacheError::EgressDenied)));
    guard.assert_nothing_attempted_since_the_refusal(&cache);
    assert_eq!(
        cache.root.mutation_attempts(),
        before,
        "no removal was attempted"
    );
    assert!(cache.tile_path(tile).exists());
    assert!(cache.source_path(tile).exists());
}

// ---- the discard of an unproven tile, the eviction, the writes of a download --------------------------------

#[tokio::test]
async fn p3bf3_a_refused_discard_removes_nothing() {
    // The consultation after the locks passes and the next, in front of the discard, is
    // refused: an unrecorded tile and a tile whose digest differs from its record both
    // stay on disk, and no removal is attempted.
    for recorded in [None, Some(vec![1_u8; hgt_bytes().len()])] {
        let dir = tempdir().unwrap();
        let (_data, store) = test_store();
        let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
        write_cached_tile(dir.path(), tile, 1);
        if let Some(other) = &recorded {
            record_digest(&store, tile, other);
        }
        let cache = TerrainCache::new_with_guard(
            options(dir.path(), "http://127.0.0.1:1/", &store),
            &SpyGuard::new(usize::MAX),
        )
        .unwrap();
        let guard = SpyGuard::new(1).watching(&cache);
        let result = cache.get_with_egress_guard(tile, &guard).await;
        assert!(
            matches!(result, Err(TerrainCacheError::EgressDenied)),
            "recorded: {}",
            recorded.is_some()
        );
        guard.assert_nothing_attempted_since_the_refusal(&cache);
        assert!(cache.tile_path(tile).exists());
        assert!(cache.source_path(tile).exists());
        assert_eq!(cache.root.mutation_attempts(), 0);
    }
}

/// A cache holding the recorded tile `new` and a much older tile `old` that the quota
/// no longer lets stay (a sparse file of the largest size a tile can have).
fn a_cache_over_its_quota(dir: &Path, store: &HubStore) -> (TerrainCache, TileId, TileId) {
    let old = TileId::from_coordinates(51.5, -0.1).unwrap();
    let new = TileId::from_coordinates(52.5, -0.1).unwrap();
    write_cached_tile(dir, old, 1);
    File::options()
        .write(true)
        .open(dir.join(format!("{}.hgt", old.name())))
        .unwrap()
        .set_len(MAX_HGT_BYTES)
        .unwrap();
    write_cached_tile(dir, new, 2);
    record_digest(store, new, &hgt_bytes());
    let mut opts = options(dir, "http://127.0.0.1:1/", store);
    opts.max_cache_bytes = MAX_TILE_CACHE_BYTES;
    let cache = TerrainCache::new_with_guard(opts, &SpyGuard::new(usize::MAX)).unwrap();
    (cache, old, new)
}

#[tokio::test]
async fn p3bf3_a_refused_eviction_removes_nothing() {
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let (cache, old, new) = a_cache_over_its_quota(dir.path(), &store);
    // The consultation after the locks passes; the next, in front of the eviction of the
    // oldest tile, is refused.
    let guard = SpyGuard::new(1).watching(&cache);
    let result = cache.get_with_egress_guard(new, &guard).await;
    assert!(matches!(result, Err(TerrainCacheError::EgressDenied)));
    guard.assert_nothing_attempted_since_the_refusal(&cache);
    assert_eq!(
        cache.root.mutation_attempts(),
        0,
        "no removal was attempted"
    );
    assert!(cache.tile_path(old).exists());
    assert!(cache.source_path(old).exists());
}

#[tokio::test]
async fn p3bf3_the_eviction_of_the_control_runs_when_the_guard_allows() {
    // The control of the two refusals above: the same cache under a guard that allows
    // evicts the oldest tile, and the removals are counted.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let (cache, old, new) = a_cache_over_its_quota(dir.path(), &store);
    cache
        .get_with_egress_guard(new, &SpyGuard::new(usize::MAX))
        .await
        .unwrap();
    assert!(!cache.tile_path(old).exists());
    assert!(!cache.source_path(old).exists());
    assert_eq!(
        cache.root.mutation_attempts(),
        3,
        "the two removals and the sync of the directory they changed are counted"
    );
}

#[test]
fn p3bf5_the_quota_syncs_only_what_it_evicted_and_only_behind_the_guard() {
    // R-P3bF5-3: the counted sync found one that ran after every enforcement of the quota,
    // evicting or not, with no consultation of its own. Nothing to evict: nothing is
    // synced. Something evicted: the sync is asked of the guard first, and a refusal
    // leaves the removals (already made, behind their own consultation) and no sync.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();
    let before = cache.root.mutation_attempts();
    cache
        .enforce_cache_quota(None, 0, &SpyGuard::new(usize::MAX))
        .unwrap();
    assert_eq!(cache.root.mutation_attempts(), before, "nothing was synced");

    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let (cache, old, new) = a_cache_over_its_quota(dir.path(), &store);
    let protected = format!("{}.hgt", new.name());
    let guard = SpyGuard::new(1).watching(&cache);
    let result = cache.enforce_cache_quota(Some(&protected), 0, &guard);
    assert!(matches!(result, Err(TerrainCacheError::EgressDenied)));
    assert!(!cache.tile_path(old).exists(), "the eviction ran");
    assert_eq!(
        cache.root.mutation_attempts(),
        2,
        "two removals and no sync"
    );
    guard.assert_nothing_attempted_since_the_refusal(&cache);
}

/// What a download of a tile that only ESA serves (AWS answers 404) came to.
struct DownloadRun {
    dir: tempfile::TempDir,
    cache: TerrainCache,
    guard: SpyGuard,
    store: HubStore,
    tile: TileId,
    result: Result<HgtTile, TerrainCacheError>,
    _data: tempfile::TempDir,
}

/// The download under a guard that passes `allowed` consultations: after the locks (1),
/// AWS (2), ESA (3), the archive's temporary file (4), the one chunk of the body (5),
/// the sync of the archive (6), the staged tile and its digest (7), the removal of the
/// archive when its entry is dropped (8), the source marker (9). The zip is a few
/// kilobytes: the body is one chunk (R-P3bF4-5).
async fn a_download_refused_at(allowed: usize) -> DownloadRun {
    let dir = tempdir().unwrap();
    let (data, store) = test_store();
    let (endpoint, task) = server(move |uri| {
        if uri.path().ends_with(".gz") {
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap()
        } else {
            Response::new(Body::from(zip_bytes()))
        }
    })
    .await;
    let cache = TerrainCache::new_with_guard(
        options(dir.path(), &endpoint, &store),
        &SpyGuard::new(usize::MAX),
    )
    .unwrap();
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    let guard = SpyGuard::new(allowed).watching(&cache);
    let result = cache.get_with_egress_guard(tile, &guard).await;
    task.abort();
    DownloadRun {
        dir,
        cache,
        guard,
        store,
        tile,
        result,
        _data: data,
    }
}

#[tokio::test]
async fn p3bf3_a_refused_archive_creates_no_temporary_file() {
    let run = a_download_refused_at(3).await;
    assert!(matches!(run.result, Err(TerrainCacheError::EgressDenied)));
    run.guard
        .assert_nothing_attempted_since_the_refusal(&run.cache);
    assert_eq!(run.cache.root.mutation_attempts(), 0, "no file was created");
    assert!(temporary_paths(run.dir.path()).is_empty());
    assert!(
        run.store
            .terrain_tile_digest(&run.tile.name())
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn p3bf3_a_refused_staging_keeps_the_archive_and_records_nothing() {
    let run = a_download_refused_at(6).await;
    assert!(matches!(run.result, Err(TerrainCacheError::EgressDenied)));
    run.guard
        .assert_nothing_attempted_since_the_refusal(&run.cache);
    // Only the archive's own temporary file was created, written and synced; it is left
    // where it is, not removed, once the guard has refused.
    assert_eq!(run.cache.root.mutation_attempts(), 3);
    assert_eq!(
        temporary_paths(run.dir.path()).len(),
        1,
        "the archive stays"
    );
    assert!(
        run.store
            .terrain_tile_digest(&run.tile.name())
            .unwrap()
            .is_none()
    );
    assert!(!run.cache.tile_path(run.tile).exists());
}

#[tokio::test]
async fn p3bf3_a_refused_source_marker_is_not_written() {
    let run = a_download_refused_at(8).await;
    assert!(matches!(run.result, Err(TerrainCacheError::EgressDenied)));
    run.guard
        .assert_nothing_attempted_since_the_refusal(&run.cache);
    // The tile and its digest were committed before the loss; the marker was not begun.
    assert!(
        run.store
            .terrain_tile_digest(&run.tile.name())
            .unwrap()
            .is_some()
    );
    assert!(run.cache.tile_path(run.tile).exists());
    assert!(!run.cache.source_path(run.tile).exists());
    assert!(
        temporary_paths(run.dir.path()).is_empty(),
        "no temporary file of the marker was created"
    );
}

#[tokio::test]
async fn p3bf3_a_download_the_guard_allows_writes_every_file() {
    // The control: nine consultations pass and the tile is stored with its marker.
    let run = a_download_refused_at(9).await;
    assert!(run.result.is_ok(), "{:?}", run.result.map(|_| ()));
    assert_eq!(run.guard.consulted.load(Ordering::SeqCst), 9);
    assert!(
        run.store
            .terrain_tile_digest(&run.tile.name())
            .unwrap()
            .is_some()
    );
    assert!(run.cache.source_path(run.tile).exists());
}

#[tokio::test]
async fn p3bf3_a_download_that_fails_after_the_loss_keeps_its_archive() {
    // The provider's body breaks off after the status line while the archive's
    // temporary file exists. The guard has refused by the time the error is handled (the
    // loss was met meanwhile): the file stays, nothing is removed, and the fallback
    // provider is not asked.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let (endpoint, task) = server(move |_| {
        Response::builder()
            .status(StatusCode::OK)
            // The status line and a first chunk go out; the body breaks off later.
            .body(Body::from_stream(futures_util::StreamExt::chain(
                futures_util::stream::once(async {
                    Ok::<Bytes, std::io::Error>(Bytes::from_static(b"partial"))
                }),
                futures_util::stream::once(async {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Err(std::io::Error::other("connection reset"))
                }),
            )))
            .unwrap()
    })
    .await;
    let cache = TerrainCache::new_with_guard(
        options(dir.path(), &endpoint, &store),
        &SpyGuard::new(usize::MAX),
    )
    .unwrap();
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    // After the locks (1), AWS (2), the archive's file (3) and the chunk (4), the guard
    // refuses at the handling of the broken body.
    let guard = SpyGuard::new(4).watching(&cache);
    let result = cache.get_with_egress_guard(tile, &guard).await;
    task.abort();
    assert!(
        matches!(result, Err(TerrainCacheError::EgressDenied)),
        "{:?} after {} consultations",
        result.map(|_| ()),
        guard.consulted.load(Ordering::SeqCst)
    );
    guard.assert_nothing_attempted_since_the_refusal(&cache);
    assert_eq!(
        cache.root.mutation_attempts(),
        2,
        "only the archive was created and its chunk written"
    );
    assert_eq!(temporary_paths(dir.path()).len(), 1, "the archive stays");
}

// ---- the counter itself ---------------------------------------------------------------------------------------

#[test]
fn p3bf3_the_file_counter_counts_a_removal_that_changes_nothing() {
    // An idempotent removal (the entry is absent) and a rewrite of the same bytes leave
    // the directory as it was; the counter still moves, which no listing could show.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/", &store)).unwrap();
    assert_eq!(cache.root.mutation_attempts(), 0);
    cache.root.remove("absent.hgt").unwrap();
    assert_eq!(cache.root.mutation_attempts(), 1);
    drop(cache.root.create_exclusive("same").unwrap());
    cache.root.rename("same", "same").unwrap();
    assert_eq!(cache.root.mutation_attempts(), 3);
    assert_eq!(cache.root.entry_names().unwrap(), vec!["same".to_owned()]);
}

// ---- B2 fix round, P3b fix 4: the archive across its awaits (R-P3bF4-5) ---------------------------------

/// The size of the archive entry of `root` now (zero when it does not exist).
fn archive_len(root: &Path) -> u64 {
    temporary_paths(root)
        .iter()
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".archive."))
        })
        .map(|path| fs::metadata(path).map_or(0, |metadata| metadata.len()))
        .sum()
}

/// A provider that sends one chunk and then waits for `release`: it then sends `second`
/// (when given) and ends the body. The wait is the test's, not the clock's.
fn gated_archive_response(
    release: Arc<tokio::sync::Notify>,
    second: Option<&'static [u8]>,
) -> Response<Body> {
    let first = futures_util::stream::once(async {
        Ok::<_, io::Error>(Bytes::from_static(b"first chunk"))
    });
    let rest = futures_util::stream::once(async move {
        release.notified().await;
        second
    })
    .filter_map(|second| async move {
        second.map(|bytes| Ok::<Bytes, io::Error>(Bytes::from_static(bytes)))
    });
    Response::new(Body::from_stream(first.chain(rest)))
}

/// Options for these runs: the smallest quota a cache accepts, and timeouts no test comes
/// near, so that only the test's own events end a download.
fn quiet_options(root: &Path, endpoint: &str, store: &HubStore) -> TerrainCacheOptions {
    let mut options = options(root, endpoint, store);
    options.max_cache_bytes = MAX_TILE_CACHE_BYTES;
    options.connect_timeout = Duration::from_secs(2 * 60 * 60);
    options.read_timeout = Duration::from_secs(2 * 60 * 60);
    options
}

/// A download that met a loss while its body was stalled.
struct StalledRun {
    dir: tempfile::TempDir,
    _data: tempfile::TempDir,
    store: HubStore,
    cache: TerrainCache,
    guard: SpyGuard,
    server: tokio::task::JoinHandle<()>,
    result: Result<HgtTile, TerrainCacheError>,
    attempts_at_loss: usize,
    archive_len_at_loss: u64,
}

/// Start the download of a tile against a provider that sends one chunk and waits; once
/// the chunk is in the archive, acknowledge the loss (the admission is restored at once),
/// then release `second` (or the end of the body).
async fn a_loss_met_while_the_body_is_stalled(second: Option<&'static [u8]>) -> StalledRun {
    let dir = tempdir().unwrap();
    let (data, store) = test_store();
    let release = Arc::new(tokio::sync::Notify::new());
    let served = Arc::clone(&release);
    let (endpoint, server) =
        server(move |_| gated_archive_response(Arc::clone(&served), second)).await;
    let cache = TerrainCache::new_with_guard(
        quiet_options(dir.path(), &endpoint, &store),
        &SpyGuard::new(usize::MAX),
    )
    .unwrap();
    let guard = SpyGuard::new(usize::MAX).watching(&cache);
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    let mut lookup = Box::pin(cache.get_with_egress_guard(tile, &guard));
    while archive_len(dir.path()) == 0 {
        assert!(
            futures_util::poll!(lookup.as_mut()).is_pending(),
            "the download ended before its first chunk was written"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    guard.lose_and_restore_the_admission();
    let attempts_at_loss = cache.root.mutation_attempts();
    let archive_len_at_loss = archive_len(dir.path());
    release.notify_one();
    let result = lookup.await;
    StalledRun {
        dir,
        _data: data,
        store,
        cache,
        guard,
        server,
        result,
        attempts_at_loss,
        archive_len_at_loss,
    }
}

#[tokio::test]
async fn p3bf4_a_chunk_released_after_a_loss_is_not_written() {
    // The first chunk is in the archive (the archive was created and written: two
    // attempts); the loss is acknowledged with the admission restored; the second chunk
    // is released. It is refused: no further write is attempted, the archive keeps its
    // length and stays, and no digest is recorded.
    let run = a_loss_met_while_the_body_is_stalled(Some(b"second chunk")).await;
    assert!(matches!(run.result, Err(TerrainCacheError::EgressDenied)));
    assert_eq!(
        run.attempts_at_loss,
        2,
        "the creation and the first chunk: {:#?}",
        crate::platform::file_attempts::entries_under(run.dir.path())
    );
    assert_eq!(
        run.cache.root.mutation_attempts(),
        run.attempts_at_loss,
        "a write was attempted after the loss"
    );
    run.guard
        .assert_nothing_attempted_since_the_refusal(&run.cache);
    assert_eq!(archive_len(run.dir.path()), run.archive_len_at_loss);
    assert_eq!(
        temporary_paths(run.dir.path()).len(),
        1,
        "the archive stays"
    );
    assert!(run.store.terrain_tile_digest("N51W001").unwrap().is_none());
    run.server.abort();
}

#[tokio::test]
async fn p3bf4_the_archive_is_not_synced_after_a_loss() {
    // The body ends after the loss: nothing is left to write, and the sync that follows
    // the last chunk is a mutation too. It is refused, and the archive stays.
    let run = a_loss_met_while_the_body_is_stalled(None).await;
    assert!(matches!(run.result, Err(TerrainCacheError::EgressDenied)));
    assert_eq!(
        run.cache.root.mutation_attempts(),
        run.attempts_at_loss,
        "a sync was attempted after the loss"
    );
    assert_eq!(archive_len(run.dir.path()), run.archive_len_at_loss);
    assert_eq!(
        temporary_paths(run.dir.path()).len(),
        1,
        "the archive stays"
    );
    run.server.abort();
}

#[tokio::test]
async fn p3bf4_a_refused_chunk_and_a_refused_sync_each_keep_the_archive() {
    // The two points on their own, by consultation: the archive is created (4), the
    // chunk (5) and the sync (6) follow. The archive is created and left in either case.
    for (allowed, attempts) in [(4_usize, 1_usize), (5, 2)] {
        let run = a_download_refused_at(allowed).await;
        assert!(
            matches!(run.result, Err(TerrainCacheError::EgressDenied)),
            "allowed {allowed}"
        );
        run.guard
            .assert_nothing_attempted_since_the_refusal(&run.cache);
        assert_eq!(
            run.cache.root.mutation_attempts(),
            attempts,
            "allowed {allowed}"
        );
        assert_eq!(
            temporary_paths(run.dir.path()).len(),
            1,
            "allowed {allowed}"
        );
        assert!(!run.cache.tile_path(run.tile).exists());
    }
}

#[tokio::test(start_paused = true)]
async fn p3bf4_a_lookup_budget_that_expires_after_a_loss_keeps_the_archive() {
    // The lookup budget is the production cancellation of a stalled download. It runs on
    // the paused clock, so it expires when the test advances it and not before: the
    // archive holds its first chunk, the loss is acknowledged (the admission is restored),
    // and the budget then drops the lookup. With the guard refusing, the archive stays
    // and no removal is attempted; the control, with the guard allowing, removes it.
    for lose in [true, false] {
        let dir = tempdir().unwrap();
        let (_data, store) = test_store();
        let (endpoint, server) = server(|_| stalled_archive_response()).await;
        let cache = TerrainCache::new_with_guard(
            quiet_options(dir.path(), &endpoint, &store),
            &SpyGuard::new(usize::MAX),
        )
        .unwrap();
        let guard = SpyGuard::new(usize::MAX).watching(&cache);
        let mut lookup = Box::pin(cache.lookup_with_egress_guard(
            51.5,
            -0.1,
            Duration::from_secs(60 * 60),
            &guard,
        ));
        // Busy-yield, never sleep: the paused clock must not move until the test says so.
        while archive_len(dir.path()) == 0 {
            assert!(futures_util::poll!(lookup.as_mut()).is_pending());
            tokio::task::yield_now().await;
        }
        if lose {
            guard.lose_and_restore_the_admission();
        }
        let attempts = cache.root.mutation_attempts();
        tokio::time::advance(Duration::from_secs(60 * 60 + 1)).await;
        let result = lookup.await;
        assert!(
            matches!(result, Err(TerrainCacheError::Timeout)),
            "lose {lose}"
        );
        if lose {
            assert_eq!(temporary_paths(dir.path()).len(), 1, "the archive stays");
            assert_eq!(
                cache.root.mutation_attempts(),
                attempts,
                "a removal was attempted after the loss"
            );
            guard.assert_nothing_attempted_since_the_refusal(&cache);
        } else {
            assert!(
                temporary_paths(dir.path()).is_empty(),
                "the control removes it"
            );
            assert_eq!(cache.root.mutation_attempts(), attempts + 1);
        }
        server.abort();
    }
}

#[tokio::test]
async fn p3bf4_a_worker_stopped_after_a_loss_keeps_the_archive() {
    // The terrain worker's stop or abort drops the pass wherever it is. The same two
    // runs as the budget: the archive stays once the guard refused, and the control
    // removes it.
    for lose in [true, false] {
        let dir = tempdir().unwrap();
        let (_data, store) = test_store();
        let (endpoint, server) = server(|_| stalled_archive_response()).await;
        let cache = Arc::new(
            TerrainCache::new_with_guard(
                quiet_options(dir.path(), &endpoint, &store),
                &SpyGuard::new(usize::MAX),
            )
            .unwrap(),
        );
        let guard = SpyGuard::new(usize::MAX).watching(&cache);
        let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
        let task = tokio::spawn({
            let cache = Arc::clone(&cache);
            let guard = guard.clone();
            async move { cache.get_with_egress_guard(tile, &guard).await.map(drop) }
        });
        while archive_len(dir.path()) == 0 {
            assert!(!task.is_finished());
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        if lose {
            guard.lose_and_restore_the_admission();
        }
        let attempts = cache.root.mutation_attempts();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        if lose {
            assert_eq!(temporary_paths(dir.path()).len(), 1, "the archive stays");
            assert_eq!(
                cache.root.mutation_attempts(),
                attempts,
                "a removal was attempted after the loss"
            );
            guard.assert_nothing_attempted_since_the_refusal(&cache);
        } else {
            assert!(
                temporary_paths(dir.path()).is_empty(),
                "the control removes it"
            );
            assert_eq!(cache.root.mutation_attempts(), attempts + 1);
        }
        server.abort();
    }
}

// ---- a kept archive must not block the next run (R-P3bF4-6) ----------------------------------------------

#[tokio::test]
async fn p3bf4_an_archive_kept_after_a_refusal_does_not_block_the_next_run() {
    // The run is refused with its archive on disk, the cache at the smallest supported
    // quota: any nonempty leftover would fail every cache miss of the next run for the
    // full-tile reservation. A fresh admitted construction within the hour reclaims it,
    // and a cache miss then succeeds with no manual cleanup.
    let run = a_loss_met_while_the_body_is_stalled(Some(b"second chunk")).await;
    run.server.abort();
    assert_eq!(
        temporary_paths(run.dir.path()).len(),
        1,
        "the archive was kept"
    );
    let (endpoint, task) = server(|uri| {
        if uri.path().ends_with(".gz") {
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap()
        } else {
            Response::new(Body::from(zip_bytes()))
        }
    })
    .await;
    let restarted = TerrainCache::new_with_guard(
        quiet_options(run.dir.path(), &endpoint, &run.store),
        &SpyGuard::new(usize::MAX),
    )
    .unwrap();
    assert!(
        temporary_paths(run.dir.path()).is_empty(),
        "the kept archive was reclaimed"
    );
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    let hgt = restarted
        .get_with_egress_guard(tile, &SpyGuard::new(usize::MAX))
        .await;
    assert!(hgt.is_ok(), "a cache miss succeeds: {:?}", hgt.map(|_| ()));
    task.abort();
}

#[tokio::test]
async fn p3bf4_a_refused_restart_reclaims_nothing() {
    let run = a_loss_met_while_the_body_is_stalled(Some(b"second chunk")).await;
    run.server.abort();
    let kept = temporary_paths(run.dir.path());
    assert_eq!(kept.len(), 1);
    // Refused at the first consultation (in front of the directory) and at the second (in
    // front of the reclaim). The second admitted the opening of the root, whose check for
    // an ACL is the one attempt counted (R-P3bF5-3); nothing else is attempted.
    for (guard, opened) in [(a_fenced_guard(), 0), (SpyGuard::new(1), 1)] {
        let before = crate::platform::file_attempts::count_under(run.dir.path());
        let refused = TerrainCache::new_with_guard(
            quiet_options(run.dir.path(), "http://127.0.0.1:1/", &run.store),
            &guard,
        );
        assert!(matches!(refused, Err(TerrainCacheError::EgressDenied)));
        assert_eq!(temporary_paths(run.dir.path()), kept, "the archive stays");
        assert_eq!(
            crate::platform::file_attempts::count_under(run.dir.path()) - before,
            opened,
            "no file mutation but the admitted root check was attempted by the refused restart"
        );
    }
}

#[tokio::test]
async fn p3bf4_an_archive_a_live_instance_owns_is_not_reclaimed() {
    // Instance A is downloading; instance B is constructed over the same root under an
    // admitting guard. B reclaims what no live writer owns and leaves A's archive alone;
    // A's archive is removed again by A itself when A is dropped under an admitting guard.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let (endpoint, server) = server(|_| stalled_archive_response()).await;
    let first = Arc::new(
        TerrainCache::new_with_guard(
            quiet_options(dir.path(), &endpoint, &store),
            &SpyGuard::new(usize::MAX),
        )
        .unwrap(),
    );
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    let task = tokio::spawn({
        let first = Arc::clone(&first);
        async move {
            first
                .get_with_egress_guard(tile, &SpyGuard::new(usize::MAX))
                .await
                .map(drop)
        }
    });
    while archive_len(dir.path()) == 0 {
        assert!(!task.is_finished());
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    // An abandoned entry of a process that died, next to the live one.
    fs::write(dir.path().join(".archive.4242.0.tmp"), b"abandoned").unwrap();
    let live = temporary_paths(dir.path());
    assert_eq!(live.len(), 2);
    let _second = TerrainCache::new_with_guard(
        quiet_options(dir.path(), &endpoint, &store),
        &SpyGuard::new(usize::MAX),
    )
    .unwrap();
    let left = temporary_paths(dir.path());
    assert_eq!(
        left.len(),
        1,
        "only the abandoned entry was reclaimed: {left:?}"
    );
    assert!(
        left[0]
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&format!(".archive.{}.", std::process::id()))),
        "the live instance's archive stays: {left:?}"
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        temporary_paths(dir.path()).is_empty(),
        "its owner removed it"
    );
    server.abort();
}

// ---- B2 fix round, P3b fix 5: ownership across processes, the guard at each removal -----------------------

/// The directory the helper process of the cross-process control writes into. Unset in an
/// ordinary run, where the helper below does nothing.
const LIVE_WRITER_DIRECTORY: &str = "TESLATLAS_P3BF5_LIVE_WRITER_DIRECTORY";

/// Not a test of its own: the live writer of the cross-process control, run by the test
/// binary itself as a second process (R-P3bF5-1). It makes the three kinds of temporary
/// entry through the helper every cache write goes through, tells the parent their names
/// and keeps them, with whatever claim of ownership that helper makes, until its standard
/// input ends or it is killed.
#[test]
fn p3bf5_helper_a_live_writer_in_a_process_of_its_own() {
    let Some(directory) = std::env::var_os(LIVE_WRITER_DIRECTORY) else {
        return;
    };
    let root = Arc::new(
        crate::platform::private_directory::open_validated(Path::new(&directory), false)
            .expect("the shared cache root"),
    );
    let held: Vec<_> = ["archive", "hgt", "source"]
        .iter()
        .map(|label| create_private_temp(&root, label).expect("a live temporary entry"))
        .collect();
    let names: Vec<&str> = held.iter().map(|entry| entry.1.as_str()).collect();
    println!("p3bf5-ready {}", names.join(" "));
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    drop(held);
}

/// The helper process, killed when the guard goes (so a failing assertion leaves none).
struct LiveWriter {
    child: std::process::Child,
    names: Vec<String>,
}

/// How long the parent waits for the helper's next line of output before it gives up
/// (B2 fix round, P3b fix 6, N4): a liveness watchdog, not an acceptance bound. It is
/// long enough for a helper start on a loaded machine; a helper that has said nothing for
/// that long is stuck, and the parent fails instead of hanging.
const HANDSHAKE_WATCHDOG: Duration = Duration::from_secs(30);

/// Read the helper's handshake from its standard output, on a thread, with a bounded wait
/// for each line. The names of the entries it holds, or why there are none: on a timeout
/// or an early end of its output the helper is killed and reaped, and the reason names
/// what was read. The thread keeps draining the output so the helper never blocks on a
/// full pipe.
fn read_handshake(
    child: &mut std::process::Child,
    watchdog: Duration,
) -> Result<Vec<String>, String> {
    use std::io::BufRead as _;
    use std::sync::mpsc::{RecvTimeoutError, channel};
    let stdout = child.stdout.take().expect("stdout");
    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let _ = sender.send(line);
        }
    });
    let mut read = Vec::new();
    let failure = loop {
        match receiver.recv_timeout(watchdog) {
            // The harness prefixes the test's own output with "test <name> ... ".
            Ok(line) => {
                if let Some((_, names)) = line.split_once("p3bf5-ready ") {
                    return Ok(names.split(' ').map(str::to_owned).collect());
                }
                read.push(line);
            }
            Err(RecvTimeoutError::Timeout) => {
                break format!("the helper said nothing for {watchdog:?}; read so far: {read:?}");
            }
            Err(RecvTimeoutError::Disconnected) => {
                break format!("the helper ended before it held its entries; read: {read:?}");
            }
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    Err(failure)
}

impl LiveWriter {
    fn start(directory: &Path) -> Self {
        let test = format!(
            "{}::p3bf5_helper_a_live_writer_in_a_process_of_its_own",
            module_path!()
                .split_once("::")
                .map_or(module_path!(), |(_, path)| path)
        );
        let mut child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", &test, "--nocapture", "--test-threads=1"])
            .env(LIVE_WRITER_DIRECTORY, directory)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the helper process");
        let names = read_handshake(&mut child, HANDSHAKE_WATCHDOG)
            .unwrap_or_else(|why| panic!("{why} (test name {test})"));
        Self { child, names }
    }

    /// Kill the process without any chance to clean up after itself.
    fn kill(&mut self) {
        self.child.kill().expect("kill the helper");
        self.child.wait().expect("reap the helper");
    }
}

impl Drop for LiveWriter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn admitted_cache(directory: &Path, store: &HubStore) -> TerrainCache {
    TerrainCache::new_with_guard(
        quiet_options(directory, "http://127.0.0.1:1/", store),
        &SpyGuard::new(usize::MAX),
    )
    .expect("an admitted construction")
}

#[test]
fn p3bf5_a_second_process_keeps_the_entries_of_a_live_writer_until_it_is_gone() {
    // R-P3bF5-1: two Hub processes share the default terrain cache, each with a data
    // directory of its own. The registry of a process knows only its own writers, so an
    // entry another process holds cannot be told from one nobody holds by a registry: it
    // is told by the lock its writer keeps on it. This process reclaims the abandoned
    // entry next to the helper's live ones and leaves all three alone; after the helper
    // is killed (it cleans up nothing) the next reclaim takes them.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let mut writer = LiveWriter::start(dir.path());
    assert_eq!(writer.names.len(), 3);
    let abandoned = dir.path().join(".archive.4242.0.tmp");
    fs::write(&abandoned, b"abandoned").unwrap();
    let live_names = || {
        let mut names: Vec<String> = temporary_paths(dir.path())
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .map(str::to_owned)
            .collect();
        names.sort();
        names
    };
    let mut expected = writer.names.clone();
    expected.sort();

    for _ in 0..2 {
        admitted_cache(dir.path(), &store);
        assert!(!abandoned.exists(), "the abandoned entry was reclaimed");
        assert_eq!(
            live_names(),
            expected,
            "an entry another process holds was reclaimed"
        );
    }

    writer.kill();
    admitted_cache(dir.path(), &store);
    assert!(
        live_names().is_empty(),
        "the entries of the dead writer stay: {:?}",
        live_names()
    );
}

#[test]
fn p3bf5_a_lock_the_writer_dropped_makes_its_entry_abandoned_and_a_kept_one_is_reclaimed() {
    // The in-process counterpart: an entry whose writer is alive is kept, one whose lease
    // is released (kept after a refusal, or dropped) is reclaimed by the next construction.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let root =
        Arc::new(crate::platform::private_directory::open_validated(dir.path(), false).unwrap());
    let held = create_private_temp(&root, "archive").unwrap();
    let name = held.1.clone();
    admitted_cache(dir.path(), &store);
    assert!(dir.path().join(&name).exists(), "a live writer's entry");
    drop(held);
    admitted_cache(dir.path(), &store);
    assert!(!dir.path().join(&name).exists(), "the released entry");
}

#[test]
fn p3bf5_the_reclaim_consults_the_guard_before_each_removal_and_before_its_sync() {
    // R-P3bF5-2: the guard is consulted twice by the construction (in front of the
    // directory and in front of the reclaim), then immediately before each removal and
    // before the directory sync, so a loss acknowledged after the first look - during the
    // wait for a lock, a scan, anything - stops the very next mutation. Two abandoned
    // entries; the guard admits `allowed` consultations and then refuses (the admission
    // is lost and restored, as the spy does). Nothing is attempted after the refusal.
    for (allowed, removed, synced) in [(2, 0, 0), (3, 1, 0), (4, 2, 0), (5, 2, 1)] {
        let dir = tempdir().unwrap();
        let (_data, store) = test_store();
        // What opening the existing root attempts by itself (its ACL check, counted since
        // fix 5, R-P3bF5-3), measured on an empty directory of its own.
        let probe = tempdir().unwrap();
        let probe_before = crate::platform::file_attempts::count_under(probe.path());
        crate::platform::private_directory::open_validated(probe.path(), true).unwrap();
        let opened = crate::platform::file_attempts::count_under(probe.path()) - probe_before;
        let first = dir.path().join(".archive.4242.0.tmp");
        let second = dir.path().join(".hgt.4242.1.tmp");
        fs::write(&first, b"one").unwrap();
        fs::write(&second, b"two").unwrap();
        let before = crate::platform::file_attempts::count_under(dir.path());
        let guard = SpyGuard::new(allowed);
        let result = TerrainCache::new_with_guard(
            quiet_options(dir.path(), "http://127.0.0.1:1/", &store),
            &guard,
        );
        let attempted = crate::platform::file_attempts::count_under(dir.path()) - before;
        let left = temporary_paths(dir.path()).len();
        if allowed == 5 {
            assert!(result.is_ok(), "{allowed}: every consultation was admitted");
        } else {
            assert!(
                matches!(result, Err(TerrainCacheError::EgressDenied)),
                "{allowed}: {:?}",
                result.map(|_| ())
            );
        }
        assert_eq!(2 - left, removed, "{allowed}: entries removed");
        assert_eq!(
            attempted,
            opened + removed + synced,
            "{allowed}: file attempts (the root, removals and syncs) after the guard refused"
        );
    }
}

// ---- B2 fix round, P3b fix 6: file locking of the root, the lease, the handshake ---------------------------

#[test]
fn p3bf6_a_root_that_cannot_lock_is_refused_before_anything_is_touched() {
    // N1: the entries of the cache are owned by `flock`. A root whose filesystem cannot
    // take one (an NFS mount without locking, some FUSE mounts) would fail every cache
    // miss and leave a file behind that no reclaim could ever lock, so the construction
    // probes the root once, with a non-blocking lock on its descriptor, and refuses it
    // with a typed error. The probe creates, removes and renames nothing.
    for code in [
        rustix::io::Errno::NOTSUP.raw_os_error(),
        rustix::io::Errno::NOLCK.raw_os_error(),
    ] {
        let dir = tempdir().unwrap();
        let (_data, store) = test_store();
        let stale = dir.path().join(".archive.4242.0.tmp");
        fs::write(&stale, b"abandoned").unwrap();
        crate::platform::private_directory::INJECTED_LOCK_ERROR.with(|cell| cell.set(Some(code)));
        let refused = TerrainCache::new_with_guard(
            quiet_options(dir.path(), "http://127.0.0.1:1/", &store),
            &SpyGuard::new(usize::MAX),
        );
        let Err(TerrainCacheError::LockingUnsupported(source)) = refused else {
            panic!(
                "a root that cannot lock must be refused: {:?}",
                refused.map(|_| ())
            );
        };
        assert_eq!(source.raw_os_error(), Some(code));
        assert!(
            TerrainCacheError::LockingUnsupported(source)
                .to_string()
                .contains("does not support file locking")
        );
        assert!(
            stale.exists(),
            "nothing was reclaimed from the refused root"
        );
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            1,
            "nothing was created"
        );
    }
}

#[test]
fn p3bf6_a_root_that_can_lock_is_admitted_and_the_probe_touches_nothing() {
    // The control of the probe, on the real filesystem: the construction succeeds, also
    // while another open file description holds the root's lock (contention is the
    // answer "locking works"), and leaves the root as empty as it found it.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    admitted_cache(dir.path(), &store);
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        0,
        "the probe left no entry"
    );
    let holder = File::open(dir.path()).unwrap();
    rustix::fs::flock(
        &holder,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .expect("the test takes the root's lock");
    admitted_cache(dir.path(), &store);
    drop(holder);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn p3bf6_the_lease_alone_owns_an_entry_whose_data_file_is_closed() {
    // N2: the descriptor of the data file is not the only holder of an entry's lock: the
    // staged tile is closed before it is renamed, the source before the rename and the
    // archive after the download. The lease the entry carries keeps the lock then; an
    // entry that is kept (after a refusal) or dropped under a refusing guard lets it go,
    // and the next admitted construction reclaims it.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let root =
        Arc::new(crate::platform::private_directory::open_validated(dir.path(), false).unwrap());

    let (file, name, lease) = create_private_temp(&root, "hgt").unwrap();
    let mut entry = TemporaryEntry::new(Arc::clone(&root), name.clone(), lease, &|| true);
    drop(file);
    admitted_cache(dir.path(), &store);
    assert!(
        dir.path().join(&name).exists(),
        "the lease keeps the entry owned with its data file closed"
    );
    entry.keep();
    drop(entry);
    admitted_cache(dir.path(), &store);
    assert!(
        !dir.path().join(&name).exists(),
        "a kept entry is reclaimed"
    );

    let (file, name, lease) = create_private_temp(&root, "archive").unwrap();
    let entry = TemporaryEntry::new(Arc::clone(&root), name.clone(), lease, &|| false);
    drop(file);
    admitted_cache(dir.path(), &store);
    assert!(dir.path().join(&name).exists(), "owned until it is dropped");
    drop(entry);
    assert!(
        dir.path().join(&name).exists(),
        "a refusing guard keeps the file at the drop"
    );
    admitted_cache(dir.path(), &store);
    assert!(
        !dir.path().join(&name).exists(),
        "and the next construction reclaims it"
    );
}

#[test]
fn p3bf6_the_reclaim_does_not_remove_a_name_that_was_re_created_after_its_lock() {
    // N2: the reclaim locks a candidate it listed; the name is then renamed away and
    // re-created by a live writer (the ABA case: names restart at attempt 0). The lock
    // it holds is on the old file, so it checks that the name still is that file and
    // leaves the live writer's file alone.
    let dir = tempdir().unwrap();
    let (_data, store) = test_store();
    let name = ".hgt.4242.0.tmp";
    fs::write(dir.path().join(name), b"abandoned").unwrap();
    let live: std::rc::Rc<std::cell::RefCell<Option<File>>> = Default::default();
    let slot = std::rc::Rc::clone(&live);
    AFTER_LOCK.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(move |root, listed| {
            root.rename(listed, ".hgt.4242.7.tmp").unwrap();
            let fresh = root.create_exclusive(listed).unwrap();
            assert!(crate::platform::private_directory::try_lock_exclusive(&fresh).unwrap());
            *slot.borrow_mut() = Some(fresh);
        }));
    });
    admitted_cache(dir.path(), &store);
    let live = live.borrow_mut().take().expect("the step ran");
    let root = crate::platform::private_directory::open_validated(dir.path(), false).unwrap();
    assert!(
        root.is_entry(name, &live).unwrap(),
        "the live writer's re-created file was removed by a lock taken on the old one"
    );
    assert!(
        dir.path().join(".hgt.4242.7.tmp").exists(),
        "the old file, no longer under the listed name, is not the reclaim's to remove"
    );
}

#[test]
fn p3bf6_a_new_entry_whose_name_was_taken_away_after_its_lock_is_not_used() {
    // N2: another process's reclaim removes a new file between its creation and our lock.
    // The lock taken on the removed file says nothing, so the writer checks that its name
    // still is the file it locked and moves on to the next name.
    let dir = tempdir().unwrap();
    let root = crate::platform::private_directory::open_validated(dir.path(), false).unwrap();
    AFTER_LOCK.with(|cell| {
        *cell.borrow_mut() = Some(Box::new(|root, created| root.remove(created).unwrap()));
    });
    let (file, name, _lease) = create_private_temp(&root, "hgt").unwrap();
    assert!(
        name.ends_with(".1.tmp"),
        "the first name was taken away after its lock: {name}"
    );
    assert!(
        root.is_entry(&name, &file).unwrap(),
        "the entry is the locked file"
    );
}

#[test]
fn p3bf6_the_handshake_fails_instead_of_hanging_when_the_helper_goes_quiet() {
    // N4: a helper that prints nothing is killed when the watchdog expires, and the
    // failure names what was read; one that ends early names what it printed. Neither
    // depends on the machine's load: the first helper never prints, the second never
    // stays.
    let mut quiet = std::process::Command::new("/bin/sleep")
        .arg("600")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("a helper that says nothing");
    let why = read_handshake(&mut quiet, Duration::from_millis(200)).unwrap_err();
    assert!(why.contains("said nothing") && why.contains("[]"), "{why}");
    assert!(
        quiet.try_wait().unwrap().is_some(),
        "the helper was killed and reaped"
    );

    let mut chatty = std::process::Command::new("/bin/echo")
        .arg("unrelated output")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("a helper that ends at once");
    let why = read_handshake(&mut chatty, HANDSHAKE_WATCHDOG).unwrap_err();
    assert!(
        why.contains("ended before") && why.contains("unrelated output"),
        "{why}"
    );
}
