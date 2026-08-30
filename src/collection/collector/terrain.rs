// SPDX-License-Identifier: AGPL-3.0-only

fn spawn_terrain_worker(
    data_dir: std::path::PathBuf,
    terrain_config: TerrainConfig,
    cursor_key: CursorKey,
    runtime_admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
) -> TerrainWorker {
    let (wake, mut wakes) = mpsc::channel(1);
    let (initialized, initialized_rx) = oneshot::channel();
    let (start, mut started) = oneshot::channel();
    let (stop, mut stopped) = oneshot::channel();
    if !terrain_config.enabled {
        let task = tokio::spawn(async move {
            let _ = initialized.send(Ok(()));
            tokio::select! {
                _ = &mut started => {}
                _ = &mut stopped => return Ok(()),
            }
            let _ = stopped.await;
            Ok(())
        });
        return TerrainWorker {
            wake,
            initialized: Some(initialized_rx),
            start: Some(start),
            stop: Some(stop),
            task,
        };
    }
    let task = tokio::spawn(async move {
        let initialized_worker = (|| {
            let store = HubStore::initialize(&data_dir).map_err(|error| {
                tracing::warn!(error = %error, "terrain worker could not open Hub store");
                CollectorError::TerrainWorkerStartup
            })?;
            let options = crate::terrain_cache::TerrainCacheOptions::from_config(
                &terrain_config,
                &data_dir,
            )
            .map_err(|error| {
                tracing::warn!(error = %terrain_error_code(&error), "terrain worker unavailable");
                CollectorError::TerrainWorkerStartup
            })?;
            let lookup = TerrainCache::new(options).map_err(|error| {
                tracing::warn!(error = %terrain_error_code(&error), "terrain worker unavailable");
                CollectorError::TerrainWorkerStartup
            })?;
            Ok::<_, CollectorError>((store, lookup))
        })();
        let (store, lookup) = match initialized_worker {
            Ok(worker) => worker,
            Err(_) => {
                // Elevation is enrichment, never a reason to stop collection.
                // Keep an owned inert task until collector shutdown.
                let _ = initialized.send(Ok(()));
                tokio::select! {
                    result = &mut started => {
                        if result.is_err() {
                            return Ok(());
                        }
                    }
                    _ = &mut stopped => return Ok(()),
                }
                loop {
                    tokio::select! {
                        received = wakes.recv() => {
                            if received.is_none() {
                                return Ok(());
                            }
                        }
                        _ = &mut stopped => return Ok(()),
                    }
                }
            }
        };
        let _ = initialized.send(Ok(()));
        tokio::select! {
            result = &mut started => {
                if result.is_err() {
                    return Ok(());
                }
            }
            _ = &mut stopped => return Ok(()),
        }
        let mut fuse = TerrainFuse::default();
        let mut first = true;
        loop {
            if !first {
                tokio::select! {
                    received = wakes.recv() => {
                        if received.is_none() {
                            return Ok(());
                        }
                    }
                    _ = sleep(TERRAIN_PERIOD) => {}
                    _ = &mut stopped => return Ok(()),
                }
            }
            first = false;
            let pass = run_terrain_enrichment_pass(
                &store,
                &lookup,
                &cursor_key,
                terrain_config.min_free_bytes,
                &mut fuse,
                runtime_admission.as_ref(),
            );
            tokio::select! {
                result = pass => {
                    if let Err(error) = result {
                        if matches!(error, CollectorError::SensitiveAccessUnavailable) {
                            return Err(error);
                        }
                        tracing::warn!(error = %error, "terrain enrichment pass failed");
                    }
                }
                _ = &mut stopped => return Ok(()),
            }
        }
    });
    TerrainWorker {
        wake,
        initialized: Some(initialized_rx),
        start: Some(start),
        stop: Some(stop),
        task,
    }
}

