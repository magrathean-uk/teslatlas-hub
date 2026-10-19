// SPDX-License-Identifier: AGPL-3.0-only

//! Bounded, restart-safe SRTM tile acquisition.
//!
//! The cache root is validated once as a private directory of the effective
//! user (G2-3f) and held by descriptor: every read, temporary file, rename and
//! removal names a single entry of that descriptor, so replacing a path
//! component after validation cannot move the cache. A cached `.hgt` file is
//! admitted only when its SHA-256 equals the digest the Hub recorded in its
//! store when it downloaded the tile; a file without a matching record is
//! unproven, is discarded and is fetched again. The tile that was hashed is
//! the one sampled: callers get the opened descriptor, never a second open by
//! path.

use std::{
    collections::HashMap,
    ffi::OsStr,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use reqwest::{Client, redirect::Policy};
use thiserror::Error;
use tokio::{io::AsyncWriteExt, sync::Mutex as AsyncMutex, time::timeout};
use url::Url;
use zip::ZipArchive;

use crate::{
    config::TerrainConfig,
    db::{HubStore, StoreError},
    geocoder::EgressGuard,
    platform::private_directory::{self, PrivateDirectory, PrivateDirectoryError},
    terrain::{HgtFileIdentity, HgtTile, SRTM1_BYTES, SRTM3_BYTES, TerrainError, TileId},
};

pub const AWS_SKADI_BASE: &str = "https://elevation-tiles-prod.s3.amazonaws.com/";
pub const ESA_SRTM_BASE: &str = "https://step.esa.int/auxdata/dem/SRTMGL1/";
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HGT_BYTES: u64 = SRTM1_BYTES;
const MAX_SOURCE_BYTES: u64 = 16;
const MAX_TILE_CACHE_BYTES: u64 = MAX_HGT_BYTES + MAX_SOURCE_BYTES;

#[derive(Debug, Error)]
pub enum TerrainCacheError {
    #[error("terrain cache configuration is invalid")]
    InvalidConfig,
    #[error("terrain cache filesystem operation failed")]
    Io(#[source] io::Error),
    #[error("terrain cache network operation failed")]
    Network(#[source] reqwest::Error),
    #[error("terrain source returned an unusable response")]
    BadResponse,
    #[error("terrain source archive is invalid or too large")]
    InvalidArchive,
    #[error("terrain cache has insufficient free space")]
    InsufficientSpace,
    #[error("terrain cache quota cannot accommodate another tile")]
    CacheQuotaExceeded,
    #[error("terrain cache lookup timed out")]
    Timeout,
    #[error("terrain tile is invalid")]
    InvalidTile(#[source] TerrainError),
    #[error("terrain egress admission is no longer valid")]
    EgressDenied,
    #[error("terrain cache root is not a trusted private directory")]
    UntrustedRoot(#[source] PrivateDirectoryError),
    #[error("terrain cache root does not support file locking")]
    LockingUnsupported(#[source] io::Error),
    #[error("terrain tile digest store operation failed")]
    Store(#[source] Box<StoreError>),
    #[error("terrain tile has no matching recorded digest")]
    UnprovenTile,
}

impl TerrainCacheError {
    fn store(error: StoreError) -> Self {
        Self::Store(Box::new(error))
    }
}

/// The guard in front of one persistent cache mutation (B2 fix round, P3b fix 3,
/// R-P3bF3-2): a directory created or tightened, a file written, renamed or removed,
/// a digest recorded. It is consulted immediately before the mutation, with no await
/// between, and after every lock or download the mutation waited for. A refusal is
/// `EgressDenied`, which the collector's pass reports as the loss of its sensitive
/// access; nothing is created, changed or deleted by the refused step.
fn permit<G: EgressGuard + ?Sized>(guard: &G) -> Result<(), TerrainCacheError> {
    guard
        .assert_egress_allowed()
        .map_err(|_| TerrainCacheError::EgressDenied)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerrainLookupResult {
    pub elevation_m: Option<i16>,
    pub tile_name: String,
    pub tile_hash: String,
    pub dataset_source: String,
    pub dataset_version: String,
}

#[derive(Clone)]
pub struct TerrainCacheOptions {
    /// Absolute path of the cache root. It must be a private directory of the
    /// effective user (see `TerrainCache::new`); a missing root is created.
    pub root: std::path::PathBuf,
    /// Where the digest of every downloaded tile is recorded and checked.
    pub store: HubStore,
    pub min_free_bytes: u64,
    pub max_cache_bytes: u64,
    pub connect_timeout: Duration,
    pub read_timeout: Duration,
    #[cfg(test)]
    pub aws_base: String,
    #[cfg(test)]
    pub esa_base: String,
    #[cfg(test)]
    pub free_space_override: Option<u64>,
}

impl TerrainCacheOptions {
    pub fn from_config(
        config: &TerrainConfig,
        data_dir: &Path,
        store: HubStore,
    ) -> Result<Self, TerrainCacheError> {
        config
            .validate()
            .map_err(|_| TerrainCacheError::InvalidConfig)?;
        Ok(Self {
            root: config.resolved_cache_dir(data_dir),
            store,
            min_free_bytes: config.min_free_bytes,
            max_cache_bytes: config.max_cache_bytes,
            connect_timeout: Duration::from_secs(config.connect_timeout_seconds),
            read_timeout: Duration::from_secs(config.read_timeout_seconds),
            #[cfg(test)]
            aws_base: AWS_SKADI_BASE.to_owned(),
            #[cfg(test)]
            esa_base: ESA_SRTM_BASE.to_owned(),
            #[cfg(test)]
            free_space_override: None,
        })
    }
}

pub struct TerrainCache {
    options: TerrainCacheOptions,
    root: Arc<PrivateDirectory>,
    client: Client,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    quota_lock: AsyncMutex<()>,
    hashes: Mutex<HashMap<String, (HgtFileIdentity, String)>>,
}

/// A tile that passed admission: the opened file, the digest it matched and
/// the identity it had when it was hashed.
struct AdmittedTile {
    tile: HgtTile,
    identity: HgtFileIdentity,
    sha256: String,
    source: String,
}

impl AdmittedTile {
    /// Fail when the admitted file was rewritten after it was hashed, so a
    /// sample is never reported under a digest the bytes no longer have.
    fn confirm_unchanged(&self) -> Result<(), TerrainCacheError> {
        let current = self
            .tile
            .file_identity()
            .map_err(TerrainCacheError::InvalidTile)?;
        if current != Some(self.identity) {
            return Err(TerrainCacheError::InvalidTile(TerrainError::Io(
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "terrain tile changed after admission",
                ),
            )));
        }
        Ok(())
    }
}

impl TerrainCache {
    /// Validate the cache root and open the cache.
    ///
    /// The root must be an absolute path whose final component is not a
    /// symlink and is owned by the effective user; an existing root with a
    /// wider mode than `0700` is tightened to it. Every existing ancestor must
    /// be owned by root or the effective user and may be group- or
    /// world-writable only when the effective user owns it (or root owns it
    /// with the sticky bit). Missing components are created `0700`, one at a
    /// time, below an ancestor that passed those rules.
    #[cfg(any(not(target_os = "macos"), test))]
    pub fn new(options: TerrainCacheOptions) -> Result<Self, TerrainCacheError> {
        Self::new_with_guard(options, &crate::geocoder::UnguardedEgress)
    }

    /// [`Self::new`] with the run's guard in front of the directory creation and
    /// tightening and in front of the reclaim of abandoned temporary files (B2 fix
    /// round, P3b fix 3, R-P3bF3-2; fix 4, R-P3bF4-6; fix 5, R-P3bF5-2): a refused guard
    /// creates, tightens and removes nothing, and the reclaim asks it again immediately
    /// before each removal and before its directory sync.
    pub fn new_with_guard<G: EgressGuard + ?Sized>(
        options: TerrainCacheOptions,
        guard: &G,
    ) -> Result<Self, TerrainCacheError> {
        if options.min_free_bytes == 0
            || options.max_cache_bytes < MAX_TILE_CACHE_BYTES
            || options.connect_timeout.is_zero()
            || options.read_timeout.is_zero()
        {
            return Err(TerrainCacheError::InvalidConfig);
        }
        permit(guard)?;
        let root = Arc::new(
            private_directory::open_validated(&options.root, true)
                .map_err(TerrainCacheError::UntrustedRoot)?,
        );
        permit(guard)?;
        // The entries of the cache are owned by locks (see [`EntryLease`]): a root whose
        // filesystem cannot take one would fail every cache miss and leave an unremovable
        // file behind each attempt, so it is refused here, before anything is created
        // in it. The probe touches no file.
        root.probe_lock_support()
            .map_err(TerrainCacheError::LockingUnsupported)?;
        reclaim_abandoned_temporary_files(&root, guard)?;
        crate::crypto::install_default_provider();
        let client = Client::builder()
            .connect_timeout(options.connect_timeout)
            .timeout(options.read_timeout)
            .redirect(Policy::none())
            .https_only(!cfg!(test))
            .build()
            .map_err(TerrainCacheError::Network)?;
        Ok(Self {
            options,
            root,
            client,
            locks: Mutex::new(HashMap::new()),
            quota_lock: AsyncMutex::new(()),
            hashes: Mutex::new(HashMap::new()),
        })
    }

    #[cfg(any(not(target_os = "macos"), test))]
    pub async fn get(&self, tile: TileId) -> Result<HgtTile, TerrainCacheError> {
        self.get_with_egress_guard(tile, &crate::geocoder::UnguardedEgress)
            .await
    }

    /// Load or acquire one tile, checking egress admission immediately before
    /// each provider request.  The same guard is used for both AWS and ESA.
    pub async fn get_with_egress_guard<G: EgressGuard + ?Sized>(
        &self,
        tile: TileId,
        egress_guard: &G,
    ) -> Result<HgtTile, TerrainCacheError> {
        Ok(self.ensure_tile(tile, egress_guard).await?.tile)
    }

    #[cfg(any(not(target_os = "macos"), test))]
    pub async fn lookup(
        &self,
        latitude: f64,
        longitude: f64,
        budget: Duration,
    ) -> Result<TerrainLookupResult, TerrainCacheError> {
        self.lookup_with_egress_guard(
            latitude,
            longitude,
            budget,
            &crate::geocoder::UnguardedEgress,
        )
        .await
    }

    /// Bounded lookup with a final egress guard for each cache-miss provider
    /// attempt.
    pub async fn lookup_with_egress_guard<G: EgressGuard + ?Sized>(
        &self,
        latitude: f64,
        longitude: f64,
        budget: Duration,
        egress_guard: &G,
    ) -> Result<TerrainLookupResult, TerrainCacheError> {
        if budget.is_zero() {
            return Err(TerrainCacheError::Timeout);
        }
        timeout(
            budget,
            self.lookup_unbounded_with_egress_guard(latitude, longitude, egress_guard),
        )
        .await
        .map_err(|_| TerrainCacheError::Timeout)?
    }

    async fn lookup_unbounded_with_egress_guard<G: EgressGuard + ?Sized>(
        &self,
        latitude: f64,
        longitude: f64,
        egress_guard: &G,
    ) -> Result<TerrainLookupResult, TerrainCacheError> {
        let tile = TileId::from_coordinates(latitude, longitude)
            .map_err(TerrainCacheError::InvalidTile)?;
        let admitted = self.ensure_tile(tile, egress_guard).await?;
        // Sample the descriptor that was hashed, then confirm the file kept the
        // identity it had when it was hashed.
        let elevation_m = admitted
            .tile
            .elevation_at(latitude, longitude)
            .map_err(TerrainCacheError::InvalidTile)?;
        admitted.confirm_unchanged()?;
        Ok(TerrainLookupResult {
            elevation_m,
            tile_name: tile.name(),
            tile_hash: admitted.sha256,
            dataset_source: admitted.source,
            dataset_version: crate::terrain::TERRAIN_DATASET_VERSION.to_owned(),
        })
    }

    /// The tile's digest and the identity it was computed for. The digest of
    /// an unchanged file identity is not computed twice.
    fn tile_hash(
        &self,
        name: &str,
        tile: &HgtTile,
    ) -> Result<(HgtFileIdentity, String), TerrainCacheError> {
        let identity = tile
            .file_identity()
            .map_err(TerrainCacheError::InvalidTile)?
            .ok_or(TerrainCacheError::InvalidConfig)?;
        {
            let hashes = self
                .hashes
                .lock()
                .map_err(|_| TerrainCacheError::InvalidConfig)?;
            if let Some((cached_identity, hash)) = hashes.get(name)
                && *cached_identity == identity
            {
                return Ok((identity, hash.clone()));
            }
        }
        let hash = tile.sha256_hex().map_err(TerrainCacheError::InvalidTile)?;
        let stable_identity = tile
            .file_identity()
            .map_err(TerrainCacheError::InvalidTile)?
            .ok_or(TerrainCacheError::InvalidConfig)?;
        if stable_identity != identity {
            return Err(TerrainCacheError::InvalidTile(TerrainError::Io(
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "terrain tile changed while hashing",
                ),
            )));
        }
        let mut hashes = self
            .hashes
            .lock()
            .map_err(|_| TerrainCacheError::InvalidConfig)?;
        if hashes.len() >= 64 && !hashes.contains_key(name) {
            hashes.clear();
        }
        hashes.insert(name.to_owned(), (identity, hash.clone()));
        Ok((identity, hash))
    }

    /// Admit the cached tile when it exists and its digest equals the one the
    /// Hub recorded at download. A file that is not a valid tile, has no
    /// record, or does not match its record is unproven: it is discarded and
    /// `None` says the tile must be fetched.
    fn admit<G: EgressGuard + ?Sized>(
        &self,
        tile: TileId,
        guard: &G,
    ) -> Result<Option<AdmittedTile>, TerrainCacheError> {
        let name = tile.name();
        let file_name = tile_file(tile);
        let hgt = match HgtTile::open_in(&name, self.root.as_ref(), &file_name) {
            Ok(hgt) => hgt,
            Err(TerrainError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(_) => {
                self.discard_tile(tile, guard)?;
                return Ok(None);
            }
        };
        let recorded = self
            .options
            .store
            .terrain_tile_digest(&name)
            .map_err(TerrainCacheError::store)?;
        let Some(recorded) = recorded else {
            self.discard_tile(tile, guard)?;
            return Ok(None);
        };
        let (identity, sha256) = self.tile_hash(&name, &hgt)?;
        if sha256 != recorded.sha256 {
            self.discard_tile(tile, guard)?;
            return Ok(None);
        }
        self.enforce_cache_quota(Some(&file_name), 0, guard)?;
        Ok(Some(AdmittedTile {
            tile: hgt,
            identity,
            sha256,
            source: self.read_source(tile),
        }))
    }

    async fn ensure_tile<G: EgressGuard + ?Sized>(
        &self,
        tile: TileId,
        egress_guard: &G,
    ) -> Result<AdmittedTile, TerrainCacheError> {
        let lock = {
            let mut locks = self
                .locks
                .lock()
                .map_err(|_| TerrainCacheError::InvalidConfig)?;
            locks
                .entry(tile.name())
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };
        let guard = lock.lock().await;
        let result = async {
            let _quota_guard = self.quota_lock.lock().await;
            // Both locks were awaited: a loss acknowledged meanwhile stops the lookup
            // before it discards, evicts or writes anything (R-P3bF3-2).
            permit(egress_guard)?;
            if let Some(admitted) = self.admit(tile, egress_guard)? {
                return Ok(admitted);
            }
            self.ensure_space()?;
            self.enforce_cache_quota(None, MAX_TILE_CACHE_BYTES, egress_guard)?;
            let source = match self.download(&tile, true, egress_guard).await {
                Ok(source) => source,
                // Admission failure is not a provider failure.  Do not use a
                // second provider after the first final egress check denied it.
                Err(TerrainCacheError::EgressDenied) => {
                    return Err(TerrainCacheError::EgressDenied);
                }
                Err(_) => self.download(&tile, false, egress_guard).await?,
            };
            permit(egress_guard)?;
            let may_remove = || permit(egress_guard).is_ok();
            write_source_atomic(&self.root, &source_file(tile), source, &may_remove)?;
            // The download recorded its digest before the tile was put in
            // place, so admission now finds the record it needs.
            self.admit(tile, egress_guard)?
                .ok_or(TerrainCacheError::UnprovenTile)
        };
        let result = result.await;
        drop(guard);
        if let Ok(mut locks) = self.locks.lock()
            && Arc::strong_count(&lock) == 2
        {
            locks.remove(&tile.name());
        }
        result
    }

    /// The validated root, as resolved. Diagnostics and tests only: every
    /// operation goes through the held descriptor.
    pub(crate) fn root_path(&self) -> &Path {
        self.root.path()
    }

    #[cfg(test)]
    fn tile_path(&self, tile: TileId) -> std::path::PathBuf {
        self.root.path().join(tile_file(tile))
    }

    #[cfg(test)]
    fn source_path(&self, tile: TileId) -> std::path::PathBuf {
        self.root.path().join(source_file(tile))
    }

    fn read_source(&self, tile: TileId) -> String {
        self.root
            .open_regular(&source_file(tile))
            .ok()
            .flatten()
            .filter(|file| {
                file.metadata()
                    .is_ok_and(|metadata| metadata.len() <= MAX_SOURCE_BYTES)
            })
            .and_then(|mut file| {
                let mut bytes = Vec::new();
                Read::by_ref(&mut file)
                    .take(MAX_SOURCE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .ok()
                    .filter(|_| bytes.len() as u64 <= MAX_SOURCE_BYTES)
                    .and_then(|_| String::from_utf8(bytes).ok())
            })
            .filter(|source| matches!(source.trim(), "aws" | "esa"))
            .unwrap_or_else(|| "cache".to_owned())
    }

    /// Evict the oldest complete tiles until `reserved_bytes` more fit under
    /// the quota. `protected` is the file name of a tile that must stay.
    fn enforce_cache_quota<G: EgressGuard + ?Sized>(
        &self,
        protected: Option<&str>,
        reserved_bytes: u64,
        guard: &G,
    ) -> Result<(), TerrainCacheError> {
        #[derive(Debug)]
        struct CachedTile {
            hgt: String,
            source: String,
            bytes: u64,
            modified: SystemTime,
        }

        let mut total = 0_u64;
        let mut tiles = Vec::new();
        for name in self.root.entry_names().map_err(TerrainCacheError::Io)? {
            if is_owned_temporary_name(OsStr::new(&name)) {
                if let Some(file) = self
                    .root
                    .open_regular(&name)
                    .map_err(TerrainCacheError::Io)?
                {
                    let length = file.metadata().map_err(TerrainCacheError::Io)?.len();
                    total = total.saturating_add(length);
                }
                continue;
            }
            let Some(stem) = name.strip_suffix(".hgt").filter(|stem| !stem.is_empty()) else {
                continue;
            };
            let Some(file) = self
                .root
                .open_regular(&name)
                .map_err(TerrainCacheError::Io)?
            else {
                continue;
            };
            let metadata = file.metadata().map_err(TerrainCacheError::Io)?;
            let source = format!("{stem}.source");
            let source_bytes = match self
                .root
                .open_regular(&source)
                .map_err(TerrainCacheError::Io)?
            {
                Some(file) => file.metadata().map_err(TerrainCacheError::Io)?.len(),
                None => 0,
            };
            let bytes = metadata.len().saturating_add(source_bytes);
            total = total.saturating_add(bytes);
            tiles.push(CachedTile {
                hgt: name.clone(),
                source,
                bytes,
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
        tiles.sort_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.hgt.cmp(&right.hgt))
        });
        let mut evicted = false;
        for tile in tiles {
            if total.saturating_add(reserved_bytes) <= self.options.max_cache_bytes {
                break;
            }
            if protected.is_some_and(|name| name == tile.hgt) {
                continue;
            }
            permit(guard)?;
            self.root
                .remove(&tile.source)
                .map_err(TerrainCacheError::Io)?;
            self.root.remove(&tile.hgt).map_err(TerrainCacheError::Io)?;
            total = total.saturating_sub(tile.bytes);
            evicted = true;
        }
        if total.saturating_add(reserved_bytes) > self.options.max_cache_bytes {
            return Err(TerrainCacheError::CacheQuotaExceeded);
        }
        // The directory is synced for the removals only, and like them behind the guard
        // (B2 fix round, P3b fix 5, R-P3bF5-3: the counted sync found one that ran after
        // every enforcement, evicting or not, with no consultation of its own).
        if evicted {
            permit(guard)?;
            let _ = self.root.sync();
        }
        Ok(())
    }

    fn ensure_space(&self) -> Result<(), TerrainCacheError> {
        let required = self
            .options
            .min_free_bytes
            .saturating_add(MAX_ARCHIVE_BYTES)
            .saturating_add(MAX_HGT_BYTES);
        #[cfg(test)]
        if let Some(bytes) = self.options.free_space_override
            && bytes < required
        {
            return Err(TerrainCacheError::InsufficientSpace);
        }
        #[cfg(not(test))]
        let available = self.root.available_bytes().map_err(TerrainCacheError::Io)?;
        #[cfg(not(test))]
        if available < required {
            return Err(TerrainCacheError::InsufficientSpace);
        }
        Ok(())
    }

    async fn download<G: EgressGuard + ?Sized>(
        &self,
        tile: &TileId,
        aws: bool,
        egress_guard: &G,
    ) -> Result<&'static str, TerrainCacheError> {
        let base = if aws {
            #[cfg(test)]
            {
                &self.options.aws_base
            }
            #[cfg(not(test))]
            {
                AWS_SKADI_BASE
            }
        } else {
            #[cfg(test)]
            {
                &self.options.esa_base
            }
            #[cfg(not(test))]
            {
                ESA_SRTM_BASE
            }
        };
        let base = Url::parse(base).map_err(|_| TerrainCacheError::InvalidConfig)?;
        let name = tile.name();
        let relative = if aws {
            format!("skadi/{}/{name}.hgt.gz", &name[..3])
        } else {
            format!("{name}.hgt.zip")
        };
        let url = base
            .join(&relative)
            .map_err(|_| TerrainCacheError::InvalidConfig)?;
        if !cfg!(test) && url.scheme() != "https" {
            return Err(TerrainCacheError::InvalidConfig);
        }
        egress_guard
            .assert_egress_allowed()
            .map_err(|_| TerrainCacheError::EgressDenied)?;
        let response = timeout(self.options.connect_timeout, self.client.get(url).send())
            .await
            .map_err(|_| TerrainCacheError::BadResponse)?
            .map_err(TerrainCacheError::Network)?;
        if !response.status().is_success() {
            return Err(TerrainCacheError::BadResponse);
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_ARCHIVE_BYTES)
        {
            return Err(TerrainCacheError::InvalidArchive);
        }
        // The archive's temporary file is a cache mutation: it is created only while the
        // guard allows, and left alone, not removed, once the guard has refused (R-P3bF3-2).
        permit(egress_guard)?;
        let (compressed, compressed_name, archive_lease) =
            create_private_temp(&self.root, "archive")?;
        // The entry removes its file only while the guard still allows it, whatever ends
        // the download: a body that fails, a lookup budget that expires, a worker that
        // is stopped or aborted all drop it (R-P3bF4-5).
        let may_remove = || permit(egress_guard).is_ok();
        let mut archive_cleanup = TemporaryEntry::new(
            Arc::clone(&self.root),
            compressed_name.clone(),
            archive_lease,
            &may_remove,
        );
        let mut output = tokio::fs::File::from_std(compressed);
        let download_result = async {
            let mut total = 0_u64;
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(TerrainCacheError::Network)?;
                total = total.saturating_add(chunk.len() as u64);
                if total > MAX_ARCHIVE_BYTES {
                    return Err(TerrainCacheError::InvalidArchive);
                }
                // The chunk arrived after an await. The previous chunk's write is let
                // finish first (a tokio file issues a write only after the one before it
                // has completed), so that the guard is the last thing in front of this
                // write, with no await between (R-P3bF4-5; fix 5, Opus N3).
                timeout(self.options.read_timeout, output.flush())
                    .await
                    .map_err(|_| TerrainCacheError::BadResponse)?
                    .map_err(TerrainCacheError::Io)?;
                permit(egress_guard)?;
                #[cfg(test)]
                self.root.note_file_attempt(&compressed_name);
                timeout(self.options.read_timeout, output.write_all(&chunk))
                    .await
                    .map_err(|_| TerrainCacheError::BadResponse)?
                    .map_err(TerrainCacheError::Io)?;
            }
            // The last chunk's write is let finish before the guard stands in front of the
            // sync (fix 5, Opus N3).
            timeout(self.options.read_timeout, output.flush())
                .await
                .map_err(|_| TerrainCacheError::BadResponse)?
                .map_err(TerrainCacheError::Io)?;
            permit(egress_guard)?;
            #[cfg(test)]
            self.root.note_file_attempt(&compressed_name);
            output.sync_all().await.map_err(TerrainCacheError::Io)
        }
        .await;
        // Decompress through the descriptor the archive was written through,
        // never by reopening its name: whoever could replace that entry
        // between the two would choose the bytes this Hub then records as
        // trusted.
        let archive = output.into_std().await;
        if let Err(error) = download_result {
            if permit(egress_guard).is_err() {
                archive_cleanup.keep();
            }
            return Err(error);
        }
        // The staged tile, its digest in the store and its rename into place are
        // mutations after the download's awaits: the same guard that stood in front of
        // the request stands in front of them (B2 fix round, P3b fix 2, R-P3bF2-1; fix
        // 3), and nothing follows it that waits. A refusal leaves the archive as it is.
        if let Err(denied) = permit(egress_guard) {
            archive_cleanup.keep();
            return Err(denied);
        }
        let staged = decompress_to_hgt(&self.root, archive, aws, &name, &may_remove)?;
        self.commit_tile(*tile, staged)?;
        Ok(if aws { "aws" } else { "esa" })
    }

    /// Put a downloaded tile in place. Its digest is recorded first: a crash
    /// or a failed rename leaves a record for a tile that is not there, which
    /// admits nothing, while the other order could leave a tile with no
    /// record. The temporary file and the destination are entries of the same
    /// validated directory, so the rename cannot leave it.
    fn commit_tile(&self, tile: TileId, staged: StagedTile<'_>) -> Result<(), TerrainCacheError> {
        self.options
            .store
            .record_terrain_tile_digest(&tile.name(), &staged.sha256, epoch_millis())
            .map_err(TerrainCacheError::store)?;
        self.root
            .rename(staged.entry.name(), &tile_file(tile))
            .map_err(TerrainCacheError::Io)?;
        staged.entry.disarm();
        let _ = self.root.sync();
        Ok(())
    }

    /// Delete an unproven tile and its source marker. The deletion is a mutation of the
    /// persistent cache: a refused guard deletes nothing and the lookup ends (B2 fix
    /// round, P3b fix 3, R-P3bF3-2).
    fn discard_tile<G: EgressGuard + ?Sized>(
        &self,
        tile: TileId,
        guard: &G,
    ) -> Result<(), TerrainCacheError> {
        permit(guard)?;
        let _ = self.root.remove(&source_file(tile));
        let _ = self.root.remove(&tile_file(tile));
        Ok(())
    }
}

fn tile_file(tile: TileId) -> String {
    format!("{}.hgt", tile.name())
}

fn source_file(tile: TileId) -> String {
    format!("{}.source", tile.name())
}

fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// The ownership of one temporary entry, held for the entry's whole life (B2 fix round,
/// P3b fix 5, R-P3bF5-1): an exclusive advisory lock on the open file description the
/// entry was created through. Two Hub processes with data directories of their own can
/// share one terrain cache, so no registry of a process can say that an entry nobody of
/// that process wrote is abandoned; the lock can. It lives as long as any descriptor
/// duplicated from the creating one stays open, and the kernel releases it when the last
/// closes, whatever ended the writer: a clean drop, an abort, a crash or a kill.
struct EntryLease(#[allow(dead_code)] File);

/// Test only (B2 fix round, P3b fix 6, N2): a step run once, on the test's thread, between
/// the lock just taken on a candidate (of the reclaim) or on a new entry (of the creation)
/// and the check that its name still is the locked file: the instant in which another
/// process may rename, remove or re-create the name.
#[cfg(test)]
type AfterLockStep = Box<dyn FnOnce(&PrivateDirectory, &str)>;

#[cfg(test)]
thread_local! {
    static AFTER_LOCK: std::cell::RefCell<Option<AfterLockStep>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn after_lock(root: &PrivateDirectory, name: &str) {
    if let Some(step) = AFTER_LOCK.with(|cell| cell.borrow_mut().take()) {
        step(root, name);
    }
}

/// Reclaim every owned temporary entry whose writer is gone, whatever its age: an archive
/// kept after a refusal, the leftover of a process that died, a staged tile nobody
/// renamed. Such an entry counts against the quota and, at the smallest supported quota,
/// would fail every cache miss of the next run until it aged out (R-P3bF4-6).
///
/// An entry is abandoned when its exclusive lock can be taken without waiting: a live
/// writer, of this process or another, holds it, and a candidate that cannot be locked
/// (or whose lock cannot be asked for) is never removed. The lock is taken before the
/// guard is asked, the name is checked to still be the file that was locked, and the
/// guard is asked again immediately before each removal and before the directory sync
/// (R-P3bF5-2): a refusal ends the reclaim with nothing further attempted. A symlink or
/// a non-regular entry is left alone.
fn reclaim_abandoned_temporary_files<G: EgressGuard + ?Sized>(
    root: &PrivateDirectory,
    guard: &G,
) -> Result<(), TerrainCacheError> {
    let mut removed = false;
    for name in root.entry_names().map_err(TerrainCacheError::Io)? {
        if !is_owned_temporary_name(OsStr::new(&name)) {
            continue;
        }
        let Some(candidate) = root.open_regular(&name).map_err(TerrainCacheError::Io)? else {
            continue;
        };
        if !matches!(private_directory::try_lock_exclusive(&candidate), Ok(true)) {
            continue;
        }
        #[cfg(test)]
        after_lock(root, &name);
        // A writer that renamed its entry into place and let go, and then another that
        // reused the name, must not lose its file to a lock taken on the old one.
        if !root
            .is_entry(&name, &candidate)
            .map_err(TerrainCacheError::Io)?
        {
            continue;
        }
        permit(guard)?;
        root.remove(&name).map_err(TerrainCacheError::Io)?;
        removed = true;
    }
    if removed {
        permit(guard)?;
        root.sync().map_err(TerrainCacheError::Io)?;
    }
    Ok(())
}

fn is_owned_temporary_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let mut parts = name.split('.');
    matches!(parts.next(), Some(""))
        && matches!(parts.next(), Some("archive" | "hgt" | "source"))
        && parts.next().is_some_and(|value| {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
        })
        && parts.next().is_some_and(|value| {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
        })
        && matches!(parts.next(), Some("tmp"))
        && parts.next().is_none()
}

fn write_source_atomic(
    root: &Arc<PrivateDirectory>,
    name: &str,
    source: &str,
    may_remove: RemovalGuard<'_>,
) -> Result<(), TerrainCacheError> {
    let (mut file, temporary, lease) = create_private_temp(root, "source")?;
    let entry = TemporaryEntry::new(Arc::clone(root), temporary, lease, may_remove);
    write_counted(&mut file, root, entry.name(), source.as_bytes())?;
    sync_counted(&file, root, entry.name())?;
    drop(file);
    root.rename(entry.name(), name)
        .map_err(TerrainCacheError::Io)?;
    entry.disarm();
    Ok(())
}

/// Write to a staged file of the cache root; the attempt is counted in tests whatever
/// follows (R-P3bF4-7).
fn write_counted(
    file: &mut File,
    #[cfg_attr(not(test), allow(unused_variables))] root: &PrivateDirectory,
    #[cfg_attr(not(test), allow(unused_variables))] name: &str,
    bytes: &[u8],
) -> Result<(), TerrainCacheError> {
    #[cfg(test)]
    root.note_file_attempt(name);
    file.write_all(bytes).map_err(TerrainCacheError::Io)
}

fn sync_counted(
    file: &File,
    #[cfg_attr(not(test), allow(unused_variables))] root: &PrivateDirectory,
    #[cfg_attr(not(test), allow(unused_variables))] name: &str,
) -> Result<(), TerrainCacheError> {
    #[cfg(test)]
    root.note_file_attempt(name);
    file.sync_all().map_err(TerrainCacheError::Io)
}

/// Whether the run's guard still allows a removal (see [`TemporaryEntry`]).
type RemovalGuard<'a> = &'a (dyn Fn() -> bool + Sync);

/// A temporary entry of the cache root, removed again unless it was renamed
/// into place (`disarm`). It names the entry relative to the held directory.
///
/// The removal is a cache mutation like any other (B2 fix round, P3b fix 4,
/// R-P3bF4-5): the entry consults the run's guard when it is dropped, whatever dropped
/// it (a failed download, the lookup budget, the worker's stop or abort), and keeps the
/// file once the guard has refused or cannot be consulted. A kept entry is no longer
/// owned by this instance: the next admitted construction reclaims it.
struct TemporaryEntry<'a> {
    directory: Arc<PrivateDirectory>,
    name: Option<String>,
    /// The lock that says a live writer owns the entry; released with the entry.
    lease: Option<EntryLease>,
    may_remove: RemovalGuard<'a>,
}

impl<'a> TemporaryEntry<'a> {
    fn new(
        directory: Arc<PrivateDirectory>,
        name: String,
        lease: EntryLease,
        may_remove: RemovalGuard<'a>,
    ) -> Self {
        Self {
            directory,
            name: Some(name),
            lease: Some(lease),
            may_remove,
        }
    }

    fn name(&self) -> &str {
        self.name.as_deref().unwrap_or_default()
    }

    /// The entry was renamed into place: nothing is left to remove.
    fn disarm(mut self) {
        self.release();
    }

    /// Leave the entry as it is, even when the value is dropped: after a refusal of the
    /// guard nothing more is removed (B2 fix round, P3b fix 3, R-P3bF3-2). The next
    /// admitted construction of the cache reclaims it.
    fn keep(&mut self) {
        self.release();
    }

    /// Give up this instance's ownership of the entry: its lock is let go, so that a
    /// reclaim (of this process or another) may take the entry if it is still there.
    fn release(&mut self) {
        self.name = None;
        self.lease = None;
    }
}

impl Drop for TemporaryEntry<'_> {
    fn drop(&mut self) {
        let Some(name) = self.name.clone() else {
            return;
        };
        // The consultation is the last thing before the removal, with no await between.
        let allowed = (self.may_remove)();
        if allowed {
            let _ = self.directory.remove(&name);
        }
        self.release();
    }
}

/// Create a fresh `0600` temporary file inside the cache root and return it with its
/// entry name and the lock that says a live writer owns it (see [`EntryLease`]).
///
/// The lock is taken on the new file at once and the name is checked to still be that
/// file: a reclaim of another process that listed the entry in the instant between the
/// creation and the lock either holds the lock still (the name is skipped; the reclaim
/// removes the file) or has removed it already (the name is skipped). A name is never
/// used without the lock held. A failure to lock (other than another holder) or to
/// duplicate leaves the new file behind, since removing it is a mutation the caller's guard
/// has not been asked about; a root whose filesystem cannot lock at all never gets here,
/// because the construction of the cache refuses it, so what is left is an unusual
/// transient error, and the next reclaim that can lock the file removes it.
fn create_private_temp(
    root: &PrivateDirectory,
    label: &str,
) -> Result<(File, String, EntryLease), TerrainCacheError> {
    for attempt in 0..16_u32 {
        let name = format!(".{label}.{}.{}.tmp", std::process::id(), attempt);
        let file = match root.create_exclusive(&name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(TerrainCacheError::Io(error)),
        };
        if !private_directory::try_lock_exclusive(&file).map_err(TerrainCacheError::Io)? {
            continue;
        }
        #[cfg(test)]
        after_lock(root, &name);
        if !root.is_entry(&name, &file).map_err(TerrainCacheError::Io)? {
            continue;
        }
        let lease = EntryLease(file.try_clone().map_err(TerrainCacheError::Io)?);
        return Ok((file, name, lease));
    }
    Err(TerrainCacheError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "temporary file collision",
    )))
}

/// A decompressed tile waiting in the cache root under a temporary name, with
/// the SHA-256 of exactly the bytes that were stored.
struct StagedTile<'a> {
    entry: TemporaryEntry<'a>,
    sha256: String,
}

fn decompress_to_hgt<'a>(
    root: &Arc<PrivateDirectory>,
    mut input: File,
    gzip: bool,
    expected_tile_name: &str,
    may_remove: RemovalGuard<'a>,
) -> Result<StagedTile<'a>, TerrainCacheError> {
    let (mut output, temporary, lease) = create_private_temp(root, "hgt")?;
    let entry = TemporaryEntry::new(Arc::clone(root), temporary, lease, may_remove);
    input
        .seek(SeekFrom::Start(0))
        .map_err(TerrainCacheError::Io)?;
    if gzip {
        let mut decoder = GzDecoder::new(input);
        copy_limited(&mut decoder, &mut output, root, entry.name())?;
    } else {
        let mut zip = ZipArchive::new(input).map_err(|_| TerrainCacheError::InvalidArchive)?;
        let expected_entry = format!("{expected_tile_name}.hgt");
        let mut found = false;
        for index in 0..zip.len() {
            let mut member = zip
                .by_index(index)
                .map_err(|_| TerrainCacheError::InvalidArchive)?;
            if member.is_dir() {
                continue;
            }
            let enclosed = member
                .enclosed_name()
                .ok_or(TerrainCacheError::InvalidArchive)?;
            if enclosed.components().count() != 1
                || enclosed.as_os_str() != std::ffi::OsStr::new(&expected_entry)
                || member.size() != SRTM3_BYTES && member.size() != SRTM1_BYTES
            {
                return Err(TerrainCacheError::InvalidArchive);
            }
            if found {
                return Err(TerrainCacheError::InvalidArchive);
            }
            found = true;
            copy_limited(&mut member, &mut output, root, entry.name())?;
        }
        if !found {
            return Err(TerrainCacheError::InvalidArchive);
        }
    }
    sync_counted(&output, root, entry.name())?;
    // Hash what was stored: read the bytes back through the descriptor they
    // were written through, which also proves the exact HGT length.
    let stored = HgtTile::from_file(expected_tile_name, output).map_err(|error| match error {
        TerrainError::InvalidHgtLength => TerrainCacheError::InvalidArchive,
        other => TerrainCacheError::InvalidTile(other),
    })?;
    let sha256 = stored
        .sha256_hex()
        .map_err(TerrainCacheError::InvalidTile)?;
    Ok(StagedTile { entry, sha256 })
}

fn copy_limited<R: Read>(
    reader: &mut R,
    writer: &mut File,
    root: &PrivateDirectory,
    name: &str,
) -> Result<u64, TerrainCacheError> {
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer).map_err(TerrainCacheError::Io)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > MAX_HGT_BYTES {
            return Err(TerrainCacheError::InvalidArchive);
        }
        write_counted(writer, root, name, &buffer[..read])?;
    }
    Ok(total)
}

#[cfg(test)]
#[path = "terrain_cache/tests.rs"]
mod tests;
