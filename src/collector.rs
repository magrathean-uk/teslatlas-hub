//! Persistence boundary for an explicit legacy owner-token compatibility read.
//!
//! Networking lives in `owner_api`; this module turns completed reads into
//! bounded, append-only Hub observations, materialises durable drive/charge
//! history through the pure lifecycle projector, and optionally runs a
//! supervised no-wake schedule. The owner token is never held in configuration
//! or argv.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::{Instant, sleep, timeout};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    config::{CollectorCadence, ConfigError, HubConfig, MqttConfig, TerrainConfig},
    credentials::{
        CredentialDirectory, CredentialError, LegacyAuthManager, LegacyAuthManagerError, OwnerToken,
    },
    db::{
        HubStore, ObservationInput, SourceDescriptor, StoreError, StreamObservationResult,
        VehicleDescriptor,
    },
    geocoder::{Geocoder, GeocoderError},
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionDeltaPackRequest, ProjectionPackError,
        ProjectionPackRequest, ProjectionPackWriter, ProjectionSnapshot,
    },
    lifecycle::{
        LifecycleError, LifecycleSample, OpenSessionState, apply_sample, force_close_for_service,
        stream_observation_payload,
    },
    legacy_auth::LegacyAuthFuse,
    location::Wgs84Point,
    mqtt::MqttSummary,
    owner_api::{
        ManualCollection, OwnerApi, OwnerApiAuthError, OwnerApiConfigError, OwnerApiError,
        OwnerApiRequestAudit, Vehicle, VehicleCollectionFailure, VehicleData, VehicleId,
    },
    protocol::{
        CursorClaims, CursorKey, LineageDelta, OpaqueCursor, PROTOCOL_V1, SequenceRange,
        Sha256Digest, HUB_PROJECTION_SCHEMA_V2,
    },
    tesla_stream::{
        StreamEvent, StreamRegion, StreamRequestAudit, TeslaStreamSupervisor, streaming_endpoint,
    },
    terrain_cache::{TerrainCache, TerrainCacheError},
};

#[cfg(test)]
use crate::db::StreamFaultPoint;

const OWNER_API_SOURCE_KIND: &str = "owner_api_compat";
const OWNER_API_SOURCE_KEY: &str = "local_installation_v1";
const EARLIEST_PLAUSIBLE_TIMESTAMP_MS: i64 = 946_684_800_000; // 2000-01-01 UTC
const FUTURE_TIMESTAMP_SKEW_MS: i64 = 5 * 60 * 1000;
const STREAM_SOURCE_KIND: &str = OWNER_API_SOURCE_KIND;
const STREAM_SOURCE_KEY: &str = OWNER_API_SOURCE_KEY;
const TERRAIN_PAGE_LIMIT: u32 = 1_000;
const TERRAIN_PERIOD: Duration = Duration::from_secs(6 * 60 * 60);
const TERRAIN_LOOKUP_BUDGET: Duration = Duration::from_millis(100);
const TERRAIN_RETRY_DELAY: Duration = TERRAIN_PERIOD;
const TERRAIN_FUSE_WINDOW: Duration = Duration::from_secs(3 * 60);
const TERRAIN_FUSE_RESET: Duration = Duration::from_secs(15 * 60);
const API_ERROR_LIMIT: usize = 3;
const API_ERROR_WINDOW: Duration = Duration::from_secs(10 * 60);
const API_ERROR_RESET: Duration = Duration::from_secs(5 * 60);
const VEHICLE_NOT_FOUND_LIMIT: usize = 8;
const VEHICLE_NOT_FOUND_WINDOW: Duration = Duration::from_secs(20 * 60);
const VEHICLE_NOT_FOUND_RESET: Duration = Duration::from_secs(10 * 60);
const LEGACY_REFRESH_RETRY: Duration = Duration::from_secs(5 * 60);
const GENERIC_DRIVING_RETRY: Duration = Duration::from_secs(10);
const GENERIC_CHARGING_RETRY: Duration = Duration::from_secs(15);
const GENERIC_ONLINE_RETRY: Duration = Duration::from_secs(20);
const GENERIC_OTHER_RETRY: Duration = Duration::from_secs(30);
const RETRY_OVERFLOW_FALLBACK: Duration = Duration::from_secs(100 * 365 * 24 * 60 * 60);
const STREAM_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

fn retry_deadline(now: Instant, seconds: u64) -> Instant {
    now.checked_add(Duration::from_secs(seconds))
        .or_else(|| now.checked_add(RETRY_OVERFLOW_FALLBACK))
        .unwrap_or(now)
}

fn observe_pre_online_power(check: &mut PreOnlineCheck, power: Option<i64>, _now: Instant) {
    match (*check, power) {
        (PreOnlineCheck::Probing { .. } | PreOnlineCheck::ConfirmedFake { .. }, Some(_)) => {
            *check = PreOnlineCheck::ConfirmedReal;
        }
        (PreOnlineCheck::Probing { deadline }, None) => {
            *check = PreOnlineCheck::ConfirmedFake { deadline };
        }
        _ => {}
    }
}

fn revoke_pre_online_confirmation(check: &mut PreOnlineCheck, deadline: Instant) {
    if matches!(*check, PreOnlineCheck::ConfirmedReal) {
        *check = PreOnlineCheck::Probing { deadline };
    }
}

fn apply_manual_probe_event(
    checks: &mut HashMap<VehicleId, PreOnlineCheck>,
    vehicle_id: VehicleId,
    event: StreamEvent,
    probe_deadline: Instant,
    allow_power_confirmation: bool,
) {
    match event {
        StreamEvent::Telemetry(update) if allow_power_confirmation => {
            let observed_at = Instant::now();
            if observed_at < probe_deadline
                && let Some(check) = checks.get_mut(&vehicle_id)
            {
                observe_pre_online_power(check, update.power, observed_at);
            }
        }
        StreamEvent::Telemetry(_) | StreamEvent::Healthy => {}
        StreamEvent::VehicleOffline => {
            if let Some(check) = checks.get_mut(&vehicle_id) {
                revoke_pre_online_confirmation(check, probe_deadline);
            }
            tracing::debug!(
                vehicle_id = vehicle_id.get(),
                reason = "stream_reported_offline",
                "manual collection probe revoked numeric stream power confirmation"
            );
        }
        StreamEvent::AuthRejected => {
            if let Some(check) = checks.get_mut(&vehicle_id) {
                revoke_pre_online_confirmation(check, probe_deadline);
            }
            tracing::warn!(
                vehicle_id = vehicle_id.get(),
                reason = "stream_auth_rejected",
                "manual collection probe revoked numeric stream power confirmation"
            );
        }
        StreamEvent::TransportUnavailable => {
            if let Some(check) = checks.get_mut(&vehicle_id) {
                revoke_pre_online_confirmation(check, probe_deadline);
            }
            tracing::debug!(
                vehicle_id = vehicle_id.get(),
                reason = "stream_unavailable",
                "manual collection probe revoked numeric stream power confirmation"
            );
        }
    }
}

fn drain_manual_probe_events(
    checks: &mut HashMap<VehicleId, PreOnlineCheck>,
    streams: &mut [VehicleStreamRuntime],
    probe_deadline: Instant,
    allow_power_confirmation: bool,
) -> bool {
    let mut received_event = false;
    for stream in streams {
        while let Ok(event) = stream.events.try_recv() {
            received_event = true;
            apply_manual_probe_event(
                checks,
                stream.vehicle_id,
                event,
                probe_deadline,
                allow_power_confirmation,
            );
        }
    }
    received_event
}

async fn stop_manual_probe_streams(streams: &mut [VehicleStreamRuntime]) {
    for stream in streams.iter_mut() {
        if let Some(shutdown) = stream._shutdown.take() {
            let _ = shutdown.send(());
        }
    }
    for stream in streams.iter_mut() {
        if timeout(STREAM_SHUTDOWN_TIMEOUT, &mut stream.task).await.is_err() {
            stream.task.abort();
            let _ = (&mut stream.task).await;
        }
    }
}

impl TerrainFuse {
    fn available(&mut self, now: Instant) -> bool {
        if self.blown_until.is_some_and(|until| now >= until) {
            self.blown_until = None;
            self.failures.clear();
        }
        self.blown_until.is_none()
    }

    fn failure(&mut self, now: Instant) {
        if !self.available(now) {
            return;
        }
        self.failures
            .retain(|at| now.saturating_duration_since(*at) < TERRAIN_FUSE_WINDOW);
        self.failures.push(now);
        if self.failures.len() >= 2 {
            self.blown_until = now.checked_add(TERRAIN_FUSE_RESET);
        }
    }
}

/// Result safe to print from a one-shot collection service. It contains local
/// UUIDs and numeric vehicle ids, but never a bearer token, URL, or response.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ManualCollectionReport {
    pub source_id: Uuid,
    pub request_audit_correlation_id: Uuid,
    pub vehicles_seen: usize,
    pub online_vehicles_seen: usize,
    pub snapshots_received: usize,
    pub observations_inserted: usize,
    pub observations_already_present: usize,
    pub snapshots_published: usize,
    pub vehicle_failures: usize,
    pub drives_closed: usize,
    pub charges_closed: usize,
    pub positions_materialised: usize,
    pub charge_samples_materialised: usize,
    pub lifecycle_quarantines: usize,
}

#[derive(Clone)]
enum CollectionAuth {
    Token {
        token: Arc<OwnerToken>,
        region: StreamRegion,
    },
    Legacy {
        manager: Arc<tokio::sync::Mutex<LegacyAuthManager>>,
        fuse: Arc<tokio::sync::Mutex<LegacyAuthFuse>>,
        region: StreamRegion,
    },
}

fn load_collection_auth(
    credentials: &CredentialDirectory,
    legacy_enabled: bool,
    provider_region: StreamRegion,
) -> Result<CollectionAuth, CollectorError> {
    if legacy_enabled {
        let manager = LegacyAuthManager::from_directory(credentials.clone())?;
        let region = manager.region();
        return Ok(CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            region,
        });
    }
    Ok(CollectionAuth::Token {
        token: Arc::new(credentials.owner_token_for_collection()?),
        region: provider_region,
    })
}

#[cfg(test)]
async fn list_vehicles_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
) -> Result<Vec<Vehicle>, CollectorError> {
    let mut vehicles = match auth {
        CollectionAuth::Token { token, .. } => client
            .list_vehicles(token)
            .await
            .map_err(CollectorError::OwnerApi),
        CollectionAuth::Legacy { manager, fuse, .. } => {
            let mut fuse = fuse.lock().await;
            let mut manager = manager.lock().await;
            client
                .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
                .await
                .map_err(Into::into)
        }
    }?;
    for vehicle in &mut vehicles {
        vehicle.settings.suspend_min_resolved = false;
    }
    Ok(vehicles)
}

#[cfg(test)]
async fn vehicle_data_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
) -> Result<VehicleData, CollectorError> {
    match auth {
        CollectionAuth::Token { token, .. } => client
            .vehicle_data(token, vehicle_id)
            .await
            .map_err(Into::into),
        CollectionAuth::Legacy { manager, fuse, .. } => {
            let mut fuse = fuse.lock().await;
            let mut manager = manager.lock().await;
            client
                .vehicle_data_with_legacy_auth_fused(&mut manager, &mut fuse, vehicle_id)
                .await
                .map_err(Into::into)
        }
    }
}

#[cfg(test)]
async fn vehicle_state_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
) -> Result<String, CollectorError> {
    match auth {
        CollectionAuth::Token { token, .. } => client
            .vehicle_state(token, vehicle_id)
            .await
            .map_err(Into::into),
        CollectionAuth::Legacy { manager, fuse, .. } => {
            let mut fuse = fuse.lock().await;
            let mut manager = manager.lock().await;
            client
                .vehicle_state_with_legacy_auth_fused(&mut manager, &mut fuse, vehicle_id)
                .await
                .map_err(Into::into)
        }
    }
}

#[cfg(test)]
async fn vehicle_probe_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
) -> Result<bool, CollectorError> {
    match auth {
        CollectionAuth::Token { token, .. } => client
            .vehicle_probe(token, vehicle_id)
            .await
            .map_err(Into::into),
        CollectionAuth::Legacy { manager, fuse, .. } => {
            let mut fuse = fuse.lock().await;
            let mut manager = manager.lock().await;
            client
                .vehicle_probe_with_legacy_auth_fused(&mut manager, &mut fuse, vehicle_id)
                .await
                .map_err(Into::into)
        }
    }
}

async fn list_vehicles_for_auth_audited(
    client: &OwnerApi,
    auth: &CollectionAuth,
    audit: &OwnerApiRequestAudit<'_>,
) -> Result<Vec<Vehicle>, CollectorError> {
    let mut vehicles = match auth {
        CollectionAuth::Token { token, .. } => client
            .list_vehicles_audited(token, audit)
            .await
            .map_err(CollectorError::OwnerApi),
        CollectionAuth::Legacy { manager, fuse, .. } => {
            let mut fuse = fuse.lock().await;
            let mut manager = manager.lock().await;
            client
                .list_vehicles_with_legacy_auth_fused_audited(&mut manager, &mut fuse, audit)
                .await
                .map_err(Into::into)
        }
    }?;
    for vehicle in &mut vehicles {
        vehicle.settings.suspend_min_resolved = false;
    }
    Ok(vehicles)
}

async fn vehicle_data_for_auth_audited(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
    audit: &OwnerApiRequestAudit<'_>,
) -> Result<VehicleData, CollectorError> {
    match auth {
        CollectionAuth::Token { token, .. } => client
            .vehicle_data_audited(token, vehicle_id, audit)
            .await
            .map_err(Into::into),
        CollectionAuth::Legacy { manager, fuse, .. } => {
            let mut fuse = fuse.lock().await;
            let mut manager = manager.lock().await;
            client
                .vehicle_data_with_legacy_auth_fused_audited(
                    &mut manager,
                    &mut fuse,
                    vehicle_id,
                    audit,
                )
                .await
                .map_err(Into::into)
        }
    }
}

async fn vehicle_state_for_auth_audited(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
    audit: &OwnerApiRequestAudit<'_>,
) -> Result<String, CollectorError> {
    match auth {
        CollectionAuth::Token { token, .. } => client
            .vehicle_state_audited(token, vehicle_id, audit)
            .await
            .map_err(Into::into),
        CollectionAuth::Legacy { manager, fuse, .. } => {
            let mut fuse = fuse.lock().await;
            let mut manager = manager.lock().await;
            client
                .vehicle_state_with_legacy_auth_fused_audited(
                    &mut manager,
                    &mut fuse,
                    vehicle_id,
                    audit,
                )
                .await
                .map_err(Into::into)
        }
    }
}

async fn vehicle_probe_for_auth_audited(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
    audit: &OwnerApiRequestAudit<'_>,
) -> Result<bool, CollectorError> {
    match auth {
        CollectionAuth::Token { token, .. } => client
            .vehicle_probe_audited(token, vehicle_id, audit)
            .await
            .map_err(Into::into),
        CollectionAuth::Legacy { manager, fuse, .. } => {
            let mut fuse = fuse.lock().await;
            let mut manager = manager.lock().await;
            client
                .vehicle_probe_with_legacy_auth_fused_audited(
                    &mut manager,
                    &mut fuse,
                    vehicle_id,
                    audit,
                )
                .await
                .map_err(Into::into)
        }
    }
}

#[cfg(test)]
async fn collect_once_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
) -> Result<ManualCollection, CollectorError> {
    let vehicles = list_vehicles_for_auth(client, auth).await?;
    let confirmed_power = probe_manual_stream_power(client, auth, &vehicles, None).await;
    let mut snapshots = Vec::new();
    let mut failures = Vec::new();
    for vehicle in &vehicles {
        if !vehicle.is_online()
            || !vehicle.settings.enabled
            || !confirmed_power.contains(&vehicle.id)
        {
            continue;
        }
        match vehicle_data_for_auth(client, auth, vehicle.id).await {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(error) => failures.push(VehicleCollectionFailure {
                vehicle_id: vehicle.id,
                error: match error {
                    CollectorError::OwnerApi(error) => error,
                    CollectorError::OwnerApiAuth(_) => OwnerApiError::LegacyAuth,
                    _ => OwnerApiError::Transport,
                },
            }),
        }
    }
    Ok(ManualCollection {
        vehicles,
        snapshots,
        failures,
    })
}

async fn collect_once_for_auth_audited(
    client: &OwnerApi,
    auth: &CollectionAuth,
    audit: &OwnerApiRequestAudit<'_>,
    stream_audit: &StreamRequestAudit,
) -> Result<ManualCollection, CollectorError> {
    let vehicles = list_vehicles_for_auth_audited(client, auth, audit).await?;
    let confirmed_power = probe_manual_stream_power(client, auth, &vehicles, Some(stream_audit)).await;
    let mut snapshots = Vec::new();
    let mut failures = Vec::new();
    for vehicle in &vehicles {
        if !vehicle.is_online()
            || !vehicle.settings.enabled
            || !confirmed_power.contains(&vehicle.id)
        {
            continue;
        }
        match vehicle_data_for_auth_audited(client, auth, vehicle.id, audit).await {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(error) => failures.push(VehicleCollectionFailure {
                vehicle_id: vehicle.id,
                error: match error {
                    CollectorError::OwnerApi(error) => error,
                    CollectorError::OwnerApiAuth(_) => OwnerApiError::LegacyAuth,
                    _ => OwnerApiError::Transport,
                },
            }),
        }
    }
    Ok(ManualCollection {
        vehicles,
        snapshots,
        failures,
    })
}