async fn run_terrain_enrichment_pass(
    store: &HubStore,
    lookup: &TerrainCache,
    cursor_key: &CursorKey,
    minimum_free_bytes: u64,
    fuse: &mut TerrainFuse,
    runtime_admission: Option<&Arc<crate::hub_user_process::AdmittedUserHub>>,
) -> Result<usize, CollectorError> {
    let now = Instant::now();
    if !fuse.available(now) {
        return Ok(0);
    }
    let attempted_at_ms = current_epoch_millis()?;
    let candidates = store.terrain_candidates(attempted_at_ms, TERRAIN_PAGE_LIMIT)?;
    let mut resolved = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if !fuse.available(Instant::now()) {
            break;
        }
        assert_runtime_sensitive_access(runtime_admission.map(Arc::as_ref))?;
        let result = terrain_lookup_with_runtime_admission(
            lookup,
            candidate.position.latitude,
            candidate.position.longitude,
            runtime_admission,
        )
        .await;
        if matches!(result, Err(TerrainCacheError::EgressDenied)) {
            return Err(CollectorError::SensitiveAccessUnavailable);
        }
        if result.is_err() {
            fuse.failure(Instant::now());
        }
        resolved.push((candidate, result));
    }

    let _publication_gate = store.acquire_publication_gate().await?;
    let mut changed_vehicles = HashSet::new();
    let mut changed = 0;
    for (candidate, result) in resolved {
        match result {
            Ok(result) => {
                if store.apply_terrain_result(
                    &candidate,
                    result.elevation_m,
                    &result.tile_name,
                    &result.tile_hash,
                    &result.dataset_source,
                    &result.dataset_version,
                    attempted_at_ms,
                )? {
                    changed += 1;
                    changed_vehicles.insert(candidate.vehicle_id);
                }
            }
            Err(error) => {
                store.record_terrain_failure(
                    &candidate,
                    terrain_error_code(&error),
                    attempted_at_ms.saturating_add(TERRAIN_RETRY_DELAY.as_millis() as i64),
                    attempted_at_ms,
                )?;
            }
        }
    }
    for vehicle_id in changed_vehicles {
        store.publish_terrain_revision(vehicle_id, cursor_key, minimum_free_bytes)?;
    }
    Ok(changed)
}

async fn terrain_lookup_with_runtime_admission(
    lookup: &TerrainCache,
    latitude: f64,
    longitude: f64,
    runtime_admission: Option<&Arc<crate::hub_user_process::AdmittedUserHub>>,
) -> Result<crate::terrain_cache::TerrainLookupResult, TerrainCacheError> {
    if let Some(admission) = runtime_admission {
        let guard = AdmittedUserEgressGuard::new(Arc::clone(admission));
        return lookup
            .lookup_with_egress_guard(latitude, longitude, TERRAIN_LOOKUP_BUDGET, &guard)
            .await;
    }

    #[cfg(any(not(unix), test))]
    {
        return lookup
            .lookup_with_egress_guard(
                latitude,
                longitude,
                TERRAIN_LOOKUP_BUDGET,
                &crate::geocoder::UnguardedEgress,
            )
            .await;
    }

    #[cfg(all(unix, not(test)))]
    {
        Err(TerrainCacheError::EgressDenied)
    }
}

fn terrain_error_code(error: &TerrainCacheError) -> &'static str {
    match error {
        TerrainCacheError::Timeout => "timeout",
        TerrainCacheError::Network(_) => "source_unavailable",
        TerrainCacheError::BadResponse => "invalid_response",
        TerrainCacheError::InvalidArchive => "invalid_archive",
        TerrainCacheError::InsufficientSpace => "insufficient_free_space",
        TerrainCacheError::CacheQuotaExceeded => "cache_quota_exceeded",
        TerrainCacheError::Io(_) => "io",
        TerrainCacheError::InvalidConfig => "invalid_config",
        TerrainCacheError::EgressDenied => "egress_denied",
        TerrainCacheError::InvalidTile(error) => match error {
            crate::terrain::TerrainError::NonFiniteCoordinate => "nonfinite_coordinate",
            crate::terrain::TerrainError::InvalidLatitude => "invalid_latitude",
            crate::terrain::TerrainError::InvalidLongitude => "invalid_longitude",
            crate::terrain::TerrainError::InvalidTileName => "invalid_tile_name",
            crate::terrain::TerrainError::WrongTile => "wrong_tile",
            crate::terrain::TerrainError::InvalidHgtLength => "invalid_hgt_length",
            crate::terrain::TerrainError::Io(_) => "io",
        },
    }
}

