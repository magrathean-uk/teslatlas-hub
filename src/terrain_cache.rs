//! Bounded, restart-safe SRTM tile acquisition.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use reqwest::{Client, redirect::Policy};
use rustix::fs::{Mode, OFlags, open};
use thiserror::Error;
use tokio::{io::AsyncWriteExt, sync::Mutex as AsyncMutex, time::timeout};
use url::Url;
use zip::ZipArchive;

use crate::{
    config::TerrainConfig,
    geocoder::EgressGuard,
    terrain::{HgtFileIdentity, HgtTile, SRTM1_BYTES, SRTM3_BYTES, TerrainError, TileId},
};

pub const AWS_SKADI_BASE: &str = "https://elevation-tiles-prod.s3.amazonaws.com/";
pub const ESA_SRTM_BASE: &str = "https://step.esa.int/auxdata/dem/SRTMGL1/";
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HGT_BYTES: u64 = SRTM1_BYTES;
const MAX_SOURCE_BYTES: u64 = 16;
const MAX_TILE_CACHE_BYTES: u64 = MAX_HGT_BYTES + MAX_SOURCE_BYTES;
const TEMPORARY_FILE_MAX_AGE: Duration = Duration::from_secs(60 * 60);

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
    pub root: PathBuf,
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
    pub fn from_config(config: &TerrainConfig, data_dir: &Path) -> Result<Self, TerrainCacheError> {
        config
            .validate()
            .map_err(|_| TerrainCacheError::InvalidConfig)?;
        Ok(Self {
            root: config.resolved_cache_dir(data_dir),
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
    client: Client,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    quota_lock: AsyncMutex<()>,
    hashes: Mutex<HashMap<String, (HgtFileIdentity, String)>>,
}

impl TerrainCache {
    pub fn new(options: TerrainCacheOptions) -> Result<Self, TerrainCacheError> {
        if options.min_free_bytes == 0
            || options.max_cache_bytes < MAX_TILE_CACHE_BYTES
            || options.connect_timeout.is_zero()
            || options.read_timeout.is_zero()
        {
            return Err(TerrainCacheError::InvalidConfig);
        }
        fs::create_dir_all(&options.root).map_err(TerrainCacheError::Io)?;
        cleanup_stale_temporary_files(&options.root, SystemTime::now())?;
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
        self.ensure_tile(tile, egress_guard).await?;
        HgtTile::open(&tile.name(), self.tile_path(tile)).map_err(TerrainCacheError::InvalidTile)
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
        let source = self.ensure_tile(tile, egress_guard).await?;
        let path = self.tile_path(tile);
        let hgt = HgtTile::open(&tile.name(), &path).map_err(TerrainCacheError::InvalidTile)?;
        Ok(TerrainLookupResult {
            elevation_m: hgt
                .elevation_at(latitude, longitude)
                .map_err(TerrainCacheError::InvalidTile)?,
            tile_name: tile.name(),
            tile_hash: self.tile_hash(&tile.name(), &hgt)?,
            dataset_source: source,
            dataset_version: crate::terrain::TERRAIN_DATASET_VERSION.to_owned(),
        })
    }

    fn tile_hash(&self, name: &str, tile: &HgtTile) -> Result<String, TerrainCacheError> {
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
                return Ok(hash.clone());
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
        Ok(hash)
    }

    async fn ensure_tile<G: EgressGuard + ?Sized>(
        &self,
        tile: TileId,
        egress_guard: &G,
    ) -> Result<String, TerrainCacheError> {
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
            let path = self.tile_path(tile);
            if path.exists() {
                match HgtTile::open(&tile.name(), &path) {
                    Ok(_) => {
                        self.enforce_cache_quota(Some(&path), 0)?;
                        return Ok(self.read_source(tile));
                    }
                    Err(_) => self.discard_tile(tile),
                }
            }
            self.ensure_space()?;
            self.enforce_cache_quota(None, MAX_TILE_CACHE_BYTES)?;
            let source = match self.download(&tile, &path, true, egress_guard).await {
                Ok(source) => source,
                // Admission failure is not a provider failure.  Do not use a
                // second provider after the first final egress check denied it.
                Err(TerrainCacheError::EgressDenied) => {
                    return Err(TerrainCacheError::EgressDenied);
                }
                Err(_) => self.download(&tile, &path, false, egress_guard).await?,
            };
            write_source_atomic(&self.source_path(tile), source)?;
            Ok(source.to_owned())
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

    fn tile_path(&self, tile: TileId) -> PathBuf {
        self.options.root.join(format!("{}.hgt", tile.name()))
    }

    fn source_path(&self, tile: TileId) -> PathBuf {
        self.options.root.join(format!("{}.source", tile.name()))
    }

    fn read_source(&self, tile: TileId) -> String {
        let path = self.source_path(tile);
        open(
            &path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .ok()
        .map(File::from)
        .filter(|file| {
            file.metadata()
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= MAX_SOURCE_BYTES)
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

    fn enforce_cache_quota(
        &self,
        protected: Option<&Path>,
        reserved_bytes: u64,
    ) -> Result<(), TerrainCacheError> {
        #[derive(Debug)]
        struct CachedTile {
            hgt: PathBuf,
            source: PathBuf,
            bytes: u64,
            modified: SystemTime,
        }

        let mut total = 0_u64;
        let mut tiles = Vec::new();
        for entry in fs::read_dir(&self.options.root).map_err(TerrainCacheError::Io)? {
            let entry = entry.map_err(TerrainCacheError::Io)?;
            let path = entry.path();
            if is_owned_temporary_name(&entry.file_name()) {
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.file_type().is_file() => metadata,
                    Ok(_) => continue,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(TerrainCacheError::Io(error)),
                };
                total = total.saturating_add(metadata.len());
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("hgt") {
                continue;
            }
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_file() => metadata,
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(TerrainCacheError::Io(error)),
            };
            let source = path.with_extension("source");
            let source_bytes = match fs::symlink_metadata(&source) {
                Ok(metadata) if metadata.file_type().is_file() => metadata.len(),
                Ok(_) => 0,
                Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
                Err(error) => return Err(TerrainCacheError::Io(error)),
            };
            let bytes = metadata.len().saturating_add(source_bytes);
            total = total.saturating_add(bytes);
            tiles.push(CachedTile {
                hgt: path,
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
        for tile in tiles {
            if total.saturating_add(reserved_bytes) <= self.options.max_cache_bytes {
                break;
            }
            if protected.is_some_and(|path| path == tile.hgt) {
                continue;
            }
            match fs::remove_file(&tile.source) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(TerrainCacheError::Io(error)),
            }
            match fs::remove_file(&tile.hgt) {
                Ok(()) => total = total.saturating_sub(tile.bytes),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    total = total.saturating_sub(tile.bytes);
                }
                Err(error) => return Err(TerrainCacheError::Io(error)),
            }
        }
        if total.saturating_add(reserved_bytes) > self.options.max_cache_bytes {
            return Err(TerrainCacheError::CacheQuotaExceeded);
        }
        if let Ok(directory) = File::open(&self.options.root) {
            let _ = directory.sync_all();
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
        let available = rustix::fs::statvfs(&self.options.root)
            .map_err(|error| {
                TerrainCacheError::Io(io::Error::from_raw_os_error(error.raw_os_error()))
            })
            .map(|stat| stat.f_bavail.saturating_mul(stat.f_frsize))?;
        #[cfg(not(test))]
        if available < required {
            return Err(TerrainCacheError::InsufficientSpace);
        }
        Ok(())
    }

    async fn download<G: EgressGuard + ?Sized>(
        &self,
        tile: &TileId,
        destination: &Path,
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
        let (compressed, compressed_path) = create_private_temp(&self.options.root, "archive")?;
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
                timeout(self.options.read_timeout, output.write_all(&chunk))
                    .await
                    .map_err(|_| TerrainCacheError::BadResponse)?
                    .map_err(TerrainCacheError::Io)?;
            }
            output.sync_all().await.map_err(TerrainCacheError::Io)
        }
        .await;
        drop(output);
        if let Err(error) = download_result {
            let _ = fs::remove_file(&compressed_path);
            return Err(error);
        }
        let result = decompress_to_hgt(&compressed_path, destination, aws, &name);
        let _ = fs::remove_file(&compressed_path);
        result.map(|_| if aws { "aws" } else { "esa" })
    }

    fn discard_tile(&self, tile: TileId) {
        let _ = fs::remove_file(self.source_path(tile));
        let _ = fs::remove_file(self.tile_path(tile));
    }
}

fn cleanup_stale_temporary_files(root: &Path, now: SystemTime) -> Result<(), TerrainCacheError> {
    let mut removed = false;
    for entry in fs::read_dir(root).map_err(TerrainCacheError::Io)? {
        let entry = entry.map_err(TerrainCacheError::Io)?;
        if !is_owned_temporary_name(&entry.file_name()) {
            continue;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) if metadata.file_type().is_file() => metadata,
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(TerrainCacheError::Io(error)),
        };
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= TEMPORARY_FILE_MAX_AGE);
        if !stale {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed = true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(TerrainCacheError::Io(error)),
        }
    }
    if removed {
        File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(TerrainCacheError::Io)?;
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

fn write_source_atomic(path: &Path, source: &str) -> Result<(), TerrainCacheError> {
    let parent = path.parent().ok_or(TerrainCacheError::InvalidConfig)?;
    let (mut file, temporary) = create_private_temp(parent, "source")?;
    file.write_all(source.as_bytes())
        .map_err(TerrainCacheError::Io)?;
    file.sync_all().map_err(TerrainCacheError::Io)?;
    drop(file);
    fs::rename(temporary, path).map_err(TerrainCacheError::Io)
}

fn create_private_temp(dir: &Path, label: &str) -> Result<(File, PathBuf), TerrainCacheError> {
    for attempt in 0..16_u32 {
        let path = dir.join(format!(".{label}.{}.{}.tmp", std::process::id(), attempt));
        match OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
                }
                return Ok((file, path));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(TerrainCacheError::Io(error)),
        }
    }
    Err(TerrainCacheError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "temporary file collision",
    )))
}

fn decompress_to_hgt(
    archive: &Path,
    destination: &Path,
    gzip: bool,
    expected_tile_name: &str,
) -> Result<(), TerrainCacheError> {
    let (mut output, temp_path) = create_private_temp(
        destination
            .parent()
            .ok_or(TerrainCacheError::InvalidConfig)?,
        "hgt",
    )?;
    let write_result = (|| -> Result<(), TerrainCacheError> {
        if gzip {
            let input = File::open(archive).map_err(TerrainCacheError::Io)?;
            let mut decoder = GzDecoder::new(input);
            copy_limited(&mut decoder, &mut output)?;
        } else {
            let input = File::open(archive).map_err(TerrainCacheError::Io)?;
            let mut zip = ZipArchive::new(input).map_err(|_| TerrainCacheError::InvalidArchive)?;
            let expected_entry = format!("{expected_tile_name}.hgt");
            let mut found = false;
            for index in 0..zip.len() {
                let mut entry = zip
                    .by_index(index)
                    .map_err(|_| TerrainCacheError::InvalidArchive)?;
                if entry.is_dir() {
                    continue;
                }
                let enclosed = entry
                    .enclosed_name()
                    .ok_or(TerrainCacheError::InvalidArchive)?;
                if enclosed.components().count() != 1
                    || enclosed.as_os_str() != std::ffi::OsStr::new(&expected_entry)
                    || entry.size() != SRTM3_BYTES && entry.size() != SRTM1_BYTES
                {
                    return Err(TerrainCacheError::InvalidArchive);
                }
                if found {
                    return Err(TerrainCacheError::InvalidArchive);
                }
                found = true;
                copy_limited(&mut entry, &mut output)?;
            }
            if !found {
                return Err(TerrainCacheError::InvalidArchive);
            }
        }
        output.sync_all().map_err(TerrainCacheError::Io)
    })();
    drop(output);
    let result = write_result.and_then(|()| {
        let length = fs::metadata(&temp_path)
            .map_err(TerrainCacheError::Io)?
            .len();
        if length != SRTM3_BYTES && length != SRTM1_BYTES {
            return Err(TerrainCacheError::InvalidArchive);
        }
        fs::rename(&temp_path, destination).map_err(TerrainCacheError::Io)?;
        if let Ok(dir) = File::open(
            destination
                .parent()
                .ok_or(TerrainCacheError::InvalidConfig)?,
        ) {
            let _ = dir.sync_all();
        }
        Ok(())
    });
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn copy_limited<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
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
        writer
            .write_all(&buffer[..read])
            .map_err(TerrainCacheError::Io)?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
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
}