async fn probe_manual_stream_power(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicles: &[Vehicle],
    stream_audit: Option<&StreamRequestAudit>,
) -> HashSet<VehicleId> {
    let probe_deadline = Instant::now() + PRE_ONLINE_TIMEOUT;
    let mut checks = HashMap::new();
    let mut streams = Vec::new();

    for vehicle in vehicles.iter().filter(|vehicle| {
        vehicle.is_online() && vehicle.settings.enabled
    }) {
        if !vehicle.settings.use_streaming_api {
            tracing::warn!(
                vehicle_id = vehicle.id.get(),
                reason = "streaming_disabled",
                "manual collection deferred until numeric stream power is confirmed"
            );
            continue;
        }
        checks.insert(
            vehicle.id,
            PreOnlineCheck::Probing {
                deadline: probe_deadline,
            },
        );
        if !ensure_vehicle_stream(
            &mut streams,
            vehicle.id,
            auth,
            client,
            PRE_ONLINE_TIMEOUT,
            stream_audit,
        ) {
            checks.remove(&vehicle.id);
            tracing::warn!(
                vehicle_id = vehicle.id.get(),
                reason = "stream_unavailable",
                "manual collection deferred until numeric stream power is confirmed"
            );
        }
    }

    while !checks.is_empty() {
        let received_event =
            drain_manual_probe_events(&mut checks, &mut streams, probe_deadline, true);
        let remaining = probe_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        if !received_event {
            sleep(remaining.min(Duration::from_millis(100))).await;
        } else {
            tokio::task::yield_now().await;
        }
    }

    stop_manual_probe_streams(&mut streams).await;
    drain_manual_probe_events(&mut checks, &mut streams, probe_deadline, false);

    let mut confirmed = HashSet::new();
    for (vehicle_id, check) in checks {
        if matches!(check, PreOnlineCheck::ConfirmedReal) {
            confirmed.insert(vehicle_id);
        } else {
            let reason = match check {
                PreOnlineCheck::ConfirmedFake { .. } => "numeric_power_missing",
                PreOnlineCheck::Probing { .. } => "pre_online_power_timeout",
                PreOnlineCheck::Idle | PreOnlineCheck::ConfirmedReal => unreachable!(),
            };
            tracing::warn!(
                vehicle_id = vehicle_id.get(),
                reason,
                "manual collection deferred until numeric stream power is confirmed"
            );
        }
    }
    confirmed
}

/// Read the decrypted systemd credential only for this explicit operation,
/// then perform one compatibility collection and persist it append-only.
pub async fn collect_once_from_systemd(
    store: &HubStore,
    config: &HubConfig,
) -> Result<ManualCollectionReport, CollectorError> {
    // Refuse a missing or invalid explicit endpoint before opening the
    // credential file. A normal Hub install therefore never touches a token
    // merely because somebody invoked the collection unit too early.
    let client = OwnerApi::new(config.collector.owner_api_options()?)?;
    let credentials = CredentialDirectory::from_systemd_environment()?
        .ok_or(CollectorError::MissingCredentialDirectory)?;
    let region = if config.collector.legacy_auth.enabled {
        StreamRegion::Global
    } else {
        config.collector.stream_region()?
    };
    let auth = load_collection_auth(&credentials, config.collector.legacy_auth.enabled, region)?;
    let cursor_key = credentials.cursor_key()?;
    let request_audit_correlation_id = Uuid::new_v4();
    let audit = OwnerApiRequestAudit::new(store, request_audit_correlation_id);
    let stream_audit = StreamRequestAudit::new(store, request_audit_correlation_id);
    let collection = collect_once_for_auth_audited(&client, &auth, &audit, &stream_audit).await?;
    let mut report = finish_collection(store, &cursor_key, &collection).await?;
    report.request_audit_correlation_id = request_audit_correlation_id;
    Ok(report)
}

async fn finish_collection(
    store: &HubStore,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
) -> Result<ManualCollectionReport, CollectorError> {
    for failure in &collection.failures {
        tracing::warn!(
            vehicle_id = failure.vehicle_id.get(),
            error = %failure.error,
            "owner API vehicle collection failed"
        );
    }
    let _publication_gate = store.acquire_publication_gate().await?;
    let received_at_ms = current_epoch_millis()?;
    let mut report = persist_collection_atomic(store, &collection, received_at_ms)?;
    let lifecycle = materialise_lifecycle_for_collection(store, &collection, received_at_ms)?;
    report.drives_closed += lifecycle.drives_closed;
    report.charges_closed += lifecycle.charges_closed;
    report.positions_materialised += lifecycle.positions_materialised;
    report.charge_samples_materialised += lifecycle.charge_samples_materialised;
    report.lifecycle_quarantines += lifecycle.lifecycle_quarantines;
    report.snapshots_published =
        publish_compatibility_snapshots(store, &cursor_key, &collection, received_at_ms)?;
    Ok(report)
}

fn spawn_terrain_worker(
    data_dir: std::path::PathBuf,
    terrain_config: TerrainConfig,
    cursor_key: CursorKey,
) -> mpsc::Sender<()> {
    let (wake, mut wakes) = mpsc::channel(1);
    tokio::spawn(async move {
        let store = match HubStore::initialize(&data_dir) {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(error = %error, "terrain worker could not open Hub store");
                return;
            }
        };
        let options = match crate::terrain_cache::TerrainCacheOptions::from_config(
            &terrain_config,
            &data_dir,
        ) {
            Ok(options) => options,
            Err(error) => {
                tracing::warn!(error = %terrain_error_code(&error), "terrain worker unavailable");
                return;
            }
        };
        let lookup = match TerrainCache::new(options) {
            Ok(lookup) => lookup,
            Err(error) => {
                tracing::warn!(error = %terrain_error_code(&error), "terrain worker unavailable");
                return;
            }
        };
        let mut fuse = TerrainFuse::default();
        let mut first = true;
        loop {
            if !first {
                tokio::select! {
                    _ = wakes.recv() => {}
                    _ = sleep(TERRAIN_PERIOD) => {}
                }
            }
            first = false;
            if let Err(error) = run_terrain_enrichment_pass(
                &store,
                &lookup,
                &cursor_key,
                terrain_config.min_free_bytes,
                &mut fuse,
            )
            .await
            {
                tracing::warn!(error = %error, "terrain enrichment pass failed");
            }
        }
    });
    wake
}

async fn run_terrain_enrichment_pass(
    store: &HubStore,
    lookup: &TerrainCache,
    cursor_key: &CursorKey,
    minimum_free_bytes: u64,
    fuse: &mut TerrainFuse,
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
        let result = lookup
            .lookup(
                candidate.position.latitude,
                candidate.position.longitude,
                TERRAIN_LOOKUP_BUDGET,
            )
            .await;
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

fn terrain_error_code(error: &TerrainCacheError) -> &'static str {
    match error {
        TerrainCacheError::Timeout => "timeout",
        TerrainCacheError::Network(_) => "source_unavailable",
        TerrainCacheError::BadResponse => "invalid_response",
        TerrainCacheError::InvalidArchive => "invalid_archive",
        TerrainCacheError::InsufficientSpace => "insufficient_free_space",
        TerrainCacheError::Io(_) => "io",
        TerrainCacheError::InvalidConfig => "invalid_config",
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

/// Supervised, opt-in no-wake collector. Requires an explicit positive interval
/// in configuration. Uses exponential backoff on transport failures and never
/// issues wake or command requests.
pub async fn run_supervised_from_systemd(
    store: &HubStore,
    config: &HubConfig,
) -> Result<(), CollectorError> {
    let _activation = config.collector.supervised_interval()?;
    let cadence = config.collector.cadence()?;
    let client = OwnerApi::new(config.collector.owner_api_options()?)?;
    let credentials = CredentialDirectory::from_systemd_environment()?
        .ok_or(CollectorError::MissingCredentialDirectory)?;
    let region = if config.collector.legacy_auth.enabled {
        StreamRegion::Global
    } else {
        config.collector.stream_region()?
    };
    let auth = load_collection_auth(&credentials, config.collector.legacy_auth.enabled, region)?;
    let cursor_key = credentials.cursor_key()?;
    let terrain_wake = spawn_terrain_worker(
        config.data_dir.clone(),
        config.terrain.clone(),
        cursor_key.clone(),
    );
    let mut scheduler = VehicleScheduler::new(cadence, Instant::now());
    let mut streams = Vec::new();

    loop {
        let request_audit_correlation_id = Uuid::new_v4();
        let audit = OwnerApiRequestAudit::new(store, request_audit_correlation_id);
        let stream_audit = StreamRequestAudit::new(store, request_audit_correlation_id);
        drain_stream_events(
            store,
            &mut scheduler,
            &mut streams,
        )
        .await?;
        let now = Instant::now();
        if scheduler.discovery_due(now) {
            match list_vehicles_for_auth_audited(&client, &auth, &audit).await {
                Ok(vehicles) => {
                    let events = scheduler.accept_discovery(vehicles, Instant::now());
                    if !events.is_empty() {
                        persist_discovery_events(store, &cursor_key, &events).await?;
                    }
                    for vehicle in scheduler.vehicles() {
                        if vehicle.is_online()
                            && vehicle.settings.enabled
                            && scheduler.should_start_stream(vehicle.id)
                        {
                            ensure_vehicle_stream(
                                &mut streams,
                                vehicle.id,
                                &auth,
                                &client,
                                cadence.stream_health_timeout,
                                Some(&stream_audit),
                            );
                        }
                    }
                }
                Err(error) => {
                    let now = Instant::now();
                    let delay = scheduler.discovery_failed_for_error(&error, now);
                    tracing::warn!(error = %error, "owner API discovery failed; backing off");
                    sleep(delay).await;
                    continue;
                }
            }
        }

        for vehicle_id in scheduler.due_offline_state_vehicles(Instant::now()) {
            match vehicle_state_for_auth_audited(&client, &auth, vehicle_id, &audit).await {
                Ok(state) => {
                    let events =
                        scheduler.accept_vehicle_state(vehicle_id, state, Instant::now());
                    if !events.is_empty() {
                        persist_discovery_events(store, &cursor_key, &events).await?;
                    }
                }
                Err(error) => {
                    scheduler.offline_state_failed_for_error(
                        vehicle_id,
                        &error,
                        Instant::now(),
                    );
                    tracing::warn!(vehicle_id = vehicle_id.get(), error = %error, "vehicle state fetch failed; retry scheduled");
                }
            }
        }

        let due = scheduler.due_vehicles(Instant::now());
        for vehicle_id in scheduler.due_service_vehicles(Instant::now()) {
            match vehicle_probe_for_auth_audited(&client, &auth, vehicle_id, &audit).await {
                Ok(true) => scheduler.service_retry(vehicle_id, Instant::now()),
                Ok(false) => {
                    scheduler.service_exited(vehicle_id, Instant::now());
                    if let Some(vehicle) = scheduler
                        .vehicles()
                        .into_iter()
                        .find(|vehicle| vehicle.id == vehicle_id)
                        && vehicle.settings.use_streaming_api
                    {
                        ensure_vehicle_stream(
                            &mut streams,
                            vehicle_id,
                            &auth,
                            &client,
                            cadence.stream_health_timeout,
                            Some(&stream_audit),
                        );
                    }
                }
                Err(error) if is_vehicle_in_service(&error) => {
                    scheduler.service_retry(vehicle_id, Instant::now());
                }
                Err(error) => scheduler.vehicle_failed_for_error(vehicle_id, &error, Instant::now()),
            }
        }
        if !due.is_empty() {
            let mut snapshots = Vec::new();
            let mut failures = Vec::new();
            let mut scheduler_events = Vec::new();
            for vehicle_id in due {
                match vehicle_data_for_auth_audited(&client, &auth, vehicle_id, &audit).await {
                    Ok(snapshot) => {
                        if snapshot_service_mode(&snapshot) == Some(true) {
                            scheduler.enter_service_mode(vehicle_id, Instant::now());
                            disconnect_vehicle_stream(&mut streams, vehicle_id).await;
                        }
                        if let Some(vehicle) = scheduler
                            .vehicles()
                            .into_iter()
                            .find(|vehicle| vehicle.id == vehicle_id)
                        {
                            if vehicle.is_online()
                                && vehicle.settings.enabled
                                && vehicle.settings.use_streaming_api
                            {
                                ensure_vehicle_stream(
                                    &mut streams,
                                    vehicle.id,
                                    &auth,
                                    &client,
                                    cadence.stream_health_timeout,
                                    Some(&stream_audit),
                                );
                            }
                        }
                        let req_not_unlocked = scheduler
                            .vehicles()
                            .into_iter()
                            .find(|vehicle| vehicle.id == vehicle_id)
                            .map_or(true, |vehicle| vehicle.settings.req_not_unlocked);
                        if let Some(event) = scheduler.vehicle_succeeded(
                            vehicle_id,
                            poll_phase(&snapshot),
                            sleep_eligible_with_policy(&snapshot, req_not_unlocked),
                            Instant::now(),
                        ) {
                            scheduler_events.push(event);
                        }
                        snapshots.push(snapshot);
                    }
                    Err(error) if is_vehicle_in_service(&error) => {
                        scheduler.enter_service_mode(vehicle_id, Instant::now());
                        disconnect_vehicle_stream(&mut streams, vehicle_id).await;
                        force_close_vehicle_for_service(
                            store,
                            vehicle_id,
                            current_epoch_millis()?,
                        )?;
                    }
                    Err(error) => {
                        scheduler.vehicle_failed_for_error(vehicle_id, &error, Instant::now());
                        failures.push(VehicleCollectionFailure {
                            vehicle_id,
                            error: match error {
                                CollectorError::OwnerApi(error) => error,
                                CollectorError::OwnerApiAuth(_) => OwnerApiError::LegacyAuth,
                                _ => OwnerApiError::Transport,
                            },
                        });
                    }
                }
            }
            let collection = ManualCollection {
                vehicles: scheduler.vehicles(),
                snapshots,
                failures,
            };
            let report = finish_collection(store, &cursor_key, &collection).await?;
            let _ = terrain_wake.try_send(());
            if !scheduler_events.is_empty() {
                persist_discovery_events(store, &cursor_key, &scheduler_events).await?;
            }
            tracing::info!(
                vehicles = report.vehicles_seen,
                online = report.online_vehicles_seen,
                inserted = report.observations_inserted,
                drives = report.drives_closed,
                charges = report.charges_closed,
                failures = report.vehicle_failures,
                "state-aware compatibility collection completed"
            );
        }

        let delay = scheduler.delay_until_next_action(Instant::now());
        let vehicles = scheduler.vehicles();
        run_address_enrichment_once(
            store,
            config,
            &cursor_key,
            &vehicles,
            current_epoch_millis()?,
        )
        .await?;
        replay_export_outbox(
            store,
            &cursor_key,
            &vehicles,
            current_epoch_millis()?,
        )
        .await?;
        if !delay.is_zero() {
            sleep(delay).await;
        }
    }
}

async fn replay_export_outbox(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
) -> Result<usize, CollectorError> {
    let _publication_gate = store.acquire_publication_gate().await?;
    let Some(claim) = store.claim_export_outbox(now_ms)? else {
        return Ok(0);
    };
    if store.vehicle_has_v2_base(claim.vehicle_id)? {
        let Some(sync_claim) = store.claim_sync_mutations(claim.vehicle_id, now_ms, 10_000)? else {
            store.complete_export_outbox(&claim)?;
            return Ok(0);
        };
        return match publish_v2_delta(store, cursor_key, &sync_claim) {
            Ok(()) => {
                store.complete_export_outbox(&claim)?;
                Ok(1)
            }
            Err(error) => {
                store.release_sync_mutations(&sync_claim)?;
                store.fail_export_outbox(&claim, "publication_failed", now_ms)?;
                Err(error)
            }
        };
    }
    let Some(source_key) = store.source_vehicle_key(claim.vehicle_id)? else {
        store.fail_export_outbox(&claim, "vehicle_identity_missing", now_ms)?;
        return Ok(0);
    };
    let source_key_number = source_key
        .strip_prefix("eid:")
        .or_else(|| source_key.strip_prefix("vid:"))
        .unwrap_or(&source_key);
    let Ok(source_vehicle_id) = source_key_number.parse::<u64>() else {
        store.fail_export_outbox(&claim, "vehicle_identity_invalid", now_ms)?;
        return Ok(0);
    };
    let Some(vehicle) = vehicles.iter().find(|vehicle| vehicle.id.get() == source_vehicle_id) else {
        store.fail_export_outbox(&claim, "vehicle_not_discovered", now_ms)?;
        return Ok(0);
    };
    let collection = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![],
        failures: vec![],
    };
    match publish_compatibility_snapshots(store, cursor_key, &collection, now_ms) {
        Ok(_) => {
            store.complete_export_outbox(&claim)?;
            Ok(1)
        }
        Err(_) => {
            store.fail_export_outbox(&claim, "publication_failed", now_ms)?;
            Ok(0)
        }
    }
}

fn publish_v2_delta(
    store: &HubStore,
    cursor_key: &CursorKey,
    claim: &crate::db::SyncMutationClaim,
) -> Result<(), CollectorError> {
    let (_, head_sequence, parent_digest) = store
        .v2_head(claim.vehicle_id)?
        .ok_or_else(|| CollectorError::Store(StoreError::LineageCatalogConflict))?;
    let from_sequence = u64::try_from(head_sequence)
        .map_err(|_| CollectorError::Store(StoreError::InvalidStoredSequence))?;
    let to_sequence = from_sequence
        .checked_add(u64::try_from(claim.mutations.len()).map_err(|_| StoreError::SequenceTooLarge)?)
        .ok_or_else(|| CollectorError::Store(StoreError::SequenceTooLarge))?;
    let binding = store.v2_projection_binding(claim.vehicle_id)?;
    let sequence = SequenceRange {
        from_exclusive: from_sequence,
        to_inclusive: to_sequence,
    };
    let delta = store.projection_delta_for_mutations(
        claim,
        binding.clone(),
        sequence,
        parent_digest,
    )?;
    let request = ProjectionDeltaPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: Uuid::new_v4(),
        ordinal: 0,
        delta: &delta,
    };
    let built = ProjectionPackWriter::new(store.packs_dir()).write_delta(&request)?;
    let chain_digest = Sha256Digest::of_bytes(
        format!("delta-v2/{}:{}", parent_digest, built.metadata.sha256).as_bytes(),
    );
    let lineage_delta = LineageDelta {
        from_sequence,
        to_sequence,
        parent_chain_digest: parent_digest,
        chain_digest,
        pack_digest: built.metadata.sha256,
        pack: built.metadata,
    };
    let terminal_cursor = OpaqueCursor::issue(
        cursor_key,
        CursorClaims {
            protocol: PROTOCOL_V1,
            schema: HUB_PROJECTION_SCHEMA_V2,
            installation_id: binding.installation_id,
            account_id: binding.account_id,
            vehicle_id: binding.vehicle_id,
            generation: binding.generation,
            sequence: to_sequence,
        },
    )
    .map_err(StoreError::Manifest)?;
    let cursor_json = serde_json::to_string(&terminal_cursor)
        .map_err(CollectorError::SerializeSnapshot)?;
    store.commit_v2_delta_claim(claim, &lineage_delta, &cursor_json)?;
    Ok(())
}