/// Run the no-wake collector inside the exact admitted Unix Serve process.
/// The Hub-owned token pair and cursor key are loaded from the selected data
/// directory.
#[cfg(unix)]
#[doc(hidden)]
pub async fn run_supervised_for_admitted_user<F>(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    ready: oneshot::Sender<CursorKey>,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    admission.assert_sensitive_access()?;
    admission.assert_store_path(&config.data_dir)?;
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(CollectorError::AdmittedStoreMismatch);
    }
    crate::diagnostics::log_runtime_inventory(store, config);
    if config.collector.provider == CollectorProvider::Fleet {
        tracing::info!("starting Fleet collector (Owner tokens are left in place if present)");
        return run_fleet_supervised_for_admitted_user(
            store, config, admission, ready, true, shutdown,
        )
        .await;
    }
    let _activation = config.collector.supervised_interval()?;
    let cadence = config.collector.cadence()?;
    if !config.collector.legacy_auth.enabled {
        return Err(CollectorError::AdmittedLegacyAuthRequired);
    }

    let manager = LegacyAuthManager::from_hub_teslamate_store_for_admitted_user(
        store.clone(),
        &config.data_dir,
        Arc::clone(&admission),
    )
    .map_err(CollectorError::from)
    .map_err(normalize_sensitive_access_error)?;
    let region = manager.region();
    let client = OwnerApi::new(config.collector.owner_api_options_for_region(region)?)?;
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir)
        .map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;

    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region,
    };
    let stream_endpoint = config.collector.stream_endpoint(region)?;
    run_supervised_with_access(
        store,
        config,
        cadence,
        client,
        auth,
        stream_endpoint,
        cursor_key,
        Some(ready),
        Some(admission),
        shutdown,
    )
    .await
}

/// Run the collector with an already-issued legacy pair. Observer mode never
/// refreshes or retries an unauthorized pair; a 401/403 ends this process so
/// an operator can replace the credentials explicitly.
#[cfg(unix)]
#[doc(hidden)]
pub async fn run_observer_for_admitted_user<F>(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    ready: oneshot::Sender<CursorKey>,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    admission.assert_sensitive_access()?;
    admission.assert_store_path(&config.data_dir)?;
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(CollectorError::AdmittedStoreMismatch);
    }
    if config.collector.provider == CollectorProvider::Fleet {
        return run_fleet_supervised_for_admitted_user(
            store, config, admission, ready, false, shutdown,
        )
        .await;
    }
    let _activation = config.collector.supervised_interval()?;
    let cadence = config.collector.cadence()?;
    if !config.collector.legacy_auth.enabled {
        return Err(CollectorError::AdmittedLegacyAuthRequired);
    }

    let manager = LegacyAuthManager::from_hub_teslamate_store_observer_for_admitted_user(
        store.clone(),
        &config.data_dir,
        Arc::clone(&admission),
    )
    .map_err(CollectorError::from)
    .map_err(normalize_sensitive_access_error)?;
    let region = manager.region();
    let client = OwnerApi::new(config.collector.owner_api_options_for_region(region)?)?;
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir)
        .map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: false,
        region,
    };
    let stream_endpoint = config.collector.stream_endpoint(region)?;
    run_supervised_with_access(
        store,
        config,
        cadence,
        client,
        auth,
        stream_endpoint,
        cursor_key,
        Some(ready),
        Some(admission),
        shutdown,
    )
    .await
}

