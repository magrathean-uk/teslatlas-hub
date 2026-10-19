// SPDX-License-Identifier: AGPL-3.0-only

/// The terrain worker of a supervised run. It receives the run's write guard and the
/// run's already initialised store (B2 fix round, P3b fix 2, R-P3bF2-1): opening the
/// store again would be a mutation path of its own (it migrates, recovers bindings and
/// spools and cleans abandoned generations), and every write of a pass is preceded by
/// the guard, after the publication gate it waits for.
fn spawn_terrain_worker(
    data_dir: std::path::PathBuf,
    terrain_config: TerrainConfig,
    cursor_key: CursorKey,
    write_guard: RunWriteGuard,
    run_store: HubStore,
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
            let store = run_store;
            let options = crate::terrain_cache::TerrainCacheOptions::from_config(
                &terrain_config,
                &data_dir,
                store.clone(),
            )
            .map_err(|error| {
                tracing::warn!(error = %terrain_error_code(&error), "terrain worker unavailable");
                CollectorError::TerrainWorkerStartup
            })?;
            // The cache creates and tightens its directories and removes stale files
            // when it is built: the run's guard stands in front of each (B2 fix round,
            // P3b fix 3, R-P3bF3-2). A refusal is the loss of the run's sensitive
            // access and ends the run, not an unavailable worker.
            let lookup = TerrainCache::new_with_guard(options, &TerrainCacheGuard(&write_guard)).map_err(|error| {
                if matches!(error, TerrainCacheError::EgressDenied) {
                    return CollectorError::SensitiveAccessUnavailable;
                }
                tracing::warn!(error = %terrain_error_code(&error), "terrain worker unavailable");
                CollectorError::TerrainWorkerStartup
            })?;
            tracing::debug!(
                cache_root = %lookup.root_path().display(),
                "terrain cache root validated"
            );
            Ok::<_, CollectorError>((store, lookup))
        })();
        let (store, lookup) = match initialized_worker {
            Ok(worker) => worker,
            Err(CollectorError::SensitiveAccessUnavailable) => {
                let _ = initialized.send(Err(CollectorError::SensitiveAccessUnavailable));
                return Err(CollectorError::SensitiveAccessUnavailable);
            }
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
                &write_guard,
                TERRAIN_LOOKUP_BUDGET,
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
    write_guard: &RunWriteGuard,
    lookup_budget: Duration,
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
        write_guard.check("terrain-lookup")?;
        let result = terrain_lookup_guarded(
            lookup,
            candidate.position.latitude,
            candidate.position.longitude,
            write_guard,
            lookup_budget,
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
                // The lookups and the publication gate were awaited: the guard is
                // consulted again immediately before each write.
                write_guard.check("terrain-result")?;
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
                write_guard.check("terrain-failure")?;
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
        // The publication's reservation, pack output and finalisation are three
        // independent mutations: the guard stands in front of each of them.
        write_guard.check("terrain-publish")?;
        store
            .publish_terrain_revision_guarded(
                vehicle_id,
                cursor_key,
                minimum_free_bytes,
                &mut |point| write_guard.refused(point),
            )
            .map_err(|error| match error {
                StoreError::WriteRefused(_) => CollectorError::SensitiveAccessUnavailable,
                other => other.into(),
            })?;
    }
    Ok(changed)
}

/// One elevation lookup of a supervised pass. The run's write guard is its egress guard:
/// it is consulted before the request goes out and again before the downloaded tile's
/// digest is recorded, which is a store mutation after the download's awaits.
async fn terrain_lookup_guarded(
    lookup: &TerrainCache,
    latitude: f64,
    longitude: f64,
    write_guard: &RunWriteGuard,
    lookup_budget: Duration,
) -> Result<crate::terrain_cache::TerrainLookupResult, TerrainCacheError> {
    lookup
        .lookup_with_egress_guard(
            latitude,
            longitude,
            lookup_budget,
            &TerrainCacheGuard(write_guard),
        )
        .await
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
        TerrainCacheError::UntrustedRoot(_) => "untrusted_root",
        TerrainCacheError::LockingUnsupported(_) => "locking_unsupported",
        TerrainCacheError::Store(_) => "store",
        TerrainCacheError::UnprovenTile => "unproven_tile",
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
    run_supervised_with_clients_for_admitted_user(
        store,
        config,
        admission,
        ready,
        |region| Ok(OwnerApi::new(config.collector.owner_api_options_for_region(region)?)?),
        |region| production_fleet_clients(config, region),
        shutdown,
    )
    .await
}