pub async fn run_address_enrichment_once(
    store: &HubStore,
    config: &HubConfig,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
) -> Result<bool, CollectorError> {
    let Some(job) = store.claim_address_enrichment_job(now_ms)? else {
        return Ok(false);
    };
    let result = match Geocoder::new(&config.geocoder) {
        Ok(geocoder) => {
            let point = Wgs84Point::new(job.latitude, job.longitude)
                .map_err(|_| GeocoderError::MalformedResponse)?;
            geocoder.reverse_cached(store, point, now_ms).await
        }
        Err(GeocoderError::Disabled) => {
            store.complete_address_enrichment(&job, None, now_ms)?;
            return Ok(false);
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(address) => {
            let _publication_gate = store.acquire_publication_gate().await?;
            let completion =
                store.complete_address_enrichment(&job, Some(&address.display_name), now_ms)?;
            if completion.changed {
                if let Some(source_key) = store.source_vehicle_key(job.vehicle_id)?
                    && let Ok(source_vehicle_id) = source_key.parse::<u64>()
                    && let Some(vehicle) = vehicles
                        .iter()
                        .find(|vehicle| vehicle.id.get() == source_vehicle_id)
                {
                    publish_compatibility_snapshots(
                        store,
                        cursor_key,
                        &ManualCollection {
                            vehicles: vec![vehicle.clone()],
                            snapshots: vec![],
                            failures: vec![],
                        },
                        now_ms,
                    )?;
                }
            }
            Ok(true)
        }
        Err(error)
            if matches!(
                error,
                GeocoderError::MalformedResponse
                    | GeocoderError::NoResult
                    | GeocoderError::Disabled
            ) =>
        {
            store.complete_address_enrichment(&job, None, now_ms)?;
            Ok(false)
        }
        Err(error) => {
            store.retry_address_enrichment(&job, &error.to_string(), now_ms)?;
            Ok(false)
        }
    }
}

struct VehicleStreamRuntime {
    vehicle_id: VehicleId,
    events: mpsc::Receiver<StreamEvent>,
    _shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), crate::tesla_stream::StreamSupervisorError>>,
}

fn ensure_vehicle_stream(
    streams: &mut Vec<VehicleStreamRuntime>,
    vehicle_id: VehicleId,
    auth: &CollectionAuth,
    client: &OwnerApi,
    health_timeout: Duration,
    audit: Option<&StreamRequestAudit>,
) -> bool {
    streams.retain(|stream| stream.vehicle_id != vehicle_id || !stream.task.is_finished());
    if streams.iter().any(|stream| stream.vehicle_id == vehicle_id) {
        return true;
    }
    let (events, receiver) = mpsc::channel(32);
    let (shutdown, stop) = oneshot::channel();
    let supervisor_result = match auth {
        CollectionAuth::Token { token, region } => match audit {
            Some(audit) => TeslaStreamSupervisor::new_shared_audited(
                vehicle_id,
                Arc::clone(token),
                *region,
                streaming_endpoint(*region).to_owned(),
                events,
                audit.clone(),
            ),
            None => TeslaStreamSupervisor::new_shared(
                vehicle_id,
                Arc::clone(token),
                *region,
                streaming_endpoint(*region).to_owned(),
                events,
            ),
        },
        CollectionAuth::Legacy { manager, region, .. } => match audit {
            Some(audit) => TeslaStreamSupervisor::new_legacy_auth_audited(
                vehicle_id,
                Arc::clone(manager),
                *region,
                streaming_endpoint(*region).to_owned(),
                client.http_client(),
                events,
                audit.clone(),
            ),
            None => TeslaStreamSupervisor::new_legacy_auth(
                vehicle_id,
                Arc::clone(manager),
                *region,
                streaming_endpoint(*region).to_owned(),
                client.http_client(),
                events,
            ),
        },
    };
    let supervisor = match supervisor_result {
        Ok(supervisor) => supervisor,
        Err(error) => {
            tracing::warn!(vehicle_id = vehicle_id.get(), error = %error, "vehicle stream unavailable");
            return false;
        }
    };
    let task = tokio::spawn(supervisor.with_health_timeout(health_timeout).run(stop));
    streams.push(VehicleStreamRuntime {
        vehicle_id,
        events: receiver,
        _shutdown: Some(shutdown),
        task,
    });
    true
}

async fn disconnect_vehicle_stream(
    streams: &mut Vec<VehicleStreamRuntime>,
    vehicle_id: VehicleId,
) {
    for stream in streams
        .iter_mut()
        .filter(|stream| stream.vehicle_id == vehicle_id)
    {
        if let Some(shutdown) = stream._shutdown.take() {
            let _ = shutdown.send(());
        }
    }
    for stream in streams
        .iter_mut()
        .filter(|stream| stream.vehicle_id == vehicle_id)
    {
        if timeout(STREAM_SHUTDOWN_TIMEOUT, &mut stream.task).await.is_err() {
            stream.task.abort();
            let _ = (&mut stream.task).await;
        }
    }
    streams.retain(|stream| stream.vehicle_id != vehicle_id);
}

fn is_vehicle_in_service(error: &CollectorError) -> bool {
    matches!(
        error,
        CollectorError::OwnerApi(OwnerApiError::VehicleInService)
            | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(
                OwnerApiError::VehicleInService
            ))
    )
}

fn owner_api_error(error: &CollectorError) -> Option<&OwnerApiError> {
    match error {
        CollectorError::OwnerApi(error) => Some(error),
        CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(error)) => Some(error),
        _ => None,
    }
}

async fn drain_stream_events(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut [VehicleStreamRuntime],
) -> Result<(), CollectorError> {
    for stream in streams.iter_mut() {
        while let Ok(event) = stream.events.try_recv() {
            match event {
                StreamEvent::Healthy => {
                    scheduler.stream_healthy(stream.vehicle_id, Instant::now());
                }
                StreamEvent::Telemetry(update) => {
                    if persist_stream_update(store, stream.vehicle_id, &update)? {
                        scheduler.pre_online_power(stream.vehicle_id, update.power, Instant::now());
                    }
                }
                StreamEvent::VehicleOffline => {
                    let now = Instant::now();
                    scheduler.stream_unhealthy(stream.vehicle_id, now);
                    scheduler.schedule_offline_state_fetch(stream.vehicle_id, now);
                }
                StreamEvent::AuthRejected => {
                    scheduler.stream_unhealthy(stream.vehicle_id, Instant::now());
                    tracing::warn!(
                        vehicle_id = stream.vehicle_id.get(),
                        "vehicle stream authentication rejected"
                    );
                }
                StreamEvent::TransportUnavailable => {
                    scheduler.stream_unhealthy(stream.vehicle_id, Instant::now());
                }
            }
        }
    }
    Ok(())
}

fn persist_stream_update(
    store: &HubStore,
    vehicle_id: VehicleId,
    update: &crate::tesla_stream::StreamUpdate,
) -> Result<bool, CollectorError> {
    let received_at_ms = current_epoch_millis()?;
    let maximum = received_at_ms.saturating_add(FUTURE_TIMESTAMP_SKEW_MS);
    if update.timestamp_ms < EARLIEST_PLAUSIBLE_TIMESTAMP_MS || update.timestamp_ms > maximum {
        tracing::debug!(
            vehicle_id = vehicle_id.get(),
            reason = "clock_or_future_timestamp",
            "stream frame rejected"
        );
        return Ok(false);
    }
    let source = store.register_source(
        &SourceDescriptor::new(STREAM_SOURCE_KIND, STREAM_SOURCE_KEY),
        received_at_ms,
    )?;
    let registered = store.register_vehicle(
        &VehicleDescriptor::new(source.source_id, vehicle_id.get().to_string())
            .with_tesla_identity(Some(vehicle_id.get() as i64), None),
        received_at_ms,
    )?;
    let result = store.accept_stream_observation_and_lifecycle(
        &ObservationInput {
            source_id: source.source_id,
            vehicle_id: registered.vehicle_id,
            observed_at_ms: update.timestamp_ms,
            payload: stream_observation_payload(update),
        },
        received_at_ms,
        compatibility_car_id(vehicle_id.get()),
    )?;
    if matches!(result, StreamObservationResult::IgnoredDuplicate) {
        tracing::debug!(
            vehicle_id = vehicle_id.get(),
            reason = "non_monotonic_timestamp",
            "stream frame rejected"
        );
        return Ok(false);
    }
    Ok(true)
}