#[cfg(unix)]
async fn run_fleet_supervised_for_admitted_user<F>(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    ready: oneshot::Sender<CursorKey>,
    allow_refresh: bool,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    tracing::info!(
        allow_refresh,
        timeout_seconds = config.collector.request_timeout_seconds,
        "Fleet supervised collector connecting with stored Fleet credentials"
    );
    let _activation = config.collector.supervised_interval()?;
    let cadence = config.collector.cadence()?;
    let manager = FleetAuthManager::from_store_for_admitted_user(
        store.clone(),
        &config.data_dir,
        Arc::clone(&admission),
    )?;
    let api = FleetApi::new(
        manager.region(),
        Duration::from_secs(config.collector.request_timeout_seconds),
    )?;
    let auth_api = FleetAuthApi::new(
        manager.region(),
        Duration::from_secs(config.collector.request_timeout_seconds),
    )?;
    let command_proxy = fleet_command_proxy(config)?;
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir)
        .map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    run_fleet_supervised_with_access(
        store,
        config,
        cadence,
        api,
        auth_api,
        command_proxy,
        Arc::new(tokio::sync::Mutex::new(manager)),
        cursor_key,
        ready,
        admission,
        allow_refresh,
        shutdown,
    )
    .await
}

#[cfg(unix)]
fn fleet_command_proxy(config: &HubConfig) -> Result<Option<FleetCommandProxy>, CollectorError> {
    const MAX_CERTIFICATE_BYTES: u64 = 128 * 1024;
    let Some(endpoint) = config.collector.fleet_command_proxy_url.as_deref() else {
        return Ok(None);
    };
    let base = FleetCommandProxyBase::parse(endpoint)?;
    let certificate = config
        .collector
        .fleet_command_proxy_root_certificate_path
        .as_deref()
        .map(|path| read_fleet_proxy_root_certificate(path, MAX_CERTIFICATE_BYTES))
        .transpose()?;
    FleetCommandProxy::new(
        base,
        Duration::from_secs(config.collector.request_timeout_seconds),
        certificate.as_deref(),
    )
    .map(Some)
    .map_err(Into::into)
}

#[cfg(unix)]
fn read_fleet_proxy_root_certificate(
    path: &Path,
    maximum: u64,
) -> Result<Vec<u8>, FleetApiConfigError> {
    read_fleet_proxy_root_certificate_after_open(path, maximum, || {})
}

#[cfg(unix)]
fn read_fleet_proxy_root_certificate_after_open(
    path: &Path,
    maximum: u64,
    after_open: impl FnOnce(),
) -> Result<Vec<u8>, FleetApiConfigError> {
    let descriptor = open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|_| FleetApiConfigError::InvalidRootCertificate)?;
    let held = fstat(&descriptor).map_err(|_| FleetApiConfigError::InvalidRootCertificate)?;
    let current_uid = getuid().as_raw();
    if !FileType::from_raw_mode(held.st_mode).is_file()
        || (held.st_uid != current_uid && held.st_uid != 0)
        || (held.st_mode as u32 & 0o022) != 0
        || held.st_nlink != 1
        || held.st_size < 0
        || u64::try_from(held.st_size)
            .ok()
            .is_none_or(|size| size > maximum)
    {
        return Err(FleetApiConfigError::InvalidRootCertificate);
    }

    after_open();

    let file: std::fs::File = descriptor.into();
    let mut bytes = Vec::with_capacity(
        usize::try_from(held.st_size)
            .unwrap_or_default()
            .min(8 * 1024),
    );
    (&file)
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| FleetApiConfigError::InvalidRootCertificate)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
        return Err(FleetApiConfigError::InvalidRootCertificate);
    }

    let after = fstat(&file).map_err(|_| FleetApiConfigError::InvalidRootCertificate)?;
    let current =
        std::fs::symlink_metadata(path).map_err(|_| FleetApiConfigError::InvalidRootCertificate)?;
    #[allow(clippy::useless_conversion)]
    let held_nlink = u64::from(held.st_nlink);
    if after.st_dev != held.st_dev
        || after.st_ino != held.st_ino
        || after.st_mode != held.st_mode
        || after.st_nlink != held.st_nlink
        || after.st_uid != held.st_uid
        || after.st_gid != held.st_gid
        || after.st_size != held.st_size
        || after.st_mtime != held.st_mtime
        || after.st_mtime_nsec != held.st_mtime_nsec
        || after.st_ctime != held.st_ctime
        || after.st_ctime_nsec != held.st_ctime_nsec
        || current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.dev() != held.st_dev as u64
        || current.ino() != held.st_ino
        || current.mode() != held.st_mode as u32
        || current.nlink() != held_nlink
        || current.uid() != held.st_uid
        || current.gid() != held.st_gid
        || current.len() != u64::try_from(held.st_size).unwrap_or(u64::MAX)
        || current.mtime() != held.st_mtime
        || current.mtime_nsec() != held.st_mtime_nsec as i64
        || current.ctime() != held.st_ctime
        || current.ctime_nsec() != held.st_ctime_nsec as i64
    {
        return Err(FleetApiConfigError::InvalidRootCertificate);
    }
    Ok(bytes)
}