/// The supervised start of [`run_supervised_for_admitted_user`], with the two
/// constructions of Tesla-facing clients made by the callers: the Owner API client
/// for the pair's region and the Fleet clients for the stored credentials' region.
/// Production passes the real ones; a test passes clients of a loopback double (or
/// of an unroutable endpoint) through this one seam, so no test of any mode can
/// reach a Tesla host even if a fence or a stop regresses. Nothing else differs.
#[cfg(unix)]
async fn run_supervised_with_clients_for_admitted_user<F>(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    ready: oneshot::Sender<CursorKey>,
    owner_client_for: impl FnOnce(crate::tesla_stream::StreamRegion) -> Result<OwnerApi, CollectorError>,
    fleet_clients_for: impl FnOnce(crate::fleet_api::FleetRegion) -> Result<FleetClients, CollectorError>,
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
    // The run's one write guard is made here, before any credential unit is built or
    // cursor key created (B2 fix round, P3b fix 3, R-P3bF3-1), and handed down.
    let write_guard = RunWriteGuard::for_run(Some(Arc::clone(&admission)));
    crate::diagnostics::log_runtime_inventory(store, config);
    let _activation = config.collector.supervised_interval()?;
    if config.collector.provider != CollectorProvider::Fleet
        && !config.collector.legacy_auth.enabled
    {
        return Err(CollectorError::AdmittedLegacyAuthRequired);
    }
    // A cadence the configuration breaks is not a reason to refuse to start
    // (B2 clarification 3): the otherwise eligible Serve takes the lease, records
    // the wait as a stop gap, hands over its cursor key and renews, collecting
    // nothing, until the owner corrects the setting and restarts the service.
    // This runs before any credential manager, client, scheduler or stream is
    // built, in every mode.
    if let Some(violation) = config.collector.cadence_violation() {
        return run_cadence_configuration_wait(
            store,
            config,
            admission,
            write_guard,
            ready,
            violation,
            Arc::new(SystemCollectorClock),
            SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
            shutdown,
        )
        .await;
    }
    if config.collector.provider == CollectorProvider::Fleet {
        tracing::info!("starting Fleet collector (Owner tokens are left in place if present)");
        return run_fleet_supervised_for_admitted_user(
            store,
            config,
            admission,
            write_guard,
            ready,
            true,
            fleet_clients_for,
            shutdown,
        )
        .await;
    }
    run_supervised_owner_wire_for_admitted_user(
        store,
        config,
        admission,
        write_guard,
        ready,
        owner_client_for,
        shutdown,
    )
    .await
}

/// The Fleet clients one supervised run uses.
#[cfg(unix)]
struct FleetClients {
    api: FleetApi,
    auth_api: FleetAuthApi,
    command_proxy: Option<FleetCommandProxy>,
}

/// The real Fleet clients of the configuration, for the stored credentials' region.
#[cfg(unix)]
fn production_fleet_clients(
    config: &HubConfig,
    region: crate::fleet_api::FleetRegion,
) -> Result<FleetClients, CollectorError> {
    let timeout = Duration::from_secs(config.collector.request_timeout_seconds);
    Ok(FleetClients {
        api: FleetApi::new(region, timeout)?,
        auth_api: FleetAuthApi::new(region, timeout)?,
        command_proxy: fleet_command_proxy(config)?,
    })
}

/// The Owner-wire collector of the admitted Serve, after its preflight: the credential
/// unit over the stored pair, the Owner API client `client_for` makes for the pair's
/// region, the cursor key and the supervised run. A test supplies a client of its own
/// for a loopback double; production always builds the real one.
#[cfg(unix)]
async fn run_supervised_owner_wire_for_admitted_user<F>(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    write_guard: RunWriteGuard,
    ready: oneshot::Sender<CursorKey>,
    client_for: impl FnOnce(crate::tesla_stream::StreamRegion) -> Result<OwnerApi, CollectorError>,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    let cadence = config.collector.cadence()?;

    let manager = LegacyAuthManager::from_hub_teslamate_store_for_admitted_user(
        store.clone(),
        &config.data_dir,
        &AuthEndpointConfig::from_config(config, false),
        Arc::clone(&admission),
        Some(write_guard.observer("credential")),
    )
    .map_err(CollectorError::from)
    .map_err(normalize_sensitive_access_error)?;
    let region = manager.region();
    let client = client_for(region)?;
    // Creating the cursor key, when it is absent, is a durable write of its own (a file
    // of the data directory): the run's guard stands in front of it (R-P3bF3-1).
    write_guard.check("cursor-key")?;
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
        write_guard,
        shutdown,
    )
    .await
}