/// Bridge point for a typed summary normalizer. The caller invokes this only
/// after the accepted vehicle transaction has committed; MQTT persistence is
/// never part of the collection transaction and disabled mode is a no-op.
pub fn enqueue_mqtt_after_commit(
    store: &HubStore,
    config: &MqttConfig,
    summary: &MqttSummary,
    committed: bool,
    updated_at_ms: i64,
) -> Result<(), CollectorError> {
    if !committed || !config.enabled {
        return Ok(());
    }
    store.enqueue_mqtt_summary(
        config.namespace.as_deref(),
        summary,
        true,
        updated_at_ms,
    )?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PollPhase {
    Driving,
    Charging,
    Updating,
    Online,
}

fn poll_phase_for_vehicle_state(state: &str) -> PollPhase {
    match state {
        "driving" => PollPhase::Driving,
        "charging" => PollPhase::Charging,
        "updating" => PollPhase::Updating,
        _ => PollPhase::Online,
    }
}

fn generic_api_retry_delay(scheduled: &ScheduledVehicle) -> Duration {
    match scheduled.vehicle.state.as_str() {
        "asleep" | "offline" | "start" | "suspended" => GENERIC_OTHER_RETRY,
        "driving" => GENERIC_DRIVING_RETRY,
        "charging" => GENERIC_CHARGING_RETRY,
        "updating" => GENERIC_ONLINE_RETRY,
        "online" => match scheduled.last_phase {
            PollPhase::Driving => GENERIC_DRIVING_RETRY,
            PollPhase::Charging => GENERIC_CHARGING_RETRY,
            PollPhase::Updating | PollPhase::Online => GENERIC_ONLINE_RETRY,
        },
        _ => GENERIC_OTHER_RETRY,
    }
}

#[derive(Clone, Debug)]
struct ScheduledVehicle {
    vehicle: Vehicle,
    settings: crate::hub_pack::ProjectionCarSettings,
    next_poll: Instant,
    failure_backoff: Duration,
    last_phase: PollPhase,
    state_since: Instant,
    offline_timeout_emitted: bool,
    last_used: Instant,
    suspended: bool,
    stream_healthy: bool,
    pre_online: PreOnlineCheck,
    service_mode: bool,
    offline_state_fetch_due: Option<Instant>,
}

#[derive(Default)]
struct VehicleFuseState {
    api_errors: Vec<Instant>,
    api_blown_until: Option<Instant>,
    not_found: Vec<Instant>,
    not_found_blown_until: Option<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreOnlineCheck {
    Idle,
    Probing { deadline: Instant },
    ConfirmedFake { deadline: Instant },
    ConfirmedReal,
}

const PRE_ONLINE_TIMEOUT: Duration = Duration::from_secs(30);

struct VehicleScheduler {
    cadence: CollectorCadence,
    vehicles: BTreeMap<VehicleId, ScheduledVehicle>,
    next_discovery: Instant,
    discovery_backoff: Duration,
    vehicle_fuses: HashMap<VehicleId, VehicleFuseState>,
}

impl VehicleScheduler {
    fn new(cadence: CollectorCadence, now: Instant) -> Self {
        Self {
            cadence,
            vehicles: BTreeMap::new(),
            next_discovery: now,
            discovery_backoff: cadence.sleeping,
            vehicle_fuses: HashMap::new(),
        }
    }

    fn discovery_due(&self, now: Instant) -> bool {
        now >= self.next_discovery
    }

    fn accept_discovery(&mut self, vehicles: Vec<Vehicle>, now: Instant) -> Vec<Vehicle> {
        let mut discovered = BTreeMap::new();
        let mut events = Vec::new();
        for vehicle in vehicles {
            let previous = self.vehicles.get(&vehicle.id);
            let state_changed =
                previous.is_none_or(|scheduled| scheduled.vehicle.state != vehicle.state);
            let newly_online = vehicle.is_online()
                && previous.is_none_or(|scheduled| !scheduled.vehicle.is_online());
            let next_poll = if newly_online {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.next_poll)
            };
            let failure_backoff = if newly_online {
                self.cadence.online
            } else {
                previous.map_or(self.cadence.online, |scheduled| scheduled.failure_backoff)
            };
            let last_phase = if state_changed {
                poll_phase_for_vehicle_state(&vehicle.state)
            } else {
                previous.map_or(PollPhase::Online, |scheduled| scheduled.last_phase)
            };
            let state_since = if state_changed {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.state_since)
            };
            let last_used = if newly_online {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.last_used)
            };
            let offline_timeout = vehicle.state == "offline"
                && !state_changed
                && previous.is_some_and(|scheduled| !scheduled.offline_timeout_emitted)
                && now.saturating_duration_since(state_since) >= self.cadence.offline_drive_timeout;
            if state_changed || offline_timeout {
                events.push(vehicle.clone());
            }
            let stream_healthy = if state_changed || !vehicle.settings.use_streaming_api {
                false
            } else {
                previous.is_some_and(|scheduled| scheduled.stream_healthy)
            };
            let service_mode = previous.is_some_and(|scheduled| scheduled.service_mode);
            let pre_online = if !vehicle.is_online() || !vehicle.settings.use_streaming_api {
                PreOnlineCheck::Idle
            } else if service_mode {
                PreOnlineCheck::Idle
            } else if newly_online {
                PreOnlineCheck::Probing {
                    deadline: now + PRE_ONLINE_TIMEOUT,
                }
            } else {
                previous.map_or(PreOnlineCheck::Idle, |scheduled| scheduled.pre_online)
            };
            discovered.insert(
                vehicle.id,
                ScheduledVehicle {
                    settings: vehicle.settings.clone(),
                    vehicle,
                    next_poll,
                    failure_backoff,
                    last_phase,
                    state_since,
                    offline_timeout_emitted: if state_changed {
                        false
                    } else {
                        previous.is_some_and(|scheduled| scheduled.offline_timeout_emitted)
                            || offline_timeout
                    },
                    last_used,
                    suspended: if state_changed {
                        false
                    } else {
                        previous.is_some_and(|scheduled| scheduled.suspended)
                    },
                    stream_healthy,
                    pre_online,
                    service_mode,
                    offline_state_fetch_due: if state_changed {
                        None
                    } else {
                        previous.and_then(|scheduled| scheduled.offline_state_fetch_due)
                    },
                },
            );
        }
        self.vehicles = discovered;
        self.vehicle_fuses.retain(|id, _| self.vehicles.contains_key(id));
        for id in self.vehicles.keys().copied() {
            self.vehicle_fuses.entry(id).or_default();
        }
        self.next_discovery = now + self.cadence.sleeping;
        self.discovery_backoff = self.cadence.sleeping;
        events
    }

    fn accept_vehicle_state(
        &mut self,
        vehicle_id: VehicleId,
        state: String,
        now: Instant,
    ) -> Vec<Vehicle> {
        let Some(mut vehicle) = self.vehicles.get(&vehicle_id).map(|scheduled| scheduled.vehicle.clone()) else {
            return Vec::new();
        };
        vehicle.state = state;
        let events = self.accept_discovery_mode(vec![vehicle], now, false);
        if let Some(scheduled) = self.vehicles.get_mut(&vehicle_id) {
            scheduled.offline_state_fetch_due = None;
        }
        events
    }

    fn accept_discovery_mode(
        &mut self,
        vehicles: Vec<Vehicle>,
        now: Instant,
        replace_all: bool,
    ) -> Vec<Vehicle> {
        let mut discovered = if replace_all {
            BTreeMap::new()
        } else {
            self.vehicles.clone()
        };
        let mut events = Vec::new();
        for vehicle in vehicles {
            let previous = self.vehicles.get(&vehicle.id);
            let state_changed =
                previous.is_none_or(|scheduled| scheduled.vehicle.state != vehicle.state);
            let newly_online = vehicle.is_online()
                && previous.is_none_or(|scheduled| !scheduled.vehicle.is_online());
            let next_poll = if newly_online {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.next_poll)
            };
            let failure_backoff = if newly_online {
                self.cadence.online
            } else {
                previous.map_or(self.cadence.online, |scheduled| scheduled.failure_backoff)
            };
            let last_phase = if state_changed {
                poll_phase_for_vehicle_state(&vehicle.state)
            } else {
                previous.map_or(PollPhase::Online, |scheduled| scheduled.last_phase)
            };
            let state_since = if state_changed {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.state_since)
            };
            let last_used = if newly_online {
                now
            } else {
                previous.map_or(now, |scheduled| scheduled.last_used)
            };
            let offline_timeout = vehicle.state == "offline"
                && !state_changed
                && previous.is_some_and(|scheduled| !scheduled.offline_timeout_emitted)
                && now.saturating_duration_since(state_since) >= self.cadence.offline_drive_timeout;
            if state_changed || offline_timeout {
                events.push(vehicle.clone());
            }
            let stream_healthy = if state_changed || !vehicle.settings.use_streaming_api {
                false
            } else {
                previous.is_some_and(|scheduled| scheduled.stream_healthy)
            };
            let service_mode = previous.is_some_and(|scheduled| scheduled.service_mode);
            let pre_online = if !vehicle.is_online() || !vehicle.settings.use_streaming_api {
                PreOnlineCheck::Idle
            } else if service_mode {
                PreOnlineCheck::Idle
            } else if newly_online {
                PreOnlineCheck::Probing {
                    deadline: now + PRE_ONLINE_TIMEOUT,
                }
            } else {
                previous.map_or(PreOnlineCheck::Idle, |scheduled| scheduled.pre_online)
            };
            discovered.insert(
                vehicle.id,
                ScheduledVehicle {
                    settings: vehicle.settings.clone(),
                    vehicle,
                    next_poll,
                    failure_backoff,
                    last_phase,
                    state_since,
                    offline_timeout_emitted: if state_changed {
                        false
                    } else {
                        previous.is_some_and(|scheduled| scheduled.offline_timeout_emitted)
                            || offline_timeout
                    },
                    last_used,
                    suspended: if state_changed {
                        false
                    } else {
                        previous.is_some_and(|scheduled| scheduled.suspended)
                    },
                    stream_healthy,
                    pre_online,
                    service_mode,
                    offline_state_fetch_due: if state_changed {
                        None
                    } else {
                        previous.and_then(|scheduled| scheduled.offline_state_fetch_due)
                    },
                },
            );
        }
        self.vehicles = discovered;
        self.vehicle_fuses.retain(|id, _| self.vehicles.contains_key(id));
        for id in self.vehicles.keys().copied() {
            self.vehicle_fuses.entry(id).or_default();
        }
        if replace_all {
            self.next_discovery = now + self.cadence.sleeping;
            self.discovery_backoff = self.cadence.sleeping;
        }
        events
    }

    fn discovery_failed(&mut self, now: Instant) -> Duration {
        let delay = self.discovery_backoff;
        self.next_discovery = now + delay;
        self.discovery_backoff = self
            .discovery_backoff
            .saturating_mul(2)
            .min(self.cadence.maximum_backoff);
        delay
    }

    fn discovery_failed_for_error(&mut self, error: &CollectorError, now: Instant) -> Duration {
        if let Some(OwnerApiError::RateLimited {
            retry_after_seconds,
        }) = owner_api_error(error)
        {
            let delay = Duration::from_secs(*retry_after_seconds);
            self.next_discovery = retry_deadline(now, *retry_after_seconds);
            return delay;
        }
        self.discovery_failed(now)
    }

    fn due_vehicles(&mut self, now: Instant) -> Vec<VehicleId> {
        for scheduled in self.vehicles.values_mut() {
            let deadline = match scheduled.pre_online {
                PreOnlineCheck::Probing { deadline }
                | PreOnlineCheck::ConfirmedFake { deadline } => Some(deadline),
                _ => None,
            };
            if deadline.is_some_and(|deadline| now >= deadline) {
                // A timeout is absence of proof, never proof that a vehicle is
                // safe to read. Keep the stream gate closed until a later
                // numeric-power frame explicitly re-confirms it.
                scheduled.next_poll = now + self.cadence.sleeping;
            }
        }
        let candidates = self
            .vehicles
            .values()
            .filter(|scheduled| {
                scheduled.vehicle.is_online()
                    && scheduled.settings.enabled
                    && !scheduled.service_mode
                    && matches!(scheduled.pre_online, PreOnlineCheck::ConfirmedReal)
                    && now >= scheduled.next_poll
            })
            .map(|scheduled| scheduled.vehicle.id)
            .collect::<Vec<_>>();
        candidates
            .into_iter()
            .filter(|id| self.vehicle_fuse_healthy(*id, now))
            .collect()
    }

    fn schedule_offline_state_fetch(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            if scheduled.offline_state_fetch_due.is_none() {
                scheduled.offline_state_fetch_due = Some(now);
            }
            scheduled.next_poll = now + GENERIC_OTHER_RETRY;
        }
    }

    fn due_offline_state_vehicles(&mut self, now: Instant) -> Vec<VehicleId> {
        self.vehicles
            .values_mut()
            .filter_map(|scheduled| {
                scheduled
                    .offline_state_fetch_due
                    .filter(|due| now >= *due)
                    .map(|_| {
                        scheduled.offline_state_fetch_due = None;
                        scheduled.vehicle.id
                    })
            })
            .collect()
    }

    fn offline_state_failed_for_error(
        &mut self,
        id: VehicleId,
        error: &CollectorError,
        now: Instant,
    ) {
        let delay = if matches!(
            error,
            CollectorError::LegacyAuthManager(_)
                | CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(_))
                | CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
        ) {
            LEGACY_REFRESH_RETRY
        } else if let Some(OwnerApiError::RateLimited {
            retry_after_seconds,
        }) = owner_api_error(error)
        {
            Duration::from_secs(*retry_after_seconds)
        } else {
            GENERIC_OTHER_RETRY
        };
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            let due = now.checked_add(delay).unwrap_or(now);
            scheduled.offline_state_fetch_due = Some(due);
            scheduled.next_poll = due;
        }
    }

    fn vehicle_succeeded(
        &mut self,
        id: VehicleId,
        phase: PollPhase,
        sleep_eligible: bool,
        now: Instant,
    ) -> Option<Vehicle> {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            if !scheduled.settings.enabled {
                return None;
            }
            if scheduled.service_mode {
                return None;
            }
            let was_suspended = scheduled.suspended;
            let (idle_suspend_after, suspended_interval) = if scheduled.settings.use_streaming_api
                && scheduled.stream_healthy
            {
                (Duration::from_secs(3 * 60), Duration::from_secs(30 * 60))
            } else {
                (
                    Duration::from_secs((scheduled.settings.suspend_after_idle_min * 60) as u64),
                    Duration::from_secs((scheduled.settings.suspend_min * 60) as u64),
                )
            };
            let interval = match (phase, sleep_eligible) {
                (PollPhase::Driving, _) => {
                    scheduled.last_used = now;
                    scheduled.suspended = false;
                    self.cadence.driving
                }
                (PollPhase::Charging, _) => {
                    scheduled.last_used = now;
                    scheduled.suspended = false;
                    self.cadence.charging
                }
                (PollPhase::Updating, _) => {
                    scheduled.last_used = now;
                    scheduled.suspended = false;
                    self.cadence.updating
                }
                (PollPhase::Online, false) => {
                    scheduled.last_used = now;
                    scheduled.suspended = false;
                    self.cadence.online
                }
                (PollPhase::Online, true)
                    if now.saturating_duration_since(scheduled.last_used) >= idle_suspend_after =>
                {
                    scheduled.suspended = true;
                    suspended_interval
                }
                (PollPhase::Online, true) => {
                    scheduled.suspended = false;
                    self.cadence.online
                }
            };
            scheduled.next_poll = now + interval;
            scheduled.failure_backoff = interval;
            scheduled.last_phase = phase;
            if scheduled.suspended != was_suspended {
                let mut event = scheduled.vehicle.clone();
                event.state = if scheduled.suspended {
                    "suspended".to_owned()
                } else {
                    "online".to_owned()
                };
                return Some(event);
            }
        }
        None
    }

    fn vehicle_failed(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            let delay = scheduled.failure_backoff;
            scheduled.next_poll = now + delay;
            scheduled.failure_backoff = scheduled
                .failure_backoff
                .saturating_mul(2)
                .min(self.cadence.maximum_backoff);
        }
    }

    fn vehicle_failed_for_error(&mut self, id: VehicleId, error: &CollectorError, now: Instant) {
        if matches!(
            error,
            CollectorError::LegacyAuthManager(_)
                | CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(_))
        ) {
            self.vehicle_retry_after(id, LEGACY_REFRESH_RETRY, now);
            return;
        }
        let Some(error) = owner_api_error(error) else {
            self.vehicle_failed(id, now);
            return;
        };
        match error {
            OwnerApiError::RateLimited {
                retry_after_seconds,
            } => self.vehicle_rate_limited(id, *retry_after_seconds, now),
            OwnerApiError::VehicleNotFound => self.vehicle_not_found(id, now),
            OwnerApiError::RequestTimeout
            | OwnerApiError::VehicleInService
            | OwnerApiError::HttpStatus(401) => self.vehicle_failed(id, now),
            OwnerApiError::LegacyAuth => self.vehicle_retry_after(id, LEGACY_REFRESH_RETRY, now),
            _ => self.vehicle_api_error(id, now),
        }
    }

    fn vehicle_retry_after(&mut self, id: VehicleId, delay: Duration, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            scheduled.next_poll = now + delay;
        }
    }

    fn vehicle_rate_limited(&mut self, id: VehicleId, seconds: u64, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            scheduled.next_poll = retry_deadline(now, seconds);
        }
    }

    fn vehicle_api_error(&mut self, id: VehicleId, now: Instant) {
        let state = self.vehicle_fuses.entry(id).or_default();
        state.api_errors.retain(|at| now.saturating_duration_since(*at) < API_ERROR_WINDOW);
        state.api_errors.push(now);
        if state.api_errors.len() >= API_ERROR_LIMIT {
            state.api_blown_until = now.checked_add(API_ERROR_RESET);
        }
        let delay = self
            .vehicles
            .get(&id)
            .map(generic_api_retry_delay)
            .unwrap_or(GENERIC_OTHER_RETRY);
        self.vehicle_retry_after(id, delay, now);
    }

    fn vehicle_not_found(&mut self, id: VehicleId, now: Instant) {
        let state = self.vehicle_fuses.entry(id).or_default();
        state.not_found.retain(|at| {
            now.saturating_duration_since(*at) < VEHICLE_NOT_FOUND_WINDOW
        });
        state.not_found.push(now);
        state.api_errors.retain(|at| now.saturating_duration_since(*at) < API_ERROR_WINDOW);
        state.api_errors.push(now);
        if state.not_found.len() >= VEHICLE_NOT_FOUND_LIMIT {
            state.not_found_blown_until = now.checked_add(VEHICLE_NOT_FOUND_RESET);
        }
        if state.api_errors.len() >= API_ERROR_LIMIT {
            state.api_blown_until = now.checked_add(API_ERROR_RESET);
        }
        self.vehicle_failed(id, now);
    }

    fn vehicle_fuse_healthy(&mut self, id: VehicleId, now: Instant) -> bool {
        let Some(state) = self.vehicle_fuses.get_mut(&id) else {
            return true;
        };
        if state.api_blown_until.is_some_and(|until| now >= until) {
            state.api_blown_until = None;
            state.api_errors.clear();
        }
        if state
            .not_found_blown_until
            .is_some_and(|until| now >= until)
        {
            state.not_found_blown_until = None;
            state.not_found.clear();
        }
        state.api_blown_until.is_none() && state.not_found_blown_until.is_none()
    }

    fn due_service_vehicles(&self, now: Instant) -> Vec<VehicleId> {
        self.vehicles
            .values()
            .filter(|scheduled| {
                scheduled.vehicle.is_online()
                    && scheduled.settings.enabled
                    && scheduled.service_mode
                    && now >= scheduled.next_poll
            })
            .map(|scheduled| scheduled.vehicle.id)
            .collect()
    }

    fn enter_service_mode(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            scheduled.service_mode = true;
            scheduled.stream_healthy = false;
            scheduled.suspended = false;
            scheduled.pre_online = PreOnlineCheck::Idle;
            scheduled.failure_backoff = self.cadence.online;
            scheduled.next_poll = now + self.cadence.online;
        }
    }

    fn service_retry(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            scheduled.service_mode = true;
            scheduled.next_poll = now + self.cadence.online;
            scheduled.failure_backoff = self.cadence.online;
        }
    }

    fn service_exited(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id) {
            scheduled.service_mode = false;
            scheduled.stream_healthy = false;
            scheduled.suspended = false;
            scheduled.next_poll = now;
            scheduled.failure_backoff = self.cadence.online;
            scheduled.pre_online = if scheduled.settings.use_streaming_api {
                PreOnlineCheck::Probing {
                    deadline: now + PRE_ONLINE_TIMEOUT,
                }
            } else {
                PreOnlineCheck::Idle
            };
        }
    }

    fn stream_healthy(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id)
            && scheduled.settings.use_streaming_api
        {
            scheduled.stream_healthy = true;
            scheduled.suspended = false;
            if !matches!(
                scheduled.pre_online,
                PreOnlineCheck::Probing { .. } | PreOnlineCheck::ConfirmedFake { .. }
            ) {
                scheduled.next_poll = scheduled.next_poll.min(now);
            }
        }
    }

    fn stream_unhealthy(&mut self, id: VehicleId, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id)
            && scheduled.settings.use_streaming_api
        {
            scheduled.stream_healthy = false;
            scheduled.suspended = false;
            if matches!(scheduled.pre_online, PreOnlineCheck::ConfirmedReal) {
                scheduled.pre_online = PreOnlineCheck::Probing {
                    deadline: now + PRE_ONLINE_TIMEOUT,
                };
            }
            if !matches!(
                scheduled.pre_online,
                PreOnlineCheck::Probing { .. } | PreOnlineCheck::ConfirmedFake { .. }
            ) {
                scheduled.next_poll = now;
            }
        }
    }

    fn pre_online_power(&mut self, id: VehicleId, power: Option<i64>, now: Instant) {
        if let Some(scheduled) = self.vehicles.get_mut(&id)
            && matches!(scheduled.pre_online, PreOnlineCheck::Probing { .. })
        {
            observe_pre_online_power(&mut scheduled.pre_online, power, now);
            match scheduled.pre_online {
                PreOnlineCheck::ConfirmedReal => scheduled.next_poll = now,
                PreOnlineCheck::ConfirmedFake { deadline } => {
                    scheduled.next_poll = deadline;
                }
                PreOnlineCheck::Idle
                | PreOnlineCheck::Probing { .. } => {}
            }
        }
    }

    fn should_start_stream(&self, id: VehicleId) -> bool {
        self.vehicles.get(&id).is_some_and(|scheduled| {
            scheduled.settings.use_streaming_api
                && matches!(
                    scheduled.pre_online,
                    PreOnlineCheck::Probing { .. }
                        | PreOnlineCheck::ConfirmedFake { .. }
                        | PreOnlineCheck::ConfirmedReal
                )
        })
    }

    fn vehicles(&self) -> Vec<Vehicle> {
        self.vehicles
            .values()
            .map(|scheduled| scheduled.vehicle.clone())
            .collect()
    }

    fn delay_until_next_action(&self, now: Instant) -> Duration {
        let next_offline_state = self
            .vehicles
            .values()
            .filter_map(|scheduled| scheduled.offline_state_fetch_due)
            .min();
        let next_vehicle = self
            .vehicles
            .values()
            .filter(|scheduled| scheduled.vehicle.is_online() && scheduled.settings.enabled)
            .map(|scheduled| match scheduled.pre_online {
                PreOnlineCheck::Probing { deadline } => deadline,
                _ => scheduled.next_poll,
            })
            .min();
        next_vehicle
            .into_iter()
            .chain(next_offline_state)
            .min()
            .unwrap_or(self.next_discovery)
            .min(self.next_discovery)
            .saturating_duration_since(now)
    }
}

async fn persist_discovery_events(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
) -> Result<(), CollectorError> {
    let _publication_gate = store.acquire_publication_gate().await?;
    let observed_at_ms = current_epoch_millis()?;
    let source = store.register_source(
        &SourceDescriptor::new(OWNER_API_SOURCE_KIND, OWNER_API_SOURCE_KEY),
        observed_at_ms,
    )?;
    for vehicle in vehicles {
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string())
            .with_tesla_identity(Some(vehicle.id.get() as i64), None);
        descriptor.vin = clean_optional_text(Some(&vehicle.vin));
        descriptor.display_name = clean_optional_text(vehicle.display_name.as_deref());
        let registered = store.register_vehicle(&descriptor, observed_at_ms)?;
        let mut live_settings = vehicle.settings.clone();
        live_settings.suspend_min_resolved = false;
        store.upsert_car_settings(
            registered.vehicle_id,
            compatibility_car_id(vehicle.id.get()),
            &live_settings,
        )?;
        store.accept_owner_observation_and_lifecycle(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: registered.vehicle_id,
                observed_at_ms,
                payload: discovery_payload(vehicle),
            },
            observed_at_ms,
            compatibility_car_id(vehicle.id.get()),
        )?;
    }
    let collection = ManualCollection {
        vehicles: vehicles.to_vec(),
        snapshots: Vec::new(),
        failures: Vec::new(),
    };
    publish_compatibility_snapshots(store, cursor_key, &collection, observed_at_ms)?;
    Ok(())
}

fn discovery_payload(vehicle: &Vehicle) -> Value {
    serde_json::json!({
        "record_type": "owner_api_discovery_v1",
        "source_vehicle_id": vehicle.id.get().to_string(),
        "source_vehicle_state": vehicle.state,
    })
}

