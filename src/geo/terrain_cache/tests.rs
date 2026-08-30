// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs::FileTimes,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use axum::{
    Router,
    body::Body,
    http::{StatusCode, Uri},
    response::Response,
    routing::any,
};
use tempfile::tempdir;

use super::*;
use crate::{geocoder::EgressGuardError, terrain::SRTM3_SIDE};

#[derive(Clone)]
struct RevocableEgressGuard(Arc<AtomicBool>);

impl EgressGuard for RevocableEgressGuard {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        self.0
            .load(Ordering::Acquire)
            .then_some(())
            .ok_or(EgressGuardError)
    }
}

fn hgt_bytes() -> Vec<u8> {
    vec![0; SRTM3_BYTES as usize]
}

fn zip_bytes() -> Vec<u8> {
    let mut output = std::io::Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(&mut output);
    archive
        .start_file("N51W001.hgt", zip::write::SimpleFileOptions::default())
        .unwrap();
    archive.write_all(&hgt_bytes()).unwrap();
    archive.finish().unwrap();
    output.into_inner()
}

fn options(root: &Path, endpoint: &str) -> TerrainCacheOptions {
    TerrainCacheOptions {
        root: root.to_owned(),
        min_free_bytes: 1,
        max_cache_bytes: 512 * 1024 * 1024,
        connect_timeout: Duration::from_secs(2),
        read_timeout: Duration::from_secs(2),
        aws_base: endpoint.to_owned(),
        esa_base: endpoint.to_owned(),
        free_space_override: None,
    }
}

fn write_cached_tile(root: &Path, tile: TileId, modified_seconds: u64) {
    let hgt = root.join(format!("{}.hgt", tile.name()));
    let source = root.join(format!("{}.source", tile.name()));
    fs::write(&hgt, hgt_bytes()).unwrap();
    fs::write(&source, "aws").unwrap();
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(modified_seconds);
    File::options()
        .write(true)
        .open(&hgt)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
}

async fn server<F>(handler: F) -> (String, tokio::task::JoinHandle<()>)
where
    F: Fn(Uri) -> Response<Body> + Clone + Send + Sync + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let app = Router::new().route("/{*path}", any(move |uri: Uri| async move { handler(uri) }));
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (endpoint, task)
}

#[tokio::test]
async fn cache_hit_does_not_use_network() {
    let dir = tempdir().unwrap();
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    fs::write(dir.path().join(format!("{}.hgt", tile.name())), hgt_bytes()).unwrap();
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/")).unwrap();
    assert_eq!(cache.get(tile).await.unwrap().side(), SRTM3_SIDE);
    assert!(cache.locks.lock().unwrap().is_empty());
}

#[test]
fn cache_quota_evicts_the_oldest_complete_tile_before_a_download() {
    let dir = tempdir().unwrap();
    let old = TileId::from_coordinates(51.5, -0.1).unwrap();
    let new = TileId::from_coordinates(52.5, -0.1).unwrap();
    write_cached_tile(dir.path(), old, 1);
    write_cached_tile(dir.path(), new, 2);
    let mut opts = options(dir.path(), "http://127.0.0.1:1/");
    opts.max_cache_bytes = MAX_TILE_CACHE_BYTES + SRTM3_BYTES + 3;
    let cache = TerrainCache::new(opts).unwrap();

    cache
        .enforce_cache_quota(None, MAX_TILE_CACHE_BYTES)
        .unwrap();

    assert!(!cache.tile_path(old).exists());
    assert!(!cache.source_path(old).exists());
    assert!(cache.tile_path(new).exists());
    assert!(cache.source_path(new).exists());
}