/// The managed configuration wait (C3 R21, R8, B2 clarification 3): what an
/// otherwise eligible Unix Serve does while its collector cadence is invalid.
///
/// It keeps every fence of a start: the admitted runtime and store, the vehicle
/// lineage, and the one live lease (a second collector is refused). It then
/// records the wait in one transaction as a `hub_stopped` gap (closing the
/// preceding stop or recording the preceding downtime first) and changes no
/// account state, hold, count, action or refusal: it is not an ordinary R7
/// start, so no reset or lift happens until a corrected configuration starts
/// normally. It logs the fault once, at ERROR, after that custody and before the
/// handoff of the cursor key, then only renews its lease until shutdown. It
/// builds no credential manager, client, scheduler, terrain worker or stream and
/// sends no request. Loss of the admission ends the wait with the
/// sensitive-access failure and no later write; an orderly end stops the
/// renewal and releases only this wait's own lease, leaving the open stop gap
/// for the next ordinary start to close.
#[cfg(unix)]
async fn run_cadence_configuration_wait<F>(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    write_guard: RunWriteGuard,
    ready: oneshot::Sender<CursorKey>,
    violation: crate::config::CadenceViolation,
    clock: Arc<dyn CollectorClock>,
    heartbeat_interval: Duration,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    admission.assert_sensitive_access()?;
    admission.assert_store_path(&config.data_dir)?;
    // The wait has the run's write guard from its first write (B2 fix round, P3b fix
    // 2, R-P3bF2-1; fix 3: the entry point made it): the lease, the wait's gap, the
    // cursor key and the release are each preceded by it.
    let admission_fence = write_guard.fence.clone();
    require_supervised_vehicle_lineage(store)?;
    // The previous run's last heartbeat starts its downtime gap (C3 R8); the
    // wait's own lease replaces it, so read it first.
    let previous_heartbeat_ms = store
        .supervised_collector_lease_status()?
        .map(|lease| lease.heartbeat_at_ms);
    let start_ms = clock.wall_now_ms()?;
    write_guard.check("cadence-lease")?;
    let collector_lease = store.acquire_supervised_collector_lease(start_ms)?;
    let alert_ms = i64::try_from(
        Duration::from_secs(config.collector.offline_drive_timeout_seconds).as_millis(),
    )
    .unwrap_or(i64::MAX);
    write_guard.check("cadence-start")?;
    let started = store.begin_cadence_wait(&crate::db::CadenceWaitStartRequest {
        start_ms,
        previous_heartbeat_ms,
        gap_min_ms: K_GAP_MIN_MS,
        gap_alert_ms: alert_ms,
    });
    let started = match started {
        Ok(started) => started,
        Err(error) => {
            // Nothing was recorded. A lease this wait took for itself, with no
            // predecessor, is given back rather than held for its whole lifetime,
            // unless the admission is gone. A lease whose acquisition replaced an
            // expired predecessor is kept to expire, as a failed ordinary start keeps
            // its own (B2 clarification 3, R8; B2 round 3, MIG-R3-1): giving it back
            // would leave the next start no heartbeat and no open stop to read, and
            // the predecessor's downtime would never be recorded.
            if previous_heartbeat_ms.is_none() && !write_guard.refused("cadence-start-release") {
                let _ = store.release_supervised_collector_lease(collector_lease);
            }
            return Err(error.into());
        }
    };
    // Creating the cursor key, when it is absent, is a durable write of its own (a file
    // of the data directory): the guard stands in front of it (R-P3bF2-1).
    write_guard.check("cadence-cursor-key")?;
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir)
        .map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    let (_collector_state, collector_state_rx) = watch::channel(SupervisedCollectorState::Active);
    let (heartbeat_shutdown, heartbeat_stop) = oneshot::channel();
    let mut heartbeat_task = tokio::spawn(run_supervised_collector_heartbeat_with(
        store.clone(),
        collector_lease,
        collector_state_rx,
        heartbeat_stop,
        heartbeat_interval,
        Arc::clone(&clock),
        Some(Arc::clone(&admission)),
        admission_fence.clone(),
    ));
    tracing::error!(
        code = crate::config::CadenceViolation::CODE,
        stopped_gap_closed = started.stopped_gap_closed,
        hub_down = started.hub_down_inserted,
        "{violation}"
    );
    let mut result = ready
        .send(cursor_key)
        .map_err(|_| CollectorError::SupervisedStartupReadyDropped);
    let mut heartbeat_finished = false;
    if result.is_ok() {
        tokio::pin!(shutdown);
        let admission_lost = admission.wait_until_invalid();
        tokio::pin!(admission_lost);
        tokio::select! {
            biased;
            _ = &mut admission_lost => {
                admission_fence.set();
                result = Err(CollectorError::SensitiveAccessUnavailable);
            }
            () = &mut shutdown => {}
            joined = &mut heartbeat_task => {
                heartbeat_finished = true;
                result = joined
                    .map_err(|_| CollectorError::SupervisedHeartbeatTask)
                    .and_then(|outcome| outcome.and(Err(CollectorError::SupervisedHeartbeatTask)));
            }
        }
    }
    if !heartbeat_finished {
        let _ = heartbeat_shutdown.send(());
        let joined = heartbeat_task
            .await
            .map_err(|_| CollectorError::SupervisedHeartbeatTask)
            .and_then(|outcome| outcome);
        if result.is_ok() {
            result = joined;
        }
    }
    // Release only with the admission intact: a process that lost it writes
    // nothing more, and the lease then expires on its own.
    if result.is_ok() {
        result = write_guard.check("cadence-release").and_then(|()| {
            store
                .release_supervised_collector_lease(collector_lease)
                .map_err(CollectorError::from)
        });
    }
    result
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
    run_observer_with_clients_for_admitted_user(
        store,
        config,
        admission,
        ready,
        |region| Ok(OwnerApi::new(config.collector.owner_api_options_for_region(region)?)?),
        |region| production_fleet_clients(config, region),
        shutdown,
    )
    .await
}