fn poll_phase(snapshot: &VehicleData) -> PollPhase {
    let fields = snapshot.fields();
    let updating = fields
        .get("vehicle_state")
        .and_then(Value::as_object)
        .and_then(|vehicle| vehicle.get("software_update"))
        .and_then(Value::as_object)
        .and_then(|update| update.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| status.eq_ignore_ascii_case("installing"));
    if updating {
        return PollPhase::Updating;
    }
    let charging = fields
        .get("charge_state")
        .and_then(Value::as_object)
        .and_then(|charge| charge.get("charging_state"))
        .and_then(Value::as_str)
        .is_some_and(|state| {
            matches!(state.to_ascii_lowercase().as_str(), "starting" | "charging")
        });
    if charging {
        return PollPhase::Charging;
    }
    let drive = fields.get("drive_state").and_then(Value::as_object);
    let shift = drive
        .and_then(|drive| drive.get("shift_state"))
        .and_then(Value::as_str);
    let speed = drive
        .and_then(|drive| drive.get("speed"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if matches!(shift, Some("D" | "R" | "N" | "d" | "r" | "n")) || speed > 0 {
        PollPhase::Driving
    } else {
        PollPhase::Online
    }
}

#[cfg(test)]
fn sleep_eligible(snapshot: &VehicleData) -> bool {
    sleep_eligible_with_policy(snapshot, true)
}

fn sleep_eligible_with_policy(snapshot: &VehicleData, req_not_unlocked: bool) -> bool {
    let fields = snapshot.fields();
    let Some(drive) = fields.get("drive_state").and_then(Value::as_object) else {
        return false;
    };
    let Some(climate) = fields.get("climate_state").and_then(Value::as_object) else {
        return false;
    };
    let Some(vehicle) = fields.get("vehicle_state").and_then(Value::as_object) else {
        return false;
    };
    if poll_phase(snapshot) != PollPhase::Online {
        return false;
    }
    let true_field = |fields: &Map<String, Value>, name: &str| {
        fields.get(name).and_then(Value::as_bool) == Some(true)
    };
    if true_field(vehicle, "is_user_present")
        || true_field(vehicle, "sentry_mode")
        || (req_not_unlocked && vehicle.get("locked").and_then(Value::as_bool) != Some(true))
        || true_field(climate, "is_preconditioning")
        || climate
            .get("climate_keeper_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("dog"))
        || drive.get("power").and_then(Value::as_i64).unwrap_or(0) > 0
    {
        return false;
    }
    for field in ["df", "pf", "dr", "pr", "ft", "rt"] {
        if vehicle.get(field).and_then(Value::as_i64).unwrap_or(0) > 0 {
            return false;
        }
    }
    if vehicle
        .get("software_update")
        .and_then(Value::as_object)
        .is_some_and(|update| {
            update
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status.eq_ignore_ascii_case("downloading"))
                && update
                    .get("download_perc")
                    .and_then(Value::as_f64)
                    .is_none_or(|percent| percent < 100.0)
        })
    {
        return false;
    }
    true
}

fn snapshot_service_mode(snapshot: &VehicleData) -> Option<bool> {
    snapshot
        .fields()
        .get("vehicle_state")
        .and_then(Value::as_object)
        .and_then(|state| state.get("service_mode"))
        .and_then(Value::as_bool)
}

/// Persist one completed compatibility collection. The supplied receipt time
/// makes storage tests deterministic; production obtains it from the system
/// clock only after the HTTP read succeeds.
pub fn persist_collection(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
) -> Result<ManualCollectionReport, CollectorError> {
    persist_collection_mode(store, collection, received_at_ms, false)
}

fn persist_collection_atomic(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
) -> Result<ManualCollectionReport, CollectorError> {
    persist_collection_mode(store, collection, received_at_ms, true)
}

fn persist_collection_mode(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
    atomic_lifecycle: bool,
) -> Result<ManualCollectionReport, CollectorError> {
    if received_at_ms < 0 {
        return Err(CollectorError::InvalidReceiptTimestamp);
    }

    let source = store.register_source(
        &SourceDescriptor::new(OWNER_API_SOURCE_KIND, OWNER_API_SOURCE_KEY),
        received_at_ms,
    )?;
    let mut vehicles = std::collections::BTreeMap::new();
    let mut online_vehicles_seen = 0;

    for vehicle in &collection.vehicles {
        if vehicle.is_online() {
            online_vehicles_seen += 1;
        }
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string())
            .with_tesla_identity(Some(vehicle.id.get() as i64), None);
        descriptor.vin = Some(vehicle.vin.clone());
        descriptor.display_name = vehicle.display_name.clone();
        let registered = store.register_vehicle(&descriptor, received_at_ms)?;
        vehicles.insert(vehicle.id.get(), registered.vehicle_id);
    }

    let mut observations_inserted = 0;
    let mut observations_already_present = 0;
    let mut lifecycle_report = LifecycleMaterialisationReport::default();
    let vehicle_states = collection
        .vehicles
        .iter()
        .map(|vehicle| (vehicle.id.get(), vehicle.state.as_str()))
        .collect::<std::collections::BTreeMap<_, _>>();
    for snapshot in &collection.snapshots {
        let source_vehicle_id = snapshot.vehicle_id().get();
        let vehicle_id = vehicles
            .get(&source_vehicle_id)
            .copied()
            .ok_or(CollectorError::SnapshotWithoutListedVehicle)?;
        let source_vehicle_state = vehicle_states
            .get(&source_vehicle_id)
            .copied()
            .ok_or(CollectorError::SnapshotWithoutListedVehicle)?;
        let input = ObservationInput {
            source_id: source.source_id,
            vehicle_id,
            observed_at_ms: observation_timestamp(snapshot, received_at_ms),
            payload: observation_payload(snapshot, source_vehicle_state),
        };
        let append = if atomic_lifecycle {
            let result = store.accept_owner_observation_and_lifecycle(
                &input,
                received_at_ms,
                compatibility_car_id(source_vehicle_id),
            )?;
            lifecycle_report.drives_closed += result.drives_closed;
            lifecycle_report.charges_closed += result.charges_closed;
            lifecycle_report.positions_materialised += result.positions_materialised;
            lifecycle_report.charge_samples_materialised += result.charge_samples_materialised;
            lifecycle_report.lifecycle_quarantines += usize::from(result.lifecycle_quarantined);
            result.append
        } else {
            store.append_observation(&input, received_at_ms)?
        };
        if append.inserted {
            observations_inserted += 1;
        } else {
            observations_already_present += 1;
        }
    }

    Ok(ManualCollectionReport {
        source_id: source.source_id,
        request_audit_correlation_id: Uuid::nil(),
        vehicles_seen: collection.vehicles.len(),
        online_vehicles_seen,
        snapshots_received: collection.snapshots.len(),
        observations_inserted,
        observations_already_present,
        snapshots_published: 0,
        vehicle_failures: collection.failures.len(),
        drives_closed: lifecycle_report.drives_closed,
        charges_closed: lifecycle_report.charges_closed,
        positions_materialised: lifecycle_report.positions_materialised,
        charge_samples_materialised: lifecycle_report.charge_samples_materialised,
        lifecycle_quarantines: lifecycle_report.lifecycle_quarantines,
    })
}

#[derive(Debug, Default)]
pub struct LifecycleMaterialisationReport {
    pub drives_closed: usize,
    pub charges_closed: usize,
    pub positions_materialised: usize,
    pub charge_samples_materialised: usize,
    pub lifecycle_quarantines: usize,
}

/// Project newly stored observations into durable drive/charge history and
/// crash-safe open-session state. Pure projection lives in `lifecycle`; this
/// function only loads the cursor, applies samples, and commits the delta.
pub fn materialise_lifecycle_for_collection(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
) -> Result<LifecycleMaterialisationReport, CollectorError> {
    let source = store.register_source(
        &SourceDescriptor::new(OWNER_API_SOURCE_KIND, OWNER_API_SOURCE_KEY),
        received_at_ms,
    )?;
    let mut report = LifecycleMaterialisationReport::default();
    for vehicle in &collection.vehicles {
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string())
            .with_tesla_identity(Some(vehicle.id.get() as i64), None);
        descriptor.vin = clean_optional_text(Some(&vehicle.vin));
        descriptor.display_name = clean_optional_text(vehicle.display_name.as_deref());
        let registered = store.register_vehicle(&descriptor, received_at_ms)?;
        let car_id = compatibility_car_id(vehicle.id.get());
        let mut live_settings = vehicle.settings.clone();
        live_settings.suspend_min_resolved = false;
        store.upsert_car_settings(registered.vehicle_id, car_id, &live_settings)?;
        let latest_snapshot = collection
            .snapshots
            .iter()
            .find(|snapshot| snapshot.vehicle_id() == vehicle.id);
        let seed_car = compatibility_car(vehicle, latest_snapshot, car_id);
        store.persist_materialised_car_if_absent(registered.vehicle_id, &seed_car)?;
        let materialised =
            materialise_vehicle_lifecycle(store, registered.vehicle_id, car_id, received_at_ms)?;
        let state = store
            .load_lifecycle_state(registered.vehicle_id)?
            .and_then(|record| OpenSessionState::decode(&record.open_session_json).ok());
        if let Some(metadata) = state.and_then(|state| state.car_metadata) {
            store.resolve_car_suspend_min(
                registered.vehicle_id,
                metadata.model.as_deref(),
                metadata.trim_badging.as_deref(),
                metadata.marketing_name.as_deref(),
            )?;
        }
        report.drives_closed += materialised.drives_closed;
        report.charges_closed += materialised.charges_closed;
        report.positions_materialised += materialised.positions_materialised;
        report.charge_samples_materialised += materialised.charge_samples_materialised;
        report.lifecycle_quarantines += materialised.lifecycle_quarantines;
    }
    Ok(report)
}

fn materialise_vehicle_lifecycle(
    store: &HubStore,
    vehicle_id: Uuid,
    car_id: i64,
    received_at_ms: i64,
) -> Result<LifecycleMaterialisationReport, CollectorError> {
    let existing = store.load_lifecycle_state(vehicle_id)?;
    let mut state = match existing.as_ref() {
        Some(record) => match OpenSessionState::decode(&record.open_session_json) {
            Ok(state) => state,
            Err(_) => {
                // Corrupt open state is quarantined and rebuilt from a clean
                // cursor so prior completed history remains untouched.
                let mut clean = OpenSessionState::new();
                clean.last_observation_id = record.last_observation_id;
                clean
            }
        },
        None => OpenSessionState::new(),
    };
    store.restore_lifecycle_open_children(vehicle_id, &mut state)?;

    let observations = store.observations_after_id_for_vehicle(
        vehicle_id,
        state.last_observation_id,
        crate::db::MAX_OBSERVATION_QUERY_LIMIT,
    )?;

    let mut report = LifecycleMaterialisationReport::default();
    let mut total_delta = crate::lifecycle::LifecycleDelta::default();
    let mut quarantined = existing.as_ref().is_some_and(|record| record.quarantined);

    for observation in observations {
        let sample = LifecycleSample {
            observation_id: observation.observation_id,
            observed_at_ms: observation.observed_at_ms,
            vehicle_state: observation_vehicle_state(&observation.payload),
            payload: observation.payload,
        };
        let step = apply_sample(state, car_id, &sample)?;
        state = step.state;
        quarantined |= step.quarantined;
        if step.quarantined {
            report.lifecycle_quarantines += 1;
        }
        report.drives_closed += step.delta.drives.len();
        report.charges_closed += step.delta.charges.len();
        report.positions_materialised += step.delta.positions.len();
        report.charge_samples_materialised += step.delta.charge_samples.len();
        total_delta.drives.extend(step.delta.drives);
        total_delta.positions.extend(step.delta.positions);
        total_delta.charges.extend(step.delta.charges);
        total_delta.charge_samples.extend(step.delta.charge_samples);
        total_delta.states.extend(step.delta.states);
        total_delta.updates.extend(step.delta.updates);
        total_delta
            .open_drive_positions
            .extend(step.delta.open_drive_positions);
        total_delta
            .open_charge_samples
            .extend(step.delta.open_charge_samples);
    }

    if let Some(open) = state.open_drive.as_mut() {
        open.positions.clear();
    }
    if let Some(open) = state.open_charge.as_mut() {
        open.samples.clear();
    }
    let encoded = state.encode().map_err(CollectorError::Lifecycle)?;
    store.commit_lifecycle_delta(&crate::db::LifecycleCommit {
        vehicle_id,
        car_id,
        open_session_json: &encoded,
        last_observation_id: state.last_observation_id,
        quarantined,
        updated_at_ms: received_at_ms,
        delta: &total_delta,
    })?;
    Ok(report)
}

fn force_close_vehicle_for_service(
    store: &HubStore,
    source_vehicle_id: VehicleId,
    closed_at_ms: i64,
) -> Result<(), CollectorError> {
    let source = store.register_source(
        &SourceDescriptor::new(OWNER_API_SOURCE_KIND, OWNER_API_SOURCE_KEY),
        closed_at_ms,
    )?;
    let registered = store.register_vehicle(
        &VehicleDescriptor::new(source.source_id, source_vehicle_id.get().to_string())
            .with_tesla_identity(Some(source_vehicle_id.get() as i64), None),
        closed_at_ms,
    )?;
    let existing = store.load_lifecycle_state(registered.vehicle_id)?;
    let state = match existing.as_ref() {
        Some(record) => OpenSessionState::decode(&record.open_session_json)
            .map_err(CollectorError::Lifecycle)?,
        None => OpenSessionState::new(),
    };
    let step = force_close_for_service(
        state,
        compatibility_car_id(source_vehicle_id.get()),
        closed_at_ms,
    )?;
    let encoded = step.state.encode().map_err(CollectorError::Lifecycle)?;
    store.commit_lifecycle_delta(&crate::db::LifecycleCommit {
        vehicle_id: registered.vehicle_id,
        car_id: compatibility_car_id(source_vehicle_id.get()),
        open_session_json: &encoded,
        last_observation_id: step.state.last_observation_id,
        quarantined: existing.as_ref().is_some_and(|record| record.quarantined),
        updated_at_ms: closed_at_ms,
        delta: &step.delta,
    })?;
    Ok(())
}

/// Publish a typed first-party mirror for every discovered owner vehicle.
/// Completed drive, position, charge, and charge-sample rows come only from
/// the materialised lifecycle store — never fabricated from a single sample.
fn publish_compatibility_snapshots(
    store: &HubStore,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
    published_at_ms: i64,
) -> Result<usize, CollectorError> {
    let source = store.register_source(
        &SourceDescriptor::new(OWNER_API_SOURCE_KIND, OWNER_API_SOURCE_KEY),
        published_at_ms,
    )?;
    let installation_id = store.installation_id()?;
    let snapshots: HashMap<u64, &VehicleData> = collection
        .snapshots
        .iter()
        .map(|snapshot| (snapshot.vehicle_id().get(), snapshot))
        .collect();
    let writer = ProjectionPackWriter::new(store.packs_dir());

    let mut published = 0;
    for vehicle in &collection.vehicles {
        let source_vehicle_id = vehicle.id.get();
        let mut descriptor =
            VehicleDescriptor::new(source.source_id, source_vehicle_id.to_string())
                .with_tesla_identity(Some(source_vehicle_id as i64), None);
        descriptor.vin = clean_optional_text(Some(&vehicle.vin));
        descriptor.display_name = clean_optional_text(vehicle.display_name.as_deref());
        let registered = store.register_vehicle(&descriptor, published_at_ms)?;
        let selected_car_id = compatibility_car_id(source_vehicle_id);
        store.upsert_car_settings(registered.vehicle_id, selected_car_id, &vehicle.settings)?;
        if store.vehicle_has_v2_base(registered.vehicle_id)? {
            if let Some(sync_claim) =
                store.claim_sync_mutations(registered.vehicle_id, published_at_ms, 10_000)?
            {
                if let Err(error) = publish_v2_delta(store, cursor_key, &sync_claim) {
                    store.release_sync_mutations(&sync_claim)?;
                    return Err(error);
                }
            }
            continue;
        }
        let history = store.materialised_history(registered.vehicle_id)?;
        let durable_car = match history.car.clone() {
            Some(car) => car,
            None => {
                let car = compatibility_car(
                    vehicle,
                    snapshots.get(&source_vehicle_id).copied(),
                    selected_car_id,
                );
                store.persist_materialised_car_if_absent(registered.vehicle_id, &car)?;
                car
            }
        };
        let states = history.states.clone();
        let updates = history.updates.clone();
        let snapshot = ProjectionSnapshot {
            cars: vec![durable_car],
            drives: history.drives,
            positions: history.positions,
            charges: history.charges,
            charge_samples: history.charge_samples,
        };
        let fingerprint = Sha256Digest::from_bytes(
            Sha256::digest(
                serde_json::to_vec(&(&snapshot, &states, &updates))
                    .map_err(CollectorError::SerializeSnapshot)?,
            )
            .into(),
        );
        let publication_gate = store.try_acquire_publication_gate()?;
        if store.snapshot_fingerprint_is_current(registered.vehicle_id, fingerprint)? {
            continue;
        }
        let sequence = store.reserve_next_full_snapshot_sequence(
            &publication_gate,
            registered.vehicle_id,
        )?;
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: ProjectionBinding {
                installation_id,
                account_id: source.source_id,
                vehicle_id: registered.vehicle_id,
                generation: source.generation,
                selected_car_id,
            },
            sequence: SequenceRange {
                from_exclusive: sequence,
                to_inclusive: sequence,
            },
            snapshot: &snapshot,
        };
        let built =
            writer.write_full_snapshot_with_states_and_updates(&request, &states, &updates)?;
        let manifest = request
            .signed_manifest_with_states_and_updates(&built, &states, &updates, cursor_key)?;
        store.finalize_import_snapshot(&manifest, fingerprint, &[])?;
        published += 1;
    }
    Ok(published)
}

fn compatibility_car(
    vehicle: &crate::owner_api::Vehicle,
    snapshot: Option<&VehicleData>,
    selected_car_id: i64,
) -> ProjectionCar {
    let raw_car_type =
        snapshot.and_then(|snapshot| nested_text(snapshot, "vehicle_config", "car_type"));
    let model = raw_car_type
        .as_deref()
        .map(crate::hub_pack::normalize_tesla_model_code)
        .unwrap_or_else(|| "Tesla".to_owned());
    let trim_badging = snapshot
        .and_then(|snapshot| nested_text(snapshot, "vehicle_config", "trim_badging"))
        .map(|value| crate::hub_pack::normalize_tesla_trim(&value));
    ProjectionCar {
        id: selected_car_id,
        name: clean_required_text(vehicle.display_name.as_deref(), "Tesla"),
        model: model.clone(),
        vin: clean_optional_text(Some(&vehicle.vin)),
        source_eid: Some(vehicle.id.get() as i64),
        source_vid: None,
        trim_badging: trim_badging.clone(),
        marketing_name: crate::hub_pack::derive_tesla_marketing_name(
            &model,
            trim_badging.as_deref(),
            raw_car_type.as_deref(),
        ),
        exterior_color: snapshot
            .and_then(|snapshot| nested_text(snapshot, "vehicle_config", "exterior_color"))
            .map(ToOwned::to_owned),
        wheel_type: snapshot
            .and_then(|snapshot| nested_text(snapshot, "vehicle_config", "wheel_type"))
            .map(ToOwned::to_owned),
        spoiler_type: snapshot
            .and_then(|snapshot| nested_text(snapshot, "vehicle_config", "spoiler_type"))
            .map(ToOwned::to_owned),
        firmware_version: clean_optional_text(
            snapshot.and_then(|snapshot| nested_text(snapshot, "vehicle_state", "car_version")),
        ),
        efficiency_wh_per_km: None,
        settings: vehicle.settings.clone(),
    }
}