#[tokio::test]
async fn active_lookup_returns_cached_provenance_without_network() {
    let dir = tempdir().unwrap();
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    fs::write(dir.path().join(format!("{}.hgt", tile.name())), hgt_bytes()).unwrap();
    fs::write(dir.path().join(format!("{}.source", tile.name())), "aws").unwrap();
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/")).unwrap();
    let result = cache
        .lookup(51.5, -0.1, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(result.elevation_m, Some(0));
    assert_eq!(result.tile_name, tile.name());
    assert_eq!(result.dataset_source, "aws");
    assert_eq!(
        result.dataset_version,
        crate::terrain::TERRAIN_DATASET_VERSION
    );
    assert_eq!(result.tile_hash.len(), 64);
    assert_eq!(cache.hashes.lock().unwrap().len(), 1);

    let repeated = cache
        .lookup(51.5, -0.1, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(repeated.tile_hash, result.tile_hash);
    assert_eq!(cache.hashes.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cached_hash_is_invalidated_by_atomic_tile_replacement() {
    let dir = tempdir().unwrap();
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    let path = dir.path().join(format!("{}.hgt", tile.name()));
    fs::write(&path, hgt_bytes()).unwrap();
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/")).unwrap();
    let first = cache
        .lookup(51.5, -0.1, Duration::from_secs(1))
        .await
        .unwrap();

    let replacement = dir.path().join("replacement.hgt");
    let mut bytes = hgt_bytes();
    bytes[0] = 1;
    fs::write(&replacement, bytes).unwrap();
    fs::rename(replacement, &path).unwrap();
    let second = cache
        .lookup(51.5, -0.1, Duration::from_secs(1))
        .await
        .unwrap();

    assert_ne!(second.tile_hash, first.tile_hash);
    assert_eq!(cache.hashes.lock().unwrap().len(), 1);
}

#[test]
fn source_marker_fifo_is_ignored_without_blocking() {
    let dir = tempdir().unwrap();
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    let source = dir.path().join(format!("{}.source", tile.name()));
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&source)
            .status()
            .expect("run mkfifo")
            .success()
    );
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/")).unwrap();

    let started = std::time::Instant::now();
    assert_eq!(cache.read_source(tile), "cache");
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn production_sources_are_https_only() {
    assert!(AWS_SKADI_BASE.starts_with("https://"));
    assert!(ESA_SRTM_BASE.starts_with("https://"));
    assert!(!AWS_SKADI_BASE.contains("http://"));
    assert!(!ESA_SRTM_BASE.contains("http://"));
}

#[test]
fn startup_removes_only_stale_owned_temporary_files() {
    let dir = tempdir().unwrap();
    let stale = dir.path().join(".archive.123.0.tmp");
    let fresh = dir.path().join(".hgt.123.1.tmp");
    let unrelated = dir.path().join("notes.tmp");
    fs::write(&stale, b"stale").unwrap();
    fs::write(&fresh, b"fresh").unwrap();
    fs::write(&unrelated, b"keep").unwrap();
    File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
        .unwrap();

    TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/")).unwrap();

    assert!(!stale.exists());
    assert!(fresh.exists());
    assert!(unrelated.exists());
}

#[test]
fn cache_quota_counts_fresh_temporary_files() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(".source.123.0.tmp"), b"x").unwrap();
    let mut opts = options(dir.path(), "http://127.0.0.1:1/");
    opts.max_cache_bytes = MAX_TILE_CACHE_BYTES;
    let cache = TerrainCache::new(opts).unwrap();

    assert!(matches!(
        cache.enforce_cache_quota(None, MAX_TILE_CACHE_BYTES),
        Err(TerrainCacheError::CacheQuotaExceeded)
    ));
}

#[tokio::test]
async fn active_lookup_enforces_budget() {
    let dir = tempdir().unwrap();
    let cache = TerrainCache::new(options(dir.path(), "http://127.0.0.1:1/")).unwrap();
    assert!(matches!(
        cache.lookup(f64::NAN, -0.1, Duration::ZERO).await,
        Err(TerrainCacheError::Timeout)
    ));
}

#[tokio::test]
async fn aws_failure_falls_back_to_esa_and_concurrent_requests_share_lock() {
    let dir = tempdir().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let count_for_server = Arc::clone(&count);
    let (endpoint, task) = server(move |uri| {
        count_for_server.fetch_add(1, Ordering::SeqCst);
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
    let mut opts = options(dir.path(), &endpoint);
    opts.esa_base = endpoint.clone();
    let cache = Arc::new(TerrainCache::new(opts).unwrap());
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    let (left, right) = tokio::join!(cache.get(tile), cache.get(tile));
    assert_eq!(left.unwrap().side(), SRTM3_SIDE);
    assert_eq!(right.unwrap().side(), SRTM3_SIDE);
    assert_eq!(count.load(Ordering::SeqCst), 2);
    task.abort();
}

#[tokio::test]
async fn revoked_before_esa_fallback_blocks_second_provider_request() {
    let dir = tempdir().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let admitted = Arc::new(AtomicBool::new(true));
    let requests_for_server = Arc::clone(&requests);
    let admission_for_server = Arc::clone(&admitted);
    let (endpoint, task) = server(move |uri| {
        requests_for_server.fetch_add(1, Ordering::SeqCst);
        if uri.path().ends_with(".gz") {
            admission_for_server.store(false, Ordering::Release);
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap()
        } else {
            Response::new(Body::from(zip_bytes()))
        }
    })
    .await;
    let mut opts = options(dir.path(), &endpoint);
    opts.esa_base = endpoint;
    let cache = TerrainCache::new(opts).unwrap();
    let guard = RevocableEgressGuard(admitted);
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();

    assert!(matches!(
        cache.get_with_egress_guard(tile, &guard).await,
        Err(TerrainCacheError::EgressDenied)
    ));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    task.abort();
}

#[tokio::test]
async fn provider_redirect_is_not_followed_without_a_second_guard() {
    let dir = tempdir().unwrap();
    let redirected_requests = Arc::new(AtomicUsize::new(0));
    let redirected_requests_for_server = Arc::clone(&redirected_requests);
    let (redirected_endpoint, redirected_task) = server(move |_| {
        redirected_requests_for_server.fetch_add(1, Ordering::SeqCst);
        Response::new(Body::from(zip_bytes()))
    })
    .await;
    let (redirecting_endpoint, redirecting_task) = server(move |_| {
        Response::builder()
            .status(StatusCode::FOUND)
            .header(axum::http::header::LOCATION, redirected_endpoint.clone())
            .body(Body::empty())
            .unwrap()
    })
    .await;
    let cache = TerrainCache::new(options(dir.path(), &redirecting_endpoint)).unwrap();
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();

    assert!(matches!(
        cache.get(tile).await,
        Err(TerrainCacheError::BadResponse)
    ));
    assert_eq!(redirected_requests.load(Ordering::SeqCst), 0);
    redirecting_task.abort();
    redirected_task.abort();
}

#[tokio::test]
async fn corrupt_zip_and_low_space_are_rejected() {
    let dir = tempdir().unwrap();
    let (endpoint, task) = server(|_| Response::new(Body::from(b"not-a-zip".to_vec()))).await;
    let cache = TerrainCache::new(options(dir.path(), &endpoint)).unwrap();
    let tile = TileId::from_coordinates(51.5, -0.1).unwrap();
    assert!(matches!(
        cache.get(tile).await,
        Err(TerrainCacheError::InvalidArchive)
    ));
    task.abort();

    let mut low = options(dir.path(), "http://127.0.0.1:1/");
    low.free_space_override = Some(0);
    let cache = TerrainCache::new(low).unwrap();
    assert!(matches!(
        cache.get(tile).await,
        Err(TerrainCacheError::InsufficientSpace)
    ));
}