/// [`run_observer_for_admitted_user`] with its client constructions made by the
/// caller (see [`run_supervised_with_clients_for_admitted_user`]).
#[cfg(unix)]
async fn run_observer_with_clients_for_admitted_user<F>(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    ready: oneshot::Sender<CursorKey>,
    owner_client_for: impl FnOnce(crate::tesla_stream::StreamRegion) -> Result<OwnerApi, CollectorError>,
    fleet_clients_for: impl FnOnce(crate::fleet_api::FleetRegion) -> Result<FleetClients, CollectorError>,
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
    // The run's one write guard, made before any credential unit or cursor key (B2 fix
    // round, P3b fix 3, R-P3bF3-1).
    let write_guard = RunWriteGuard::for_run(Some(Arc::clone(&admission)));
    if config.collector.provider == CollectorProvider::Fleet {
        return run_fleet_supervised_for_admitted_user(
            store,
            config,
            admission,
            write_guard,
            ready,
            false,
            fleet_clients_for,
            shutdown,
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
        &AuthEndpointConfig::from_config(config, false),
        Arc::clone(&admission),
        Some(write_guard.observer("credential")),
    )
    .map_err(CollectorError::from)
    .map_err(normalize_sensitive_access_error)?;
    let region = manager.region();
    let client = owner_client_for(region)?;
    write_guard.check("cursor-key")?;
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
        write_guard,
        shutdown,
    )
    .await
}