async fn fleet_list_vehicles_with_auth(
    api: &FleetApi,
    auth_api: &FleetAuthApi,
    manager: &Arc<tokio::sync::Mutex<FleetAuthManager>>,
    allow_refresh: bool,
) -> Result<Vec<Vehicle>, CollectorError> {
    let mut manager = manager.lock().await;
    if allow_refresh {
        manager.refresh_if_due(auth_api, SystemTime::now()).await?;
    }
    let first = api
        .list_vehicles(manager.access_token_for_sensitive_use()?)
        .await;
    if allow_refresh && matches!(first, Err(FleetApiError::HttpStatus(401 | 403))) {
        manager.mark_refresh_due();
        manager.refresh_if_due(auth_api, SystemTime::now()).await?;
        return api
            .list_vehicles(manager.access_token_for_sensitive_use()?)
            .await
            .map_err(Into::into);
    }
    first.map_err(Into::into)
}

async fn fleet_vehicle_data_with_auth(
    api: &FleetApi,
    auth_api: &FleetAuthApi,
    manager: &Arc<tokio::sync::Mutex<FleetAuthManager>>,
    vehicle: &Vehicle,
    allow_refresh: bool,
) -> Result<VehicleData, CollectorError> {
    let vin = VehicleVin::parse(&vehicle.vin)
        .map_err(|_| CollectorError::FleetApi(FleetApiError::InvalidResponse))?;
    let mut manager = manager.lock().await;
    if allow_refresh {
        manager.refresh_if_due(auth_api, SystemTime::now()).await?;
    }
    let first = api
        .vehicle_data(manager.access_token_for_sensitive_use()?, vehicle.id, &vin)
        .await;
    if allow_refresh && matches!(first, Err(FleetApiError::HttpStatus(401 | 403))) {
        manager.mark_refresh_due();
        manager.refresh_if_due(auth_api, SystemTime::now()).await?;
        return api
            .vehicle_data(manager.access_token_for_sensitive_use()?, vehicle.id, &vin)
            .await
            .map_err(Into::into);
    }
    first.map_err(Into::into)
}

fn fleet_collection_must_stop(error: &CollectorError) -> bool {
    matches!(
        error,
        CollectorError::FleetApi(FleetApiError::HttpStatus(401 | 403))
    ) || matches!(error, CollectorError::FleetCredential(error) if error.is_sensitive_access_failure())
}

fn fleet_failure_as_owner_error(error: &CollectorError) -> OwnerApiError {
    match error {
        CollectorError::FleetApi(FleetApiError::RequestTimeout) => OwnerApiError::RequestTimeout,
        CollectorError::FleetApi(FleetApiError::RequestNotSent | FleetApiError::Transport) => {
            OwnerApiError::Transport
        }
        CollectorError::FleetApi(
            FleetApiError::HttpStatus(status) | FleetApiError::ProviderHttpStatus { status, .. },
        ) => OwnerApiError::HttpStatus(*status),
        CollectorError::FleetApi(FleetApiError::RateLimited {
            retry_after_seconds,
        }) => OwnerApiError::RateLimited {
            retry_after_seconds: *retry_after_seconds,
        },
        CollectorError::FleetApi(FleetApiError::ResponseTooLarge) => {
            OwnerApiError::ResponseTooLarge
        }
        CollectorError::FleetApi(FleetApiError::ResponseRead) => OwnerApiError::ResponseRead,
        CollectorError::FleetApi(_) => OwnerApiError::InvalidVehicleDataEnvelope,
        CollectorError::FleetCredential(_) => OwnerApiError::LegacyAuth,
        _ => OwnerApiError::Transport,
    }
}
