// SPDX-License-Identifier: AGPL-3.0-only

#[tokio::test]
async fn terrain_pass_uses_an_explicit_private_cache_root() {
    // The cache root is explicit and temporary: the default resolver reads the
    // real user's home, which a test must not touch. Two missing levels below
    // the data directory mirror the Linux default layout.
    let data = crate::private_tempdir().expect("data");
    let store = HubStore::initialize(data.path()).expect("store");
    let config = TerrainConfig {
        cache_dir: Some(data.path().join("cache/terrain")),
        ..TerrainConfig::default()
    };
    let options =
        crate::terrain_cache::TerrainCacheOptions::from_config(&config, data.path(), store.clone())
            .expect("cache options");
    let cache = TerrainCache::new(options).expect("cache");
    assert_eq!(
        fs::metadata(data.path().join("cache/terrain"))
            .expect("cache root")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let mut fuse = TerrainFuse::default();
    assert_eq!(
        run_terrain_enrichment_pass(
            &store,
            &cache,
            &CursorKey::from_bytes([4; 32]),
            1,
            &mut fuse,
            None,
        )
        .await
        .expect("terrain pass"),
        0
    );
}

#[tokio::test]
async fn terrain_startup_with_an_untrusted_cache_root_is_nonfatal_and_writes_nothing() {
    let data = crate::private_tempdir().expect("data");
    let elsewhere = crate::private_tempdir().expect("elsewhere");
    let link = data.path().join("terrain-link");
    symlink(elsewhere.path(), &link).expect("symlinked cache root");
    let config = TerrainConfig {
        enabled: true,
        cache_dir: Some(link),
        ..TerrainConfig::default()
    };
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(data.path()).expect("admit runtime");
    let mut worker = spawn_terrain_worker(
        data.path().to_path_buf(),
        config,
        CursorKey::from_bytes([7; 32]),
        Some(admission),
    );

    worker
        .wait_until_initialized()
        .await
        .expect("an untrusted root is nonfatal");
    worker.start().expect("start inert worker");
    assert!(!worker.task.is_finished(), "inert worker remains owned");
    assert_eq!(
        fs::read_dir(elsewhere.path()).expect("target").count(),
        0,
        "nothing is written through the symlink"
    );
    worker
        .shutdown(false)
        .await
        .expect("inert worker joins on shutdown");
}

#[tokio::test]
async fn terrain_startup_failure_is_nonfatal_with_runtime_admission() {
    let data = crate::private_tempdir().expect("data");
    let config = TerrainConfig {
        enabled: true,
        min_free_bytes: 0,
        ..TerrainConfig::default()
    };
    let admission =
        crate::hub_user_process::AdmittedUserHub::for_test(data.path()).expect("admit runtime");
    let mut worker = spawn_terrain_worker(
        data.path().to_path_buf(),
        config,
        CursorKey::from_bytes([5; 32]),
        Some(admission),
    );

    worker
        .wait_until_initialized()
        .await
        .expect("terrain failure is nonfatal");
    worker.start().expect("start inert worker");
    assert!(!worker.task.is_finished(), "inert worker remains owned");
    worker
        .shutdown(false)
        .await
        .expect("inert worker joins on shutdown");
}

#[tokio::test]
async fn disabled_terrain_worker_does_not_open_store_or_cache() {
    let root = crate::private_tempdir().expect("root");
    let data = root.path().join("hub-data");
    let config = TerrainConfig {
        enabled: false,
        ..TerrainConfig::default()
    };
    let mut worker =
        spawn_terrain_worker(data.clone(), config, CursorKey::from_bytes([6; 32]), None);

    worker
        .wait_until_initialized()
        .await
        .expect("disabled terrain worker initializes");
    worker.start().expect("disabled terrain worker starts");
    assert!(!data.exists(), "disabled worker must not open Hub state");
    worker
        .shutdown(false)
        .await
        .expect("disabled worker shuts down");
}

#[tokio::test]
async fn aborting_outer_collector_owner_aborts_terrain_worker() {
    struct DropSignal(Option<oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(signal) = self.0.take() {
                let _ = signal.send(());
            }
        }
    }

    let (outer_ready_tx, outer_ready_rx) = oneshot::channel();
    let (child_ready_tx, child_ready_rx) = oneshot::channel();
    let (child_dropped_tx, child_dropped_rx) = oneshot::channel();
    let outer = tokio::spawn(async move {
        let (wake, _wakes) = mpsc::channel(1);
        let (_initialized_tx, initialized) = oneshot::channel();
        let (start, _started) = oneshot::channel();
        let (stop, _stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _drop_signal = DropSignal(Some(child_dropped_tx));
            let _ = child_ready_tx.send(());
            std::future::pending::<()>().await;
            Ok(())
        });
        let _worker = TerrainWorker {
            wake,
            initialized: Some(initialized),
            start: Some(start),
            stop: Some(stop),
            task,
        };
        let _ = child_ready_rx.await;
        let _ = outer_ready_tx.send(());
        std::future::pending::<()>().await;
    });

    outer_ready_rx.await.expect("outer owns terrain worker");
    outer.abort();
    let _ = outer.await;
    tokio::time::timeout(Duration::from_secs(1), child_dropped_rx)
        .await
        .expect("terrain task abort is bounded")
        .expect("terrain task was dropped");
}

#[test]
fn terrain_trust_failures_have_stable_persisted_codes() {
    use crate::platform::private_directory::PrivateDirectoryError;

    assert_eq!(
        terrain_error_code(&TerrainCacheError::UntrustedRoot(
            PrivateDirectoryError::InvalidPath
        )),
        "untrusted_root"
    );
    assert_eq!(
        terrain_error_code(&TerrainCacheError::Store(Box::new(
            crate::db::StoreError::InvalidTerrainTileDigest
        ))),
        "store"
    );
    assert_eq!(
        terrain_error_code(&TerrainCacheError::UnprovenTile),
        "unproven_tile"
    );
}