#[cfg(unix)]
async fn run_fleet_supervised_for_admitted_user<F>(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    write_guard: RunWriteGuard,
    ready: oneshot::Sender<CursorKey>,
    allow_refresh: bool,
    fleet_clients_for: impl FnOnce(crate::fleet_api::FleetRegion) -> Result<FleetClients, CollectorError>,
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
    // An identified unresolved refresh (C5 R47, C3 R47, B2 clarification 3) starts
    // as a running credential stop with an active lease and the reconnect
    // attention: the manager is fenced and sends nothing. Every other case keeps
    // the ordinary constructor and its errors.
    let manager = FleetAuthManager::from_store_for_supervised_start(
        store.clone(),
        &config.data_dir,
        Arc::clone(&admission),
        Some(write_guard.observer("credential")),
    )?;
    let FleetClients {
        api,
        auth_api,
        command_proxy,
    } = fleet_clients_for(manager.region())?;
    write_guard.check("cursor-key")?;
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
        write_guard,
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

/// Whether this tick has refreshed the Fleet token (C3 R47: at most one token
/// refresh per tick). The tick's requests share one; a request that carried the
/// token the tick's own refresh produced and still met a 401 is not refreshed
/// again, because the credential itself is what the 401 refuses (a Fleet
/// credential stop, R47), and a second refresh would only repeat the evidence.
#[derive(Default)]
struct TickRefresh(std::sync::atomic::AtomicBool);

impl TickRefresh {
    fn refreshed(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn note(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Refresh the token when it is due before a request is sent, and note it in the
/// tick's latch when the refresh installed a new pair (C3 R47).
async fn refresh_for_request(
    manager: &mut FleetAuthManager,
    auth_api: &FleetAuthApi,
    tick: &TickRefresh,
) -> Result<(), CollectorError> {
    let before = manager.credential_generation();
    manager.refresh_if_due(auth_api, SystemTime::now()).await?;
    if manager.credential_generation() != before {
        tick.note();
    }
    Ok(())
}

/// Force the one refresh a 401 asks for, and note it in the tick's latch.
async fn refresh_after_rejection(
    manager: &mut FleetAuthManager,
    auth_api: &FleetAuthApi,
    tick: &TickRefresh,
) -> Result<(), CollectorError> {
    manager.mark_refresh_due();
    manager.refresh_if_due(auth_api, SystemTime::now()).await?;
    tick.note();
    Ok(())
}

/// List the account's cars (C3 R24: a request of category `state`). `counting` is
/// the request counter (R25): the loop admitted and counted the first send, this
/// function completes its count with Tesla's answer, and a resend after a token
/// refresh is admitted here under the same cap and reservation as any request
/// (R27, R47). A resend the reservation or the cap does not cover is not sent and
/// returns [`CollectorError::FleetResendWithheld`]: no answer, no failure and no
/// wait. A request that carried the token this tick's refresh produced and met a
/// 401 returns that 401 without a second refresh (`tick`). A failure before the
/// request reaches the network takes the start back.
async fn fleet_list_vehicles_with_auth(
    api: &FleetApi,
    auth_api: &FleetAuthApi,
    manager: &Arc<tokio::sync::Mutex<FleetAuthManager>>,
    allow_refresh: bool,
    counting: Option<&RequestLedger>,
    tick: &TickRefresh,
) -> Result<Vec<Vehicle>, CollectorError> {
    let mut counted = CountedStart::admitted(counting, RequestCategory::State);
    let mut manager = manager.lock().await;
    if allow_refresh {
        refresh_for_request(&mut manager, auth_api, tick).await?;
    }
    // The token this request carries was produced by the tick's refresh when the
    // latch is set now (the manager's lock is held from here to the answer).
    let carried_tick_refresh = tick.refreshed();
    let first = api
        .list_vehicles(
            manager.access_token_for_sensitive_use()?,
            manager.refresh_token_for_sensitive_use()?,
        )
        .await;
    counted.complete(&first);
    // Only a 401 asks for a refresh and a resend (C3 R47). A Fleet 403 is the
    // account's `FleetAccessRefused` on the list and the car's `AccessRefused`
    // on a read (R42, R50, R51): a new token does not change it, and a resend
    // would be a second billed request, which R56 forbids. A 401 that names
    // `mobile_access_disabled` is a provider status and never matches here.
    if allow_refresh
        && !carried_tick_refresh
        && matches!(first, Err(FleetApiError::HttpStatus(401)))
    {
        refresh_after_rejection(&mut manager, auth_api, tick).await?;
        let Some(mut resend) = CountedStart::resend(counting, RequestCategory::State) else {
            return Err(CollectorError::FleetResendWithheld);
        };
        let second = api
            .list_vehicles(
                manager.access_token_for_sensitive_use()?,
                manager.refresh_token_for_sensitive_use()?,
            )
            .await;
        resend.complete(&second);
        return second.map_err(Into::into);
    }
    first.map_err(Into::into)
}

/// Read one car (C3 R24: a request of category `data`); `tick` as for
/// [`fleet_list_vehicles_with_auth`]. The caller admitted the read and took its
/// [`CountedStart`] before it boxed this future (C3 R47, B2 clarification): a
/// future dropped without ever being polled, as a credential stop drops the
/// queued requests of its tick, then still takes the start back. The helper
/// consumes the guard, completes it with Tesla's answer and admits a resend through
/// the same ledger.
async fn fleet_vehicle_data_with_auth(
    api: &FleetApi,
    auth_api: &FleetAuthApi,
    manager: &Arc<tokio::sync::Mutex<FleetAuthManager>>,
    vehicle: &Vehicle,
    allow_refresh: bool,
    mut counted: CountedStart<'_>,
    tick: &TickRefresh,
) -> Result<VehicleData, CollectorError> {
    let vin = VehicleVin::parse(&vehicle.vin)
        .map_err(|_| CollectorError::FleetApi(FleetApiError::InvalidResponse))?;
    let mut manager = manager.lock().await;
    if allow_refresh {
        refresh_for_request(&mut manager, auth_api, tick).await?;
    }
    let carried_tick_refresh = tick.refreshed();
    let first = api
        .vehicle_data(
            manager.access_token_for_sensitive_use()?,
            manager.refresh_token_for_sensitive_use()?,
            vehicle.id,
            &vin,
        )
        .await;
    counted.complete(&first);
    // Only a 401 asks for a refresh and a resend (C3 R47); see the list.
    if allow_refresh
        && !carried_tick_refresh
        && matches!(first, Err(FleetApiError::HttpStatus(401)))
    {
        refresh_after_rejection(&mut manager, auth_api, tick).await?;
        let Some(mut resend) = counted.resend_start() else {
            return Err(CollectorError::FleetResendWithheld);
        };
        // The retry reads the pair the refresh just installed.
        let second = api
            .vehicle_data(
                manager.access_token_for_sensitive_use()?,
                manager.refresh_token_for_sensitive_use()?,
                vehicle.id,
                &vin,
            )
            .await;
        resend.complete(&second);
        return second.map_err(Into::into);
    }
    first.map_err(Into::into)
}

/// Whether a Fleet request's error is a credential stop (C3 R47): a 401 that
/// survived the tick's one refresh and resend, or met the token that refresh
/// produced. The account stops inside the loop (`CredentialStop`); the owner's
/// "retry collection" lifts it. A Fleet 403 is not one: it is the account's
/// `FleetAccessRefused` on the list and the car's `AccessRefused` on a car's
/// request (C3 R42, R50, R51).
fn fleet_credential_stop(error: &CollectorError) -> bool {
    matches!(
        error,
        CollectorError::FleetApi(FleetApiError::HttpStatus(401))
    )
}

/// Whether an error is the loss of the process's sensitive access (C3 R47, B2
/// clarification): the retained admission that guards the store failed, which says
/// the process is no longer the admitted Hub instance, not that a credential
/// failed. It is not an account state: the run ends at once, with no stop, no gap
/// and no further collector write.
///
/// Only the denial itself is that loss. A Fleet credential that is missing, lacks
/// its scopes or claims, cannot be decrypted or has an unknown refresh outcome is a
/// credential condition with the admission intact: the loops handle it as an
/// authentication outcome, and it never publishes a terminal lease.
fn loses_sensitive_access(error: &CollectorError) -> bool {
    is_sensitive_access_failure(error)
        || matches!(error, CollectorError::FleetCredential(error) if error.is_admission_loss())
}

/// Whether a Fleet request's error is the identified unresolved refresh (C5 R47,
/// C3 R47, B2 clarification 3): the manager refused because a started token
/// receipt is bound to the stored pair and fenced, and the store still shows it.
/// The error variant alone is not enough, because `RotationOutcomeUnknown` also
/// stands for a generation mismatch. The account then stops inside the loop, as for
/// any Fleet credential stop, with the reconnect attention and an active lease.
fn fleet_refresh_outcome_unknown(store: &HubStore, error: &CollectorError) -> bool {
    matches!(
        error,
        CollectorError::FleetCredential(FleetCredentialError::RotationOutcomeUnknown)
    ) && store.identified_unresolved_fleet_refresh().unwrap_or(false)
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
