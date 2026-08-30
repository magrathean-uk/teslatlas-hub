// SPDX-License-Identifier: AGPL-3.0-only

#[tokio::test]
async fn terrain_pass_uses_the_safe_cache_resolver() {
    let data = crate::private_tempdir().expect("data");
    let store = HubStore::initialize(data.path()).expect("store");
    let options = crate::terrain_cache::TerrainCacheOptions::from_config(
        &TerrainConfig::default(),
        data.path(),
    )
    .expect("cache options");
    let cache = TerrainCache::new(options).expect("cache");
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