fn compatibility_car_id(source_vehicle_id: u64) -> i64 {
    // The pack contract uses a positive signed local car ID. This is only an
    // in-pack foreign key; the durable Hub identity is the registered UUID.
    i64::try_from(source_vehicle_id).expect("owner API admission bounds vehicle IDs")
}

fn nested_text<'a>(snapshot: &'a VehicleData, group: &str, field: &str) -> Option<&'a str> {
    snapshot
        .fields()
        .get(group)
        .and_then(Value::as_object)
        .and_then(|fields| fields.get(field))
        .and_then(Value::as_str)
}

fn clean_required_text(value: Option<&str>, fallback: &str) -> String {
    clean_optional_text(value).unwrap_or_else(|| fallback.to_owned())
}

fn clean_optional_text(value: Option<&str>) -> Option<String> {
    const MAX_COMPATIBILITY_TEXT_BYTES: usize = 512;
    let value = value?.trim();
    (!value.is_empty()
        && value.len() <= MAX_COMPATIBILITY_TEXT_BYTES
        && !value.chars().any(char::is_control))
    .then(|| value.to_owned())
}

fn observation_payload(snapshot: &VehicleData, source_vehicle_state: &str) -> Value {
    let mut payload = Map::new();
    payload.insert(
        "record_type".to_owned(),
        Value::String("owner_api_vehicle_data_v1".to_owned()),
    );
    payload.insert(
        "source_vehicle_id".to_owned(),
        Value::String(snapshot.vehicle_id().get().to_string()),
    );
    payload.insert(
        "source_vehicle_state".to_owned(),
        Value::String(source_vehicle_state.to_owned()),
    );
    payload.insert(
        "vehicle_data".to_owned(),
        Value::Object(snapshot.fields().clone()),
    );
    Value::Object(payload)
}

fn observation_vehicle_state(payload: &Value) -> String {
    payload
        .get("source_vehicle_state")
        .and_then(Value::as_str)
        .filter(|state| {
            !state.is_empty() && state.len() <= 64 && !state.chars().any(char::is_control)
        })
        .unwrap_or("unknown")
        .to_owned()
}

fn observation_timestamp(snapshot: &VehicleData, received_at_ms: i64) -> i64 {
    let fields = snapshot.fields();
    let candidates = [
        fields
            .get("drive_state")
            .and_then(Value::as_object)
            .and_then(|drive_state| drive_state.get("timestamp")),
        fields.get("timestamp"),
    ];
    let maximum = received_at_ms.saturating_add(FUTURE_TIMESTAMP_SKEW_MS);
    candidates
        .into_iter()
        .flatten()
        .filter_map(Value::as_i64)
        .find(|timestamp| {
            (*timestamp >= EARLIEST_PLAUSIBLE_TIMESTAMP_MS) && (*timestamp <= maximum)
        })
        .unwrap_or(received_at_ms)
}

fn current_epoch_millis() -> Result<i64, CollectorError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CollectorError::SystemClockBeforeEpoch)?;
    i64::try_from(duration.as_millis()).map_err(|_| CollectorError::SystemClockOutOfRange)
}

#[derive(Debug, Error)]
pub enum CollectorError {
    #[error("manual collection requires a systemd credential directory")]
    MissingCredentialDirectory,
    #[error("manual collection receipt timestamp is invalid")]
    InvalidReceiptTimestamp,
    #[error("manual collection received data for a vehicle absent from discovery")]
    SnapshotWithoutListedVehicle,
    #[error("system clock is before the Unix epoch")]
    SystemClockBeforeEpoch,
    #[error("system clock is outside the supported timestamp range")]
    SystemClockOutOfRange,
    #[error("cannot serialize compatibility snapshot")]
    SerializeSnapshot(serde_json::Error),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Credential(#[from] CredentialError),
    #[error(transparent)]
    OwnerApiConfig(#[from] OwnerApiConfigError),
    #[error(transparent)]
    OwnerApi(#[from] OwnerApiError),
    #[error(transparent)]
    OwnerApiAuth(#[from] OwnerApiAuthError),
    #[error(transparent)]
    LegacyAuthManager(#[from] LegacyAuthManagerError),
    #[error(transparent)]
    Projection(#[from] ProjectionPackError),
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),
    #[error(transparent)]
    Geocoder(#[from] GeocoderError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::{get, post},
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    use super::*;
    use crate::{
        credentials::LegacyAuthManager,
        lifecycle::OpenSessionState,
        owner_api::{Vehicle, VehicleData},
    };

    #[test]
    fn persists_a_collected_snapshot_and_retries_without_duplication() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let received_at_ms = 1_800_000_000_000;
        let collection = ManualCollection {
            vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({"drive_state": {"timestamp": received_at_ms - 1}}),
            )],
            failures: vec![],
        };

        let first =
            persist_collection(&store, &collection, received_at_ms).expect("first collection");
        let second =
            persist_collection(&store, &collection, received_at_ms).expect("retry collection");

        assert_eq!(first.observations_inserted, 1);
        assert_eq!(second.observations_inserted, 0);
        assert_eq!(second.observations_already_present, 1);
        let vehicle_id = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("stored UUID");
        let observations = store
            .observations_for_vehicle(vehicle_id, crate::db::ObservationQuery::from_start(1))
            .expect("stored observation");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].observed_at_ms, received_at_ms - 1);
        assert_eq!(
            observations[0].payload["record_type"],
            "owner_api_vehicle_data_v1"
        );
        assert_eq!(observations[0].payload["source_vehicle_state"], "online");
    }

    #[test]
    fn invalid_or_future_source_times_fall_back_to_receipt_time() {
        let received_at_ms = 1_800_000_000_000;
        for timestamp in [1_i64, received_at_ms + FUTURE_TIMESTAMP_SKEW_MS + 1] {
            let snapshot =
                VehicleData::for_test(9, json!({"drive_state": {"timestamp": timestamp}}));
            assert_eq!(
                observation_timestamp(&snapshot, received_at_ms),
                received_at_ms
            );
        }
    }

    #[derive(Clone)]
    struct LegacyRuntimeMock {
        unauthorized: Arc<AtomicUsize>,
        token_calls: Arc<AtomicUsize>,
        authorization: Arc<Mutex<Vec<String>>>,
    }

    async fn legacy_products(
        State(state): State<LegacyRuntimeMock>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        state.authorization.lock().unwrap().push(
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned(),
        );
        let was_unauthorized =
            state
                .unauthorized
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                    count.checked_sub(1)
                });
        if was_unauthorized.is_ok() {
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
        (
            StatusCode::OK,
            r#"{"response":[{"vehicle_id":9,"id":9,"vin":"5YJ3E1EA7KF000001","state":"online"}],"count":1}"#,
        )
            .into_response()
    }

    async fn legacy_token(
        State(state): State<LegacyRuntimeMock>,
        _body: String,
    ) -> impl IntoResponse {
        state.token_calls.fetch_add(1, Ordering::SeqCst);
        (
            StatusCode::OK,
            json!({
                "access_token": "rotated-access",
                "refresh_token": "rotated-refresh",
                "token_type": "Bearer",
                "expires_in": 1_000_000_000u64,
                "created_at": 1_800_000_000i64,
            })
            .to_string(),
        )
    }

    #[tokio::test]
    async fn legacy_collector_refresh_persists_then_stream_uses_rotated_access() {
        let state = LegacyRuntimeMock {
            unauthorized: Arc::new(AtomicUsize::new(1)),
            token_calls: Arc::new(AtomicUsize::new(0)),
            authorization: Arc::new(Mutex::new(Vec::new())),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = url::Url::parse(&format!("http://{address}/")).unwrap();
        let issuer = base.join("oauth2/v3/").unwrap();
        let server_state = state.clone();
        let http_server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/oauth2/v3/token", post(legacy_token))
                    .route("/api/1/products", get(legacy_products))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });

        let persisted = tempfile::tempdir().unwrap();
        let persisted_path = persisted.path().join("teslamate-owner-tokens");
        let persisted_for_callback = persisted_path.clone();
        let auth = crate::legacy_auth::LegacyAuth::for_test(
            issuer,
            "old-access-secret",
            "old-refresh-secret",
        );
        let manager = Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
            auth,
            Arc::new(move |access, refresh| {
                let encoded = STANDARD
                    .encode(json!({"access_token": access, "refresh_token": refresh}).to_string());
                CredentialDirectory::replace_encrypted_generation(
                    &persisted_for_callback,
                    encoded.as_bytes(),
                )
            }),
        )));
        let collection_auth = CollectionAuth::Legacy {
            manager: Arc::clone(&manager),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            region: StreamRegion::Global,
        };
        let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();
        let vehicles = list_vehicles_for_auth(&client, &collection_auth)
            .await
            .unwrap();
        assert_eq!(vehicles.len(), 1);
        assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.authorization.lock().unwrap().as_slice(),
            &["Bearer old-access-secret", "Bearer rotated-access"]
        );
        let stored = std::fs::read(&persisted_path).unwrap();
        assert!(
            !stored
                .windows("old-refresh-secret".len())
                .any(|window| window == b"old-refresh-secret")
        );
        assert!(
            !stored
                .windows("rotated-refresh".len())
                .any(|window| window == b"rotated-refresh")
        );

        let ws_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_endpoint = format!("ws://{}/streaming/", ws_listener.local_addr().unwrap());
        let ws_server = tokio::spawn(async move {
            let (tcp, _) = ws_listener.accept().await.unwrap();
            let mut socket = accept_async(tcp).await.unwrap();
            let message = socket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("stream subscribe must be text")
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(frame["token"], "rotated-access");
            socket
                .send(Message::Text(
                    r#"{"msg_type":"control:hello","code":200}"#.into(),
                ))
                .await
                .unwrap();
        });
        let (events, _receiver) = mpsc::channel(4);
        let supervisor = TeslaStreamSupervisor::new_legacy_auth(
            VehicleId::from_test(9),
            manager,
            StreamRegion::Global,
            ws_endpoint,
            client.http_client(),
            events,
        )
        .unwrap();
        let (stop, shutdown) = oneshot::channel();
        let task = tokio::spawn(supervisor.run(shutdown));
        ws_server.await.unwrap();
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        http_server.abort();
    }

    #[test]
    fn compatibility_collection_publishes_a_real_car_only_phone_snapshot() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let collected_at_ms = 1_800_000_000_000;
        let collection = ManualCollection {
            vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {"timestamp": collected_at_ms - 1},
                    "vehicle_config": {"car_type": "model3"},
                    "vehicle_state": {"car_version": "2026.20"}
                }),
            )],
            failures: vec![],
        };

        persist_collection(&store, &collection, collected_at_ms).expect("raw observation");
        materialise_lifecycle_for_collection(&store, &collection, collected_at_ms)
            .expect("lifecycle");
        let published = publish_compatibility_snapshots(
            &store,
            &CursorKey::from_bytes([7; 32]),
            &collection,
            collected_at_ms,
        )
        .expect("typed projection");

        assert_eq!(published, 1);
        let vehicle_id = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("stored UUID");
        let manifest = store
            .manifest_for_vehicle(vehicle_id)
            .expect("manifest query")
            .expect("published manifest");
        assert_eq!(manifest.chunk_count, 1);
        assert_eq!(manifest.total_rows, 2);
        assert_eq!(
            manifest.chunks[0].tables,
            vec![crate::protocol::MirrorTable::Car]
        );
        assert_eq!(store.published_vehicles().expect("published cars").len(), 1);
    }

    #[test]
    fn outbox_uses_sparse_delta_after_immutable_base_and_preserves_base_pack() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let cursor_key = CursorKey::from_bytes([17; 32]);
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
        let first_time = 1_800_000_000_000_i64;
        let first = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {
                        "shift_state": "D", "speed": 12, "latitude": 47.0,
                        "longitude": 19.0, "timestamp": first_time
                    },
                    "vehicle_config": {"car_type": "model3"},
                    "vehicle_state": {"car_version": "2026.20"}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &first, first_time).expect("first raw observation");
        materialise_lifecycle_for_collection(&store, &first, first_time)
            .expect("first lifecycle");
        publish_compatibility_snapshots(&store, &cursor_key, &first, first_time)
            .expect("base publication");
        let vehicle_id = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("UUID");
        let (base_digest, base_path): (String, _) = {
            let connection = store.open().expect("database");
            let digest: String = connection
                .query_row(
                    "SELECT base_digest FROM sync_bases WHERE vehicle_id = ?1",
                    rusqlite::params![vehicle_id.to_string()],
                    |row| row.get(0),
                )
                .expect("base digest");
            let path = store
                .packs_dir()
                .join("sha256")
                .join(format!("{digest}.sqlite.zst"));
            (digest, path)
        };
        let base_metadata = std::fs::metadata(&base_path).expect("base pack metadata");
        let base_modified = base_metadata.modified().expect("base pack mtime");
        let base_bytes = std::fs::read(&base_path).expect("base pack bytes");

        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(replay_export_outbox(&store, &cursor_key, std::slice::from_ref(&vehicle), first_time))
            .expect("clear base outbox");
        let second_time = first_time + 60_000;
        let second = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {
                        "shift_state": "P", "speed": 0, "latitude": 47.01,
                        "longitude": 19.01, "timestamp": second_time
                    },
                    "vehicle_config": {"car_type": null},
                    "vehicle_state": {"car_version": null}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &second, second_time).expect("second raw observation");
        materialise_lifecycle_for_collection(&store, &second, second_time)
            .expect("second lifecycle");
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(replay_export_outbox(&store, &cursor_key, std::slice::from_ref(&vehicle), second_time))
            .expect("delta publication");

        let connection = store.open().expect("database");
        let delta_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sync_deltas WHERE vehicle_id = ?1",
                rusqlite::params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("delta count");
        let unpublished: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sync_mutations
                 WHERE vehicle_id = ?1 AND published = 0",
                rusqlite::params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("pending mutations");
        assert_eq!(delta_count, 1);
        assert_eq!(unpublished, 0);
        drop(connection);
        assert_eq!(std::fs::read(&base_path).expect("base bytes"), base_bytes);
        assert_eq!(
            std::fs::metadata(&base_path)
                .expect("base metadata")
                .modified()
                .expect("base mtime"),
            base_modified
        );
        assert_eq!(
            store
                .open()
                .expect("database")
                .query_row(
                    "SELECT base_digest FROM sync_bases WHERE vehicle_id = ?1",
                    rusqlite::params![vehicle_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .expect("base digest after delta"),
            base_digest
        );
    }

    #[test]
    fn sparse_live_metadata_preserves_durable_car_and_new_pack_metadata_after_restart() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let vehicle = Vehicle::for_test(9, "5YJFULLVIN123456", "online");
        let full = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "display_name": "Road car",
                    "vin": "5YJFULLVIN123456",
                    "drive_state": {"shift_state":"D", "speed":20, "latitude":47.0, "longitude":19.0, "timestamp":1800000000000_i64},
                    "vehicle_config": {"car_type":"model3", "trim_badging":"74d", "exterior_color":"Pearl White"},
                    "vehicle_state": {"car_version":"2026.20"}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &full, 1_800_000_000_000).expect("persist full");
        materialise_lifecycle_for_collection(&store, &full, 1_800_000_000_000)
            .expect("materialise full");
        publish_compatibility_snapshots(&store, &CursorKey::from_bytes([11; 32]), &full, 1_800_000_000_000)
            .expect("publish full");
        let vehicle_id = store
            .open().expect("db")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| row.get::<_, String>(0))
            .expect("vehicle id").parse::<Uuid>().expect("uuid");
        let before = store.materialised_history(vehicle_id).expect("history").car.expect("car");

        let store = HubStore::initialize(temp.path()).expect("restart");
        let sparse = ManualCollection {
            vehicles: vec![vehicle],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {"shift_state":"D", "speed":21, "latitude":47.01, "longitude":19.01, "timestamp":1800000060000_i64},
                    "vehicle_config": {"car_type":null, "trim_badging":null, "exterior_color":null},
                    "vehicle_state": {"car_version":null}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &sparse, 1_800_000_060_000).expect("persist sparse");
        materialise_lifecycle_for_collection(&store, &sparse, 1_800_000_060_000)
            .expect("materialise sparse");
        publish_compatibility_snapshots(&store, &CursorKey::from_bytes([11; 32]), &sparse, 1_800_000_060_000)
            .expect("publish sparse");
        let after = store.materialised_history(vehicle_id).expect("history").car.expect("car");
        assert_eq!(before, after);
        let manifest = store.manifest_for_vehicle(vehicle_id).expect("manifest").expect("published");
        let pack = store.pack_for_digest(manifest.chunks[0].sha256).expect("pack").expect("pack file");
        let bytes = zstd::stream::decode_all(std::fs::File::open(pack.path).expect("pack open")).expect("decode");
        let inspect = temp.path().join("metadata.sqlite");
        std::fs::write(&inspect, bytes).expect("write inspect");
        let connection = rusqlite::Connection::open(inspect).expect("inspect");
        let packed: (String, String, Option<String>, Option<i64>, Option<String>, Option<String>) = connection
            .query_row("SELECT name, model, vin, source_eid, exterior_color, firmware_version FROM cars", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
            }).expect("packed car");
        assert_eq!(packed, (after.name, after.model, after.vin, after.source_eid, after.exterior_color, after.firmware_version));
    }

    #[test]
    fn live_publication_includes_v2_state_and_update_history() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
        let t0 = 1_800_000_000_000_i64;
        let t1 = t0 + 1_000;
        let t2 = t0 + 2_000;
        let t3 = t0 + 3_000;
        let collections = [
            (
                ManualCollection {
                    vehicles: vec![vehicle.clone()],
                    snapshots: vec![VehicleData::for_test(
                        9,
                        json!({
                            "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t0},
                            "vehicle_state": {"timestamp": t0, "car_version": "2026.1"}
                        }),
                    )],
                    failures: vec![],
                },
                t0,
            ),
            (
                ManualCollection {
                    vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "asleep")],
                    snapshots: vec![VehicleData::for_test(
                        9,
                        json!({"vehicle_state": {"timestamp": t1, "car_version": "2026.1"}}),
                    )],
                    failures: vec![],
                },
                t1,
            ),
            (
                ManualCollection {
                    vehicles: vec![vehicle.clone()],
                    snapshots: vec![VehicleData::for_test(
                        9,
                        json!({
                            "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t2},
                            "vehicle_state": {
                                "timestamp": t2,
                                "car_version": "2026.1",
                                "software_update": {"status": "installing"}
                            }
                        }),
                    )],
                    failures: vec![],
                },
                t2,
            ),
            (
                ManualCollection {
                    vehicles: vec![vehicle.clone()],
                    snapshots: vec![VehicleData::for_test(
                        9,
                        json!({
                            "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t3},
                            "vehicle_state": {
                                "timestamp": t3,
                                "car_version": "2026.20.1",
                                "software_update": {"status": ""}
                            }
                        }),
                    )],
                    failures: vec![],
                },
                t3,
            ),
        ];

        for (collection, received_at_ms) in &collections {
            persist_collection(&store, collection, *received_at_ms).expect("persist fixture");
            materialise_lifecycle_for_collection(&store, collection, *received_at_ms)
                .expect("materialise fixture");
        }

        publish_compatibility_snapshots(
            &store,
            &CursorKey::from_bytes([8; 32]),
            &collections[3].0,
            t3,
        )
        .expect("publish v2 projection");

        let vehicle_id = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("stored UUID");
        let manifest = store
            .manifest_for_vehicle(vehicle_id)
            .expect("manifest query")
            .expect("published manifest");
        assert_eq!(
            manifest.chunks[0].schema,
            crate::hub_pack::HUB_PROJECTION_SCHEMA_V2
        );

        let stored_pack = store
            .pack_for_digest(manifest.chunks[0].sha256)
            .expect("pack catalog")
            .expect("stored pack");
        let sqlite_bytes =
            zstd::stream::decode_all(std::fs::File::open(&stored_pack.path).expect("pack file"))
                .expect("decompress pack");
        let inspect_path = temp.path().join("inspect-v2.sqlite");
        std::fs::write(&inspect_path, sqlite_bytes).expect("write inspection copy");
        let connection = rusqlite::Connection::open(inspect_path).expect("inspect sqlite");
        let states: Vec<(i64, String, i64, Option<i64>)> = connection
            .prepare("SELECT id, state, start_date_ms, end_date_ms FROM states ORDER BY id")
            .expect("states query")
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .expect("states rows")
            .map(|row| row.expect("state row"))
            .collect();
        assert_eq!(
            states,
            vec![
                (1, "online".to_owned(), t0, Some(t1)),
                (2, "asleep".to_owned(), t1, Some(t2)),
                (3, "online".to_owned(), t2, None),
            ]
        );
        let update: (i64, i64, i64, String) = connection
            .query_row(
                "SELECT id, start_date_ms, end_date_ms, version FROM updates",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("update row");
        assert_eq!(update, (1, t2, t3, "2026.20.1".to_owned()));
    }

    #[test]
    fn synthetic_drive_and_charge_survive_mid_session_restart() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let t0 = 1_800_000_500_000_i64;
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");

        // Open a drive.
        let open_drive = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![
                VehicleData::for_test(
                    9,
                    json!({
                        "drive_state": {
                            "shift_state": "D",
                            "speed": 25,
                            "latitude": 47.0,
                            "longitude": 19.0,
                            "timestamp": t0
                        },
                        "vehicle_state": {"odometer": 1000.0},
                        "charge_state": {"battery_level": 70, "battery_range": 200.0}
                    }),
                ),
                VehicleData::for_test(
                    9,
                    json!({
                        "drive_state": {
                            "shift_state": "D",
                            "speed": 30,
                            "latitude": 47.01,
                            "longitude": 19.01,
                            "timestamp": t0 + 60_000
                        },
                        "vehicle_state": {"odometer": 1001.0},
                        "charge_state": {"battery_level": 69, "battery_range": 198.0}
                    }),
                ),
            ],
            failures: vec![],
        };
        persist_collection(&store, &open_drive, t0).expect("persist open");
        materialise_lifecycle_for_collection(&store, &open_drive, t0).expect("materialise open");

        let vehicle_id = store
            .open()
            .expect("db")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("id")
            .parse::<Uuid>()
            .expect("uuid");
        let open_state = store
            .load_lifecycle_state(vehicle_id)
            .expect("load")
            .expect("open state exists");
        let decoded = OpenSessionState::decode(&open_state.open_session_json).expect("decode");
        assert!(decoded.open_drive.is_some());

        // Simulate process restart: reopen store path and finish the drive.
        let store = HubStore::initialize(temp.path()).expect("reopen store");
        let close_drive = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {
                        "shift_state": "P",
                        "speed": 0,
                        "latitude": 47.01,
                        "longitude": 19.01,
                        "timestamp": t0 + 120_000
                    },
                    "charge_state": {"battery_level": 68, "battery_range": 195.0}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &close_drive, t0 + 120_000).expect("persist close");
        let lifecycle = materialise_lifecycle_for_collection(&store, &close_drive, t0 + 120_000)
            .expect("materialise close");
        assert_eq!(lifecycle.drives_closed, 1);
        assert_eq!(lifecycle.positions_materialised, 3);

        // Charge lifecycle on the same durable vehicle.
        let charge_open = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "charge_state": {
                        "charging_state": "Charging",
                        "battery_level": 40,
                        "charge_energy_added": 1.0,
                        "charger_power": 11.0,
                        "battery_range": 120.0
                    },
                    "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t0 + 200_000}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &charge_open, t0 + 200_000).expect("persist charge open");
        materialise_lifecycle_for_collection(&store, &charge_open, t0 + 200_000)
            .expect("materialise charge open");

        let store = HubStore::initialize(temp.path()).expect("second reopen");
        let charge_close = ManualCollection {
            vehicles: vec![vehicle],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "charge_state": {
                        "charging_state": "Complete",
                        "battery_level": 80,
                        "charge_energy_added": 12.0,
                        "charger_power": 0.0,
                        "battery_range": 220.0
                    },
                    "drive_state": {"shift_state": "P", "speed": 0, "timestamp": t0 + 800_000}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &charge_close, t0 + 800_000).expect("persist charge close");
        let lifecycle = materialise_lifecycle_for_collection(&store, &charge_close, t0 + 800_000)
            .expect("materialise charge close");
        assert_eq!(lifecycle.charges_closed, 1);
        assert!(lifecycle.charge_samples_materialised >= 1);

        let history = store.materialised_history(vehicle_id).expect("history");
        assert_eq!(history.drives.len(), 1);
        assert_eq!(history.charges.len(), 1);
        assert_eq!(history.charges[0].end_battery_level, Some(80));
        assert_eq!(history.charges[0].charge_energy_added, Some(11.0));

        publish_compatibility_snapshots(
            &store,
            &CursorKey::from_bytes([9; 32]),
            &charge_close,
            t0 + 800_000,
        )
        .expect("publish");
        let manifest = store
            .manifest_for_vehicle(vehicle_id)
            .expect("manifest")
            .expect("published");
        assert!(manifest.total_rows > 1);
    }

    fn test_cadence() -> CollectorCadence {
        CollectorCadence {
            driving: Duration::from_millis(2_500),
            charging: Duration::from_secs(5),
            online: Duration::from_secs(60),
            sleeping: Duration::from_secs(30),
            offline_drive_timeout: Duration::from_secs(15 * 60),
            idle_suspend_after: Duration::from_secs(15 * 60),
            suspended: Duration::from_secs(21 * 60),
            updating: Duration::from_secs(15),
            stream_health_timeout: Duration::from_secs(30),
            maximum_backoff: Duration::from_secs(900),
        }
    }

    #[test]
    fn classifies_teslamate_poll_phases() {
        let driving =
            VehicleData::for_test(1, json!({"drive_state":{"shift_state":"D","speed":1}}));
        let charging = VehicleData::for_test(
            1,
            json!({
                "drive_state":{"shift_state":"D","speed":1},
                "charge_state":{"charging_state":"Charging"}
            }),
        );
        let online = VehicleData::for_test(1, json!({"drive_state":{"shift_state":"P","speed":0}}));
        let updating = VehicleData::for_test(
            1,
            json!({
                "drive_state":{"shift_state":"D","speed":1},
                "charge_state":{"charging_state":"Charging"},
                "vehicle_state":{"software_update":{"status":"installing"}}
            }),
        );

        assert_eq!(poll_phase(&driving), PollPhase::Driving);
        assert_eq!(poll_phase(&charging), PollPhase::Charging);
        assert_eq!(poll_phase(&updating), PollPhase::Updating);
        assert_eq!(poll_phase(&online), PollPhase::Online);
    }

    #[test]
    fn scheduler_keeps_failure_backoff_per_vehicle() {
        let now = Instant::now();
        let first = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let second = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
        let first_id = first.id;
        let second_id = second.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![first, second], now);
        scheduler.pre_online_power(first_id, Some(0), now);
        scheduler.pre_online_power(second_id, Some(0), now);

        assert_eq!(scheduler.due_vehicles(now), vec![first_id, second_id]);
        scheduler.vehicle_failed(first_id, now);
        scheduler.vehicle_succeeded(second_id, PollPhase::Driving, false, now);

        assert_eq!(
            scheduler.due_vehicles(now + Duration::from_millis(2_500)),
            vec![second_id]
        );
        assert!(
            !scheduler
                .due_vehicles(now + Duration::from_secs(30))
                .contains(&first_id)
        );
    }

    #[test]
    fn sleeping_vehicle_gets_discovery_only() {
        let now = Instant::now();
        let asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![asleep], now);

        assert!(scheduler.due_vehicles(now).is_empty());
        assert_eq!(
            scheduler.delay_until_next_action(now),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn newly_online_vehicle_waits_for_pre_online_confirmation() {
        let now = Instant::now();
        let asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
        let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let online_id = online.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![asleep], now);
        scheduler.accept_discovery(vec![online], now + Duration::from_secs(30));

        assert!(
            scheduler
                .due_vehicles(now + Duration::from_secs(30))
                .is_empty()
        );
        scheduler.pre_online_power(online_id, Some(0), now + Duration::from_secs(31));
        assert_eq!(
            scheduler.due_vehicles(now + Duration::from_secs(31)),
            vec![online_id]
        );
    }

    #[test]
    fn pre_online_fake_and_transport_failure_wait_for_deadline() {
        let now = Instant::now();
        let asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
        let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let id = online.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![asleep], now);
        scheduler.accept_discovery(vec![online], now + Duration::from_secs(30));

        scheduler.stream_unhealthy(id, now + Duration::from_secs(31));
        scheduler.pre_online_power(id, None, now + Duration::from_secs(31));
        assert!(
            scheduler
                .due_vehicles(now + Duration::from_secs(59))
                .is_empty()
        );
        assert_eq!(
            scheduler.due_vehicles(now + Duration::from_secs(60)),
            vec![id]
        );
        scheduler.vehicle_succeeded(id, PollPhase::Online, false, now + Duration::from_secs(60));
        assert!(
            scheduler
                .due_vehicles(now + Duration::from_secs(60))
                .is_empty()
        );
    }

    #[test]
    fn restart_online_discovery_starts_gate_without_duplicate_fetch() {
        let now = Instant::now();
        let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let id = online.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![online], now);

        assert!(scheduler.should_start_stream(id));
        assert!(scheduler.due_vehicles(now).is_empty());
        scheduler.pre_online_power(id, Some(1), now + Duration::from_secs(1));
        assert_eq!(
            scheduler.due_vehicles(now + Duration::from_secs(1)),
            vec![id]
        );
        scheduler.vehicle_succeeded(id, PollPhase::Online, false, now + Duration::from_secs(1));
        assert!(
            scheduler
                .due_vehicles(now + Duration::from_secs(1))
                .is_empty()
        );
    }

    #[test]
    fn offline_discovery_emits_transition_and_one_timeout_checkpoint() {
        let now = Instant::now();
        let offline = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "offline");
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);

        assert_eq!(
            scheduler.accept_discovery(vec![offline.clone()], now),
            vec![offline.clone()]
        );
        assert!(
            scheduler
                .accept_discovery(vec![offline.clone()], now + Duration::from_secs(30))
                .is_empty()
        );
        assert_eq!(
            scheduler.accept_discovery(vec![offline.clone()], now + Duration::from_secs(15 * 60)),
            vec![offline.clone()]
        );
        assert!(
            scheduler
                .accept_discovery(vec![offline], now + Duration::from_secs(16 * 60))
                .is_empty()
        );
    }

    #[test]
    fn stream_offline_state_fetch_coalesces_and_retries_before_timeout_checkpoint() {
        let now = Instant::now();
        let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let id = online.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![online], now);

        scheduler.schedule_offline_state_fetch(id, now);
        scheduler.schedule_offline_state_fetch(id, now + Duration::from_secs(1));
        assert_eq!(scheduler.due_offline_state_vehicles(now), vec![id]);

        scheduler.offline_state_failed_for_error(
            id,
            &CollectorError::OwnerApi(OwnerApiError::Transport),
            now,
        );
        assert!(
            scheduler
                .due_offline_state_vehicles(now + Duration::from_secs(29))
                .is_empty()
        );
        let retry_at = now + GENERIC_OTHER_RETRY;
        assert_eq!(scheduler.due_offline_state_vehicles(retry_at), vec![id]);

        let events = scheduler.accept_vehicle_state(id, "offline".to_owned(), retry_at);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].state, "offline");
        assert!(scheduler.due_offline_state_vehicles(retry_at).is_empty());
    }

    #[test]
    fn offline_discovery_event_materialises_timed_out_drive() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let now = current_epoch_millis().expect("clock");
        let last_position = now - 15 * 60 * 1_000;
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
        let driving = ManualCollection {
            vehicles: vec![vehicle],
            snapshots: vec![
                VehicleData::for_test(
                    9,
                    json!({
                        "drive_state":{
                            "shift_state":"D",
                            "speed":20,
                            "latitude":47.5,
                            "longitude":19.0,
                            "timestamp":last_position - 1_000
                        },
                        "vehicle_state":{"odometer":1000.0}
                    }),
                ),
                VehicleData::for_test(
                    9,
                    json!({
                        "drive_state":{
                            "shift_state":"D",
                            "speed":20,
                            "latitude":47.51,
                            "longitude":19.01,
                            "timestamp":last_position
                        },
                        "vehicle_state":{"odometer":1000.1}
                    }),
                ),
            ],
            failures: vec![],
        };
        persist_collection(&store, &driving, now).expect("persist drive");
        materialise_lifecycle_for_collection(&store, &driving, now).expect("open drive");

        let offline = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "offline");
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(persist_discovery_events(&store, &CursorKey::from_bytes([4; 32]), &[offline]))
            .expect("persist offline discovery");

        let vehicle_id = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("stored UUID");
        let history = store.materialised_history(vehicle_id).expect("history");
        assert_eq!(history.drives.len(), 1);
        assert_eq!(history.positions.len(), 2);
        let observations = store
            .observations_after_id_for_vehicle(
                vehicle_id,
                0,
                crate::db::MAX_OBSERVATION_QUERY_LIMIT,
            )
            .expect("observations");
        assert!(
            observations.iter().any(|observation| {
                observation.payload["record_type"] == "owner_api_discovery_v1"
            })
        );
    }

    fn safe_idle_snapshot() -> VehicleData {
        VehicleData::for_test(
            1,
            json!({
                "drive_state":{"shift_state":"P","speed":0,"power":0},
                "charge_state":{"charging_state":"Complete"},
                "climate_state":{"is_preconditioning":false,"climate_keeper_mode":"off"},
                "vehicle_state":{
                    "is_user_present":false,
                    "sentry_mode":false,
                    "locked":true,
                    "df":0,"pf":0,"dr":0,"pr":0,"ft":0,"rt":0
                }
            }),
        )
    }

    #[test]
    fn safe_idle_vehicle_enters_and_leaves_suspended_cadence() {
        let now = Instant::now();
        let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);
        scheduler.stream_healthy(vehicle_id, now);

        assert!(
            scheduler
                .vehicle_succeeded(vehicle_id, PollPhase::Online, true, now)
                .is_none()
        );
        let suspended = scheduler
            .vehicle_succeeded(
                vehicle_id,
                PollPhase::Online,
                true,
                now + Duration::from_secs(15 * 60),
            )
            .expect("suspended transition");
        assert_eq!(suspended.state, "suspended");
        assert_eq!(
            scheduler
                .vehicles
                .get(&vehicle_id)
                .expect("scheduled vehicle")
                .next_poll,
            now + Duration::from_secs(45 * 60)
        );

        let resumed = scheduler
            .vehicle_succeeded(
                vehicle_id,
                PollPhase::Online,
                false,
                now + Duration::from_secs(45 * 60),
            )
            .expect("online transition");
        assert_eq!(resumed.state, "online");
    }

    #[test]
    fn stream_health_switches_between_streaming_and_fallback_sleep_cadence() {
        let now = Instant::now();
        let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);

        assert!(!scheduler.vehicles[&vehicle_id].stream_healthy);
        scheduler.stream_healthy(vehicle_id, now);
        scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, true, now);
        assert_eq!(
            scheduler.vehicles[&vehicle_id].next_poll,
            now + Duration::from_secs(60)
        );

        let streaming_idle = now + Duration::from_secs(3 * 60);
        let suspended = scheduler
            .vehicle_succeeded(vehicle_id, PollPhase::Online, true, streaming_idle)
            .expect("healthy stream uses TeslaMate idle threshold");
        assert_eq!(suspended.state, "suspended");
        assert_eq!(
            scheduler.vehicles[&vehicle_id].next_poll,
            streaming_idle + Duration::from_secs(30 * 60)
        );

        let fallback_at = streaming_idle + Duration::from_secs(1);
        scheduler.stream_unhealthy(vehicle_id, fallback_at);
        assert!(!scheduler.vehicles[&vehicle_id].stream_healthy);
        assert!(!scheduler.vehicles[&vehicle_id].suspended);
        assert!(scheduler.due_vehicles(fallback_at).contains(&vehicle_id));
        scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, false, fallback_at);
        let fallback_idle = fallback_at + Duration::from_secs(15 * 60);
        let suspended = scheduler
            .vehicle_succeeded(vehicle_id, PollPhase::Online, true, fallback_idle)
            .expect("unhealthy stream uses owner polling sleep threshold");
        assert_eq!(suspended.state, "suspended");
        assert_eq!(
            scheduler.vehicles[&vehicle_id].next_poll,
            fallback_idle + Duration::from_secs(21 * 60)
        );

        let recovered_at = fallback_idle + Duration::from_secs(1);
        scheduler.stream_healthy(vehicle_id, recovered_at);
        assert!(scheduler.vehicles[&vehicle_id].stream_healthy);
        assert!(scheduler.due_vehicles(recovered_at).contains(&vehicle_id));
        scheduler.vehicle_succeeded(vehicle_id, PollPhase::Driving, false, recovered_at);
        assert_eq!(
            scheduler.vehicles[&vehicle_id].next_poll,
            recovered_at + Duration::from_millis(2_500)
        );
    }

    #[test]
    fn service_fixture_closes_drive_and_blocks_full_poll_until_exit() {
        let now = Instant::now();
        let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);
        scheduler.enter_service_mode(vehicle_id, now);

        assert!(scheduler.due_vehicles(now).is_empty());
        assert!(scheduler.due_service_vehicles(now).is_empty());
        assert!(!scheduler.should_start_stream(vehicle_id));

        let probe_at = now + test_cadence().online;
        assert_eq!(scheduler.due_service_vehicles(probe_at), vec![vehicle_id]);
        scheduler.service_retry(vehicle_id, probe_at);
        assert!(scheduler.due_vehicles(probe_at).is_empty());

        scheduler.service_exited(vehicle_id, probe_at + Duration::from_secs(1));
        assert!(scheduler.should_start_stream(vehicle_id));
        assert!(
            scheduler
                .due_vehicles(probe_at + Duration::from_secs(1))
                .is_empty()
        );
        assert!(!scheduler.vehicles[&vehicle_id].service_mode);
    }

    #[test]
    fn teslamate_sleep_safety_blockers_are_enforced() {
        assert!(sleep_eligible(&safe_idle_snapshot()));
        let blocked = [
            json!({
                "drive_state":{"shift_state":"P","speed":0,"power":1},
                "charge_state":{"charging_state":"Complete"},
                "climate_state":{"is_preconditioning":false},
                "vehicle_state":{"locked":true}
            }),
            json!({
                "drive_state":{"shift_state":"P","speed":0,"power":0},
                "charge_state":{"charging_state":"Complete"},
                "climate_state":{"is_preconditioning":true},
                "vehicle_state":{"locked":true}
            }),
            json!({
                "drive_state":{"shift_state":"P","speed":0,"power":0},
                "charge_state":{"charging_state":"Complete"},
                "climate_state":{"is_preconditioning":false,"climate_keeper_mode":"dog"},
                "vehicle_state":{"locked":true}
            }),
            json!({
                "drive_state":{"shift_state":"P","speed":0,"power":0},
                "charge_state":{"charging_state":"Complete"},
                "climate_state":{"is_preconditioning":false},
                "vehicle_state":{"locked":false}
            }),
            json!({
                "drive_state":{"shift_state":"P","speed":0,"power":0},
                "charge_state":{"charging_state":"Complete"},
                "climate_state":{"is_preconditioning":false},
                "vehicle_state":{"locked":true,"is_user_present":true}
            }),
            json!({
                "drive_state":{"shift_state":"P","speed":0,"power":0},
                "charge_state":{"charging_state":"Complete"},
                "climate_state":{"is_preconditioning":false},
                "vehicle_state":{"locked":true,"sentry_mode":true}
            }),
            json!({
                "drive_state":{"shift_state":"P","speed":0,"power":0},
                "charge_state":{"charging_state":"Complete"},
                "climate_state":{"is_preconditioning":false},
                "vehicle_state":{"locked":true,"df":1}
            }),
            json!({
                "drive_state":{"shift_state":"P","speed":0,"power":0},
                "charge_state":{"charging_state":"Complete"},
                "climate_state":{"is_preconditioning":false},
                "vehicle_state":{
                    "locked":true,
                    "software_update":{"status":"downloading","download_perc":50.0}
                }
            }),
        ];
        for fields in blocked {
            assert!(!sleep_eligible(&VehicleData::for_test(1, fields)));
        }
    }

    #[test]
    fn car_policy_is_independent_for_scheduler_and_sleep() {
        let now = Instant::now();
        let mut disabled = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        disabled.settings.enabled = false;
        disabled.settings.use_streaming_api = false;
        let mut legacy = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
        legacy.settings.use_streaming_api = false;
        legacy.settings.suspend_after_idle_min = 2;
        legacy.settings.suspend_min = 7;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![disabled, legacy.clone()], now);

        assert_eq!(scheduler.due_vehicles(now), vec![legacy.id]);
        scheduler.vehicle_succeeded(legacy.id, PollPhase::Online, true, now);
        assert!(
            !scheduler
                .due_vehicles(now + Duration::from_secs(60))
                .contains(&VehicleId::from_test(1))
        );
        let transition = scheduler
            .vehicle_succeeded(
                legacy.id,
                PollPhase::Online,
                true,
                now + Duration::from_secs(2 * 60),
            )
            .expect("legacy car suspends");
        assert_eq!(transition.state, "suspended");
        assert_eq!(
            scheduler.vehicles[&legacy.id].next_poll,
            now + Duration::from_secs(9 * 60)
        );

        let safe = safe_idle_snapshot();
        assert!(sleep_eligible_with_policy(&safe, false));
        assert!(!sleep_eligible_with_policy(
            &VehicleData::for_test(
                2,
                json!({
                    "drive_state":{"shift_state":"P","speed":0,"power":0},
                    "charge_state":{"charging_state":"Complete"},
                    "climate_state":{"is_preconditioning":false},
                    "vehicle_state":{"locked":false}
                })
            ),
            true
        ));
    }

    #[test]
    fn stream_watermark_rejects_duplicate_and_old_frames_after_restart() {
        let temp = tempfile::tempdir().expect("temporary store");
        let vehicle_id = VehicleId::from_test(9);
        let first_timestamp = current_epoch_millis().expect("clock") - 60_000;
        let update = |timestamp_ms: i64, odometer: f64| crate::tesla_stream::StreamUpdate {
            tag: vehicle_id.to_string(),
            timestamp_ms,
            speed: Some(20),
            odometer: Some(odometer),
            soc: Some(80),
            elevation: Some(25),
            est_heading: Some(180),
            est_lat: Some(51.5),
            est_lng: Some(-0.1),
            power: Some(12),
            shift_state: Some("D".to_owned()),
            range: Some(200),
            est_range: Some(210),
            heading: Some(180),
        };

        let store = HubStore::initialize(temp.path()).expect("store");
        persist_stream_update(&store, vehicle_id, &update(first_timestamp, 100.0))
            .expect("first frame");
        persist_stream_update(&store, vehicle_id, &update(first_timestamp, 100.0))
            .expect("duplicate frame");
        persist_stream_update(&store, vehicle_id, &update(first_timestamp - 1, 99.0))
            .expect("old frame");
        drop(store);

        let store = HubStore::initialize(temp.path()).expect("restart");
        persist_stream_update(&store, vehicle_id, &update(first_timestamp + 1_000, 101.0))
            .expect("new frame");

        let registered = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle identity")
            .parse::<Uuid>()
            .expect("vehicle UUID");
        let observations = store
            .observations_for_vehicle(registered, crate::db::ObservationQuery::from_start(10))
            .expect("stream observations");
        assert_eq!(observations.len(), 2);
        assert_eq!(
            observations
                .iter()
                .map(|observation| observation.observed_at_ms)
                .collect::<Vec<_>>(),
            vec![first_timestamp, first_timestamp + 1_000]
        );
        let lifecycle = store
            .load_lifecycle_state(registered)
            .expect("lifecycle state")
            .expect("open lifecycle state");
        let open = crate::lifecycle::OpenSessionState::decode(&lifecycle.open_session_json)
            .expect("open session");
        assert!(open.open_drive.expect("open drive").positions.is_empty());
        let provisional: i64 = store
            .open()
            .expect("database")
            .query_row(
                "SELECT COUNT(*) FROM lifecycle_open_rows
                 WHERE vehicle_id = ?1 AND domain = 'position'",
                [registered.to_string()],
                |row| row.get(0),
            )
            .expect("provisional positions");
        assert_eq!(provisional, 2);
    }

    #[test]
    fn stream_transaction_rolls_back_every_stage_and_retry_commits_once() {
        let points = [
            StreamFaultPoint::RawInsert,
            StreamFaultPoint::LifecycleWrite,
            StreamFaultPoint::WatermarkUpdate,
            StreamFaultPoint::Commit,
        ];
        for (index, point) in points.into_iter().enumerate() {
            let temp = tempfile::tempdir().expect("temporary store");
            let store = HubStore::initialize(temp.path()).expect("store");
            let vehicle_id = VehicleId::from_test(index as u64 + 40);
            let timestamp = current_epoch_millis().expect("clock") - 60_000;
            let update = crate::tesla_stream::StreamUpdate {
                tag: vehicle_id.to_string(),
                timestamp_ms: timestamp,
                speed: Some(20),
                odometer: Some(100.0),
                soc: Some(80),
                elevation: Some(25),
                est_heading: Some(180),
                est_lat: Some(51.5),
                est_lng: Some(-0.1),
                power: Some(12),
                shift_state: Some("D".to_owned()),
                range: Some(200),
                est_range: Some(210),
                heading: Some(180),
            };
            store.inject_stream_fault(point);
            assert!(persist_stream_update(&store, vehicle_id, &update).is_err());
            let connection = store.open().expect("database");
            for table in ["raw_observations", "stream_watermarks", "vehicle_lifecycle_state"] {
                let count: i64 = connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
                    .expect("fault count");
                assert_eq!(count, 0, "fault point {point:?} left {table}");
            }
            drop(connection);

            assert!(persist_stream_update(&store, vehicle_id, &update).expect("retry"));
            assert!(!persist_stream_update(&store, vehicle_id, &update).expect("duplicate"));
            let connection = store.open().expect("database");
            for table in ["raw_observations", "stream_watermarks, vehicle_lifecycle_state"] {
                let table = table.replace(", ", " UNION ALL SELECT COUNT(*) FROM ");
                let counts: Vec<i64> = connection
                    .prepare(&format!("SELECT COUNT(*) FROM {table}"))
                    .expect("count query")
                    .query_map([], |row| row.get(0))
                    .expect("count rows")
                    .map(|row| row.expect("count row"))
                    .collect();
                assert!(counts.iter().all(|count| *count == 1));
            }
        }
    }

    #[test]
    fn concurrent_same_timestamp_has_one_committed_winner_and_restart_is_idempotent() {
        let temp = tempfile::tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let vehicle_id = VehicleId::from_test(90);
        let timestamp = current_epoch_millis().expect("clock") - 60_000;
        let update = crate::tesla_stream::StreamUpdate {
            tag: vehicle_id.to_string(),
            timestamp_ms: timestamp,
            speed: Some(20),
            odometer: Some(100.0),
            soc: Some(80),
            elevation: Some(25),
            est_heading: Some(180),
            est_lat: Some(51.5),
            est_lng: Some(-0.1),
            power: Some(12),
            shift_state: Some("D".to_owned()),
            range: Some(200),
            est_range: Some(210),
            heading: Some(180),
        };
        let first = store.clone();
        let second = store.clone();
        let left_update = update.clone();
        let right_update = update.clone();
        let left =
            std::thread::spawn(move || persist_stream_update(&first, vehicle_id, &left_update));
        let right = std::thread::spawn(move || persist_stream_update(&second, vehicle_id, &right_update));
        let results = [left.join().expect("left").expect("left result"), right
            .join()
            .expect("right")
            .expect("right result")];
        assert_eq!(results.iter().filter(|value| **value).count(), 1);
        assert_eq!(results.iter().filter(|value| !**value).count(), 1);
        let registered = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| row.get::<_, String>(0))
            .expect("vehicle")
            .parse::<Uuid>()
            .expect("uuid");
        assert_eq!(
            store
                .observations_for_vehicle(registered, crate::db::ObservationQuery::from_start(10))
                .expect("observations")
                .len(),
            1
        );
        drop(store);
        let restarted = HubStore::initialize(temp.path()).expect("restart");
        assert!(!persist_stream_update(&restarted, vehicle_id, &update).expect("restart retry"));
    }

    #[test]
    fn per_car_api_fuse_isolated_and_resets_after_five_minutes() {
        let now = Instant::now();
        let first = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let second = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
        let first_id = first.id;
        let second_id = second.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![first, second], now);
        scheduler.pre_online_power(first_id, Some(1), now);
        scheduler.pre_online_power(second_id, Some(1), now);

        for offset in 0..3 {
            scheduler.vehicle_failed_for_error(
                first_id,
                &CollectorError::OwnerApi(OwnerApiError::HttpStatus(500)),
                now + Duration::from_secs(offset),
            );
        }
        let due = scheduler.due_vehicles(now + Duration::from_secs(2 * 60));
        assert!(due.contains(&second_id));
        assert!(!due.contains(&first_id));
        assert!(
            scheduler
                .due_vehicles(now + Duration::from_secs(8 * 60))
                .contains(&first_id)
        );
    }

    #[test]
    fn generic_api_errors_use_teslamate_state_retry_delays() {
        let cases = [
            ("driving", GENERIC_DRIVING_RETRY),
            ("charging", GENERIC_CHARGING_RETRY),
            ("online", GENERIC_ONLINE_RETRY),
            ("updating", GENERIC_ONLINE_RETRY),
            ("asleep", GENERIC_OTHER_RETRY),
            ("offline", GENERIC_OTHER_RETRY),
            ("start", GENERIC_OTHER_RETRY),
            ("suspended", GENERIC_OTHER_RETRY),
            ("unknown", GENERIC_OTHER_RETRY),
        ];
        for (index, (state, expected)) in cases.into_iter().enumerate() {
            let now = Instant::now() + Duration::from_secs(index as u64 * 100);
            let id = VehicleId::from_test(index as u64 + 1);
            let vehicle = Vehicle::for_test(id.get(), "5YJ3E1EA7KF000001", state);
            let mut scheduler = VehicleScheduler::new(test_cadence(), now);
            scheduler.accept_discovery(vec![vehicle], now);
            if state == "online" {
                scheduler.vehicle_succeeded(id, PollPhase::Online, false, now);
            }
            scheduler.vehicle_failed_for_error(
                id,
                &CollectorError::OwnerApi(OwnerApiError::HttpStatus(500)),
                now,
            );
            assert_eq!(scheduler.vehicles[&id].next_poll, now + expected, "state={state}");
        }
    }

    #[test]
    fn special_retry_precedence_is_exact_and_per_vehicle() {
        let now = Instant::now();
        let first = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let second = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
        let first_id = first.id;
        let second_id = second.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![first, second], now);

        scheduler.vehicle_failed_for_error(
            first_id,
            &CollectorError::OwnerApi(OwnerApiError::RateLimited {
                retry_after_seconds: 17,
            }),
            now,
        );
        assert_eq!(scheduler.vehicles[&first_id].next_poll, now + Duration::from_secs(17));

        scheduler.vehicle_failed_for_error(
            second_id,
            &CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(
                LegacyAuthManagerError::Auth(crate::legacy_auth::LegacyAuthError::Transport),
            )),
            now,
        );
        assert_eq!(
            scheduler.vehicles[&second_id].next_poll,
            now + LEGACY_REFRESH_RETRY
        );

        scheduler.vehicle_failed_for_error(
            first_id,
            &CollectorError::OwnerApi(OwnerApiError::VehicleNotFound),
            now,
        );
        assert_eq!(scheduler.vehicles[&first_id].next_poll, now + test_cadence().online);

        scheduler.vehicle_failed_for_error(
            second_id,
            &CollectorError::OwnerApi(OwnerApiError::VehicleInService),
            now,
        );
        assert_eq!(scheduler.vehicles[&second_id].next_poll, now + test_cadence().online);
    }

    #[test]
    fn rate_limit_is_exact_and_vehicle_not_found_resets_at_ten_minutes() {
        let now = Instant::now();
        let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);
        scheduler.pre_online_power(id, Some(1), now);
        scheduler.vehicle_failed_for_error(
            id,
            &CollectorError::OwnerApi(OwnerApiError::RateLimited {
                retry_after_seconds: 17,
            }),
            now,
        );
        assert!(scheduler.due_vehicles(now + Duration::from_secs(16)).is_empty());
        assert_eq!(scheduler.due_vehicles(now + Duration::from_secs(17)), vec![id]);

        for offset in 0..8 {
            scheduler.vehicle_failed_for_error(
                id,
                &CollectorError::OwnerApi(OwnerApiError::VehicleNotFound),
                now + Duration::from_secs(offset),
            );
        }
        assert!(scheduler.due_vehicles(now + Duration::from_secs(9 * 60)).is_empty());
        assert!(scheduler.vehicle_fuse_healthy(id, now + Duration::from_secs(11 * 60)));
    }
}
#[cfg(test)]
mod terrain_worker_tests {
    use super::*;

    #[test]
    fn terrain_fuse_blows_after_two_failures_and_recovers() {
        let start = Instant::now();
        let mut fuse = TerrainFuse::default();
        assert!(fuse.available(start));
        fuse.failure(start);
        assert!(fuse.available(start + Duration::from_secs(179)));
        fuse.failure(start + Duration::from_secs(179));
        assert!(!fuse.available(start + Duration::from_secs(180)));
        assert!(fuse.available(start + Duration::from_secs(179) + TERRAIN_FUSE_RESET + Duration::from_secs(1)));
    }

    #[tokio::test]
    async fn terrain_pass_uses_the_safe_cache_resolver() {
        let data = tempfile::tempdir().expect("data");
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
            )
            .await
            .expect("terrain pass"),
            0
        );
    }
}

#[derive(Default)]
struct TerrainFuse {
    failures: Vec<Instant>,
    blown_until: Option<Instant>,
}
