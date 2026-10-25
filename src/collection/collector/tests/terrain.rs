// SPDX-License-Identifier: AGPL-3.0-only

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
        let (_, status) = watch::channel(TerrainStatus::new(false));
        let _worker = TerrainWorker {
            status,
            logged_phase: None,
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
        terrain_error_code(&TerrainCacheError::LockingUnsupported(
            std::io::Error::from_raw_os_error(1)
        )),
        "locking_unsupported"
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
