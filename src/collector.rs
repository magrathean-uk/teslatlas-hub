//! Persistence boundary for TeslaMate legacy Owner API reads.
//!
//! Networking lives in `owner_api`; this module turns completed reads into
//! bounded, append-only Hub observations, materialises durable drive/charge
//! history through the pure lifecycle projector, and optionally runs a
//! supervised no-wake schedule. Credentials are never held in configuration or
//! argv.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    io::Read,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rustix::{
    fs::{FileType, Mode, OFlags, fstat, open},
    process::getuid,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::time::{Instant, MissedTickBehavior, sleep, timeout};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot, watch},
    task::{JoinError, JoinHandle},
};
use uuid::Uuid;

use crate::{
    config::{CollectorCadence, CollectorProvider, ConfigError, HubConfig, TerrainConfig},
    credentials::{CredentialError, LegacyAuthManager, LegacyAuthManagerError, OwnerTokens},
    db::{
        HubStore, ObservationInput, OutboundRequestCompletion, OutboundRequestOperation,
        OutboundRequestOutcome, OutboundRequestPrecondition, OutboundRequestSafetyClass,
        OutboundRequestStart, OutboundRequestTransport, SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
        SourceDescriptor, StoreError, StreamObservationResult, SupervisedCollectorLease,
        SupervisedCollectorState, VehicleDescriptor,
    },
    fleet_api::{
        FleetApi, FleetApiConfigError, FleetApiError, FleetAuthApi, FleetCommand,
        FleetCommandProxy, FleetCommandProxyBase, FleetCommandResult, FleetTelemetryConfigBuilder,
        FleetTelemetryVins, VehicleVin, WakeResult,
    },
    fleet_credentials::{FleetAuthManager, FleetCredentialError, FleetSetupCredentials},
    fleet_telemetry::FleetTelemetrySnapshot,
    geocoder::{AdmittedUserEgressGuard, Geocoder, GeocoderError},
    hub_pack::{
        ProjectionBinding, ProjectionCar, ProjectionDeltaPackRequest, ProjectionPackError,
        ProjectionPackRequest, ProjectionPackWriter, ProjectionSnapshot,
    },
    legacy_auth::{LegacyAuth, LegacyAuthError, LegacyAuthFuse},
    lifecycle::{
        LifecycleError, LifecycleSample, OpenSessionState, apply_sample, force_close_for_service,
        stream_observation_payload,
    },
    location::Wgs84Point,
    owner_api::{
        LegacyVehicleAction, LegacyVehicleActionResult, ManualCollection, OwnerApi,
        OwnerApiAuthError, OwnerApiConfigError, OwnerApiError, StreamVehicleId, Vehicle,
        VehicleCollectionFailure, VehicleData, VehicleId,
    },
    protocol::{
        CursorClaims, CursorKey, HUB_PROJECTION_SCHEMA_V2, LineageDelta, OpaqueCursor, PROTOCOL_V1,
        ProtocolError, ProtocolLimits, SequenceRange, Sha256Digest, canonical_delta_chain_digest,
    },
    terrain_cache::{TerrainCache, TerrainCacheError},
    tesla_stream::{
        StreamEvent, StreamPowerGate, StreamRegion, TeslaStreamSupervisor, streaming_endpoint,
    },
};

#[cfg(test)]
use crate::db::StreamFaultPoint;
const OWNER_API_SOURCE_KIND: &str = "owner_api_compat";
const OWNER_API_SOURCE_KEY: &str = "local_installation_v1";
const FLEET_API_SOURCE_KIND: &str = "fleet_api_compat";
const FLEET_API_SOURCE_KEY: &str = "local_installation_v1";
const EARLIEST_PLAUSIBLE_TIMESTAMP_MS: i64 = 946_684_800_000; // 2000-01-01 UTC
const FUTURE_TIMESTAMP_SKEW_MS: i64 = 5 * 60 * 1000;
const STREAM_SOURCE_KIND: &str = OWNER_API_SOURCE_KIND;
const STREAM_SOURCE_KEY: &str = OWNER_API_SOURCE_KEY;
const FLEET_TELEMETRY_CONFIG_LIFETIME_SECONDS: u64 = 30 * 24 * 60 * 60;
const FLEET_TELEMETRY_CONFIG_RENEWAL_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const FLEET_TELEMETRY_CONFIG_RETRY_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const FLEET_REFRESH_REQUEST_NOT_SENT_RETRY: Duration = Duration::from_secs(5 * 60);

fn provider_source(provider: CollectorProvider) -> SourceDescriptor {
    match provider {
        CollectorProvider::Legacy => {
            SourceDescriptor::new(OWNER_API_SOURCE_KIND, OWNER_API_SOURCE_KEY)
        }
        CollectorProvider::Fleet => {
            SourceDescriptor::new(FLEET_API_SOURCE_KIND, FLEET_API_SOURCE_KEY)
        }
    }
}

const fn provider_vehicle_data_record_type(provider: CollectorProvider) -> &'static str {
    match provider {
        CollectorProvider::Legacy => "owner_api_vehicle_data_v1",
        CollectorProvider::Fleet => "fleet_api_vehicle_data_v1",
    }
}

const fn provider_discovery_record_type(provider: CollectorProvider) -> &'static str {
    match provider {
        CollectorProvider::Legacy => "owner_api_discovery_v1",
        CollectorProvider::Fleet => "fleet_api_discovery_v1",
    }
}
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
// Keep a continuously noisy stream from monopolising the collection loop.
// The channel absorbs bounded network/address work while the 100 ms active
// drain cadence keeps normal Tesla telemetry well below capacity.
const STREAM_EVENT_CHANNEL_CAPACITY: usize = 256;
const MAX_STREAM_EVENTS_PER_DRAIN: usize = 16;

// The production collector deliberately uses conservative retry timing and
// cannot expose an interstitial gate state.  Keep the acceptance witness
// task-local and test-only: it neither changes a production call path nor
// leaks across concurrently running collector tests.
#[cfg(test)]
tokio::task_local! {
    static SUPERVISED_COLLECTOR_TEST_SEAM: Arc<SupervisedCollectorTestSeam>;
}

#[cfg(test)]
#[derive(Default)]
struct SupervisedCollectorTestSeam {
    owner_api_failure_retry: Option<Duration>,
    collection_completed: tokio::sync::Mutex<Option<SupervisedCollectionCompletion>>,
}

#[cfg(test)]
struct SupervisedCollectionCompletion {
    completed: oneshot::Sender<SupervisedCollectionCheckpoint>,
    resume: Option<oneshot::Receiver<()>>,
}

/// A test-owned receipt emitted only after `finish_collection` has committed.
/// It avoids opening a second SQLite connection while the supervised writer is
/// still active.
#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
struct SupervisedCollectionCheckpoint {
    snapshots_received: usize,
    observations_inserted: usize,
    observations_already_present: usize,
    snapshots_published: usize,
    vehicle_failures: usize,
    drives_closed: usize,
    charges_closed: usize,
    positions_materialised: usize,
    charge_samples_materialised: usize,
    lifecycle_quarantines: usize,
}

#[cfg(test)]
impl From<&ManualCollectionReport> for SupervisedCollectionCheckpoint {
    fn from(report: &ManualCollectionReport) -> Self {
        Self {
            snapshots_received: report.snapshots_received,
            observations_inserted: report.observations_inserted,
            observations_already_present: report.observations_already_present,
            snapshots_published: report.snapshots_published,
            vehicle_failures: report.vehicle_failures,
            drives_closed: report.drives_closed,
            charges_closed: report.charges_closed,
            positions_materialised: report.positions_materialised,
            charge_samples_materialised: report.charge_samples_materialised,
            lifecycle_quarantines: report.lifecycle_quarantines,
        }
    }
}

#[cfg(test)]
impl SupervisedCollectorTestSeam {
    async fn arm_paused_collection_completion(
        &self,
    ) -> (
        oneshot::Receiver<SupervisedCollectionCheckpoint>,
        oneshot::Sender<()>,
    ) {
        let (completed_tx, completed_rx) = oneshot::channel();
        let (resume_tx, resume_rx) = oneshot::channel();
        let mut completed = self.collection_completed.lock().await;
        assert!(
            completed.is_none(),
            "only one collection completion witness may be armed"
        );
        *completed = Some(SupervisedCollectionCompletion {
            completed: completed_tx,
            resume: Some(resume_rx),
        });
        (completed_rx, resume_tx)
    }

    async fn collection_finished(&self, report: &ManualCollectionReport) {
        let completed = self.collection_completed.lock().await.take();
        if let Some(completed) = completed {
            let _ = completed.completed.send(report.into());
            if let Some(resume) = completed.resume {
                let _ = resume.await;
            }
        }
    }
}

#[cfg(test)]
async fn supervised_test_collection_finished(report: &ManualCollectionReport) {
    let seam = SUPERVISED_COLLECTOR_TEST_SEAM.try_with(Arc::clone).ok();
    if let Some(seam) = seam {
        seam.collection_finished(report).await;
    }
}

#[cfg(test)]
fn supervised_test_owner_api_failure_retry() -> Option<Duration> {
    SUPERVISED_COLLECTOR_TEST_SEAM
        .try_with(|seam| seam.owner_api_failure_retry)
        .ok()
        .flatten()
}

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

async fn stop_manual_probe_streams(streams: &mut [VehicleStreamRuntime]) {
    for stream in streams.iter_mut() {
        if let Some(shutdown) = stream._shutdown.take() {
            let _ = shutdown.send(());
        }
    }
    for stream in streams.iter_mut() {
        if let Some(mut task) = stream.task.take()
            && timeout(STREAM_SHUTDOWN_TIMEOUT, &mut task).await.is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

async fn stop_and_clear_manual_probe_streams(streams: &mut Vec<VehicleStreamRuntime>) {
    stop_manual_probe_streams(streams).await;
    streams.clear();
}

/// A sensitive authority failure forbids every later credential-bearing stream
/// operation. Abort locally first, then await the child before dropping its
/// sender: `VehicleStreamRuntime::Drop` treats a live sender as an orderly
/// shutdown request and would otherwise send `data:unsubscribe`.
async fn abort_and_clear_manual_probe_streams_without_egress(
    streams: &mut Vec<VehicleStreamRuntime>,
) {
    for stream in streams.iter_mut() {
        if let Some(task) = stream.task.as_ref() {
            task.abort();
        }
    }
    for stream in streams.iter_mut() {
        if let Some(task) = stream.task.take() {
            let _ = task.await;
        }
        let _ = stream._shutdown.take();
    }
    streams.clear();
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

/// Safe first-run receipt. It contains only the selected numeric vehicle id,
/// its optional display name, and whether the initial V2 base was published.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct NativeSetupReport {
    pub selected_vehicle_id: i64,
    pub display_name: Option<String>,
    pub snapshots_published: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct NativeSetupBatchReport {
    pub vehicles: Vec<NativeSetupVehicle>,
    pub snapshots_published: usize,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct NativeSetupVehicle {
    pub vehicle_id: i64,
    pub display_name: Option<String>,
}

#[derive(Clone)]
enum CollectionAuth {
    Legacy {
        manager: Arc<tokio::sync::Mutex<LegacyAuthManager>>,
        fuse: Arc<tokio::sync::Mutex<LegacyAuthFuse>>,
        refresh: Arc<LegacyRefreshCoordinator>,
        allow_refresh: bool,
        region: StreamRegion,
    },
}

/// One legacy-401 recovery is allowed at a time. The request that observed a
/// 401 returns immediately; later logical Owner requests wait for this ticket
/// before they can read the current pair.
#[derive(Default)]
struct LegacyRefreshCoordinator {
    ticket: tokio::sync::Mutex<Option<Arc<LegacyRefreshTicket>>>,
    // A failed helper/persistence/CAS operation means the legacy credential
    // authority is no longer trustworthy.  Keep that typed terminal state
    // separate from the per-refresh ticket so later requests cannot forget it
    // when they replace a completed ticket.
    sensitive_failure: Arc<AtomicBool>,
    sensitive_failure_notify: Arc<tokio::sync::Notify>,
}

struct LegacyRefreshTicket {
    complete: AtomicBool,
    notify: tokio::sync::Notify,
    task: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LegacyUnauthorizedFacadeObservation {
    pub owner_requests: usize,
    pub refresh_requests: usize,
    pub owner_retries: usize,
    pub owner_pairs: Vec<String>,
    pub refresh_pairs: Vec<String>,
    pub owner_statuses: Vec<u16>,
    pub refresh_statuses: Vec<u16>,
    pub durable_pair: String,
    pub logical_resident_pair: String,
    pub attempts_before_signout: usize,
    pub fuse_melts: usize,
    pub fuse_blown: bool,
    pub pre_restart_retry_ms: i64,
    pub restart_retry_ms: i64,
}

impl LegacyRefreshTicket {
    fn new() -> Self {
        Self {
            complete: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
            task: tokio::sync::Mutex::new(None),
        }
    }
}

impl LegacyRefreshCoordinator {
    fn has_sensitive_failure(&self) -> bool {
        self.sensitive_failure.load(Ordering::Acquire)
    }

    fn sensitive_failure_result(&self) -> Result<(), CollectorError> {
        if self.has_sensitive_failure() {
            Err(CollectorError::SensitiveAccessUnavailable)
        } else {
            Ok(())
        }
    }

    async fn wait_for_sensitive_failure(&self) {
        loop {
            let notified = self.sensitive_failure_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.has_sensitive_failure() {
                return;
            }
            notified.await;
        }
    }

    async fn wait_for_prior(&self) -> Result<(), CollectorError> {
        self.sensitive_failure_result()?;
        let ticket = self.ticket.lock().await.clone();
        let Some(ticket) = ticket else {
            return self.sensitive_failure_result();
        };
        loop {
            let notified = ticket.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if ticket.complete.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
        if let Some(handle) = ticket.task.lock().await.take() {
            let _ = handle.await;
        }
        self.sensitive_failure_result()
    }

    async fn enqueue(
        &self,
        client: OwnerApi,
        manager: Arc<tokio::sync::Mutex<LegacyAuthManager>>,
        fuse: Arc<tokio::sync::Mutex<LegacyAuthFuse>>,
    ) {
        if self.has_sensitive_failure() {
            return;
        }
        let mut current = self.ticket.lock().await;
        if self.has_sensitive_failure() {
            return;
        }
        if current
            .as_ref()
            .is_some_and(|ticket| !ticket.complete.load(Ordering::Acquire))
        {
            return;
        }
        let ticket = Arc::new(LegacyRefreshTicket::new());
        // Do not form a task -> ticket -> JoinHandle -> task reference cycle.
        let worker_ticket = Arc::downgrade(&ticket);
        let worker_sensitive_failure = Arc::clone(&self.sensitive_failure);
        let worker_sensitive_failure_notify = Arc::clone(&self.sensitive_failure_notify);
        let task = tokio::spawn(async move {
            let result = {
                let mut manager = manager.lock().await;
                let result = manager
                    .refresh_now(&client.legacy_auth_http_client(), SystemTime::now())
                    .await;
                // Publish the terminal fence before releasing the manager. A
                // waiting Owner/stream path must not read a rotated but
                // undurably persisted bearer between these two operations.
                if result
                    .as_ref()
                    .is_err_and(LegacyAuthManagerError::is_sensitive_access_failure)
                    && !worker_sensitive_failure.swap(true, Ordering::AcqRel)
                {
                    worker_sensitive_failure_notify.notify_waiters();
                }
                result
            };
            if result.is_ok() {
                fuse.lock().await.reset();
            }
            if let Some(ticket) = worker_ticket.upgrade() {
                ticket.complete.store(true, Ordering::Release);
                ticket.notify.notify_waiters();
            }
        });
        *ticket.task.lock().await = Some(task);
        *current = Some(ticket);
    }

    async fn shutdown(&self) {
        let ticket = self.ticket.lock().await.take();
        let Some(ticket) = ticket else {
            return;
        };
        if let Some(handle) = ticket.task.lock().await.take() {
            let _ = handle.await;
        }
    }
}

/// Refresh a newly imported pair or a persisted pair whose saved schedule is
/// due. A failed refresh keeps the restored pair and schedules a retry.
async fn refresh_restored_legacy_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
) -> Result<(), CollectorError> {
    let CollectionAuth::Legacy {
        manager,
        fuse,
        allow_refresh,
        ..
    } = auth;
    if !*allow_refresh {
        return Ok(());
    }
    let result = {
        let mut manager = manager.lock().await;
        manager
            .refresh_if_due(&client.legacy_auth_http_client(), SystemTime::now())
            .await
    };
    match result {
        Ok(()) => fuse.lock().await.reset(),
        Err(error) if error.is_sensitive_access_failure() => {
            return Err(CollectorError::SensitiveAccessUnavailable);
        }
        Err(error) => {
            tracing::warn!(error = %error, "startup legacy token refresh failed; restored pair retained");
        }
    }
    Ok(())
}

fn is_wrapped_legacy_unauthorized(error: &CollectorError) -> bool {
    matches!(
        error,
        CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401)))
    )
}

fn normalize_sensitive_access_error(error: CollectorError) -> CollectorError {
    match error {
        CollectorError::LegacyAuthManager(error) if error.is_sensitive_access_failure() => {
            CollectorError::SensitiveAccessUnavailable
        }
        CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(error))
            if error.is_sensitive_access_failure() =>
        {
            CollectorError::SensitiveAccessUnavailable
        }
        CollectorError::OwnerApi(OwnerApiError::CredentialAuthorityUnavailable) => {
            CollectorError::SensitiveAccessUnavailable
        }
        error => error,
    }
}

fn is_sensitive_access_failure(error: &CollectorError) -> bool {
    matches!(error, CollectorError::SensitiveAccessUnavailable)
}

fn observer_auth_failure(auth: &CollectionAuth, error: &CollectorError) -> bool {
    let CollectionAuth::Legacy { allow_refresh, .. } = auth;
    !*allow_refresh
        && matches!(
            error,
            CollectorError::OwnerApi(OwnerApiError::HttpStatus(401 | 403))
                | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(
                    OwnerApiError::HttpStatus(401 | 403),
                ))
                | CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
        )
}

fn auth_allows_refresh(auth: &CollectionAuth) -> bool {
    let CollectionAuth::Legacy { allow_refresh, .. } = auth;
    *allow_refresh
}

fn observer_stream_authentication_error() -> CollectorError {
    CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401)))
}

fn must_stop_supervised_collection(error: &CollectorError) -> bool {
    // Local token persistence loss is fatal, not an Owner API outage. Do not
    // hide it behind a retry delay.
    is_sensitive_access_failure(error)
}

async fn wait_for_legacy_refresh_before_owner(auth: &CollectionAuth) -> Result<(), CollectorError> {
    let CollectionAuth::Legacy { refresh, .. } = auth;
    refresh.wait_for_prior().await?;
    Ok(())
}

async fn wait_for_legacy_refresh_sensitive_failure(
    auth: &CollectionAuth,
) -> Result<(), CollectorError> {
    let CollectionAuth::Legacy { refresh, .. } = auth;
    refresh.wait_for_sensitive_failure().await;
    Err(CollectorError::SensitiveAccessUnavailable)
}

async fn enqueue_legacy_refresh_after_unauthorized(client: &OwnerApi, auth: &CollectionAuth) {
    let CollectionAuth::Legacy {
        manager,
        fuse,
        refresh,
        allow_refresh,
        ..
    } = auth;
    if !*allow_refresh {
        return;
    }
    refresh
        .enqueue(client.clone(), Arc::clone(manager), Arc::clone(fuse))
        .await;
}

async fn refresh_after_stream_authentication_rejection(
    client: &OwnerApi,
    auth: &CollectionAuth,
    transition: StreamAuthenticationTransition,
) -> Result<(), CollectorError> {
    if !matches!(transition, StreamAuthenticationTransition::Rejected) {
        return Ok(());
    }
    if !auth_allows_refresh(auth) {
        return Err(observer_stream_authentication_error());
    }
    enqueue_legacy_refresh_after_unauthorized(client, auth).await;
    Ok(())
}

async fn shutdown_legacy_refresh(auth: &CollectionAuth) {
    let CollectionAuth::Legacy { refresh, .. } = auth;
    refresh.shutdown().await;
}

#[cfg(test)]
async fn drain_and_shutdown_legacy_refresh(auth: &CollectionAuth) -> Result<(), CollectorError> {
    let result = wait_for_legacy_refresh_before_owner(auth).await;
    shutdown_legacy_refresh(auth).await;
    result
}

async fn list_vehicles_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
) -> Result<Vec<Vehicle>, CollectorError> {
    wait_for_legacy_refresh_before_owner(auth).await?;
    let CollectionAuth::Legacy { manager, fuse, .. } = auth;
    let vehicles = {
        let mut fuse = fuse.lock().await;
        let mut manager = manager.lock().await;
        client
            .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
            .await
            .map_err(Into::into)
    };
    let vehicles = vehicles.map_err(normalize_sensitive_access_error);
    if let Err(error) = &vehicles
        && is_wrapped_legacy_unauthorized(error)
    {
        enqueue_legacy_refresh_after_unauthorized(client, auth).await;
    }
    let mut vehicles = vehicles?;
    for vehicle in &mut vehicles {
        vehicle.settings.suspend_min_resolved = false;
    }
    Ok(vehicles)
}

async fn vehicle_data_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
    power_gate: Option<&StreamPowerGate>,
) -> Result<VehicleData, CollectorError> {
    wait_for_legacy_refresh_before_owner(auth).await?;
    if power_gate.is_some_and(|gate| !gate.is_confirmed()) {
        return Err(CollectorError::OwnerApi(
            OwnerApiError::StreamPowerNotConfirmed,
        ));
    }
    let CollectionAuth::Legacy { manager, fuse, .. } = auth;
    let result = {
        let mut fuse = fuse.lock().await;
        let mut manager = manager.lock().await;
        client
            .vehicle_data_with_legacy_auth_fused(&mut manager, &mut fuse, vehicle_id, power_gate)
            .await
            .map_err(Into::into)
    };
    let result = result.map_err(normalize_sensitive_access_error);
    if let Err(error) = &result
        && is_wrapped_legacy_unauthorized(error)
    {
        enqueue_legacy_refresh_after_unauthorized(client, auth).await;
    }
    result
}

async fn vehicle_state_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
) -> Result<String, CollectorError> {
    wait_for_legacy_refresh_before_owner(auth).await?;
    let CollectionAuth::Legacy { manager, fuse, .. } = auth;
    let result = {
        let mut fuse = fuse.lock().await;
        let mut manager = manager.lock().await;
        client
            .vehicle_state_with_legacy_auth_fused(&mut manager, &mut fuse, vehicle_id)
            .await
            .map_err(Into::into)
    };
    let result = result.map_err(normalize_sensitive_access_error);
    if let Err(error) = &result
        && is_wrapped_legacy_unauthorized(error)
    {
        enqueue_legacy_refresh_after_unauthorized(client, auth).await;
    }
    result
}

async fn vehicle_probe_for_auth(
    client: &OwnerApi,
    auth: &CollectionAuth,
    vehicle_id: VehicleId,
) -> Result<bool, CollectorError> {
    wait_for_legacy_refresh_before_owner(auth).await?;
    let CollectionAuth::Legacy { manager, fuse, .. } = auth;
    let result = {
        let mut fuse = fuse.lock().await;
        let mut manager = manager.lock().await;
        client
            .vehicle_probe_with_legacy_auth_fused(&mut manager, &mut fuse, vehicle_id)
            .await
            .map_err(Into::into)
    };
    let result = result.map_err(normalize_sensitive_access_error);
    if let Err(error) = &result
        && is_wrapped_legacy_unauthorized(error)
    {
        enqueue_legacy_refresh_after_unauthorized(client, auth).await;
    }
    result
}

async fn finish_collection(
    store: &HubStore,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
) -> Result<ManualCollectionReport, CollectorError> {
    finish_collection_for_provider(store, cursor_key, collection, CollectorProvider::Legacy).await
}

async fn finish_collection_for_provider(
    store: &HubStore,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
    provider: CollectorProvider,
) -> Result<ManualCollectionReport, CollectorError> {
    for failure in &collection.failures {
        tracing::warn!(
            vehicle_id = failure.vehicle_id.get(),
            error = %failure.error,
            "owner API vehicle collection failed"
        );
    }
    let publication_gate = store.acquire_publication_gate().await?;
    let received_at_ms = current_epoch_millis()?;
    let mut report =
        persist_collection_atomic_for_provider(store, collection, received_at_ms, provider)?;
    let lifecycle =
        materialise_lifecycle_for_collection_provider(store, collection, received_at_ms, provider)?;
    report.drives_closed += lifecycle.drives_closed;
    report.charges_closed += lifecycle.charges_closed;
    report.positions_materialised += lifecycle.positions_materialised;
    report.charge_samples_materialised += lifecycle.charge_samples_materialised;
    report.lifecycle_quarantines += lifecycle.lifecycle_quarantines;
    report.snapshots_published = publish_compatibility_snapshots_for_provider(
        store,
        &publication_gate,
        cursor_key,
        collection,
        received_at_ms,
        provider,
    )?;
    Ok(report)
}

/// Restore the latest complete Fleet snapshot for one configured VIN. Native
/// Fleet Telemetry sends deltas, so restart must seed the in-memory
/// accumulator from this durable allowlisted state.
pub(crate) fn fleet_telemetry_seed_for_vin(
    store: &HubStore,
    vin: &str,
) -> Result<Option<Value>, CollectorError> {
    let (vehicle_id, _, _) = configured_fleet_vehicle_for_vin(store, vin)?;
    let observations = store.current_observations_for_vehicle(vehicle_id)?;
    Ok(observations.into_iter().rev().find_map(|observation| {
        (observation
            .payload
            .get("record_type")
            .and_then(Value::as_str)
            == Some(provider_vehicle_data_record_type(CollectorProvider::Fleet)))
        .then(|| {
            observation
                .payload
                .get("provider_raw_json")
                .and_then(|raw| raw.get("response"))
                .cloned()
        })
        .flatten()
    }))
}

/// Commit one accumulated native Fleet Telemetry snapshot through the same
/// atomic lifecycle and projection path used by ordinary Fleet responses.
/// Returning success is the receiver's permission to acknowledge the vehicle.
pub(crate) async fn persist_fleet_telemetry_snapshot(
    store: &HubStore,
    cursor_key: &CursorKey,
    snapshot: &FleetTelemetrySnapshot,
) -> Result<ManualCollectionReport, CollectorError> {
    let (_, eid, settings) = configured_fleet_vehicle_for_vin(store, &snapshot.vin)?;
    let source_vehicle_id =
        VehicleId::try_from_i64(eid).ok_or(CollectorError::SelectedVehicleMissing)?;
    let stream_id =
        StreamVehicleId::try_from_i64(eid).ok_or(CollectorError::SelectedVehicleMissing)?;
    let source_vehicle_state = snapshot
        .owner_data
        .get("state")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64)
        .unwrap_or("unknown")
        .to_owned();
    let data = VehicleData::from_provider_raw_json(
        source_vehicle_id,
        serde_json::json!({"response": snapshot.owner_data}),
    )?;
    let vehicle = Vehicle {
        id: source_vehicle_id,
        stream_id,
        vin: snapshot.vin.clone(),
        state: source_vehicle_state,
        display_name: None,
        settings,
    };
    finish_collection_for_provider(
        store,
        cursor_key,
        &ManualCollection {
            vehicles: vec![vehicle],
            snapshots: vec![data],
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FleetTelemetrySetupReport {
    pub vehicles_configured: usize,
    pub vehicles_skipped: usize,
    pub vehicles_revoked: usize,
    pub expires_at: u64,
}

/// Send the fixed low-cost Fleet Telemetry field policy through Tesla's local
/// command proxy. VINs come only from configured Hub identities; no discovery
/// or vehicle-data request is made by this operation.
#[cfg(unix)]
pub async fn configure_fleet_telemetry_for_admitted_user(
    store: &HubStore,
    config: &HubConfig,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
) -> Result<FleetTelemetrySetupReport, CollectorError> {
    let mut manager = FleetAuthManager::from_store_for_admitted_user(
        store.clone(),
        &config.data_dir,
        Arc::clone(&admission),
    )?;
    let auth_api = FleetAuthApi::new(
        manager.region(),
        Duration::from_secs(config.collector.request_timeout_seconds),
    )?;
    apply_fleet_telemetry_configuration(store, config, &mut manager, &auth_api, &admission).await
}

#[cfg(unix)]
async fn apply_fleet_telemetry_configuration(
    store: &HubStore,
    config: &HubConfig,
    manager: &mut FleetAuthManager,
    auth_api: &FleetAuthApi,
    admission: &Arc<crate::hub_user_process::AdmittedUserHub>,
) -> Result<FleetTelemetrySetupReport, CollectorError> {
    admission.assert_sensitive_access()?;
    let telemetry = config
        .collector
        .fleet_telemetry
        .as_ref()
        .ok_or(ConfigError::InvalidFleetTelemetry)?;
    let proxy = fleet_command_proxy(config)?.ok_or(ConfigError::InvalidFleetTelemetry)?;
    let certificate =
        read_fleet_proxy_root_certificate(&telemetry.ca_certificate_path, 128 * 1024)?;
    let certificate =
        std::str::from_utf8(&certificate).map_err(|_| FleetApiConfigError::InvalidTelemetryCa)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CollectorError::InvalidReceiptTimestamp)?
        .as_secs();
    let expires_at = now
        .checked_add(FLEET_TELEMETRY_CONFIG_LIFETIME_SECONDS)
        .ok_or(CollectorError::InvalidReceiptTimestamp)?;
    let destination = FleetTelemetryConfigBuilder::new(
        telemetry.hostname.clone(),
        telemetry.port,
        certificate,
        expires_at,
    )
    .with_recommended_fields()
    .build()?;
    let mut enabled_vins = Vec::new();
    let mut disabled_vins = Vec::new();
    for (vehicle_id, _, settings) in store.configured_tesla_vehicles()? {
        let Some((_, Some(vin))) = store.configured_tesla_vehicle_identity(vehicle_id)? else {
            return Err(CollectorError::SelectedVehicleMissing);
        };
        if settings.enabled {
            enabled_vins.push(vin);
        } else {
            disabled_vins.push(VehicleVin::parse(&vin)?);
        }
    }
    manager.refresh_if_due(auth_api, SystemTime::now()).await?;

    let (vehicles_configured, vehicles_skipped) = if enabled_vins.is_empty() {
        (0, 0)
    } else {
        let vins = FleetTelemetryVins::parse(&enabled_vins)?;
        admission.assert_sensitive_access()?;
        let first = proxy
            .configure_fleet_telemetry(
                manager.access_token_for_sensitive_use()?,
                &vins,
                &destination,
            )
            .await;
        let result = if matches!(first, Err(FleetApiError::HttpStatus(401 | 403))) {
            manager.mark_refresh_due();
            manager.refresh_if_due(auth_api, SystemTime::now()).await?;
            admission.assert_sensitive_access()?;
            proxy
                .configure_fleet_telemetry(
                    manager.access_token_for_sensitive_use()?,
                    &vins,
                    &destination,
                )
                .await?
        } else {
            first?
        };
        (result.updated_vehicles, result.skipped_vehicles.len())
    };

    let mut vehicles_revoked = 0;
    for vin in disabled_vins {
        admission.assert_sensitive_access()?;
        let first = proxy
            .remove_fleet_telemetry(manager.access_token_for_sensitive_use()?, &vin)
            .await;
        let removal = if matches!(first, Err(FleetApiError::HttpStatus(401 | 403))) {
            manager.mark_refresh_due();
            manager.refresh_if_due(auth_api, SystemTime::now()).await?;
            admission.assert_sensitive_access()?;
            proxy
                .remove_fleet_telemetry(manager.access_token_for_sensitive_use()?, &vin)
                .await
        } else {
            first
        };
        match removal {
            Ok(()) => vehicles_revoked += 1,
            Err(error) if error.http_status() == Some(404) => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(FleetTelemetrySetupReport {
        vehicles_configured,
        vehicles_skipped,
        vehicles_revoked,
        expires_at,
    })
}

fn configured_fleet_vehicle_for_vin(
    store: &HubStore,
    vin: &str,
) -> Result<(Uuid, i64, crate::hub_pack::ProjectionCarSettings), CollectorError> {
    let mut matched = None;
    for (vehicle_id, eid, settings) in store.configured_tesla_vehicles()? {
        let Some((identity_eid, configured_vin)) =
            store.configured_tesla_vehicle_identity(vehicle_id)?
        else {
            continue;
        };
        if identity_eid == eid && configured_vin.as_deref() == Some(vin) {
            if !settings.enabled {
                continue;
            }
            if matched.is_some() {
                return Err(CollectorError::SelectedVehicleMissing);
            }
            matched = Some((vehicle_id, eid, settings));
        }
    }
    matched.ok_or(CollectorError::SelectedVehicleMissing)
}

/// Configure one clean Hub directly from a bounded legacy token pair. This
/// performs products discovery only: no vehicle-data read, wake, or command.
pub async fn setup_native_vehicle(
    store: &HubStore,
    config: &HubConfig,
    tokens: &OwnerTokens,
    requested_vehicle_id: Option<i64>,
) -> Result<NativeSetupReport, CollectorError> {
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(CollectorError::NativeSetupStoreMismatch);
    }
    if !config.collector.legacy_auth.enabled {
        return Err(CollectorError::NativeSetupLegacyAuthRequired);
    }

    let auth = LegacyAuth::from_access_token(
        tokens.access_token().to_owned(),
        tokens.refresh_token().to_owned(),
    )?;
    let client = OwnerApi::new(
        config
            .collector
            .owner_api_options_for_region(auth.region())?,
    )?;
    setup_native_vehicle_with_client(
        store,
        &config.data_dir,
        &client,
        &auth,
        requested_vehicle_id,
    )
    .await
}

/// Configure every discovered vehicle from one account-wide legacy pair.
/// Discovery is one bounded products request and never wakes a vehicle.
pub async fn setup_native_vehicles(
    store: &HubStore,
    config: &HubConfig,
    tokens: &OwnerTokens,
) -> Result<NativeSetupBatchReport, CollectorError> {
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(CollectorError::NativeSetupStoreMismatch);
    }
    if !config.collector.legacy_auth.enabled {
        return Err(CollectorError::NativeSetupLegacyAuthRequired);
    }
    let auth = LegacyAuth::from_access_token(
        tokens.access_token().to_owned(),
        tokens.refresh_token().to_owned(),
    )?;
    let client = OwnerApi::new(
        config
            .collector
            .owner_api_options_for_region(auth.region())?,
    )?;
    setup_native_vehicles_with_client(store, &config.data_dir, &client, &auth).await
}

/// Configure one vehicle from a bounded Fleet OAuth credential object.
/// Discovery is read-only and never wakes a vehicle.
pub async fn setup_fleet_vehicle(
    store: &HubStore,
    config: &HubConfig,
    credentials: &FleetSetupCredentials,
    admission: &crate::hub_user_process::AdmittedUserHub,
    requested_vehicle_id: Option<i64>,
) -> Result<NativeSetupReport, CollectorError> {
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(CollectorError::NativeSetupStoreMismatch);
    }
    if config.collector.provider != CollectorProvider::Fleet {
        return Err(CollectorError::NativeSetupFleetProviderRequired);
    }
    let client = FleetApi::new(
        credentials.region(),
        Duration::from_secs(config.collector.request_timeout_seconds),
    )?;
    let access_token = credentials.access_token()?;
    admission.assert_sensitive_access()?;
    let vehicles = client.list_vehicles(&access_token).await?;
    ensure_fleet_inventory_contains_configured(store, &vehicles)?;
    let mut vehicle = select_native_setup_vehicle(vehicles, requested_vehicle_id)?;
    let existing = store.configured_tesla_vehicles()?;
    if let Some(settings) = configured_settings_for_discovered_vehicle(store, &existing, &vehicle)?
    {
        vehicle.settings = settings;
    }
    vehicle.settings.use_streaming_api = false;
    let selected_vehicle_id =
        i64::try_from(vehicle.id.get()).map_err(|_| CollectorError::NativeSetupVehicleIdInvalid)?;
    let display_name = vehicle.display_name.clone();
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir)
        .map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    let report = finish_collection_for_provider(
        store,
        &cursor_key,
        &ManualCollection {
            vehicles: vec![vehicle],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await?;
    Ok(NativeSetupReport {
        selected_vehicle_id,
        display_name,
        snapshots_published: report.snapshots_published,
    })
}

/// Configure every vehicle returned by one Fleet account without waking any.
pub async fn setup_fleet_vehicles(
    store: &HubStore,
    config: &HubConfig,
    credentials: &FleetSetupCredentials,
    admission: &crate::hub_user_process::AdmittedUserHub,
) -> Result<NativeSetupBatchReport, CollectorError> {
    if store.database_path() != config.data_dir.join("hub.sqlite") {
        return Err(CollectorError::NativeSetupStoreMismatch);
    }
    if config.collector.provider != CollectorProvider::Fleet {
        return Err(CollectorError::NativeSetupFleetProviderRequired);
    }
    let client = FleetApi::new(
        credentials.region(),
        Duration::from_secs(config.collector.request_timeout_seconds),
    )?;
    let access_token = credentials.access_token()?;
    admission.assert_sensitive_access()?;
    let mut vehicles = client.list_vehicles(&access_token).await?;
    ensure_fleet_inventory_contains_configured(store, &vehicles)?;
    if vehicles.is_empty() {
        return Err(CollectorError::NativeSetupNoVehicles);
    }
    vehicles.sort_by_key(|vehicle| vehicle.id);
    vehicles.dedup_by_key(|vehicle| vehicle.id);
    let existing = store.configured_tesla_vehicles()?;
    for vehicle in &mut vehicles {
        if let Some(settings) =
            configured_settings_for_discovered_vehicle(store, &existing, vehicle)?
        {
            vehicle.settings = settings;
        }
        vehicle.settings.use_streaming_api = false;
    }
    let configured = vehicles
        .iter()
        .map(|vehicle| {
            i64::try_from(vehicle.id.get())
                .map(|vehicle_id| NativeSetupVehicle {
                    vehicle_id,
                    display_name: vehicle.display_name.clone(),
                })
                .map_err(|_| CollectorError::NativeSetupVehicleIdInvalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(&config.data_dir)
        .map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    let report = finish_collection_for_provider(
        store,
        &cursor_key,
        &ManualCollection {
            vehicles,
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
        CollectorProvider::Fleet,
    )
    .await?;
    Ok(NativeSetupBatchReport {
        vehicles: configured,
        snapshots_published: report.snapshots_published,
    })
}

fn configured_settings_for_discovered_vehicle(
    store: &HubStore,
    configured: &[(Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
    discovered: &Vehicle,
) -> Result<Option<crate::hub_pack::ProjectionCarSettings>, CollectorError> {
    let mut matched = None;
    for (hub_vehicle_id, configured_eid, settings) in configured {
        let (_, configured_vin) = store
            .configured_tesla_vehicle_identity(*hub_vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        let identity_matches = *configured_eid as u64 == discovered.id.get()
            || configured_vin
                .as_deref()
                .filter(|vin| !vin.is_empty())
                .is_some_and(|vin| vin.eq_ignore_ascii_case(&discovered.vin));
        if identity_matches {
            if matched.is_some() {
                return Err(CollectorError::FleetSetupInventoryMismatch);
            }
            matched = Some(settings.clone());
        }
    }
    Ok(matched)
}

fn ensure_fleet_inventory_contains_configured(
    store: &HubStore,
    discovered: &[Vehicle],
) -> Result<(), CollectorError> {
    let mut matched_discovered = HashSet::new();
    for (hub_vehicle_id, configured_eid, _) in store.configured_tesla_vehicles()? {
        let (_, configured_vin) = store
            .configured_tesla_vehicle_identity(hub_vehicle_id)?
            .ok_or(StoreError::LineageCatalogConflict)?;
        let matches = discovered
            .iter()
            .enumerate()
            .filter_map(|(index, vehicle)| {
                (vehicle.id.get() == configured_eid as u64
                    || configured_vin
                        .as_deref()
                        .filter(|vin| !vin.is_empty())
                        .is_some_and(|vin| vin.eq_ignore_ascii_case(&vehicle.vin)))
                .then_some(index)
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 || !matched_discovered.insert(matches[0]) {
            return Err(CollectorError::FleetSetupInventoryMismatch);
        }
    }
    Ok(())
}

/// Execute one explicitly confirmed legacy vehicle action. This path never
/// refreshes or retries credentials and is not reachable from the collector.
async fn execute_resident_legacy_vehicle_action(
    store: &HubStore,
    client: &OwnerApi,
    manager: &Arc<tokio::sync::Mutex<LegacyAuthManager>>,
    fuse: &Arc<tokio::sync::Mutex<LegacyAuthFuse>>,
    refresh: &Arc<LegacyRefreshCoordinator>,
    hub_vehicle_id: Uuid,
    action: LegacyVehicleAction,
) -> Result<LegacyVehicleActionReport, ResidentActionExecutionError> {
    refresh
        .wait_for_prior()
        .await
        .map_err(|_| ResidentActionExecutionError::Authentication)?;
    let tesla_eid = store
        .configured_tesla_vehicles()?
        .into_iter()
        .find_map(|(vehicle_id, eid, _)| (vehicle_id == hub_vehicle_id).then_some(eid))
        .ok_or(ResidentActionExecutionError::VehicleMissing)?;
    let vehicle_id =
        VehicleId::try_from_i64(tesla_eid).ok_or(ResidentActionExecutionError::VehicleMissing)?;
    let receipt_id = store.begin_outbound_request(&OutboundRequestStart {
        correlation_id: Uuid::new_v4(),
        vehicle_tesla_id: Some(tesla_eid),
        transport: OutboundRequestTransport::OwnerApi,
        operation: if action == LegacyVehicleAction::Wake {
            OutboundRequestOperation::VehicleWake
        } else {
            OutboundRequestOperation::VehicleCommand
        },
        safety_class: if action == LegacyVehicleAction::Wake {
            OutboundRequestSafetyClass::DirectWakeCommand
        } else {
            OutboundRequestSafetyClass::ExplicitVehicleCommand
        },
        precondition: OutboundRequestPrecondition::NotRequired,
    })?;
    let result = {
        let mut fuse = fuse.lock().await;
        let mut manager = manager.lock().await;
        client
            .execute_vehicle_action_with_legacy_auth_fused(
                &mut manager,
                &mut fuse,
                vehicle_id,
                action,
            )
            .await
    };
    if matches!(
        &result,
        Err(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401)))
    ) {
        refresh
            .enqueue(client.clone(), Arc::clone(manager), Arc::clone(fuse))
            .await;
    }
    let completion = legacy_action_completion(result.as_ref().err());
    store
        .complete_outbound_request(receipt_id, &completion)
        .map_err(ResidentActionExecutionError::CompletionUnknown)?;
    Ok(LegacyVehicleActionReport {
        provider: CollectorProvider::Legacy,
        hub_vehicle_id,
        tesla_eid,
        action,
        result: result?,
        audit_receipt_id: receipt_id.0,
    })
}

async fn execute_resident_fleet_vehicle_action(
    store: &HubStore,
    api: &FleetApi,
    auth_api: &FleetAuthApi,
    command_proxy: Option<&FleetCommandProxy>,
    manager: &Arc<tokio::sync::Mutex<FleetAuthManager>>,
    hub_vehicle_id: Uuid,
    action: LegacyVehicleAction,
) -> Result<LegacyVehicleActionReport, ResidentActionExecutionError> {
    let (tesla_eid, vin) = store
        .configured_tesla_vehicle_identity(hub_vehicle_id)?
        .ok_or(ResidentActionExecutionError::VehicleMissing)?;
    let vin = vin
        .as_deref()
        .ok_or(ResidentActionExecutionError::VehicleMissing)
        .and_then(|vin| {
            VehicleVin::parse(vin).map_err(|_| ResidentActionExecutionError::VehicleMissing)
        })?;
    let mut manager = manager.lock().await;
    manager
        .refresh_if_due(auth_api, SystemTime::now())
        .await
        .map_err(ResidentActionExecutionError::FleetCredential)?;
    let receipt_id = store.begin_outbound_request(&OutboundRequestStart {
        correlation_id: Uuid::new_v4(),
        vehicle_tesla_id: Some(tesla_eid),
        transport: OutboundRequestTransport::FleetApi,
        operation: if action == LegacyVehicleAction::Wake {
            OutboundRequestOperation::VehicleWake
        } else {
            OutboundRequestOperation::VehicleCommand
        },
        safety_class: if action == LegacyVehicleAction::Wake {
            OutboundRequestSafetyClass::DirectWakeCommand
        } else {
            OutboundRequestSafetyClass::ExplicitVehicleCommand
        },
        precondition: OutboundRequestPrecondition::NotRequired,
    })?;
    let access_token = match manager.access_token_for_sensitive_use() {
        Ok(token) => token,
        Err(error) => {
            store
                .complete_outbound_request(
                    receipt_id,
                    &OutboundRequestCompletion {
                        outcome: OutboundRequestOutcome::Cancelled,
                        http_status: None,
                        retry_after_seconds: None,
                    },
                )
                .map_err(ResidentActionExecutionError::CompletionUnknown)?;
            return Err(ResidentActionExecutionError::FleetCredential(error));
        }
    };
    let result = match action {
        LegacyVehicleAction::Wake => api
            .wake(access_token, &vin)
            .await
            .map(|WakeResult { state }| LegacyVehicleActionResult { state: Some(state) }),
        action => {
            let proxy = command_proxy.ok_or(FleetApiError::CommandProxyUnavailable);
            match proxy {
                Ok(proxy) => proxy
                    .execute(access_token, &vin, fleet_command(action)?)
                    .await
                    .map(|FleetCommandResult { .. }| LegacyVehicleActionResult { state: None }),
                Err(error) => Err(error),
            }
        }
    };
    if matches!(result, Err(FleetApiError::HttpStatus(401 | 403))) {
        manager.mark_refresh_due();
    }
    let completion = fleet_action_completion(result.as_ref().err());
    store
        .complete_outbound_request(receipt_id, &completion)
        .map_err(ResidentActionExecutionError::CompletionUnknown)?;
    Ok(LegacyVehicleActionReport {
        provider: CollectorProvider::Fleet,
        hub_vehicle_id,
        tesla_eid,
        action,
        result: result.map_err(ResidentActionExecutionError::FleetProvider)?,
        audit_receipt_id: receipt_id.0,
    })
}

fn fleet_command(
    action: LegacyVehicleAction,
) -> Result<FleetCommand, ResidentActionExecutionError> {
    match action {
        LegacyVehicleAction::Wake => Err(ResidentActionExecutionError::FleetProvider(
            FleetApiError::InvalidCommand,
        )),
        LegacyVehicleAction::ClimateStart => Ok(FleetCommand::ClimateStart),
        LegacyVehicleAction::ClimateStop => Ok(FleetCommand::ClimateStop),
        LegacyVehicleAction::ChargeStart => Ok(FleetCommand::ChargeStart),
        LegacyVehicleAction::ChargeStop => Ok(FleetCommand::ChargeStop),
        LegacyVehicleAction::SetChargeLimit(percent) => {
            Ok(FleetCommand::SetChargeLimit { percent })
        }
        LegacyVehicleAction::Lock => Ok(FleetCommand::Lock),
        LegacyVehicleAction::Unlock => Ok(FleetCommand::Unlock),
        LegacyVehicleAction::FlashLights => Ok(FleetCommand::FlashLights),
        LegacyVehicleAction::HonkHorn => Ok(FleetCommand::HonkHorn),
    }
}

#[derive(Debug, Error)]
enum ResidentActionExecutionError {
    #[error("vehicle command target is not configured")]
    VehicleMissing,
    #[error("vehicle command audit could not start")]
    Audit(#[from] StoreError),
    #[error("resident vehicle credential authority is unavailable")]
    Authentication,
    #[error("vehicle provider rejected the command")]
    Provider(#[from] OwnerApiAuthError),
    #[error("Fleet provider rejected the command")]
    FleetProvider(#[from] FleetApiError),
    #[error("Fleet credential authority is unavailable")]
    FleetCredential(#[from] FleetCredentialError),
    #[error("vehicle command outcome is ambiguous because its audit could not complete")]
    CompletionUnknown(StoreError),
}

fn legacy_action_completion(error: Option<&OwnerApiAuthError>) -> OutboundRequestCompletion {
    let (outcome, http_status, retry_after_seconds) = match error {
        None => (OutboundRequestOutcome::Success, Some(200), None),
        Some(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401)))
        | Some(OwnerApiAuthError::NotSignedIn) => (
            OutboundRequestOutcome::AuthenticationRejected,
            Some(401),
            None,
        ),
        Some(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(status))) => {
            (OutboundRequestOutcome::HttpError, Some(*status), None)
        }
        Some(OwnerApiAuthError::Owner(OwnerApiError::RateLimited {
            retry_after_seconds,
        })) => (
            OutboundRequestOutcome::HttpError,
            Some(429),
            Some(*retry_after_seconds),
        ),
        Some(OwnerApiAuthError::Owner(OwnerApiError::RequestTimeout)) => {
            (OutboundRequestOutcome::Timeout, None, None)
        }
        Some(OwnerApiAuthError::Owner(OwnerApiError::Transport | OwnerApiError::ResponseRead)) => {
            (OutboundRequestOutcome::TransportError, None, None)
        }
        Some(OwnerApiAuthError::Owner(OwnerApiError::ResponseTooLarge)) => {
            (OutboundRequestOutcome::ResponseTooLarge, None, None)
        }
        Some(_) => (OutboundRequestOutcome::ProtocolError, None, None),
    };
    OutboundRequestCompletion {
        outcome,
        http_status,
        retry_after_seconds,
    }
}

fn fleet_action_completion(error: Option<&FleetApiError>) -> OutboundRequestCompletion {
    let (outcome, http_status, retry_after_seconds) = match error {
        None => (OutboundRequestOutcome::Success, Some(200), None),
        Some(FleetApiError::HttpStatus(status @ (401 | 403)))
        | Some(FleetApiError::ProviderHttpStatus {
            status: status @ (401 | 403),
            ..
        }) => (
            OutboundRequestOutcome::AuthenticationRejected,
            Some(*status),
            None,
        ),
        Some(
            FleetApiError::HttpStatus(status) | FleetApiError::ProviderHttpStatus { status, .. },
        ) => (OutboundRequestOutcome::HttpError, Some(*status), None),
        Some(FleetApiError::RateLimited {
            retry_after_seconds,
        }) => (
            OutboundRequestOutcome::HttpError,
            Some(429),
            Some(*retry_after_seconds),
        ),
        Some(FleetApiError::RequestTimeout) => (OutboundRequestOutcome::Timeout, None, None),
        Some(
            FleetApiError::RequestNotSent | FleetApiError::Transport | FleetApiError::ResponseRead,
        ) => (OutboundRequestOutcome::TransportError, None, None),
        Some(FleetApiError::ResponseTooLarge) => {
            (OutboundRequestOutcome::ResponseTooLarge, None, None)
        }
        Some(_) => (OutboundRequestOutcome::ProtocolError, None, None),
    };
    OutboundRequestCompletion {
        outcome,
        http_status,
        retry_after_seconds,
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyVehicleActionReport {
    pub provider: CollectorProvider,
    pub hub_vehicle_id: Uuid,
    pub tesla_eid: i64,
    pub action: LegacyVehicleAction,
    pub result: LegacyVehicleActionResult,
    pub audit_receipt_id: i64,
}

const RESIDENT_CONTROL_PROTOCOL: u8 = 1;
const RESIDENT_CONTROL_REQUEST_BYTES: u64 = 8 * 1024;
const RESIDENT_CONTROL_RESPONSE_BYTES: u64 = 16 * 1024;
const RESIDENT_CONTROL_IO_TIMEOUT: Duration = Duration::from_secs(30);
const RESIDENT_CONTROL_SOCKET_NAME: &str = ".vehicle-control.sock";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResidentVehicleActionRequest {
    protocol: u8,
    hub_vehicle_id: Uuid,
    action: LegacyVehicleAction,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ResidentVehicleActionFailure {
    InvalidRequest,
    VehicleMissing,
    AuthenticationRejected,
    ProviderRejected,
    AuditUnavailable,
    OutcomeAmbiguous,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ResidentVehicleActionResponse {
    Ok { report: LegacyVehicleActionReport },
    Error { code: ResidentVehicleActionFailure },
}

#[derive(Debug, Error)]
pub enum ResidentVehicleActionError {
    #[error("resident Hub vehicle-control service is unavailable")]
    Unavailable,
    #[error("resident Hub vehicle-control request timed out")]
    Timeout,
    #[error("resident Hub vehicle-control protocol failed")]
    Protocol,
    #[error("vehicle command target is not configured")]
    VehicleMissing,
    #[error("resident Hub credentials rejected the vehicle command")]
    AuthenticationRejected,
    #[error("vehicle provider rejected the command")]
    ProviderRejected,
    #[error("vehicle command audit is unavailable")]
    AuditUnavailable,
    #[error("vehicle command outcome is ambiguous; do not repeat it")]
    OutcomeAmbiguous,
}

pub async fn request_resident_vehicle_action(
    data_dir: &Path,
    hub_vehicle_id: Uuid,
    action: LegacyVehicleAction,
) -> Result<LegacyVehicleActionReport, ResidentVehicleActionError> {
    let request = ResidentVehicleActionRequest {
        protocol: RESIDENT_CONTROL_PROTOCOL,
        hub_vehicle_id,
        action,
    };
    let request = serde_json::to_vec(&request).map_err(|_| ResidentVehicleActionError::Protocol)?;
    if request.len() as u64 > RESIDENT_CONTROL_REQUEST_BYTES {
        return Err(ResidentVehicleActionError::Protocol);
    }
    let path = data_dir.join(RESIDENT_CONTROL_SOCKET_NAME);
    let response = tokio::time::timeout(RESIDENT_CONTROL_IO_TIMEOUT, async move {
        let mut socket = UnixStream::connect(path)
            .await
            .map_err(|_| ResidentVehicleActionError::Unavailable)?;
        socket
            .write_all(&request)
            .await
            .map_err(|_| ResidentVehicleActionError::Unavailable)?;
        socket
            .shutdown()
            .await
            .map_err(|_| ResidentVehicleActionError::Unavailable)?;
        let mut response = Vec::new();
        (&mut socket)
            .take(RESIDENT_CONTROL_RESPONSE_BYTES + 1)
            .read_to_end(&mut response)
            .await
            .map_err(|_| ResidentVehicleActionError::Protocol)?;
        if response.len() as u64 > RESIDENT_CONTROL_RESPONSE_BYTES {
            return Err(ResidentVehicleActionError::Protocol);
        }
        serde_json::from_slice::<ResidentVehicleActionResponse>(&response)
            .map_err(|_| ResidentVehicleActionError::Protocol)
    })
    .await
    .map_err(|_| ResidentVehicleActionError::Timeout)??;

    match response {
        ResidentVehicleActionResponse::Ok { report } => Ok(report),
        ResidentVehicleActionResponse::Error { code } => Err(match code {
            ResidentVehicleActionFailure::InvalidRequest => ResidentVehicleActionError::Protocol,
            ResidentVehicleActionFailure::VehicleMissing => {
                ResidentVehicleActionError::VehicleMissing
            }
            ResidentVehicleActionFailure::AuthenticationRejected => {
                ResidentVehicleActionError::AuthenticationRejected
            }
            ResidentVehicleActionFailure::ProviderRejected => {
                ResidentVehicleActionError::ProviderRejected
            }
            ResidentVehicleActionFailure::AuditUnavailable => {
                ResidentVehicleActionError::AuditUnavailable
            }
            ResidentVehicleActionFailure::OutcomeAmbiguous => {
                ResidentVehicleActionError::OutcomeAmbiguous
            }
        }),
    }
}

struct ResidentControlSocket {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl ResidentControlSocket {
    fn bind(data_dir: &Path) -> Result<Self, CollectorError> {
        let path = data_dir.join(RESIDENT_CONTROL_SOCKET_NAME);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_socket()
                    && metadata.uid() == rustix::process::getuid().as_raw()
                    && metadata.nlink() == 1 =>
            {
                std::fs::remove_file(&path).map_err(|_| CollectorError::ResidentControlSocket)?;
            }
            Ok(_) => return Err(CollectorError::ResidentControlSocket),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(CollectorError::ResidentControlSocket),
        }
        let listener =
            UnixListener::bind(&path).map_err(|_| CollectorError::ResidentControlSocket)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| CollectorError::ResidentControlSocket)?;
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| CollectorError::ResidentControlSocket)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(CollectorError::ResidentControlSocket);
        }
        Ok(Self {
            listener,
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    async fn serve(
        self,
        store: HubStore,
        client: OwnerApi,
        manager: Arc<tokio::sync::Mutex<LegacyAuthManager>>,
        fuse: Arc<tokio::sync::Mutex<LegacyAuthFuse>>,
        refresh: Arc<LegacyRefreshCoordinator>,
    ) -> Result<(), CollectorError> {
        loop {
            let (mut socket, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| CollectorError::ResidentControlSocket)?;
            let response = match tokio::time::timeout(
                RESIDENT_CONTROL_IO_TIMEOUT,
                read_resident_vehicle_action_request(&mut socket),
            )
            .await
            {
                Ok(Ok(request)) if request.protocol == RESIDENT_CONTROL_PROTOCOL => {
                    match execute_resident_legacy_vehicle_action(
                        &store,
                        &client,
                        &manager,
                        &fuse,
                        &refresh,
                        request.hub_vehicle_id,
                        request.action,
                    )
                    .await
                    {
                        Ok(report) => ResidentVehicleActionResponse::Ok { report },
                        Err(error) => ResidentVehicleActionResponse::Error {
                            code: classify_resident_action_error(&error),
                        },
                    }
                }
                _ => ResidentVehicleActionResponse::Error {
                    code: ResidentVehicleActionFailure::InvalidRequest,
                },
            };
            let response =
                serde_json::to_vec(&response).map_err(|_| CollectorError::ResidentControlSocket)?;
            if response.len() as u64 > RESIDENT_CONTROL_RESPONSE_BYTES {
                return Err(CollectorError::ResidentControlSocket);
            }
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        }
    }

    async fn serve_fleet(
        self,
        store: HubStore,
        api: FleetApi,
        auth_api: FleetAuthApi,
        command_proxy: Option<FleetCommandProxy>,
        manager: Arc<tokio::sync::Mutex<FleetAuthManager>>,
    ) -> Result<(), CollectorError> {
        loop {
            let (mut socket, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| CollectorError::ResidentControlSocket)?;
            let response = match tokio::time::timeout(
                RESIDENT_CONTROL_IO_TIMEOUT,
                read_resident_vehicle_action_request(&mut socket),
            )
            .await
            {
                Ok(Ok(request)) if request.protocol == RESIDENT_CONTROL_PROTOCOL => {
                    match execute_resident_fleet_vehicle_action(
                        &store,
                        &api,
                        &auth_api,
                        command_proxy.as_ref(),
                        &manager,
                        request.hub_vehicle_id,
                        request.action,
                    )
                    .await
                    {
                        Ok(report) => ResidentVehicleActionResponse::Ok { report },
                        Err(error) => ResidentVehicleActionResponse::Error {
                            code: classify_resident_action_error(&error),
                        },
                    }
                }
                _ => ResidentVehicleActionResponse::Error {
                    code: ResidentVehicleActionFailure::InvalidRequest,
                },
            };
            let response =
                serde_json::to_vec(&response).map_err(|_| CollectorError::ResidentControlSocket)?;
            if response.len() as u64 > RESIDENT_CONTROL_RESPONSE_BYTES {
                return Err(CollectorError::ResidentControlSocket);
            }
            let _ = socket.write_all(&response).await;
            let _ = socket.shutdown().await;
        }
    }
}

impl Drop for ResidentControlSocket {
    fn drop(&mut self) {
        let removable = std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && metadata.uid() == rustix::process::getuid().as_raw()
                && metadata.nlink() == 1
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
        });
        if removable {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

async fn read_resident_vehicle_action_request(
    socket: &mut UnixStream,
) -> Result<ResidentVehicleActionRequest, ()> {
    let mut request = Vec::new();
    socket
        .take(RESIDENT_CONTROL_REQUEST_BYTES + 1)
        .read_to_end(&mut request)
        .await
        .map_err(|_| ())?;
    if request.len() as u64 > RESIDENT_CONTROL_REQUEST_BYTES {
        return Err(());
    }
    serde_json::from_slice(&request).map_err(|_| ())
}

fn classify_resident_action_error(
    error: &ResidentActionExecutionError,
) -> ResidentVehicleActionFailure {
    match error {
        ResidentActionExecutionError::VehicleMissing => {
            ResidentVehicleActionFailure::VehicleMissing
        }
        ResidentActionExecutionError::Audit(_) => ResidentVehicleActionFailure::AuditUnavailable,
        ResidentActionExecutionError::Authentication => {
            ResidentVehicleActionFailure::AuthenticationRejected
        }
        ResidentActionExecutionError::CompletionUnknown(_) => {
            ResidentVehicleActionFailure::OutcomeAmbiguous
        }
        ResidentActionExecutionError::Provider(OwnerApiAuthError::NotSignedIn)
        | ResidentActionExecutionError::Provider(OwnerApiAuthError::Owner(
            OwnerApiError::HttpStatus(401 | 403),
        )) => ResidentVehicleActionFailure::AuthenticationRejected,
        ResidentActionExecutionError::Provider(_) => ResidentVehicleActionFailure::ProviderRejected,
        ResidentActionExecutionError::FleetCredential(_) => {
            ResidentVehicleActionFailure::AuthenticationRejected
        }
        ResidentActionExecutionError::FleetProvider(FleetApiError::HttpStatus(401 | 403)) => {
            ResidentVehicleActionFailure::AuthenticationRejected
        }
        ResidentActionExecutionError::FleetProvider(_) => {
            ResidentVehicleActionFailure::ProviderRejected
        }
    }
}

async fn setup_native_vehicle_with_client(
    store: &HubStore,
    data_dir: &std::path::Path,
    client: &OwnerApi,
    auth: &LegacyAuth,
    requested_vehicle_id: Option<i64>,
) -> Result<NativeSetupReport, CollectorError> {
    let existing = store.configured_tesla_vehicles()?;
    let effective_vehicle_id =
        requested_vehicle_id.or_else(|| (existing.len() == 1).then(|| existing[0].1));
    let vehicles = client.list_vehicles_with_legacy_auth_once(auth).await?;
    let mut vehicle = select_native_setup_vehicle(vehicles, effective_vehicle_id)?;
    let selected_vehicle_id =
        i64::try_from(vehicle.id.get()).map_err(|_| CollectorError::NativeSetupVehicleIdInvalid)?;

    if let Some((_, _, settings)) = existing
        .into_iter()
        .find(|(_, eid, _)| *eid == selected_vehicle_id)
    {
        vehicle.settings = settings;
    }

    let display_name = vehicle.display_name.clone();
    let cursor_key =
        crate::teslamate_credentials::load_or_create_cursor_key(data_dir).map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    let report = finish_collection(
        store,
        &cursor_key,
        &ManualCollection {
            vehicles: vec![vehicle],
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
    )
    .await?;

    Ok(NativeSetupReport {
        selected_vehicle_id,
        display_name,
        snapshots_published: report.snapshots_published,
    })
}

async fn setup_native_vehicles_with_client(
    store: &HubStore,
    data_dir: &Path,
    client: &OwnerApi,
    auth: &LegacyAuth,
) -> Result<NativeSetupBatchReport, CollectorError> {
    let existing = store.configured_tesla_vehicles()?;
    let mut vehicles = client.list_vehicles_with_legacy_auth_once(auth).await?;
    if vehicles.is_empty() {
        return Err(CollectorError::NativeSetupNoVehicles);
    }
    vehicles.sort_by_key(|vehicle| vehicle.id);
    vehicles.dedup_by_key(|vehicle| vehicle.id);
    for vehicle in &mut vehicles {
        if let Some((_, _, settings)) = existing
            .iter()
            .find(|(_, eid, _)| *eid as u64 == vehicle.id.get())
        {
            vehicle.settings = settings.clone();
        }
    }
    let configured = vehicles
        .iter()
        .map(|vehicle| {
            i64::try_from(vehicle.id.get())
                .map(|vehicle_id| NativeSetupVehicle {
                    vehicle_id,
                    display_name: vehicle.display_name.clone(),
                })
                .map_err(|_| CollectorError::NativeSetupVehicleIdInvalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let cursor_key =
        crate::teslamate_credentials::load_or_create_cursor_key(data_dir).map_err(|error| {
            CollectorError::Credential(CredentialError::TeslaMateCredentialFile(error))
        })?;
    let report = finish_collection(
        store,
        &cursor_key,
        &ManualCollection {
            vehicles,
            snapshots: Vec::new(),
            failures: Vec::new(),
        },
    )
    .await?;
    Ok(NativeSetupBatchReport {
        vehicles: configured,
        snapshots_published: report.snapshots_published,
    })
}

fn select_native_setup_vehicle(
    mut vehicles: Vec<Vehicle>,
    requested_vehicle_id: Option<i64>,
) -> Result<Vehicle, CollectorError> {
    if vehicles.is_empty() {
        return Err(CollectorError::NativeSetupNoVehicles);
    }
    if let Some(requested_vehicle_id) = requested_vehicle_id {
        let requested = u64::try_from(requested_vehicle_id)
            .ok()
            .filter(|id| *id > 0)
            .ok_or(CollectorError::NativeSetupVehicleIdInvalid)?;
        return vehicles
            .into_iter()
            .find(|vehicle| vehicle.id.get() == requested)
            .ok_or(CollectorError::NativeSetupVehicleNotFound(
                requested_vehicle_id,
            ));
    }
    if vehicles.len() != 1 {
        return Err(CollectorError::NativeSetupVehicleSelectionRequired {
            discovered: vehicles.len(),
        });
    }
    Ok(vehicles.pop().expect("one discovered vehicle"))
}

struct TerrainWorker {
    wake: mpsc::Sender<()>,
    initialized: Option<oneshot::Receiver<Result<(), ()>>>,
    start: Option<oneshot::Sender<()>>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), CollectorError>>,
}

impl TerrainWorker {
    async fn wait_until_initialized(&mut self) -> Result<(), CollectorError> {
        self.initialized
            .take()
            .ok_or(CollectorError::TerrainWorkerStartup)?
            .await
            .map_err(|_| CollectorError::TerrainWorkerStartup)?
            .map_err(|_| CollectorError::TerrainWorkerStartup)
    }

    fn start(&mut self) -> Result<(), CollectorError> {
        self.start
            .take()
            .ok_or(CollectorError::TerrainWorkerTask)?
            .send(())
            .map_err(|_| CollectorError::TerrainWorkerTask)
    }

    async fn wait_until_exit(&mut self) -> Result<(), CollectorError> {
        (&mut self.task)
            .await
            .map_err(|_| CollectorError::TerrainWorkerTask)?
    }

    async fn shutdown(mut self, already_finished: bool) -> Result<(), CollectorError> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if already_finished {
            return Ok(());
        }
        self.wait_until_exit().await
    }
}

impl Drop for TerrainWorker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
}

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

#[allow(clippy::too_many_arguments)]
async fn run_fleet_supervised_with_access<F>(
    store: &HubStore,
    config: &HubConfig,
    cadence: CollectorCadence,
    api: FleetApi,
    auth_api: FleetAuthApi,
    command_proxy: Option<FleetCommandProxy>,
    manager: Arc<tokio::sync::Mutex<FleetAuthManager>>,
    cursor_key: CursorKey,
    ready: oneshot::Sender<CursorKey>,
    admission: Arc<crate::hub_user_process::AdmittedUserHub>,
    allow_refresh: bool,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    if store.configured_tesla_vehicles()?.is_empty() {
        return Err(CollectorError::SelectedVehicleMissing);
    }
    let collector_lease = store.acquire_supervised_collector_lease(current_epoch_millis()?)?;
    let (collector_state, collector_state_rx) = watch::channel(SupervisedCollectorState::Active);
    let (heartbeat_shutdown, heartbeat_stop) = oneshot::channel();
    let mut heartbeat_task = tokio::spawn(run_supervised_collector_heartbeat(
        store.clone(),
        collector_lease,
        collector_state_rx,
        heartbeat_stop,
        SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
    ));
    let mut terrain_worker = spawn_terrain_worker(
        config.data_dir.clone(),
        config.terrain.clone(),
        cursor_key.clone(),
        Some(Arc::clone(&admission)),
    );
    let terrain_wake = terrain_worker.wake.clone();
    let mut startup_result = terrain_worker.wait_until_initialized().await;
    if startup_result.is_ok() {
        startup_result = admission
            .assert_sensitive_access()
            .map_err(CollectorError::from);
    }
    if startup_result.is_ok() && allow_refresh {
        startup_result = manager
            .lock()
            .await
            .refresh_if_due(&auth_api, SystemTime::now())
            .await
            .map_err(Into::into);
    }
    if startup_result.is_ok() {
        startup_result = terrain_worker.start();
    }
    if startup_result.is_ok() && (heartbeat_task.is_finished() || terrain_worker.task.is_finished())
    {
        startup_result = Err(CollectorError::SupervisedHeartbeatTask);
    }
    let resident_socket = if startup_result.is_ok() && allow_refresh {
        match ResidentControlSocket::bind(&config.data_dir) {
            Ok(socket) => Some(socket),
            Err(error) => {
                startup_result = Err(error);
                None
            }
        }
    } else {
        None
    };
    if startup_result.is_ok() {
        startup_result = ready
            .send(cursor_key.clone())
            .map_err(|_| CollectorError::SupervisedStartupReadyDropped);
    }
    let mut scheduler = VehicleScheduler::new(cadence, Instant::now());
    let (mut collection_result, heartbeat_finished, terrain_finished) = {
        let resident_control_loop = async {
            match resident_socket {
                Some(socket) => {
                    socket
                        .serve_fleet(
                            store.clone(),
                            api.clone(),
                            auth_api.clone(),
                            command_proxy.clone(),
                            Arc::clone(&manager),
                        )
                        .await
                }
                None => std::future::pending::<Result<(), CollectorError>>().await,
            }
        };
        tokio::pin!(resident_control_loop);
        let collection_loop = async {
            startup_result?;
            if config.collector.fleet_telemetry.is_some() {
                return run_fleet_telemetry_maintenance_loop(
                    store,
                    config,
                    &auth_api,
                    &manager,
                    &cursor_key,
                    &terrain_wake,
                    &admission,
                    allow_refresh,
                )
                .await;
            }
            loop {
                admission.assert_sensitive_access()?;
                let configured = store.configured_tesla_vehicles()?;
                if configured.is_empty() {
                    return Err(CollectorError::SelectedVehicleMissing);
                }
                scheduler.apply_control_settings(&configured, Instant::now());
                let now = Instant::now();
                if scheduler.discovery_due(now) {
                    match fleet_list_vehicles_with_auth(&api, &auth_api, &manager, allow_refresh)
                        .await
                    {
                        Ok(vehicles) => {
                            let vehicles = filter_configured_vehicles_for_provider(
                                vehicles,
                                &configured,
                                CollectorProvider::Fleet,
                            );
                            report_successful_owner_api_request(&collector_state, false);
                            let events = scheduler.accept_discovery(vehicles, Instant::now());
                            if !events.is_empty() {
                                persist_discovery_events_with_timeout(
                                    store,
                                    &cursor_key,
                                    &events,
                                    CollectorProvider::Fleet,
                                    cadence.offline_drive_timeout,
                                )
                                .await?;
                            }
                        }
                        Err(error) => {
                            report_terminal_auth_failure(&collector_state, &error);
                            if fleet_collection_must_stop(&error) {
                                return Err(error);
                            }
                            let delay =
                                scheduler.discovery_failed_for_error(&error, Instant::now());
                            tracing::warn!(error = %error, "Fleet discovery failed; backing off");
                            sleep(delay).await;
                            continue;
                        }
                    }
                }

                let offline_due = scheduler.due_offline_state_vehicles(Instant::now());
                if !offline_due.is_empty() {
                    match fleet_list_vehicles_with_auth(&api, &auth_api, &manager, allow_refresh)
                        .await
                    {
                        Ok(discovered) => {
                            let discovered = filter_configured_vehicles_for_provider(
                                discovered,
                                &configured,
                                CollectorProvider::Fleet,
                            );
                            let mut events = Vec::new();
                            for vehicle_id in offline_due {
                                if let Some(vehicle) =
                                    discovered.iter().find(|vehicle| vehicle.id == vehicle_id)
                                {
                                    events.extend(scheduler.accept_vehicle_state(
                                        vehicle_id,
                                        vehicle.state.clone(),
                                        Instant::now(),
                                    ));
                                }
                            }
                            if !events.is_empty() {
                                persist_discovery_events_with_timeout(
                                    store,
                                    &cursor_key,
                                    &events,
                                    CollectorProvider::Fleet,
                                    cadence.offline_drive_timeout,
                                )
                                .await?;
                            }
                        }
                        Err(error) => {
                            if fleet_collection_must_stop(&error) {
                                return Err(error);
                            }
                            for vehicle_id in offline_due {
                                scheduler.vehicle_failed_for_error(
                                    vehicle_id,
                                    &error,
                                    Instant::now(),
                                );
                            }
                        }
                    }
                }

                for vehicle_id in scheduler.due_service_vehicles(Instant::now()) {
                    let Some(vehicle) = scheduler
                        .vehicles()
                        .into_iter()
                        .find(|vehicle| vehicle.id == vehicle_id)
                    else {
                        continue;
                    };
                    match fleet_vehicle_data_with_auth(
                        &api,
                        &auth_api,
                        &manager,
                        &vehicle,
                        allow_refresh,
                    )
                    .await
                    {
                        Ok(snapshot) if snapshot_service_mode(&snapshot) == Some(true) => {
                            scheduler.service_retry(vehicle_id, Instant::now());
                        }
                        Ok(_) => scheduler.service_exited(vehicle_id, Instant::now()),
                        Err(error) => {
                            if fleet_collection_must_stop(&error) {
                                return Err(error);
                            }
                            scheduler.vehicle_failed_for_error(vehicle_id, &error, Instant::now());
                        }
                    }
                }

                let due = scheduler.due_vehicles(Instant::now());
                if !due.is_empty() {
                    let mut snapshots = Vec::new();
                    let mut failures = Vec::new();
                    let mut scheduler_events = Vec::new();
                    for vehicle_id in due {
                        let Some(vehicle) = scheduler
                            .vehicles()
                            .into_iter()
                            .find(|vehicle| vehicle.id == vehicle_id)
                        else {
                            continue;
                        };
                        match fleet_vehicle_data_with_auth(
                            &api,
                            &auth_api,
                            &manager,
                            &vehicle,
                            allow_refresh,
                        )
                        .await
                        {
                            Ok(snapshot) => {
                                report_successful_owner_api_request(&collector_state, false);
                                if snapshot_service_mode(&snapshot) == Some(true) {
                                    scheduler.enter_service_mode(vehicle_id, Instant::now());
                                    force_close_vehicle_for_service_provider(
                                        store,
                                        vehicle_id,
                                        current_epoch_millis()?,
                                        CollectorProvider::Fleet,
                                    )?;
                                }
                                if let Some(event) = scheduler.vehicle_succeeded(
                                    vehicle_id,
                                    poll_phase(&snapshot),
                                    sleep_eligible_with_policy(
                                        &snapshot,
                                        vehicle.settings.req_not_unlocked,
                                    ),
                                    Instant::now(),
                                ) {
                                    scheduler_events.push(event);
                                }
                                snapshots.push(snapshot);
                            }
                            Err(error) => {
                                report_terminal_auth_failure(&collector_state, &error);
                                if fleet_collection_must_stop(&error) {
                                    return Err(error);
                                }
                                scheduler.vehicle_failed_for_error(
                                    vehicle_id,
                                    &error,
                                    Instant::now(),
                                );
                                failures.push(VehicleCollectionFailure {
                                    vehicle_id,
                                    error: fleet_failure_as_owner_error(&error),
                                });
                            }
                        }
                    }
                    let collection = ManualCollection {
                        vehicles: scheduler.vehicles(),
                        snapshots,
                        failures,
                    };
                    let _report = finish_collection_for_provider(
                        store,
                        &cursor_key,
                        &collection,
                        CollectorProvider::Fleet,
                    )
                    .await?;
                    #[cfg(test)]
                    supervised_test_collection_finished(&_report).await;
                    let _ = terrain_wake.try_send(());
                    if !scheduler_events.is_empty() {
                        persist_discovery_events_with_timeout(
                            store,
                            &cursor_key,
                            &scheduler_events,
                            CollectorProvider::Fleet,
                            cadence.offline_drive_timeout,
                        )
                        .await?;
                    }
                }
                let delay = scheduler.delay_until_next_action(Instant::now());
                let vehicles = scheduler.vehicles();
                run_address_enrichment_once_with_runtime_admission(
                    store,
                    config,
                    &cursor_key,
                    &vehicles,
                    current_epoch_millis()?,
                    Some(&admission),
                )
                .await?;
                replay_export_outbox(store, &cursor_key, &vehicles, current_epoch_millis()?)
                    .await?;
                if !delay.is_zero() {
                    sleep(delay.min(CONTROL_SETTINGS_REFRESH)).await;
                }
            }
        };
        tokio::pin!(collection_loop);
        tokio::pin!(shutdown);
        tokio::select! {
            biased;
            result = &mut resident_control_loop => (result, false, false),
            result = &mut collection_loop => (result, false, false),
            () = &mut shutdown => (Ok(()), false, false),
            result = &mut heartbeat_task => {
                let result = result
                    .map_err(|_| CollectorError::SupervisedHeartbeatTask)
                    .and_then(|result| result);
                (result, true, false)
            }
            result = terrain_worker.wait_until_exit() => (result, false, true),
        }
    };
    if let Err(error) = &collection_result {
        report_terminal_auth_failure(&collector_state, error);
    }
    let terrain_result = terrain_worker.shutdown(terrain_finished).await;
    if collection_result.is_ok() {
        collection_result = terrain_result;
    }
    if !heartbeat_finished {
        let _ = heartbeat_shutdown.send(());
        let heartbeat_result = heartbeat_task
            .await
            .map_err(|_| CollectorError::SupervisedHeartbeatTask)
            .and_then(|result| result);
        if collection_result.is_ok() {
            collection_result = heartbeat_result;
        }
    }
    let release_result = store
        .release_supervised_collector_lease(collector_lease)
        .map_err(CollectorError::from);
    if collection_result.is_ok() {
        collection_result = release_result;
    }
    collection_result
}

/// Keep Fleet credentials, deferred publication, and local enrichment healthy
/// while native Fleet Telemetry supplies observations. This mode deliberately
/// makes no Fleet list or vehicle-data requests and has no polling fallback.
#[allow(clippy::too_many_arguments)]
async fn run_fleet_telemetry_maintenance_loop(
    store: &HubStore,
    config: &HubConfig,
    auth_api: &FleetAuthApi,
    manager: &Arc<tokio::sync::Mutex<FleetAuthManager>>,
    cursor_key: &CursorKey,
    terrain_wake: &mpsc::Sender<()>,
    admission: &Arc<crate::hub_user_process::AdmittedUserHub>,
    allow_refresh: bool,
) -> Result<(), CollectorError> {
    const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
    tracing::info!("Fleet Telemetry push collection active; paid vehicle-data polling is disabled");
    let mut next_configuration_renewal = Instant::now();
    loop {
        admission.assert_sensitive_access()?;
        let vehicles = configured_fleet_telemetry_vehicles(store)?;
        if allow_refresh && Instant::now() >= next_configuration_renewal {
            let result = {
                let mut manager = manager.lock().await;
                apply_fleet_telemetry_configuration(
                    store,
                    config,
                    &mut manager,
                    auth_api,
                    admission,
                )
                .await
            };
            let retry_after = match result {
                Ok(report) if report.vehicles_skipped == 0 => {
                    tracing::info!(
                        vehicles_configured = report.vehicles_configured,
                        vehicles_revoked = report.vehicles_revoked,
                        expires_at = report.expires_at,
                        "Fleet Telemetry configuration renewed"
                    );
                    FLEET_TELEMETRY_CONFIG_RENEWAL_INTERVAL
                }
                Ok(report) => {
                    tracing::warn!(
                        vehicles_configured = report.vehicles_configured,
                        vehicles_skipped = report.vehicles_skipped,
                        vehicles_revoked = report.vehicles_revoked,
                        "Fleet Telemetry configuration skipped vehicles; retrying"
                    );
                    FLEET_TELEMETRY_CONFIG_RETRY_INTERVAL
                }
                Err(error) => {
                    tracing::warn!(error = %error, "Fleet Telemetry configuration renewal failed; existing push collection remains active");
                    FLEET_TELEMETRY_CONFIG_RETRY_INTERVAL
                }
            };
            next_configuration_renewal = Instant::now() + retry_after;
        }
        if allow_refresh {
            let refresh = manager
                .lock()
                .await
                .refresh_if_due(auth_api, SystemTime::now())
                .await;
            if let Err(error) = refresh {
                if let Some(delay) = fleet_refresh_retry_delay(&error) {
                    tracing::warn!(
                        retry_seconds = delay.as_secs(),
                        "Fleet token refresh request was not sent; push collection remains active"
                    );
                    sleep(delay).await;
                    continue;
                }
                return Err(error.into());
            }
        }
        if vehicles.is_empty() {
            sleep(MAINTENANCE_INTERVAL).await;
            continue;
        }
        let now_ms = current_epoch_millis()?;
        run_address_enrichment_once_with_runtime_admission(
            store,
            config,
            cursor_key,
            &vehicles,
            now_ms,
            Some(admission),
        )
        .await?;
        replay_export_outbox(store, cursor_key, &vehicles, now_ms).await?;
        let _ = terrain_wake.try_send(());
        sleep(MAINTENANCE_INTERVAL).await;
    }
}

fn fleet_refresh_retry_delay(error: &FleetCredentialError) -> Option<Duration> {
    matches!(
        error,
        FleetCredentialError::Api(FleetApiError::RequestNotSent)
    )
    .then_some(FLEET_REFRESH_REQUEST_NOT_SENT_RETRY)
}

fn configured_fleet_telemetry_vehicles(store: &HubStore) -> Result<Vec<Vehicle>, CollectorError> {
    let mut vehicles = Vec::new();
    for (vehicle_id, eid, settings) in store.configured_tesla_vehicles()? {
        if !settings.enabled {
            continue;
        }
        let Some((identity_eid, vin)) = store.configured_tesla_vehicle_identity(vehicle_id)? else {
            return Err(CollectorError::SelectedVehicleMissing);
        };
        if identity_eid != eid {
            return Err(CollectorError::SelectedVehicleMissing);
        }
        let vin = vin.ok_or(CollectorError::SelectedVehicleMissing)?;
        let id = VehicleId::try_from_i64(eid).ok_or(CollectorError::SelectedVehicleMissing)?;
        let stream_id = crate::owner_api::StreamVehicleId::try_from_i64(eid)
            .ok_or(CollectorError::SelectedVehicleMissing)?;
        let materialised = store.materialised_car_for_vehicle(vehicle_id)?;
        vehicles.push(Vehicle {
            id,
            stream_id,
            vin,
            state: "online".to_owned(),
            display_name: materialised.map(|car| car.name),
            settings,
        });
    }
    Ok(vehicles)
}

fn filter_configured_vehicles(
    vehicles: Vec<Vehicle>,
    configured: &[(uuid::Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
) -> Vec<Vehicle> {
    filter_configured_vehicles_for_provider(vehicles, configured, CollectorProvider::Legacy)
}

fn filter_configured_vehicles_for_provider(
    vehicles: Vec<Vehicle>,
    configured: &[(uuid::Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
    provider: CollectorProvider,
) -> Vec<Vehicle> {
    vehicles
        .into_iter()
        .filter_map(|mut vehicle| {
            configured
                .iter()
                .find(|(_, eid, _)| vehicle.id.get() == *eid as u64)
                .map(|(_, _, settings)| {
                    vehicle.settings = settings.clone();
                    if provider == CollectorProvider::Fleet {
                        vehicle.settings.use_streaming_api = false;
                    }
                    vehicle
                })
        })
        .collect()
}

async fn run_supervised_with_access<F>(
    store: &HubStore,
    config: &HubConfig,
    cadence: CollectorCadence,
    client: OwnerApi,
    auth: CollectionAuth,
    stream_endpoint: String,
    cursor_key: CursorKey,
    ready: Option<oneshot::Sender<CursorKey>>,
    runtime_admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
    shutdown: F,
) -> Result<(), CollectorError>
where
    F: Future<Output = ()>,
{
    if store.configured_tesla_vehicles()?.is_empty() {
        return Err(CollectorError::SelectedVehicleMissing);
    }
    let collector_lease = store.acquire_supervised_collector_lease(current_epoch_millis()?)?;
    let (collector_state, collector_state_rx) = watch::channel(SupervisedCollectorState::Active);
    let (heartbeat_shutdown, heartbeat_stop) = oneshot::channel();
    let mut heartbeat_task = tokio::spawn(run_supervised_collector_heartbeat(
        store.clone(),
        collector_lease,
        collector_state_rx,
        heartbeat_stop,
        SUPERVISED_COLLECTOR_HEARTBEAT_INTERVAL,
    ));
    let mut terrain_worker = spawn_terrain_worker(
        config.data_dir.clone(),
        config.terrain.clone(),
        cursor_key.clone(),
        runtime_admission.clone(),
    );
    let terrain_wake = terrain_worker.wake.clone();
    let mut startup_result = terrain_worker.wait_until_initialized().await;
    if startup_result.is_ok() && heartbeat_task.is_finished() {
        startup_result = Err(CollectorError::SupervisedHeartbeatTask);
    }
    if startup_result.is_ok() {
        startup_result = assert_runtime_sensitive_access(runtime_admission.as_deref());
    }
    if startup_result.is_ok()
        && let CollectionAuth::Legacy { manager, .. } = &auth
    {
        startup_result = manager
            .lock()
            .await
            .assert_sensitive_access()
            .map_err(CollectorError::from)
            .map_err(normalize_sensitive_access_error);
    }
    if startup_result.is_ok() {
        startup_result = terrain_worker.start();
    }
    if startup_result.is_ok() {
        startup_result = refresh_restored_legacy_auth(&client, &auth).await;
    }
    if startup_result.is_ok() && heartbeat_task.is_finished() {
        startup_result = Err(CollectorError::SupervisedHeartbeatTask);
    }
    if startup_result.is_ok() && terrain_worker.task.is_finished() {
        startup_result = Err(CollectorError::TerrainWorkerTask);
    }
    if startup_result.is_ok() {
        startup_result = assert_runtime_sensitive_access(runtime_admission.as_deref());
    }
    if startup_result.is_ok()
        && let CollectionAuth::Legacy { manager, .. } = &auth
    {
        startup_result = manager
            .lock()
            .await
            .assert_sensitive_access()
            .map_err(CollectorError::from)
            .map_err(normalize_sensitive_access_error);
    }
    let resident_control_authority = match &auth {
        CollectionAuth::Legacy {
            manager,
            fuse,
            refresh,
            allow_refresh: true,
            ..
        } => Some((Arc::clone(manager), Arc::clone(fuse), Arc::clone(refresh))),
        CollectionAuth::Legacy { .. } => None,
    };
    let resident_control_socket = if startup_result.is_ok() && resident_control_authority.is_some()
    {
        match ResidentControlSocket::bind(&config.data_dir) {
            Ok(socket) => Some(socket),
            Err(error) => {
                startup_result = Err(error);
                None
            }
        }
    } else {
        None
    };
    if startup_result.is_ok()
        && let Some(ready) = ready
    {
        startup_result = ready
            .send(cursor_key.clone())
            .map_err(|_| CollectorError::SupervisedStartupReadyDropped);
    }
    let mut scheduler = VehicleScheduler::new(cadence, Instant::now());
    let mut streams = Vec::new();
    // An Owner API response cannot establish that the Streaming API accepted
    // the same credential. Once a stream rejects authentication, keep the
    // durable collector state terminal until a later authenticated stream
    // handshake proves recovery. A restart reloads an explicitly replaced
    // credential and establishes its own stream-authentication proof.
    let mut stream_authentication_rejected = false;
    let mut logical_legacy_sign_out = false;
    let mut stream_projection_car_ids = HashMap::new();

    let (mut collection_result, heartbeat_finished, terrain_finished) = {
        let resident_control_loop = async {
            match (resident_control_socket, resident_control_authority) {
                (Some(socket), Some((manager, fuse, refresh))) => {
                    socket
                        .serve(store.clone(), client.clone(), manager, fuse, refresh)
                        .await
                }
                _ => std::future::pending::<Result<(), CollectorError>>().await,
            }
        };
        tokio::pin!(resident_control_loop);
        let collection_loop = async {
            startup_result?;
            'collection: loop {
                if logical_legacy_sign_out {
                    // The process remains alive, but the blown legacy fuse
                    // authoritatively fences every later Owner/stream action.
                    sleep(LEGACY_REFRESH_RETRY).await;
                    continue;
                }
                let configured_vehicles = store.configured_tesla_vehicles()?;
                if configured_vehicles.is_empty() {
                    return Err(CollectorError::SelectedVehicleMissing);
                }
                for vehicle_id in
                    scheduler.apply_control_settings(&configured_vehicles, Instant::now())
                {
                    disconnect_vehicle_stream(&mut streams, vehicle_id).await;
                }
                let stream_drain = drain_stream_events_with_cache(
                    store,
                    &mut scheduler,
                    &mut streams,
                    &mut stream_projection_car_ids,
                )
                .await?;
                report_stream_authentication_transition(
                    &collector_state,
                    &mut stream_authentication_rejected,
                    stream_drain.transition,
                );
                refresh_after_stream_authentication_rejection(
                    &client,
                    &auth,
                    stream_drain.transition,
                )
                .await?;
                if let Some(error) = stream_drain.terminal_error {
                    return Err(error);
                }
                if stream_drain.backlog {
                    // Drain a noisy stream to below the bounded queue before
                    // beginning Owner API, projection, or enrichment work.
                    // This keeps collection work from starving the receiver.
                    tokio::task::yield_now().await;
                    continue;
                }
                let now = Instant::now();
                if scheduler.discovery_due(now) {
                    match list_vehicles_for_auth(&client, &auth).await {
                        Ok(vehicles) => {
                            let vehicles =
                                filter_configured_vehicles(vehicles, &configured_vehicles);
                            report_successful_owner_api_request(
                                &collector_state,
                                stream_authentication_rejected,
                            );
                            let events = scheduler.accept_discovery(vehicles, Instant::now());
                            disconnect_streams_not_in_scheduler(&mut streams, &scheduler).await;
                            if !events.is_empty() {
                                persist_discovery_events_with_timeout(
                                    store,
                                    &cursor_key,
                                    &events,
                                    CollectorProvider::Legacy,
                                    cadence.offline_drive_timeout,
                                )
                                .await?;
                            }
                            for vehicle in scheduler.vehicles() {
                                if vehicle.is_online()
                                    && vehicle.settings.enabled
                                    && scheduler.should_start_stream(vehicle.id)
                                {
                                    ensure_vehicle_stream(
                                        store,
                                        &mut streams,
                                        vehicle.id,
                                        vehicle.stream_id,
                                        &auth,
                                        &client,
                                        cadence.stream_health_timeout,
                                        Some(&stream_endpoint),
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            if observer_auth_failure(&auth, &error)
                                || must_stop_supervised_collection(&error)
                            {
                                return Err(error);
                            }
                            report_terminal_auth_failure(&collector_state, &error);
                            if matches!(
                                error,
                                CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
                            ) {
                                logical_legacy_sign_out = true;
                                stop_and_clear_manual_probe_streams(&mut streams).await;
                                continue 'collection;
                            }
                            let now = Instant::now();
                            let delay = scheduler.discovery_failed_for_error(&error, now);
                            tracing::warn!(error = %error, "owner API discovery failed; backing off");
                            sleep(delay).await;
                            continue;
                        }
                    }
                }

                for vehicle_id in scheduler.due_offline_state_vehicles(Instant::now()) {
                    match vehicle_state_for_auth(&client, &auth, vehicle_id).await {
                        Ok(state) => {
                            report_successful_owner_api_request(
                                &collector_state,
                                stream_authentication_rejected,
                            );
                            let events =
                                scheduler.accept_vehicle_state(vehicle_id, state, Instant::now());
                            if !events.is_empty() {
                                persist_discovery_events_with_timeout(
                                    store,
                                    &cursor_key,
                                    &events,
                                    CollectorProvider::Legacy,
                                    cadence.offline_drive_timeout,
                                )
                                .await?;
                            }
                        }
                        Err(error) => {
                            if observer_auth_failure(&auth, &error)
                                || must_stop_supervised_collection(&error)
                            {
                                return Err(error);
                            }
                            report_terminal_auth_failure(&collector_state, &error);
                            if matches!(
                                error,
                                CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
                            ) {
                                logical_legacy_sign_out = true;
                                stop_and_clear_manual_probe_streams(&mut streams).await;
                                continue 'collection;
                            }
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
                    match vehicle_probe_for_auth(&client, &auth, vehicle_id).await {
                        Ok(true) => {
                            report_successful_owner_api_request(
                                &collector_state,
                                stream_authentication_rejected,
                            );
                            scheduler.service_retry(vehicle_id, Instant::now());
                        }
                        Ok(false) => {
                            report_successful_owner_api_request(
                                &collector_state,
                                stream_authentication_rejected,
                            );
                            scheduler.service_exited(vehicle_id, Instant::now());
                            if let Some(vehicle) = scheduler
                                .vehicles()
                                .into_iter()
                                .find(|vehicle| vehicle.id == vehicle_id)
                                && vehicle.settings.use_streaming_api
                            {
                                ensure_vehicle_stream(
                                    store,
                                    &mut streams,
                                    vehicle_id,
                                    vehicle.stream_id,
                                    &auth,
                                    &client,
                                    cadence.stream_health_timeout,
                                    Some(&stream_endpoint),
                                );
                            }
                        }
                        Err(error) if is_vehicle_in_service(&error) => {
                            report_successful_owner_api_request(
                                &collector_state,
                                stream_authentication_rejected,
                            );
                            scheduler.service_retry(vehicle_id, Instant::now());
                        }
                        Err(error) => {
                            if observer_auth_failure(&auth, &error)
                                || must_stop_supervised_collection(&error)
                            {
                                return Err(error);
                            }
                            report_terminal_auth_failure(&collector_state, &error);
                            if matches!(
                                error,
                                CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
                            ) {
                                logical_legacy_sign_out = true;
                                stop_and_clear_manual_probe_streams(&mut streams).await;
                                continue 'collection;
                            }
                            scheduler.vehicle_failed_for_error(vehicle_id, &error, Instant::now())
                        }
                    }
                }
                if !due.is_empty() {
                    let mut snapshots = Vec::new();
                    let mut failures = Vec::new();
                    let mut scheduler_events = Vec::new();
                    for vehicle_id in due {
                        let power_gate = if scheduler.requires_live_stream_power_gate(vehicle_id) {
                            let Some(power_gate) = streams
                                .iter()
                                .find(|stream| stream.vehicle_id == vehicle_id)
                                .map(|stream| Arc::clone(&stream.power_gate))
                            else {
                                scheduler.vehicle_failed_for_error(
                                    vehicle_id,
                                    &CollectorError::OwnerApi(
                                        OwnerApiError::StreamPowerNotConfirmed,
                                    ),
                                    Instant::now(),
                                );
                                continue;
                            };
                            Some(power_gate)
                        } else {
                            None
                        };
                        match vehicle_data_for_auth(
                            &client,
                            &auth,
                            vehicle_id,
                            power_gate.as_deref(),
                        )
                        .await
                        {
                            Ok(snapshot) => {
                                report_successful_owner_api_request(
                                    &collector_state,
                                    stream_authentication_rejected,
                                );
                                if snapshot_service_mode(&snapshot) == Some(true) {
                                    scheduler.enter_service_mode(vehicle_id, Instant::now());
                                    disconnect_vehicle_stream(&mut streams, vehicle_id).await;
                                }
                                if let Some(vehicle) = scheduler
                                    .vehicles()
                                    .into_iter()
                                    .find(|vehicle| vehicle.id == vehicle_id)
                                    && vehicle.is_online()
                                    && vehicle.settings.enabled
                                    && vehicle.settings.use_streaming_api
                                {
                                    ensure_vehicle_stream(
                                        store,
                                        &mut streams,
                                        vehicle.id,
                                        vehicle.stream_id,
                                        &auth,
                                        &client,
                                        cadence.stream_health_timeout,
                                        Some(&stream_endpoint),
                                    );
                                }
                                let req_not_unlocked = scheduler
                                    .vehicles()
                                    .into_iter()
                                    .find(|vehicle| vehicle.id == vehicle_id)
                                    .is_none_or(|vehicle| vehicle.settings.req_not_unlocked);
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
                                report_successful_owner_api_request(
                                    &collector_state,
                                    stream_authentication_rejected,
                                );
                                scheduler.enter_service_mode(vehicle_id, Instant::now());
                                disconnect_vehicle_stream(&mut streams, vehicle_id).await;
                                force_close_vehicle_for_service(
                                    store,
                                    vehicle_id,
                                    current_epoch_millis()?,
                                )?;
                            }
                            Err(error) => {
                                if observer_auth_failure(&auth, &error)
                                    || must_stop_supervised_collection(&error)
                                {
                                    return Err(error);
                                }
                                report_terminal_auth_failure(&collector_state, &error);
                                if matches!(
                                    error,
                                    CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
                                ) {
                                    logical_legacy_sign_out = true;
                                    stop_and_clear_manual_probe_streams(&mut streams).await;
                                    continue 'collection;
                                }
                                scheduler.vehicle_failed_for_error(
                                    vehicle_id,
                                    &error,
                                    Instant::now(),
                                );
                                failures.push(VehicleCollectionFailure {
                                    vehicle_id,
                                    error: match error {
                                        CollectorError::OwnerApi(error) => error,
                                        CollectorError::OwnerApiAuth(_) => {
                                            OwnerApiError::LegacyAuth
                                        }
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
                    #[cfg(test)]
                    supervised_test_collection_finished(&report).await;
                    let _ = terrain_wake.try_send(());
                    if !scheduler_events.is_empty() {
                        persist_discovery_events_with_timeout(
                            store,
                            &cursor_key,
                            &scheduler_events,
                            CollectorProvider::Legacy,
                            cadence.offline_drive_timeout,
                        )
                        .await?;
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
                assert_runtime_sensitive_access(runtime_admission.as_deref())?;
                run_address_enrichment_once_with_runtime_admission(
                    store,
                    config,
                    &cursor_key,
                    &vehicles,
                    current_epoch_millis()?,
                    runtime_admission.as_ref(),
                )
                .await?;
                replay_export_outbox(store, &cursor_key, &vehicles, current_epoch_millis()?)
                    .await?;
                if !delay.is_zero() {
                    let cap = collection_sleep_cap(!streams.is_empty());
                    sleep(delay.min(cap)).await;
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), CollectorError>(())
        };
        tokio::pin!(collection_loop);
        let legacy_refresh_failure = wait_for_legacy_refresh_sensitive_failure(&auth);
        tokio::pin!(legacy_refresh_failure);
        tokio::pin!(shutdown);
        tokio::select! {
            biased;
            result = &mut legacy_refresh_failure => (result, false, false),
            result = &mut resident_control_loop => (result, false, false),
            result = &mut collection_loop => (result, false, false),
            () = &mut shutdown => (Ok(()), false, false),
            result = &mut heartbeat_task => {
                let result = result
                    .map_err(|_| CollectorError::SupervisedHeartbeatTask)
                    .and_then(|result| result);
                (result, true, false)
            }
            result = terrain_worker.wait_until_exit() => {
                (result, false, true)
            }
        }
    };

    // Dropping the active collection future stops new work. If it already
    // observed authority loss, close sockets before joining the refresh task.
    // Otherwise join first: a concurrent helper/persistence failure must win
    // over an orderly shutdown so the sender cannot issue an unsubscribe after
    // that authority has been revoked.
    let initial_sensitive_failure = collection_result
        .as_ref()
        .is_err_and(is_sensitive_access_failure);
    if initial_sensitive_failure {
        let error = collection_result
            .as_ref()
            .expect_err("sensitive collection result");
        report_terminal_auth_failure(&collector_state, error);
        abort_and_clear_manual_probe_streams_without_egress(&mut streams).await;
    }
    let refresh_result = wait_for_legacy_refresh_before_owner(&auth).await;
    shutdown_legacy_refresh(&auth).await;
    if let Err(error) = refresh_result {
        collection_result = Err(error);
    }
    if !initial_sensitive_failure {
        if let Err(error) = &collection_result
            && is_sensitive_access_failure(error)
        {
            report_terminal_auth_failure(&collector_state, error);
            abort_and_clear_manual_probe_streams_without_egress(&mut streams).await;
        } else {
            stop_and_clear_manual_probe_streams(&mut streams).await;
        }
    }
    let terrain_result = terrain_worker.shutdown(terrain_finished).await;
    if collection_result.is_ok() {
        collection_result = terrain_result;
    }
    if !heartbeat_finished {
        let _ = heartbeat_shutdown.send(());
        let heartbeat_result = heartbeat_task
            .await
            .map_err(|_| CollectorError::SupervisedHeartbeatTask)
            .and_then(|result| result);
        if collection_result.is_ok() {
            collection_result = heartbeat_result;
        }
    }
    let release_result = store
        .release_supervised_collector_lease(collector_lease)
        .map_err(CollectorError::from);
    if collection_result.is_ok() {
        collection_result = release_result;
    }
    collection_result
}

async fn run_supervised_collector_heartbeat(
    store: HubStore,
    lease: SupervisedCollectorLease,
    mut state: watch::Receiver<SupervisedCollectorState>,
    mut shutdown: oneshot::Receiver<()>,
    interval: Duration,
) -> Result<(), CollectorError> {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                renew_supervised_collector_heartbeat(
                    &store,
                    lease,
                    *state.borrow_and_update(),
                )?;
            }
            changed = state.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                renew_supervised_collector_heartbeat(
                    &store,
                    lease,
                    *state.borrow_and_update(),
                )?;
            }
            _ = &mut shutdown => return Ok(()),
        }
    }
}

fn renew_supervised_collector_heartbeat(
    store: &HubStore,
    lease: SupervisedCollectorLease,
    state: SupervisedCollectorState,
) -> Result<(), CollectorError> {
    match store.heartbeat_supervised_collector_lease(lease, state, current_epoch_millis()?) {
        Ok(()) => Ok(()),
        Err(StoreError::SupervisedCollectorLeaseWrite(error)) => {
            tracing::warn!(%error, "supervised collector heartbeat deferred by local SQLite contention");
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn set_supervised_collector_state(
    sender: &watch::Sender<SupervisedCollectorState>,
    next: SupervisedCollectorState,
) {
    sender.send_if_modified(|current| {
        if *current == next {
            false
        } else {
            *current = next;
            true
        }
    });
}

fn report_terminal_auth_failure(
    state: &watch::Sender<SupervisedCollectorState>,
    error: &CollectorError,
) {
    if is_terminal_auth_failure(error) {
        set_supervised_collector_state(state, SupervisedCollectorState::AuthenticationTerminal);
    }
}

fn report_successful_owner_api_request(
    state: &watch::Sender<SupervisedCollectorState>,
    stream_authentication_rejected: bool,
) {
    if !stream_authentication_rejected {
        set_supervised_collector_state(state, SupervisedCollectorState::Active);
    }
}

fn report_stream_authentication_transition(
    state: &watch::Sender<SupervisedCollectorState>,
    stream_authentication_rejected: &mut bool,
    transition: StreamAuthenticationTransition,
) {
    match transition {
        StreamAuthenticationTransition::NoChange => {}
        StreamAuthenticationTransition::Rejected => {
            *stream_authentication_rejected = true;
            set_supervised_collector_state(state, SupervisedCollectorState::AuthenticationTerminal);
        }
        StreamAuthenticationTransition::Authenticated => {
            *stream_authentication_rejected = false;
            set_supervised_collector_state(state, SupervisedCollectorState::Active);
        }
    }
}

fn is_terminal_auth_failure(error: &CollectorError) -> bool {
    matches!(
        error,
        CollectorError::SensitiveAccessUnavailable
            | CollectorError::OwnerApi(OwnerApiError::HttpStatus(401 | 403))
            | CollectorError::FleetApi(FleetApiError::HttpStatus(401 | 403))
            | CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
            | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(
                403
            )))
    ) || matches!(error, CollectorError::FleetCredential(error) if error.is_sensitive_access_failure())
}

async fn replay_export_outbox(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
) -> Result<usize, CollectorError> {
    let publication_gate = store.acquire_publication_gate().await?;
    let Some(claim) = store.claim_export_outbox(now_ms)? else {
        return Ok(0);
    };
    if store.vehicle_has_v2_base(claim.vehicle_id)? {
        let Some(sync_claim) = store.claim_sync_mutations(claim.vehicle_id, now_ms, 10_000)? else {
            if store.has_unpublished_sync_mutations(claim.vehicle_id)? {
                store.release_export_outbox(&claim)?;
                return Ok(0);
            }
            store.complete_export_outbox(&claim)?;
            return Ok(0);
        };
        return match publish_v2_delta(store, cursor_key, &sync_claim) {
            Ok(()) => {
                // A single outbox revision can represent more mutations than
                // one bounded claim. Keep it scheduled until every mutation
                // is durably published; otherwise the first 10,000 rows would
                // consume the outbox row and strand the remainder forever.
                if store.has_unpublished_sync_mutations(claim.vehicle_id)? {
                    store.release_export_outbox(&claim)?;
                } else {
                    store.complete_export_outbox(&claim)?;
                }
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
    let Some(vehicle) = vehicles
        .iter()
        .find(|vehicle| vehicle.id.get() == source_vehicle_id)
    else {
        store.fail_export_outbox(&claim, "vehicle_not_discovered", now_ms)?;
        return Ok(0);
    };
    let collection = ManualCollection {
        vehicles: vec![vehicle.clone()],
        snapshots: vec![],
        failures: vec![],
    };
    match publish_compatibility_snapshots(store, &publication_gate, cursor_key, &collection, now_ms)
    {
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
    compact_v2_lineage_if_needed(store, cursor_key, claim.vehicle_id)?;
    let (base_snapshot_id, head_sequence, parent_digest) = store
        .v2_head(claim.vehicle_id)?
        .ok_or_else(|| CollectorError::Store(StoreError::LineageCatalogConflict))?;
    let from_sequence = u64::try_from(head_sequence)
        .map_err(|_| CollectorError::Store(StoreError::InvalidStoredSequence))?;
    let to_sequence = from_sequence
        .checked_add(
            u64::try_from(claim.mutations.len()).map_err(|_| StoreError::SequenceTooLarge)?,
        )
        .ok_or_else(|| CollectorError::Store(StoreError::SequenceTooLarge))?;
    let binding = store.v2_projection_binding(claim.vehicle_id)?;
    let sequence = SequenceRange {
        from_exclusive: from_sequence,
        to_inclusive: to_sequence,
    };
    let ordinal = store.next_v2_pack_ordinal(base_snapshot_id)?;
    let delta =
        store.projection_delta_for_mutations(claim, binding.clone(), sequence, parent_digest)?;
    let request = ProjectionDeltaPackRequest {
        pack_id: Uuid::new_v4(),
        snapshot_id: base_snapshot_id,
        ordinal,
        delta: &delta,
    };
    let built = ProjectionPackWriter::new(store.packs_dir()).write_delta(&request)?;
    let chain_digest = canonical_delta_chain_digest(parent_digest, built.metadata.sha256);
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
    store.commit_v2_delta_claim(claim, &lineage_delta, cursor_key, &terminal_cursor)?;
    Ok(())
}

fn compact_v2_lineage_if_needed(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
) -> Result<(), CollectorError> {
    compact_v2_lineage_if_needed_with_limits(
        store,
        cursor_key,
        vehicle_id,
        ProtocolLimits::default(),
    )
}

fn compact_v2_lineage_if_needed_with_limits(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicle_id: Uuid,
    limits: ProtocolLimits,
) -> Result<(), CollectorError> {
    let pack_count = store.v2_lineage_pack_count(vehicle_id)?;
    // Preserve enough headroom that a failed or temporarily unavailable
    // compaction does not make an otherwise valid lineage unservable. The
    // hard pre-commit validation remains the final authority at 512.
    let trigger = limits.max_chunks.saturating_sub(limits.max_chunks.min(8));
    if pack_count.saturating_add(1) <= trigger {
        return Ok(());
    }
    let Some(plan) = store.plan_live_delta_compaction(vehicle_id)? else {
        if pack_count.saturating_add(1) > limits.max_chunks {
            return Err(StoreError::LineageCapacityExhausted.into());
        }
        return Ok(());
    };
    let binding = store.v2_projection_binding(vehicle_id)?;
    let projection = store.projection_delta_for_compaction(&plan, binding.clone())?;
    let built = match ProjectionPackWriter::new(store.packs_dir()).write_delta(
        &ProjectionDeltaPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: plan.base_snapshot_id,
            ordinal: plan.first_ordinal,
            delta: &projection,
        },
    ) {
        Ok(built) => built,
        Err(error) if may_defer_compaction_capacity_error(&error, pack_count, limits) => {
            tracing::warn!(
                vehicle_id = %vehicle_id,
                pack_count,
                %error,
                "deferred oversized lineage compaction while aggregate slots remain"
            );
            return Ok(());
        }
        Err(error) if is_compaction_pack_capacity_error(&error) => {
            return Err(StoreError::LineageCapacityExhausted.into());
        }
        Err(error) => return Err(error.into()),
    };
    let chain_digest = canonical_delta_chain_digest(plan.anchor_digest, built.metadata.sha256);
    let compacted = LineageDelta {
        from_sequence: plan.anchor_sequence,
        to_sequence: plan.head_sequence,
        parent_chain_digest: plan.anchor_digest,
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
            sequence: plan.head_sequence,
        },
    )
    .map_err(StoreError::Manifest)?;
    let replaced = plan.replaced_spans.len();
    match store.commit_live_delta_compaction(&plan, &compacted, cursor_key, &terminal_cursor) {
        Ok(()) => {}
        Err(error)
            if is_compaction_catalog_capacity_error(&error)
                && pack_count.saturating_add(1) <= limits.max_chunks =>
        {
            tracing::warn!(
                vehicle_id = %vehicle_id,
                pack_count,
                %error,
                "deferred aggregate-oversized lineage compaction while a slot remains"
            );
            return Ok(());
        }
        Err(error) if is_compaction_catalog_capacity_error(&error) => {
            return Err(StoreError::LineageCapacityExhausted.into());
        }
        Err(error) => return Err(error.into()),
    }
    tracing::info!(
        vehicle_id = %vehicle_id,
        replaced_delta_packs = replaced,
        compacted_delta_packs = 1,
        "compacted collector-owned V2 lineage suffix"
    );
    Ok(())
}

fn is_compaction_pack_capacity_error(error: &ProjectionPackError) -> bool {
    matches!(
        error,
        ProjectionPackError::TooManyRows
            | ProjectionPackError::Protocol(
                ProtocolError::CompressedSizeOutOfBounds(_)
                    | ProtocolError::UncompressedSizeOutOfBounds(_)
                    | ProtocolError::RowCountOutOfBounds(_)
                    | ProtocolError::PackTooLarge
            )
    )
}

fn may_defer_compaction_capacity_error(
    error: &ProjectionPackError,
    pack_count: usize,
    limits: ProtocolLimits,
) -> bool {
    is_compaction_pack_capacity_error(error) && pack_count.saturating_add(1) <= limits.max_chunks
}

fn is_compaction_catalog_capacity_error(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::Manifest(ProtocolError::LineageAggregateLimitExceeded)
    )
}

pub async fn run_address_enrichment_once(
    store: &HubStore,
    config: &HubConfig,
    _cursor_key: &CursorKey,
    _vehicles: &[Vehicle],
    now_ms: i64,
) -> Result<bool, CollectorError> {
    run_address_enrichment_once_with_runtime_admission(
        store,
        config,
        _cursor_key,
        _vehicles,
        now_ms,
        None,
    )
    .await
}

async fn run_address_enrichment_once_with_runtime_admission(
    store: &HubStore,
    config: &HubConfig,
    _cursor_key: &CursorKey,
    _vehicles: &[Vehicle],
    now_ms: i64,
    runtime_admission: Option<&Arc<crate::hub_user_process::AdmittedUserHub>>,
) -> Result<bool, CollectorError> {
    if !config.geocoder.enabled {
        return Ok(false);
    }
    let Some(job) = store.claim_address_enrichment_job(now_ms)? else {
        return Ok(false);
    };
    let result = match Geocoder::new(&config.geocoder) {
        Ok(geocoder) => {
            let point = Wgs84Point::new(job.latitude, job.longitude)
                .map_err(|_| GeocoderError::MalformedResponse)?;
            if let Some(admission) = runtime_admission {
                let guard = AdmittedUserEgressGuard::new(Arc::clone(admission));
                geocoder
                    .reverse_cached_with_egress_guard(store, point, now_ms, &guard)
                    .await
            } else {
                #[cfg(any(not(target_os = "macos"), test))]
                {
                    geocoder
                        .reverse_cached_with_egress_guard(
                            store,
                            point,
                            now_ms,
                            &crate::geocoder::UnguardedEgress,
                        )
                        .await
                }
                #[cfg(all(target_os = "macos", not(test)))]
                {
                    Err(GeocoderError::EgressDenied)
                }
            }
        }
        Err(GeocoderError::Disabled) => {
            store.complete_address_enrichment(&job, None, now_ms)?;
            return Ok(false);
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(address) => {
            store.complete_address_enrichment(&job, Some(&address.display_name), now_ms)?;
            Ok(true)
        }
        Err(
            GeocoderError::MalformedResponse | GeocoderError::NoResult | GeocoderError::Disabled,
        ) => {
            store.complete_address_enrichment(&job, None, now_ms)?;
            Ok(false)
        }
        Err(GeocoderError::EgressDenied) => Err(CollectorError::SensitiveAccessUnavailable),
        Err(error) => {
            store.retry_address_enrichment(&job, &error.to_string(), now_ms)?;
            Ok(false)
        }
    }
}

struct VehicleStreamRuntime {
    vehicle_id: VehicleId,
    power_gate: Arc<StreamPowerGate>,
    sensitive_access_failure: Arc<AtomicBool>,
    events: mpsc::Receiver<StreamEvent>,
    _shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<Result<(), crate::tesla_stream::StreamSupervisorError>>>,
}

impl Drop for VehicleStreamRuntime {
    fn drop(&mut self) {
        // `JoinHandle` detaches on drop.  Cancel instead: a detached stream
        // still has the credential manager and could outlive the collector.
        // The supervisor revokes `power_gate` synchronously on abort.
        if let Some(shutdown) = self._shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
    }
}

fn ensure_vehicle_stream(
    store: &HubStore,
    streams: &mut Vec<VehicleStreamRuntime>,
    vehicle_id: VehicleId,
    stream_vehicle_id: StreamVehicleId,
    auth: &CollectionAuth,
    client: &OwnerApi,
    health_timeout: Duration,
    stream_endpoint: Option<&str>,
) -> bool {
    let CollectionAuth::Legacy {
        manager,
        fuse,
        refresh,
        region,
        ..
    } = auth;
    if refresh.has_sensitive_failure() {
        return false;
    }
    // Stream creation is synchronous at this call site. A contended fuse is
    // treated conservatively, so a sixth 401 cannot race a new stream.
    let Ok(fuse) = fuse.try_lock() else {
        return false;
    };
    if fuse.is_blown() {
        return false;
    }
    let refresh_failure = Some(Arc::clone(refresh));
    if streams.iter().any(|stream| stream.vehicle_id == vehicle_id) {
        return true;
    }
    let (events, receiver) = mpsc::channel(STREAM_EVENT_CHANNEL_CAPACITY);
    let power_gate = Arc::new(StreamPowerGate::default());
    let (shutdown, stop) = oneshot::channel();
    let endpoint = stream_endpoint
        .unwrap_or_else(|| streaming_endpoint(*region))
        .to_owned();
    #[cfg(not(test))]
    let supervisor_result = TeslaStreamSupervisor::new_legacy_auth(
        vehicle_id,
        stream_vehicle_id,
        Arc::clone(manager),
        *region,
        endpoint,
        client.legacy_auth_http_client(),
        events,
        store.clone(),
    );
    #[cfg(test)]
    let supervisor_result = TeslaStreamSupervisor::new_legacy_auth_for_test(
        vehicle_id,
        stream_vehicle_id,
        Arc::clone(manager),
        *region,
        endpoint,
        client.legacy_auth_http_client(),
        events,
    )
    .map(|supervisor| supervisor.with_audit_store(store.clone()));
    let supervisor = match supervisor_result {
        Ok(supervisor) => supervisor,
        Err(error) => {
            tracing::warn!(vehicle_id = vehicle_id.get(), error = %error, "vehicle stream unavailable");
            return false;
        }
    };
    let supervisor = supervisor
        .with_health_timeout(health_timeout)
        .with_power_gate(Arc::clone(&power_gate));
    let sensitive_access_failure = Arc::new(AtomicBool::new(false));
    let worker_sensitive_access_failure = Arc::clone(&sensitive_access_failure);
    let task = tokio::spawn(async move {
        let run = supervisor.run(stop);
        tokio::pin!(run);
        let result = if let Some(refresh) = refresh_failure {
            tokio::select! {
                biased;
                () = refresh.wait_for_sensitive_failure() => {
                    Err(crate::tesla_stream::StreamSupervisorError::CredentialAuthorityUnavailable)
                }
                result = &mut run => result,
            }
        } else {
            run.await
        };
        if matches!(
            &result,
            Err(crate::tesla_stream::StreamSupervisorError::CredentialAuthorityUnavailable)
        ) {
            worker_sensitive_access_failure.store(true, Ordering::Release);
        }
        result
    });
    streams.push(VehicleStreamRuntime {
        vehicle_id,
        power_gate,
        sensitive_access_failure,
        events: receiver,
        _shutdown: Some(shutdown),
        task: Some(task),
    });
    true
}

async fn disconnect_vehicle_stream(streams: &mut Vec<VehicleStreamRuntime>, vehicle_id: VehicleId) {
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
        if let Some(mut task) = stream.task.take()
            && timeout(STREAM_SHUTDOWN_TIMEOUT, &mut task).await.is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
    streams.retain(|stream| stream.vehicle_id != vehicle_id);
}

async fn disconnect_streams_not_in_scheduler(
    streams: &mut Vec<VehicleStreamRuntime>,
    scheduler: &VehicleScheduler,
) {
    let configured = scheduler
        .vehicles()
        .into_iter()
        .map(|vehicle| vehicle.id)
        .collect::<HashSet<_>>();
    let stale = streams
        .iter()
        .map(|stream| stream.vehicle_id)
        .filter(|vehicle_id| !configured.contains(vehicle_id))
        .collect::<Vec<_>>();
    for vehicle_id in stale {
        disconnect_vehicle_stream(streams, vehicle_id).await;
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamAuthenticationTransition {
    NoChange,
    Rejected,
    Authenticated,
}

#[cfg(test)]
async fn drain_stream_events(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut [VehicleStreamRuntime],
) -> Result<StreamAuthenticationTransition, CollectorError> {
    let mut projection_car_ids = HashMap::new();
    let result =
        drain_stream_events_with_cache(store, scheduler, streams, &mut projection_car_ids).await?;
    if let Some(error) = result.terminal_error {
        Err(error)
    } else {
        Ok(result.transition)
    }
}

struct StreamDrainResult {
    transition: StreamAuthenticationTransition,
    backlog: bool,
    terminal_error: Option<CollectorError>,
}

fn process_stream_event(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    stream: &VehicleStreamRuntime,
    event: StreamEvent,
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
    authentication_rejected: &mut bool,
    authenticated: &mut bool,
) -> Result<(), CollectorError> {
    match event {
        StreamEvent::Healthy => {
            *authenticated = true;
            scheduler.stream_healthy(stream.vehicle_id, Instant::now());
        }
        StreamEvent::Telemetry(update) => {
            *authenticated = true;
            scheduler.stream_healthy(stream.vehicle_id, Instant::now());
            process_stream_telemetry_with_cache(
                store,
                scheduler,
                stream.vehicle_id,
                &update,
                projection_car_ids,
            )?;
        }
        StreamEvent::VehicleOffline => {
            let now = Instant::now();
            scheduler.stream_unhealthy(stream.vehicle_id, now);
            scheduler.schedule_offline_state_fetch(stream.vehicle_id, now);
        }
        StreamEvent::AuthRejected => {
            *authentication_rejected = true;
            scheduler.stream_unhealthy(stream.vehicle_id, Instant::now());
            tracing::warn!(
                vehicle_id = stream.vehicle_id.get(),
                "vehicle stream authentication rejected"
            );
        }
        StreamEvent::TransportUnavailable | StreamEvent::ProtocolViolation => {
            scheduler.stream_unhealthy(stream.vehicle_id, Instant::now());
        }
    }
    Ok(())
}

async fn drain_stream_events_with_cache(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut [VehicleStreamRuntime],
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
) -> Result<StreamDrainResult, CollectorError> {
    let mut authentication_rejected = false;
    let mut authenticated = false;
    let mut terminal_error = None;
    for stream in streams.iter_mut() {
        for _ in 0..MAX_STREAM_EVENTS_PER_DRAIN {
            let Ok(event) = stream.events.try_recv() else {
                break;
            };
            process_stream_event(
                store,
                scheduler,
                stream,
                event,
                projection_car_ids,
                &mut authentication_rejected,
                &mut authenticated,
            )?;
        }
        if stream.task.as_ref().is_some_and(JoinHandle::is_finished) {
            // Once the producer has finished, its bounded queue is stable.
            // Drain every final event before consuming the JoinHandle so the
            // last telemetry and authentication transition reach the caller.
            while let Ok(event) = stream.events.try_recv() {
                process_stream_event(
                    store,
                    scheduler,
                    stream,
                    event,
                    projection_car_ids,
                    &mut authentication_rejected,
                    &mut authenticated,
                )?;
            }
            let task = stream
                .task
                .take()
                .expect("finished stream task remains owned until consumed");
            let task_error = classify_stream_task_result(task.await);
            if terminal_error.is_none() {
                terminal_error = Some(task_error);
            }
        }
        if stream.sensitive_access_failure.load(Ordering::Acquire) {
            terminal_error = Some(CollectorError::SensitiveAccessUnavailable);
        }
    }
    let transition = if authentication_rejected {
        // A rejection wins when the same drain sees both events: a success on
        // one stream cannot authenticate another rejected stream.
        StreamAuthenticationTransition::Rejected
    } else if authenticated {
        StreamAuthenticationTransition::Authenticated
    } else {
        StreamAuthenticationTransition::NoChange
    };
    Ok(StreamDrainResult {
        transition,
        backlog: streams.iter().any(|stream| !stream.events.is_empty()),
        terminal_error,
    })
}

#[cfg(test)]
fn process_stream_telemetry(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    vehicle_id: VehicleId,
    update: &crate::tesla_stream::StreamUpdate,
) -> Result<bool, CollectorError> {
    if !scheduler.should_persist_stream_telemetry(vehicle_id, update.power, Instant::now()) {
        return Ok(false);
    }
    scheduler.schedule_stream_charging_poll(
        vehicle_id,
        update.shift_state.as_deref(),
        update.power,
        Instant::now(),
    );
    persist_stream_update(store, vehicle_id, update)
}

fn process_stream_telemetry_with_cache(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    vehicle_id: VehicleId,
    update: &crate::tesla_stream::StreamUpdate,
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
) -> Result<bool, CollectorError> {
    if !scheduler.should_persist_stream_telemetry(vehicle_id, update.power, Instant::now()) {
        return Ok(false);
    }
    scheduler.schedule_stream_charging_poll(
        vehicle_id,
        update.shift_state.as_deref(),
        update.power,
        Instant::now(),
    );
    let context = if let Some(context) = projection_car_ids.get(&vehicle_id) {
        *context
    } else {
        let context = stream_context(store, vehicle_id)?;
        projection_car_ids.insert(vehicle_id, context);
        context
    };
    persist_stream_update_with_projection(store, vehicle_id, update, Some(context))
}

#[cfg(test)]
fn persist_stream_update(
    store: &HubStore,
    vehicle_id: VehicleId,
    update: &crate::tesla_stream::StreamUpdate,
) -> Result<bool, CollectorError> {
    persist_stream_update_with_projection(store, vehicle_id, update, None)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StreamContext {
    source_id: Uuid,
    registered_vehicle_id: Uuid,
    selected_car_id: i64,
}

fn stream_context(
    store: &HubStore,
    vehicle_id: VehicleId,
) -> Result<StreamContext, CollectorError> {
    let received_at_ms = current_epoch_millis()?;
    let source = store.register_source(
        &SourceDescriptor::new(STREAM_SOURCE_KIND, STREAM_SOURCE_KEY),
        received_at_ms,
    )?;
    let registered = store.register_vehicle(
        &VehicleDescriptor::new(source.source_id, vehicle_id.get().to_string())
            .with_tesla_identity(Some(vehicle_id.get() as i64), None),
        received_at_ms,
    )?;
    Ok(StreamContext {
        source_id: source.source_id,
        registered_vehicle_id: registered.vehicle_id,
        selected_car_id: projection_car_id_for_vehicle(
            store,
            registered.vehicle_id,
            vehicle_id.get(),
        )?,
    })
}

fn persist_stream_update_with_projection(
    store: &HubStore,
    vehicle_id: VehicleId,
    update: &crate::tesla_stream::StreamUpdate,
    context: Option<StreamContext>,
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
    let (source_id, registered_vehicle_id, pack_car_id) = if let Some(context) = context {
        (
            context.source_id,
            context.registered_vehicle_id,
            context.selected_car_id,
        )
    } else {
        let source = store.register_source(
            &SourceDescriptor::new(STREAM_SOURCE_KIND, STREAM_SOURCE_KEY),
            received_at_ms,
        )?;
        let registered = store.register_vehicle(
            &VehicleDescriptor::new(source.source_id, vehicle_id.get().to_string())
                .with_tesla_identity(Some(vehicle_id.get() as i64), None),
            received_at_ms,
        )?;
        (
            source.source_id,
            registered.vehicle_id,
            projection_car_id_for_vehicle(store, registered.vehicle_id, vehicle_id.get())?,
        )
    };
    let result = store.accept_stream_observation_and_lifecycle(
        &ObservationInput {
            source_id,
            vehicle_id: registered_vehicle_id,
            observed_at_ms: update.timestamp_ms,
            payload: stream_observation_payload(update),
        },
        received_at_ms,
        pack_car_id,
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
    #[cfg(test)]
    if let Some(delay) = supervised_test_owner_api_failure_retry() {
        return delay;
    }
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
    FallbackReady,
}

const PRE_ONLINE_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_SETTINGS_REFRESH: Duration = Duration::from_secs(30);
const STREAM_EVENT_DRAIN_INTERVAL: Duration = Duration::from_millis(100);

const fn collection_sleep_cap(has_active_streams: bool) -> Duration {
    if has_active_streams {
        STREAM_EVENT_DRAIN_INTERVAL
    } else {
        CONTROL_SETTINGS_REFRESH
    }
}

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

    fn apply_control_settings(
        &mut self,
        configured: &[(uuid::Uuid, i64, crate::hub_pack::ProjectionCarSettings)],
        now: Instant,
    ) -> Vec<VehicleId> {
        let mut disconnect = Vec::new();
        let mut rediscover = false;
        let removed = self
            .vehicles
            .keys()
            .copied()
            .filter(|vehicle_id| {
                !configured
                    .iter()
                    .any(|(_, eid, _)| vehicle_id.get() == *eid as u64)
            })
            .collect::<Vec<_>>();
        for vehicle_id in removed {
            self.vehicles.remove(&vehicle_id);
            self.vehicle_fuses.remove(&vehicle_id);
            disconnect.push(vehicle_id);
            rediscover = true;
        }
        for scheduled in self.vehicles.values_mut() {
            let Some((_, _, settings)) = configured
                .iter()
                .find(|(_, eid, _)| scheduled.vehicle.id.get() == *eid as u64)
            else {
                continue;
            };
            if scheduled.settings == *settings {
                continue;
            }
            let was_enabled = scheduled.settings.enabled;
            let was_streaming = scheduled.settings.use_streaming_api;
            scheduled.settings = settings.clone();
            scheduled.vehicle.settings = settings.clone();
            if !settings.enabled || !settings.use_streaming_api {
                scheduled.stream_healthy = false;
                scheduled.pre_online = PreOnlineCheck::Idle;
                disconnect.push(scheduled.vehicle.id);
            } else if scheduled.vehicle.is_online() {
                scheduled.next_poll = now;
                rediscover |= !was_enabled || !was_streaming;
            }
        }
        if rediscover {
            self.next_discovery = now;
        }
        disconnect
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
            let pre_online =
                if !vehicle.is_online() || !vehicle.settings.use_streaming_api || service_mode {
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
        self.vehicle_fuses
            .retain(|id, _| self.vehicles.contains_key(id));
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
        let Some(mut vehicle) = self
            .vehicles
            .get(&vehicle_id)
            .map(|scheduled| scheduled.vehicle.clone())
        else {
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
            let pre_online =
                if !vehicle.is_online() || !vehicle.settings.use_streaming_api || service_mode {
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
        self.vehicle_fuses
            .retain(|id, _| self.vehicles.contains_key(id));
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
            if let PreOnlineCheck::Probing { deadline } = scheduled.pre_online
                && now >= deadline
            {
                // TeslaMate falls back to vehicle_data when a new stream stays
                // silent. A nil-power frame remains a confirmed fake-online
                // signal and deliberately does not take this fallback.
                scheduled.pre_online = PreOnlineCheck::FallbackReady;
                scheduled.next_poll = now;
            }
        }
        // Streaming cars require numeric stream power before vehicle_data.
        // A car with streaming disabled uses normal Owner API polling.
        let candidates = self
            .vehicles
            .values()
            .filter(|scheduled| {
                scheduled.vehicle.is_online()
                    && scheduled.settings.enabled
                    && !scheduled.service_mode
                    && (!scheduled.settings.use_streaming_api
                        || matches!(
                            scheduled.pre_online,
                            PreOnlineCheck::ConfirmedReal | PreOnlineCheck::FallbackReady
                        ))
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
                    if scheduled.settings.use_streaming_api && scheduled.stream_healthy {
                        Duration::from_secs(15)
                    } else {
                        self.cadence.driving
                    }
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
                | CollectorError::FleetCredential(_)
        ) {
            self.vehicle_retry_after(id, LEGACY_REFRESH_RETRY, now);
            return;
        }
        if let CollectorError::FleetApi(error) = error {
            match error {
                FleetApiError::RateLimited {
                    retry_after_seconds,
                } => self.vehicle_rate_limited(id, *retry_after_seconds, now),
                FleetApiError::HttpStatus(404)
                | FleetApiError::ProviderHttpStatus { status: 404, .. } => {
                    self.vehicle_not_found(id, now)
                }
                FleetApiError::RequestTimeout
                | FleetApiError::HttpStatus(401 | 403)
                | FleetApiError::ProviderHttpStatus {
                    status: 401 | 403, ..
                } => {
                    self.vehicle_failed(id, now);
                }
                _ => self.vehicle_api_error(id, now),
            }
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
        state
            .api_errors
            .retain(|at| now.saturating_duration_since(*at) < API_ERROR_WINDOW);
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
        state
            .not_found
            .retain(|at| now.saturating_duration_since(*at) < VEHICLE_NOT_FOUND_WINDOW);
        state.not_found.push(now);
        state
            .api_errors
            .retain(|at| now.saturating_duration_since(*at) < API_ERROR_WINDOW);
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
            let recovered = !scheduled.stream_healthy;
            scheduled.stream_healthy = true;
            scheduled.suspended = false;
            if recovered
                && !matches!(
                    scheduled.pre_online,
                    PreOnlineCheck::Probing { .. } | PreOnlineCheck::ConfirmedFake { .. }
                )
            {
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
                PreOnlineCheck::ConfirmedReal | PreOnlineCheck::FallbackReady => {
                    scheduled.next_poll = now;
                }
                PreOnlineCheck::ConfirmedFake { deadline } => {
                    scheduled.next_poll = deadline;
                }
                PreOnlineCheck::Idle | PreOnlineCheck::Probing { .. } => {}
            }
        }
    }

    fn should_persist_stream_telemetry(
        &mut self,
        id: VehicleId,
        power: Option<i64>,
        now: Instant,
    ) -> bool {
        let Some(scheduled) = self.vehicles.get_mut(&id) else {
            return false;
        };
        if !matches!(scheduled.vehicle.state.as_str(), "asleep" | "offline") {
            self.pre_online_power(id, power, now);
            return true;
        }

        match (&scheduled.pre_online, power) {
            (PreOnlineCheck::Idle, None) => {
                scheduled.pre_online = PreOnlineCheck::ConfirmedFake {
                    deadline: now + PRE_ONLINE_TIMEOUT,
                };
            }
            (PreOnlineCheck::Idle, Some(_)) => {
                scheduled.pre_online = PreOnlineCheck::ConfirmedReal;
            }
            _ => observe_pre_online_power(&mut scheduled.pre_online, power, now),
        }
        if matches!(scheduled.pre_online, PreOnlineCheck::ConfirmedReal) {
            scheduled.next_poll = now;
        }
        power.is_some()
    }

    fn schedule_stream_charging_poll(
        &mut self,
        id: VehicleId,
        shift_state: Option<&str>,
        power: Option<i64>,
        now: Instant,
    ) {
        let Some(scheduled) = self.vehicles.get_mut(&id) else {
            return;
        };
        let charging_hint = power.is_some_and(|power| power < 0)
            && (shift_state.is_none() || (scheduled.suspended && shift_state == Some("P")));
        if charging_hint
            && scheduled.vehicle.is_online()
            && scheduled.last_phase != PollPhase::Charging
        {
            scheduled.last_phase = PollPhase::Charging;
            scheduled.suspended = false;
            scheduled.last_used = now;
            scheduled.next_poll = now;
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
                        | PreOnlineCheck::FallbackReady
                )
        })
    }

    /// Numeric stream power is the strict no-wake prerequisite. A stream that
    /// stays silent for the bounded startup window instead uses TeslaMate's
    /// Owner API fallback after products has already reported the car online.
    fn requires_live_stream_power_gate(&self, id: VehicleId) -> bool {
        self.vehicles.get(&id).is_some_and(|scheduled| {
            scheduled.settings.use_streaming_api
                && matches!(scheduled.pre_online, PreOnlineCheck::ConfirmedReal)
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

#[cfg(test)]
async fn persist_discovery_events(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
) -> Result<(), CollectorError> {
    persist_discovery_events_with_timeout(
        store,
        cursor_key,
        vehicles,
        CollectorProvider::Legacy,
        crate::lifecycle::DEFAULT_OFFLINE_DRIVE_TIMEOUT,
    )
    .await
}

async fn persist_discovery_events_with_timeout(
    store: &HubStore,
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    provider: CollectorProvider,
    offline_drive_timeout: Duration,
) -> Result<(), CollectorError> {
    let publication_gate = store.acquire_publication_gate().await?;
    let observed_at_ms = current_epoch_millis()?;
    let source = store.register_source(&provider_source(provider), observed_at_ms)?;
    for vehicle in vehicles {
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string())
            .with_tesla_identity(Some(vehicle.id.get() as i64), None);
        descriptor.vin = clean_optional_text(Some(&vehicle.vin));
        descriptor.display_name = clean_optional_text(vehicle.display_name.as_deref());
        let registered = store.register_vehicle(&descriptor, observed_at_ms)?;
        let pack_car_id =
            projection_car_id_for_vehicle(store, registered.vehicle_id, vehicle.id.get())?;
        let mut live_settings = vehicle.settings.clone();
        live_settings.suspend_min_resolved = false;
        store.upsert_car_settings(registered.vehicle_id, pack_car_id, &live_settings)?;
        store.accept_owner_observation_and_lifecycle_with_offline_timeout(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: registered.vehicle_id,
                observed_at_ms,
                payload: discovery_payload(vehicle, provider),
            },
            observed_at_ms,
            pack_car_id,
            offline_drive_timeout,
        )?;
    }
    let collection = ManualCollection {
        vehicles: vehicles.to_vec(),
        snapshots: Vec::new(),
        failures: Vec::new(),
    };
    publish_compatibility_snapshots_for_provider(
        store,
        &publication_gate,
        cursor_key,
        &collection,
        observed_at_ms,
        provider,
    )?;
    Ok(())
}

fn discovery_payload(vehicle: &Vehicle, provider: CollectorProvider) -> Value {
    serde_json::json!({
        "record_type": provider_discovery_record_type(provider),
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
    if matches!(shift, Some("D" | "R" | "N" | "d" | "r" | "n")) {
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
    persist_collection_mode(
        store,
        collection,
        received_at_ms,
        false,
        CollectorProvider::Legacy,
    )
}

fn persist_collection_atomic_for_provider(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
    provider: CollectorProvider,
) -> Result<ManualCollectionReport, CollectorError> {
    persist_collection_mode(store, collection, received_at_ms, true, provider)
}

fn persist_collection_mode(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
    atomic_lifecycle: bool,
    provider: CollectorProvider,
) -> Result<ManualCollectionReport, CollectorError> {
    if received_at_ms < 0 {
        return Err(CollectorError::InvalidReceiptTimestamp);
    }

    let source = store.register_source(&provider_source(provider), received_at_ms)?;
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
            payload: observation_payload(snapshot, source_vehicle_state, provider),
        };
        let append = if atomic_lifecycle {
            let pack_car_id = projection_car_id_for_vehicle(store, vehicle_id, source_vehicle_id)?;
            let result = store.accept_owner_observation_and_lifecycle(
                &input,
                received_at_ms,
                pack_car_id,
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
    materialise_lifecycle_for_collection_provider(
        store,
        collection,
        received_at_ms,
        CollectorProvider::Legacy,
    )
}

fn materialise_lifecycle_for_collection_provider(
    store: &HubStore,
    collection: &ManualCollection,
    received_at_ms: i64,
    provider: CollectorProvider,
) -> Result<LifecycleMaterialisationReport, CollectorError> {
    let source = store.register_source(&provider_source(provider), received_at_ms)?;
    let mut report = LifecycleMaterialisationReport::default();
    for vehicle in &collection.vehicles {
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string())
            .with_tesla_identity(Some(vehicle.id.get() as i64), None);
        descriptor.vin = clean_optional_text(Some(&vehicle.vin));
        descriptor.display_name = clean_optional_text(vehicle.display_name.as_deref());
        let registered = store.register_vehicle(&descriptor, received_at_ms)?;
        let car_id = projection_car_id_for_vehicle(store, registered.vehicle_id, vehicle.id.get())?;
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
    // After TeslaMate import, materialised_* already holds pack-local ids
    // (position_id up to 10M+). Seed next_* so live Owner-API projection never
    // collides with imported primary keys (R06 continuity UNIQUE constraint).
    seed_lifecycle_ids_from_materialised(store, vehicle_id, &mut state)?;
    // Incremental lifecycle: do not rehydrate full open child collections here.
    // Aggregates + lifecycle_open_rows keep close/materialization correct.

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
        // Stationary / free positions and charge samples that close in this step.
        report.positions_materialised += step
            .delta
            .positions
            .iter()
            .filter(|position| position.drive_id.is_none())
            .count();
        report.charge_samples_materialised += 0; // closed-session samples counted below
        total_delta.drives.extend(step.delta.drives);
        for discarded_drive_id in &step.delta.discarded_drive_ids {
            total_delta
                .open_drive_positions
                .retain(|position| position.drive_id != Some(*discarded_drive_id));
        }
        total_delta
            .discarded_drive_ids
            .extend(step.delta.discarded_drive_ids);
        total_delta.positions.extend(step.delta.positions);
        total_delta.charges.extend(step.delta.charges);
        total_delta.charge_samples.extend(step.delta.charge_samples);
        total_delta.states.extend(step.delta.states);
        total_delta.updates.extend(step.delta.updates);
        total_delta
            .charge_start_coordinates
            .extend(step.delta.charge_start_coordinates);
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
    // Closed drives/charges materialise durable open-row children that were never
    // kept in active memory. Count them for the materialisation report.
    for drive in &total_delta.drives {
        let mut ids = std::collections::HashSet::new();
        for position in total_delta
            .positions
            .iter()
            .filter(|position| position.drive_id == Some(drive.id))
        {
            ids.insert(position.id);
        }
        for position in total_delta
            .open_drive_positions
            .iter()
            .filter(|position| position.drive_id == Some(drive.id))
        {
            ids.insert(position.id);
        }
        for position in store.open_drive_positions(vehicle_id, drive.id)? {
            ids.insert(position.id);
        }
        report.positions_materialised += ids.len();
    }
    for charge in &total_delta.charges {
        let mut ids = std::collections::HashSet::new();
        for sample in total_delta
            .charge_samples
            .iter()
            .filter(|sample| sample.charge_process_id == charge.id)
        {
            ids.insert(sample.id);
        }
        for sample in total_delta
            .open_charge_samples
            .iter()
            .filter(|sample| sample.charge_process_id == charge.id)
        {
            ids.insert(sample.id);
        }
        for sample in store.open_charge_samples(vehicle_id, charge.id)? {
            ids.insert(sample.id);
        }
        report.charge_samples_materialised += ids.len();
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
    force_close_vehicle_for_service_provider(
        store,
        source_vehicle_id,
        closed_at_ms,
        CollectorProvider::Legacy,
    )
}

fn force_close_vehicle_for_service_provider(
    store: &HubStore,
    source_vehicle_id: VehicleId,
    closed_at_ms: i64,
    provider: CollectorProvider,
) -> Result<(), CollectorError> {
    let source = store.register_source(&provider_source(provider), closed_at_ms)?;
    let registered = store.register_vehicle(
        &VehicleDescriptor::new(source.source_id, source_vehicle_id.get().to_string())
            .with_tesla_identity(Some(source_vehicle_id.get() as i64), None),
        closed_at_ms,
    )?;
    let pack_car_id =
        projection_car_id_for_vehicle(store, registered.vehicle_id, source_vehicle_id.get())?;
    let existing = store.load_lifecycle_state(registered.vehicle_id)?;
    let state = match existing.as_ref() {
        Some(record) => OpenSessionState::decode(&record.open_session_json)
            .map_err(CollectorError::Lifecycle)?,
        None => OpenSessionState::new(),
    };
    let step = force_close_for_service(state, pack_car_id, closed_at_ms)?;
    let encoded = step.state.encode().map_err(CollectorError::Lifecycle)?;
    store.commit_lifecycle_delta(&crate::db::LifecycleCommit {
        vehicle_id: registered.vehicle_id,
        car_id: pack_car_id,
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
    publication_gate: &crate::db::PublicationGate,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
    published_at_ms: i64,
) -> Result<usize, CollectorError> {
    publish_compatibility_snapshots_for_provider(
        store,
        publication_gate,
        cursor_key,
        collection,
        published_at_ms,
        CollectorProvider::Legacy,
    )
}

fn publish_compatibility_snapshots_for_provider(
    store: &HubStore,
    publication_gate: &crate::db::PublicationGate,
    cursor_key: &CursorKey,
    collection: &ManualCollection,
    published_at_ms: i64,
    provider: CollectorProvider,
) -> Result<usize, CollectorError> {
    let source = store.register_source(&provider_source(provider), published_at_ms)?;
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
        let selected_car_id =
            projection_car_id_for_vehicle(store, registered.vehicle_id, source_vehicle_id)?;
        // Discovery can be the first live Owner-API event after a TeslaMate
        // import. Imported history lives in immutable packs and therefore does
        // not guarantee a collector-side materialised car row. Seed that row
        // before recording the settings mutation so a sparse V2 successor can
        // always resolve both mutations from durable materialised state.
        let seed_car = compatibility_car(
            vehicle,
            snapshots.get(&source_vehicle_id).copied(),
            selected_car_id,
        );
        store.persist_materialised_car_if_absent(registered.vehicle_id, &seed_car)?;
        store.upsert_car_settings(registered.vehicle_id, selected_car_id, &vehicle.settings)?;
        if store.vehicle_has_v2_base(registered.vehicle_id)? {
            if let Some(sync_claim) =
                store.claim_sync_mutations(registered.vehicle_id, published_at_ms, 10_000)?
                && let Err(error) = publish_v2_delta(store, cursor_key, &sync_claim)
            {
                store.release_sync_mutations(&sync_claim)?;
                return Err(error);
            }
            continue;
        }
        let history = store.materialised_history(registered.vehicle_id)?;
        let durable_car = match history.car.clone() {
            Some(car) => car,
            None => {
                return Err(crate::db::StoreError::SyncMutation(
                    "missing materialised car after compatibility seed".into(),
                )
                .into());
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
        if store.snapshot_fingerprint_is_current(registered.vehicle_id, fingerprint)? {
            continue;
        }
        let sequence =
            store.reserve_next_full_snapshot_sequence(publication_gate, registered.vehicle_id)?;
        let binding = ProjectionBinding {
            installation_id,
            account_id: source.source_id,
            vehicle_id: registered.vehicle_id,
            generation: source.generation,
            selected_car_id,
        };
        let request = ProjectionPackRequest {
            pack_id: Uuid::new_v4(),
            snapshot_id: Uuid::new_v4(),
            ordinal: 0,
            binding: binding.clone(),
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
        store.finalize_import_snapshot_with_binding(&manifest, fingerprint, &[], &binding)?;
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
        .map(crate::hub_pack::normalize_tesla_model_code)
        .unwrap_or_else(|| "Tesla".to_owned());
    let trim_badging = snapshot
        .and_then(|snapshot| nested_text(snapshot, "vehicle_config", "trim_badging"))
        .map(crate::hub_pack::normalize_tesla_trim);
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
            raw_car_type,
            Some(&vehicle.vin),
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

/// Pack-local car id for live collection on a registered vehicle.
///
/// When the vehicle already has an immutable V2 base (TeslaMate import), live
/// Owner-API materialisation and deltas must reuse that base's
/// `selected_car_id` (TeslaMate car_id, e.g. 1). Using the Owner-API EID as
/// `compatibility_car_id` would create pack-local id conflicts and break
/// migration-to-Hub continuity (R06).
fn projection_car_id_for_vehicle(
    store: &HubStore,
    vehicle_id: Uuid,
    source_vehicle_id: u64,
) -> Result<i64, CollectorError> {
    if store.vehicle_has_v2_base(vehicle_id)? {
        let binding = store.v2_projection_binding(vehicle_id)?;
        if binding.selected_car_id > 0 {
            return Ok(binding.selected_car_id);
        }
    }
    Ok(compatibility_car_id(source_vehicle_id))
}

/// Raise open-session id cursors above any durable materialised history for
/// this vehicle so import-backed rows and live collection share one id space.
///
/// TeslaMate import publishes the full history as V2 packs and may only
/// materialise a subset (e.g. positions/states) into `materialised_*`. Drive /
/// charge / sample / update ids still occupy pack-local primary keys. Live
/// collection must not reuse those ids or the client V2 integrity check fails
/// when a delta upserts drive id=1 over an imported drive with different times
/// (positions fall outside the drive interval).
const LEGACY_IMPORT_MAX_ID_SQL: &str = "SELECT COALESCE(MAX(entity_id), 0)
       FROM teslamate_import_projection_rows
      WHERE vehicle_id = ?1 AND entity = ?2";
const CURRENT_IMPORT_MAX_ID_SQL: &str = "SELECT COALESCE(MAX(entity_id), 0)
       FROM teslamate_import_projection_state_rows
      WHERE vehicle_id = ?1 AND entity_ordinal = ?2";

fn seed_lifecycle_ids_from_materialised(
    store: &HubStore,
    vehicle_id: Uuid,
    state: &mut OpenSessionState,
) -> Result<(), CollectorError> {
    if state.id_cursors_seeded {
        return Ok(());
    }
    let connection = store.open().map_err(CollectorError::from)?;
    let max_i64 = |table: &str, column: &str| -> Result<i64, CollectorError> {
        let sql = format!("SELECT COALESCE(MAX({column}), 0) FROM {table} WHERE vehicle_id = ?1");
        connection
            .query_row(&sql, rusqlite::params![vehicle_id.to_string()], |row| {
                row.get(0)
            })
            .map_err(StoreError::Query)
            .map_err(CollectorError::Store)
    };
    // Tables may be empty; COALESCE handles that.
    let max_drive = max_i64("materialised_drives", "drive_id")?;
    let max_position = max_i64("materialised_positions", "position_id")?;
    let max_charge = max_i64("materialised_charges", "charge_id")?;
    let max_sample = max_i64("materialised_charge_samples", "sample_id")?;
    let max_state = max_i64("materialised_states", "state_id")?;
    let max_update = max_i64("materialised_updates", "update_id")?;

    // Both import catalogues have covering primary keys beginning with
    // (vehicle_id, entity/ordinal, entity_id). Point-range MAX queries can seek
    // directly to the final row. The former UNION/GROUP BY scanned and sorted
    // millions of imported keys on every live telemetry sample.
    let import_max = |entity: &str, ordinal: i64| -> Result<i64, CollectorError> {
        let legacy = connection
            .query_row(
                LEGACY_IMPORT_MAX_ID_SQL,
                rusqlite::params![vehicle_id.to_string(), entity],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Query)
            .map_err(CollectorError::Store)?;
        let current = connection
            .query_row(
                CURRENT_IMPORT_MAX_ID_SQL,
                rusqlite::params![vehicle_id.to_string(), ordinal],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StoreError::Query)
            .map_err(CollectorError::Store)?;
        Ok(legacy.max(current))
    };
    let import_drive = import_max("drive", 1)?;
    let import_position = import_max("position", 2)?;
    let import_charge = import_max("charge", 3)?;
    let import_sample = import_max("charge_sample", 4)?;
    let import_state = import_max("state", 5)?;
    let import_update = import_max("update", 6)?;

    let max_drive = max_drive.max(import_drive);
    let max_position = max_position.max(import_position);
    let max_charge = max_charge.max(import_charge);
    let max_sample = max_sample.max(import_sample);
    let max_state = max_state.max(import_state);
    let max_update = max_update.max(import_update);

    state.next_drive_id = state.next_drive_id.max(max_drive.saturating_add(1).max(1));
    state.next_position_id = state
        .next_position_id
        .max(max_position.saturating_add(1).max(1));
    state.next_charge_id = state
        .next_charge_id
        .max(max_charge.saturating_add(1).max(1));
    state.next_charge_sample_id = state
        .next_charge_sample_id
        .max(max_sample.saturating_add(1).max(1));
    state.next_state_id = state.next_state_id.max(max_state.saturating_add(1).max(1));
    state.next_update_id = state
        .next_update_id
        .max(max_update.saturating_add(1).max(1));
    state.id_cursors_seeded = true;
    Ok(())
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

fn observation_payload(
    snapshot: &VehicleData,
    source_vehicle_state: &str,
    provider: CollectorProvider,
) -> Value {
    let mut payload = Map::new();
    payload.insert(
        "record_type".to_owned(),
        Value::String(provider_vehicle_data_record_type(provider).to_owned()),
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
        "provider_raw_json".to_owned(),
        snapshot.provider_raw_json().clone(),
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

fn assert_runtime_sensitive_access(
    admission: Option<&crate::hub_user_process::AdmittedUserHub>,
) -> Result<(), CollectorError> {
    let Some(admission) = admission else {
        return Ok(());
    };
    admission
        .assert_sensitive_access()
        .map_err(|_| CollectorError::SensitiveAccessUnavailable)
}

#[derive(Debug, Error)]
pub enum StreamTaskOutcome {
    #[error("completed normally")]
    CompletedNormally,
    #[error("supervisor failed: {0}")]
    Supervisor(#[source] crate::tesla_stream::StreamSupervisorError),
    #[error("task panicked")]
    Panicked,
    #[error("task was cancelled")]
    Cancelled,
}

fn classify_stream_task_result(
    result: Result<Result<(), crate::tesla_stream::StreamSupervisorError>, JoinError>,
) -> CollectorError {
    let outcome = match result {
        Ok(Ok(())) => StreamTaskOutcome::CompletedNormally,
        Ok(Err(error)) => StreamTaskOutcome::Supervisor(error),
        Err(error) if error.is_panic() => StreamTaskOutcome::Panicked,
        Err(error) => {
            debug_assert!(error.is_cancelled());
            StreamTaskOutcome::Cancelled
        }
    };
    CollectorError::StreamTask(outcome)
}

#[derive(Debug, Error)]
pub enum CollectorError {
    #[error("Serve requires one configured vehicle")]
    SelectedVehicleMissing,
    #[error("native setup store does not match the configured Hub data directory")]
    NativeSetupStoreMismatch,
    #[error("native setup requires legacy Owner API authentication to be enabled")]
    NativeSetupLegacyAuthRequired,
    #[error("Fleet setup requires collector.provider = \"fleet\"")]
    NativeSetupFleetProviderRequired,
    #[error("native setup found no vehicles")]
    NativeSetupNoVehicles,
    #[error("native setup found {discovered} vehicles; select one with --vehicle-id")]
    NativeSetupVehicleSelectionRequired { discovered: usize },
    #[error("native setup vehicle id must be positive")]
    NativeSetupVehicleIdInvalid,
    #[error("native setup vehicle {0} was not found")]
    NativeSetupVehicleNotFound(i64),
    #[error("Fleet account does not contain every already-configured vehicle")]
    FleetSetupInventoryMismatch,
    #[error("vehicle command target is not configured")]
    CommandVehicleMissing,
    #[error("resident vehicle-control socket is unavailable")]
    ResidentControlSocket,
    #[error(
        "Hub is already configured for vehicle {existing}; refusing requested vehicle {requested}"
    )]
    NativeSetupVehicleConflict { existing: i64, requested: i64 },
    #[error("supervised collector heartbeat task stopped unexpectedly")]
    SupervisedHeartbeatTask,
    #[error("terrain worker stopped unexpectedly")]
    TerrainWorkerTask,
    #[error("vehicle stream task stopped unexpectedly: {0}")]
    StreamTask(StreamTaskOutcome),
    #[error("terrain worker failed during local startup")]
    TerrainWorkerStartup,
    #[error("runtime sensitive-access admission is unavailable")]
    SensitiveAccessUnavailable,
    #[error("supervised collector startup receiver closed")]
    SupervisedStartupReadyDropped,
    #[cfg(unix)]
    #[error("admitted collector store does not match the selected Hub store")]
    AdmittedStoreMismatch,
    #[cfg(unix)]
    #[error("admitted collection requires legacy authentication")]
    AdmittedLegacyAuthRequired,
    #[cfg(unix)]
    #[error(transparent)]
    UserAdmission(#[from] crate::hub_user_process::UserLifetimeLockError),
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
    LegacyAuth(#[from] LegacyAuthError),
    #[error(transparent)]
    FleetApiConfig(#[from] FleetApiConfigError),
    #[error(transparent)]
    FleetApi(#[from] FleetApiError),
    #[error(transparent)]
    FleetCredential(#[from] FleetCredentialError),
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
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{
        Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::{get, post},
    };
    use futures_util::{SinkExt, StreamExt};
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use reqwest::{Certificate, Client};
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    use super::*;
    use crate::{
        credentials::{LegacyAuthManager, OwnerTokens},
        db::{SourceDescriptor, SyncMutation, SyncMutationClaim, VehicleDescriptor},
        lifecycle::OpenSessionState,
        owner_api::{Vehicle, VehicleData},
    };

    #[test]
    fn fleet_provider_http_errors_keep_http_classification() {
        let error = FleetApiError::ProviderHttpStatus {
            status: 400,
            error: "invalid_configuration".to_owned(),
            description: None,
        };
        assert_eq!(
            fleet_action_completion(Some(&error)),
            OutboundRequestCompletion {
                outcome: OutboundRequestOutcome::HttpError,
                http_status: Some(400),
                retry_after_seconds: None,
            }
        );
        assert_eq!(
            fleet_failure_as_owner_error(&CollectorError::FleetApi(error)),
            OwnerApiError::HttpStatus(400)
        );
    }

    #[test]
    fn fleet_provider_not_found_uses_not_found_schedule() {
        let now = Instant::now();
        let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);

        scheduler.vehicle_failed_for_error(
            vehicle_id,
            &CollectorError::FleetApi(FleetApiError::ProviderHttpStatus {
                status: 404,
                error: "vehicle_not_found".to_owned(),
                description: None,
            }),
            now,
        );

        assert_eq!(scheduler.vehicle_fuses[&vehicle_id].not_found.len(), 1);
    }

    #[test]
    fn lifecycle_id_seed_uses_covering_import_indexes_once() {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let connection = store.open().expect("connection");
        for (sql, second) in [
            (
                LEGACY_IMPORT_MAX_ID_SQL,
                rusqlite::types::Value::Text("position".to_owned()),
            ),
            (
                CURRENT_IMPORT_MAX_ID_SQL,
                rusqlite::types::Value::Integer(2),
            ),
        ] {
            let mut statement = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .expect("query plan");
            let details = statement
                .query_map(
                    rusqlite::params![Uuid::new_v4().to_string(), second],
                    |row| row.get::<_, String>(3),
                )
                .expect("plan rows")
                .collect::<Result<Vec<_>, _>>()
                .expect("plan details")
                .join("\n");
            assert!(
                details.contains("SEARCH")
                    && !details.contains("SCAN")
                    && !details.contains("TEMP B-TREE"),
                "ID seed must seek the covering import key: {details}"
            );
        }

        let mut state = OpenSessionState::new();
        assert!(!state.id_cursors_seeded);
        seed_lifecycle_ids_from_materialised(&store, Uuid::new_v4(), &mut state)
            .expect("indexed seed");
        assert!(state.id_cursors_seeded);
        assert!(
            OpenSessionState::decode(&state.encode().expect("state encode"))
                .expect("state decode")
                .id_cursors_seeded
        );
    }

    #[test]
    fn fleet_proxy_root_certificate_is_descriptor_pinned_and_private() {
        let temporary = tempfile::tempdir().expect("temporary certificate root");
        let certificate = temporary.path().join("proxy-ca.pem");
        fs::write(&certificate, b"trusted-ca").expect("write certificate");
        fs::set_permissions(&certificate, fs::Permissions::from_mode(0o600))
            .expect("protect certificate");
        assert_eq!(
            read_fleet_proxy_root_certificate(&certificate, 128).expect("read safe certificate"),
            b"trusted-ca"
        );

        let link = temporary.path().join("proxy-ca-link.pem");
        symlink(&certificate, &link).expect("certificate symlink");
        assert!(matches!(
            read_fleet_proxy_root_certificate(&link, 128),
            Err(FleetApiConfigError::InvalidRootCertificate)
        ));

        fs::set_permissions(&certificate, fs::Permissions::from_mode(0o622))
            .expect("make certificate writable");
        assert!(matches!(
            read_fleet_proxy_root_certificate(&certificate, 128),
            Err(FleetApiConfigError::InvalidRootCertificate)
        ));
        fs::set_permissions(&certificate, fs::Permissions::from_mode(0o600))
            .expect("restore certificate mode");

        assert!(matches!(
            read_fleet_proxy_root_certificate(&certificate, 4),
            Err(FleetApiConfigError::InvalidRootCertificate)
        ));

        let replacement = temporary.path().join("replacement.pem");
        fs::write(&replacement, b"other-ca").expect("write replacement");
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600))
            .expect("protect replacement");
        assert!(matches!(
            read_fleet_proxy_root_certificate_after_open(&certificate, 128, || {
                fs::rename(&replacement, &certificate).expect("replace admitted certificate");
            }),
            Err(FleetApiConfigError::InvalidRootCertificate)
        ));
    }

    #[tokio::test]
    async fn stream_task_completion_outcomes_are_typed_and_secret_safe() {
        let normal = classify_stream_task_result(
            tokio::spawn(async { Ok::<_, crate::tesla_stream::StreamSupervisorError>(()) }).await,
        );
        let supervisor = classify_stream_task_result(
            tokio::spawn(async {
                Err::<(), _>(crate::tesla_stream::StreamSupervisorError::EventQueueFull)
            })
            .await,
        );
        let panic = classify_stream_task_result(
            tokio::spawn(async { panic!("access-secret refresh-secret") }).await,
        );
        let cancelled_task = tokio::spawn(async {
            std::future::pending::<Result<(), crate::tesla_stream::StreamSupervisorError>>().await
        });
        cancelled_task.abort();
        let cancelled = classify_stream_task_result(cancelled_task.await);

        assert!(matches!(
            &normal,
            CollectorError::StreamTask(StreamTaskOutcome::CompletedNormally)
        ));
        assert!(matches!(
            &supervisor,
            CollectorError::StreamTask(StreamTaskOutcome::Supervisor(
                crate::tesla_stream::StreamSupervisorError::EventQueueFull
            ))
        ));
        assert!(matches!(
            &panic,
            CollectorError::StreamTask(StreamTaskOutcome::Panicked)
        ));
        assert!(matches!(
            &cancelled,
            CollectorError::StreamTask(StreamTaskOutcome::Cancelled)
        ));
        for error in [&normal, &supervisor, &panic, &cancelled] {
            let rendered = format!("{error} {error:?}");
            assert!(!rendered.contains("access-secret"));
            assert!(!rendered.contains("refresh-secret"));
        }

        let tokens =
            OwnerTokens::from_secret_parts("access-secret".to_owned(), "refresh-secret".to_owned())
                .expect("bounded bearer pair");
        let auth = crate::legacy_auth::LegacyAuth::for_test(
            url::Url::parse("https://auth.tesla.com/oauth2/v3/token").unwrap(),
            "access-secret",
            "refresh-secret",
        );
        let manager = LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
        for rendered in [format!("{tokens:?}"), format!("{manager:?}")] {
            assert!(!rendered.contains("access-secret"));
            assert!(!rendered.contains("refresh-secret"));
        }
    }

    #[tokio::test]
    async fn completed_stream_task_drains_final_events_and_is_reaped_once() {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let vehicle = Vehicle::for_test(29, "5YJ3E1EA7KF000029", "online");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), Instant::now());
        scheduler.accept_discovery(vec![vehicle], Instant::now());
        let (events, receiver) = mpsc::channel(2);
        events
            .send(StreamEvent::Telemetry(Box::new(
                crate::tesla_stream::StreamUpdate {
                    tag: vehicle_id.to_string(),
                    timestamp_ms: current_epoch_millis().expect("clock") - 1_000,
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
                },
            )))
            .await
            .expect("final telemetry");
        events
            .send(StreamEvent::AuthRejected)
            .await
            .expect("final auth transition");
        drop(events);
        let (shutdown, _stop) = oneshot::channel();
        let task = tokio::spawn(async {
            Err::<(), _>(crate::tesla_stream::StreamSupervisorError::EventQueueFull)
        });
        let mut streams = vec![VehicleStreamRuntime {
            vehicle_id,
            power_gate: Arc::new(StreamPowerGate::default()),
            sensitive_access_failure: Arc::new(AtomicBool::new(false)),
            events: receiver,
            _shutdown: Some(shutdown),
            task: Some(task),
        }];
        while !streams[0]
            .task
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            tokio::task::yield_now().await;
        }

        let mut projection_car_ids = HashMap::new();
        let result = drain_stream_events_with_cache(
            &store,
            &mut scheduler,
            &mut streams,
            &mut projection_car_ids,
        )
        .await
        .expect("final events must drain before task failure");
        assert_eq!(result.transition, StreamAuthenticationTransition::Rejected);
        assert!(matches!(
            result.terminal_error,
            Some(CollectorError::StreamTask(StreamTaskOutcome::Supervisor(
                crate::tesla_stream::StreamSupervisorError::EventQueueFull
            )))
        ));
        assert!(streams[0].task.is_none(), "completed task was not consumed");
        assert!(!scheduler.vehicles[&vehicle_id].stream_healthy);
        let registered = projection_car_ids[&vehicle_id].registered_vehicle_id;
        let observations = store
            .current_observations_for_vehicle(registered)
            .expect("final telemetry observation");
        assert!(
            observations.iter().any(|observation| {
                observation.payload["record_type"] == "tesla_stream_update_v1"
            })
        );

        stop_and_clear_manual_probe_streams(&mut streams).await;
        assert!(streams.is_empty());
    }

    #[tokio::test]
    async fn resident_control_socket_is_private_bounded_and_service_owned() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let socket = ResidentControlSocket::bind(temporary.path()).expect("control socket");
        let socket_path = temporary.path().join(RESIDENT_CONTROL_SOCKET_NAME);
        let metadata = std::fs::symlink_metadata(&socket_path).expect("socket metadata");
        assert!(metadata.file_type().is_socket());
        assert_eq!(metadata.mode() & 0o777, 0o600);

        let client = OwnerApi::for_fake_http(
            url::Url::parse("http://127.0.0.1:9/").expect("loopback URL"),
            Duration::from_secs(1),
        )
        .expect("bounded Owner client");
        let manager = Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
            LegacyAuth::for_test(
                url::Url::parse("https://auth.tesla.com/oauth2/v3/token").expect("issuer URL"),
                "resident-access",
                "resident-refresh",
            ),
            Arc::new(|_, _| Ok(())),
        )));
        let fuse = Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default()));
        let refresh = Arc::new(LegacyRefreshCoordinator::default());
        let mut task = tokio::spawn(socket.serve(store, client, manager, fuse, refresh));

        let error = request_resident_vehicle_action(
            temporary.path(),
            Uuid::new_v4(),
            LegacyVehicleAction::Wake,
        )
        .await
        .expect_err("unconfigured vehicle rejected locally");
        assert!(matches!(error, ResidentVehicleActionError::VehicleMissing));

        task.abort();
        let _ = (&mut task).await;
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn resident_fleet_control_uses_selected_vehicle_shared_bearer_proxy_and_audit() {
        const SELECTED_EID: i64 = 70;
        const SELECTED_VIN: &str = "5YJ3E1EA7KF000001";
        const ACCESS_TOKEN: &str = "resident-fleet-access";

        crate::crypto::install_default_provider();
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let selected = select_native_setup_vehicle(
            vec![
                Vehicle::for_test(SELECTED_EID as u64, SELECTED_VIN, "online"),
                Vehicle::for_test(90, "5YJ3E1EA7KF000002", "online"),
            ],
            Some(SELECTED_EID),
        )
        .expect("explicit Fleet vehicle selection");
        let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        finish_collection_for_provider(
            &store,
            &cursor_key,
            &ManualCollection {
                vehicles: vec![selected],
                snapshots: Vec::new(),
                failures: Vec::new(),
            },
            CollectorProvider::Fleet,
        )
        .await
        .expect("persist selected Fleet vehicle");
        let (hub_vehicle_id, eid, _) = store
            .configured_tesla_vehicles()
            .expect("configured vehicles")
            .into_iter()
            .next()
            .expect("selected vehicle");
        assert_eq!(eid, SELECTED_EID);

        let credentials = FleetSetupCredentials::new(
            ACCESS_TOKEN.to_owned(),
            "resident-fleet-refresh".to_owned(),
            "resident-fleet-client".to_owned(),
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("Fleet credentials");
        crate::fleet_credentials::persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &credentials,
            SystemTime::now(),
        )
        .expect("persist encrypted Fleet credentials");
        let manager = Arc::new(tokio::sync::Mutex::new(
            FleetAuthManager::from_store(store.clone(), temporary.path())
                .expect("resident Fleet credential manager"),
        ));

        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let proxy_store = store.clone();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake proxy listener");
        let address = listener.local_addr().expect("fake proxy address");
        let router = Router::new().route(
            &format!("/api/1/vehicles/{SELECTED_VIN}/command/door_lock"),
            post(move |headers: HeaderMap, body: axum::body::Bytes| {
                let recorded = Arc::clone(&recorded);
                let proxy_store = proxy_store.clone();
                async move {
                    let audit_started = proxy_store
                        .open()
                        .expect("proxy-side audit catalogue")
                        .query_row(
                            "SELECT EXISTS(
                                SELECT 1 FROM outbound_request_receipts
                                 WHERE transport = 'fleet_api'
                                   AND operation = 'vehicle_command'
                                   AND outcome = 'started'
                            )",
                            [],
                            |row| row.get::<_, bool>(0),
                        )
                        .expect("pre-egress audit receipt");
                    recorded.lock().expect("proxy ledger").push((
                        audit_started,
                        headers.get("authorization").is_some_and(|value| {
                            value.as_bytes() == b"Bearer resident-fleet-access"
                        }),
                        headers
                            .get("content-type")
                            .is_some_and(|value| value.as_bytes() == b"application/json"),
                        body.to_vec(),
                    ));
                    axum::Json(json!({"response": {"result": true, "reason": ""}}))
                }
            }),
        );
        let proxy_server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("fake proxy server");
        });
        let proxy_base =
            FleetCommandProxyBase::parse_loopback_http_for_test(&format!("http://{address}/"))
                .expect("loopback proxy URL");
        let proxy = FleetCommandProxy::for_fake_http(proxy_base, Duration::from_secs(2))
            .expect("fake command proxy");
        let api = FleetApi::new(
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            Duration::from_secs(2),
        )
        .expect("Fleet API client");
        let auth_api = FleetAuthApi::new(
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            Duration::from_secs(2),
        )
        .expect("Fleet auth client");
        let socket = ResidentControlSocket::bind(temporary.path()).expect("resident socket");
        let mut resident =
            tokio::spawn(socket.serve_fleet(store.clone(), api, auth_api, Some(proxy), manager));

        let report = request_resident_vehicle_action(
            temporary.path(),
            hub_vehicle_id,
            LegacyVehicleAction::Lock,
        )
        .await
        .expect("resident Fleet command");
        assert_eq!(report.provider, CollectorProvider::Fleet);
        assert_eq!(report.hub_vehicle_id, hub_vehicle_id);
        assert_eq!(report.tesla_eid, SELECTED_EID);
        assert!(matches!(report.action, LegacyVehicleAction::Lock));
        assert_eq!(report.result.state, None);
        assert_eq!(
            *requests.lock().expect("proxy ledger"),
            vec![(true, true, true, b"{}".to_vec())]
        );

        let receipt = store
            .open()
            .expect("receipt catalogue")
            .query_row(
                "SELECT transport, operation, safety_class, precondition, outcome,
                        http_status, completed_at_ms IS NOT NULL
                   FROM outbound_request_receipts WHERE id = ?1",
                [report.audit_receipt_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<u16>>(5)?,
                        row.get::<_, bool>(6)?,
                    ))
                },
            )
            .expect("terminal outbound receipt");
        assert_eq!(
            receipt,
            (
                "fleet_api".to_owned(),
                "vehicle_command".to_owned(),
                "explicit_vehicle_command".to_owned(),
                "not_required".to_owned(),
                "success".to_owned(),
                Some(200),
                true,
            )
        );

        resident.abort();
        let _ = (&mut resident).await;
        proxy_server.abort();
        let _ = proxy_server.await;
    }

    #[tokio::test]
    async fn fleet_observer_revalidates_admission_before_discovery_egress() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        let credentials = FleetSetupCredentials::new(
            "observer-fleet-access".to_owned(),
            "observer-fleet-refresh".to_owned(),
            "observer-fleet-client".to_owned(),
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("Fleet credentials");
        crate::fleet_credentials::persist_fleet_setup_credentials(
            &store,
            temporary.path(),
            &credentials,
            SystemTime::now(),
        )
        .expect("persist Fleet credentials");
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(temporary.path())
            .expect("admit observer");
        let manager = Arc::new(tokio::sync::Mutex::new(
            FleetAuthManager::from_store_for_admitted_user(store, temporary.path(), admission)
                .expect("observer manager"),
        ));

        let requests = Arc::new(Mutex::new(0_usize));
        let recorded = Arc::clone(&requests);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake Fleet listener");
        let address = listener.local_addr().expect("fake Fleet address");
        let router = Router::new().route(
            "/api/1/vehicles",
            get(move || {
                let recorded = Arc::clone(&recorded);
                async move {
                    *recorded.lock().expect("request ledger") += 1;
                    axum::Json(json!({"response": [], "count": 0}))
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("fake Fleet server");
        });
        let base = url::Url::parse(&format!("http://{address}/")).expect("fake Fleet URL");
        let api = FleetApi::for_fake_http(base.clone(), Duration::from_secs(2))
            .expect("fake Fleet client");
        let auth_api = FleetAuthApi::for_fake_http(
            base.join("oauth2/v3/token").expect("fake auth URL"),
            Duration::from_secs(2),
        )
        .expect("fake auth client");

        let lock_path = temporary
            .path()
            .join(crate::user_lifetime_lock::LOCK_FILE_NAME);
        std::fs::remove_file(&lock_path).expect("remove admitted lock path");
        std::fs::write(&lock_path, b"").expect("replace admitted lock path");
        std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600))
            .expect("replacement lock mode");

        assert!(matches!(
            fleet_list_vehicles_with_auth(&api, &auth_api, &manager, false).await,
            Err(CollectorError::FleetCredential(
                FleetCredentialError::SensitiveAccessUnavailable
            ))
        ));
        assert_eq!(*requests.lock().expect("request ledger"), 0);

        server.abort();
        let _ = server.await;
    }

    #[test]
    fn fleet_push_refresh_retries_only_a_proven_unsent_request() {
        assert_eq!(
            fleet_refresh_retry_delay(&FleetCredentialError::Api(FleetApiError::RequestNotSent)),
            Some(FLEET_REFRESH_REQUEST_NOT_SENT_RETRY)
        );
        for error in [
            FleetApiError::RequestTimeout,
            FleetApiError::Transport,
            FleetApiError::HttpStatus(500),
            FleetApiError::InvalidResponse,
        ] {
            assert_eq!(
                fleet_refresh_retry_delay(&FleetCredentialError::Api(error)),
                None,
                "ambiguous Fleet refresh failure must remain terminal"
            );
        }
        assert_eq!(
            fleet_refresh_retry_delay(&FleetCredentialError::RotationOutcomeUnknown),
            None
        );
    }

    #[test]
    fn native_setup_requires_an_explicit_choice_for_multiple_vehicles() {
        let first = Vehicle::for_test(7, "5YJ3E1EA7KF000001", "asleep");
        let second = Vehicle::for_test(9, "5YJ3E1EA7KF000002", "online");
        assert!(matches!(
            select_native_setup_vehicle(vec![first.clone(), second.clone()], None),
            Err(CollectorError::NativeSetupVehicleSelectionRequired { discovered: 2 })
        ));
        assert_eq!(
            select_native_setup_vehicle(vec![first, second], Some(9))
                .expect("selected vehicle")
                .id
                .get(),
            9
        );
    }

    #[tokio::test]
    async fn fleet_setup_requires_every_configured_vehicle_by_eid_or_vin() {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        finish_collection_for_provider(
            &store,
            &cursor_key,
            &ManualCollection {
                vehicles: vec![
                    Vehicle::for_test(70, "5YJ3E1EA7KF000001", "online"),
                    Vehicle::for_test(90, "5YJ3E1EA7KF000002", "online"),
                ],
                snapshots: Vec::new(),
                failures: Vec::new(),
            },
            CollectorProvider::Fleet,
        )
        .await
        .expect("configured Fleet vehicles");

        let complete_inventory = vec![
            Vehicle::for_test(70, "5YJ3E1EA7KF000099", "online"),
            Vehicle::for_test(999, "5YJ3E1EA7KF000002", "online"),
        ];
        ensure_fleet_inventory_contains_configured(&store, &complete_inventory)
            .expect("EID or VIN matches every configured vehicle");

        assert!(matches!(
            ensure_fleet_inventory_contains_configured(&store, &complete_inventory[..1]),
            Err(CollectorError::FleetSetupInventoryMismatch)
        ));
    }

    #[tokio::test]
    async fn native_fleet_telemetry_commits_and_restores_through_fleet_projection() {
        const VIN: &str = "5YJ3E1EA7KF000001";
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        finish_collection_for_provider(
            &store,
            &cursor_key,
            &ManualCollection {
                vehicles: vec![Vehicle::for_test(70, VIN, "online")],
                snapshots: Vec::new(),
                failures: Vec::new(),
            },
            CollectorProvider::Fleet,
        )
        .await
        .expect("configured Fleet vehicle");
        assert!(
            fleet_telemetry_seed_for_vin(&store, VIN)
                .expect("empty seed lookup")
                .is_none()
        );

        let mut accumulator =
            crate::fleet_telemetry::FleetTelemetryAccumulator::empty(VIN).expect("accumulator");
        let t0 = current_epoch_millis().expect("clock") - 1_000;
        let snapshot = accumulator
            .apply_json(
                &serde_json::to_vec(&serde_json::json!({
                    "version": 1,
                    "vin": VIN,
                    "txid": "tx-drive-1",
                    "tx_type": "vehicle_data",
                    "received_at_ms": t0 + 100,
                    "timestamp_ms": t0,
                    "payload": {
                        "vin": VIN,
                        "createdAt": "2027-01-15T08:00:00Z",
                        "data": {
                            "Location": {"locationValue": {"latitude": 51.5, "longitude": -0.12}},
                            "VehicleSpeed": {"doubleValue": 48.0},
                            "Gear": {"stringValue": "drive"},
                            "Power": {"doubleValue": 12.0},
                            "BatteryLevel": {"doubleValue": 80.0},
                            "Soc": {"doubleValue": 79.0}
                        }
                    }
                }))
                .expect("telemetry JSON"),
            )
            .expect("telemetry snapshot");
        let report = persist_fleet_telemetry_snapshot(&store, &cursor_key, &snapshot)
            .await
            .expect("telemetry commit");
        assert_eq!(report.observations_inserted, 1);

        let restored = fleet_telemetry_seed_for_vin(&store, VIN)
            .expect("restored seed")
            .expect("Fleet state");
        assert_eq!(restored["charge_state"]["battery_level"], 80);
        assert_eq!(restored["drive_state"]["shift_state"], "D");

        let duplicate = persist_fleet_telemetry_snapshot(&store, &cursor_key, &snapshot)
            .await
            .expect("duplicate telemetry commit");
        assert_eq!(duplicate.observations_inserted, 0);
        assert_eq!(duplicate.observations_already_present, 1);
    }

    #[tokio::test]
    async fn fleet_vin_match_rotates_eid_without_losing_car_settings() {
        const VIN: &str = "5YJ3E1EA7KF000001";
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(temporary.path())
            .expect("cursor key");
        let mut original = Vehicle::for_test(70, VIN, "online");
        original.settings.enabled = false;
        original.settings.suspend_after_idle_min = 123;
        original.settings.suspend_min = 456;
        original.settings.req_not_unlocked = false;
        finish_collection_for_provider(
            &store,
            &cursor_key,
            &ManualCollection {
                vehicles: vec![original],
                snapshots: Vec::new(),
                failures: Vec::new(),
            },
            CollectorProvider::Fleet,
        )
        .await
        .expect("initial Fleet vehicle");

        let existing = store
            .configured_tesla_vehicles()
            .expect("configured vehicle");
        let mut rotated = Vehicle::for_test(999, VIN, "online");
        rotated.settings = configured_settings_for_discovered_vehicle(&store, &existing, &rotated)
            .expect("unambiguous VIN match")
            .expect("existing settings");
        rotated.settings.use_streaming_api = false;
        finish_collection_for_provider(
            &store,
            &cursor_key,
            &ManualCollection {
                vehicles: vec![rotated],
                snapshots: Vec::new(),
                failures: Vec::new(),
            },
            CollectorProvider::Fleet,
        )
        .await
        .expect("rotated Fleet vehicle");

        let configured = store
            .configured_tesla_vehicles()
            .expect("canonical configured vehicle");
        assert_eq!(configured.len(), 1);
        assert_eq!(configured[0].1, 999);
        assert!(!configured[0].2.enabled);
        assert!(!configured[0].2.use_streaming_api);
        assert_eq!(configured[0].2.suspend_after_idle_min, 123);
        assert_eq!(configured[0].2.suspend_min, 456);
        assert!(!configured[0].2.req_not_unlocked);
        assert_eq!(
            store
                .configured_tesla_vehicle_identity(configured[0].0)
                .expect("canonical identity"),
            Some((999, Some(VIN.to_owned())))
        );
        assert!(matches!(
            configured_fleet_vehicle_for_vin(&store, VIN),
            Err(CollectorError::SelectedVehicleMissing)
        ));
    }

    #[tokio::test]
    async fn native_setup_discovers_and_publishes_one_vehicle_without_wake() {
        use crate::fake_tesla::{AdvanceMode, FIXTURE_EID, FakeTeslaSource};

        crate::crypto::install_default_provider();
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
            .await
            .expect("loopback Tesla");
        let auth = LegacyAuth::for_test(
            fake.oauth_issuer_url(),
            "native-setup-access",
            "native-setup-refresh",
        );
        let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
            .expect("loopback Owner client");

        let report =
            setup_native_vehicle_with_client(&store, temporary.path(), &client, &auth, None)
                .await
                .expect("native setup");

        assert_eq!(report.selected_vehicle_id, FIXTURE_EID as i64);
        assert_eq!(report.snapshots_published, 1);
        assert_eq!(
            store.selected_tesla_eid().expect("selection").unwrap().0,
            FIXTURE_EID as i64
        );
        assert_eq!(store.published_vehicles().expect("vehicles").len(), 1);
        let requests = fake.audited_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, "/api/1/products");
        assert!(!requests[0].rejected);

        #[cfg(target_os = "macos")]
        {
            let tokens = OwnerTokens::from_secret_parts(
                "native-setup-access".to_owned(),
                "native-setup-refresh".to_owned(),
            )
            .expect("setup tokens");
            let key = b"native-setup-key";
            let (access, refresh) =
                crate::teslamate_token::encrypt_legacy_owner_tokens(key, &tokens)
                    .expect("encrypted setup tokens");
            let stored = crate::db::TeslaMateLegacyTokenStore::imported(access, refresh)
                .expect("stored setup tokens");
            crate::teslamate_credentials::replace_key_and_tokens(
                temporary.path(),
                &store,
                key,
                &stored,
            )
            .expect("persist setup tokens");
            crate::macos_launch_agent::preflight_hub(temporary.path())
                .expect("native setup is service-ready");
        }
    }

    #[tokio::test]
    async fn native_setup_can_configure_every_discovered_vehicle_in_one_request() {
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&requests);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake listener");
        let address = listener.local_addr().expect("fake address");
        let router = Router::new().route(
            "/api/1/products",
            get(move || {
                let counted = Arc::clone(&counted);
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({
                        "response": [
                            {"vehicle_id": 71, "id": 70, "vin": "5YJ3E1EA7KF000001", "state": "asleep", "display_name": "One"},
                            {"vehicle_id": 91, "id": 90, "vin": "5YJ3E1EA7KF000002", "state": "online", "display_name": "Two"}
                        ],
                        "count": 2
                    }))
                }
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("fake server");
        });
        let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");
        let client =
            OwnerApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("Owner client");
        let auth = LegacyAuth::for_test(base, "setup-access", "setup-refresh");
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");

        let report = setup_native_vehicles_with_client(&store, temporary.path(), &client, &auth)
            .await
            .expect("multi-vehicle setup");

        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(report.vehicles.len(), 2);
        assert_eq!(store.configured_tesla_vehicles().expect("cars").len(), 2);
        assert_eq!(store.published_vehicles().expect("published").len(), 2);
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn persists_a_collected_snapshot_and_retries_without_duplication() {
        let temp = crate::private_tempdir().expect("temporary store");
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

    #[tokio::test]
    async fn disabled_geocoder_leaves_pending_jobs_untouched() {
        let temp = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let source = store
            .register_source(&SourceDescriptor::new("test", "geocoder-disabled"), 1_000)
            .expect("source");
        let vehicle = store
            .register_vehicle(&VehicleDescriptor::new(source.source_id, "vehicle"), 1_000)
            .expect("vehicle");
        store
            .open()
            .expect("database")
            .execute(
                "INSERT INTO address_enrichment_jobs(
                    job_key, vehicle_id, target_type, target_id, field,
                    latitude, longitude, status, attempts, next_attempt_ms,
                    lease_until_ms
                 ) VALUES (?1, ?2, 'drive', 1, 'start_address', 1.0, 2.0,
                           'pending', 0, 0, 0)",
                rusqlite::params!["disabled-geocoder-job", vehicle.vehicle_id.to_string()],
            )
            .expect("address job");
        let config = HubConfig {
            data_dir: temp.path().to_path_buf(),
            bind: "127.0.0.1:8080".parse().expect("bind"),
            tls: None,
            collector: crate::config::CollectorConfig::default(),
            geocoder: crate::config::GeocoderConfig {
                enabled: false,
                ..crate::config::GeocoderConfig::default()
            },
            teslamate: crate::config::TeslaMateConfig::default(),
            terrain: TerrainConfig::default(),
        };

        assert!(
            !run_address_enrichment_once(
                &store,
                &config,
                &CursorKey::from_bytes([7; 32]),
                &[],
                1_000,
            )
            .await
            .expect("disabled enrichment")
        );
        let status = store
            .open()
            .expect("database")
            .query_row(
                "SELECT status FROM address_enrichment_jobs WHERE job_key = 'disabled-geocoder-job'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("job status");
        assert_eq!(status, "pending");
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

    #[tokio::test]
    async fn supervised_heartbeat_renews_during_idle_and_publishes_auth_recovery() {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let lease = store
            .acquire_supervised_collector_lease(current_epoch_millis().expect("clock"))
            .expect("collector lease");
        let initial_heartbeat: i64 = store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT heartbeat_at_ms FROM supervised_collector_lease",
                [],
                |row| row.get(0),
            )
            .expect("initial heartbeat");
        let (state, state_rx) = watch::channel(SupervisedCollectorState::Active);
        let (shutdown, stop) = oneshot::channel();
        let task = tokio::spawn(run_supervised_collector_heartbeat(
            store.clone(),
            lease,
            state_rx,
            stop,
            Duration::from_millis(20),
        ));

        // No scheduler work is running in this test. The independent ticker
        // must still advance the durable heartbeat during the idle period.
        tokio::time::sleep(Duration::from_millis(70)).await;
        let renewed_heartbeat: i64 = store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT heartbeat_at_ms FROM supervised_collector_lease",
                [],
                |row| row.get(0),
            )
            .expect("renewed heartbeat");
        assert!(renewed_heartbeat > initial_heartbeat);

        state.send_replace(SupervisedCollectorState::AuthenticationTerminal);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let terminal: String = store
            .open()
            .expect("catalogue")
            .query_row("SELECT state FROM supervised_collector_lease", [], |row| {
                row.get(0)
            })
            .expect("terminal state");
        assert_eq!(terminal, "auth_terminal");

        state.send_replace(SupervisedCollectorState::Active);
        tokio::time::sleep(Duration::from_millis(20)).await;
        store
            .service_readiness_at(true, current_epoch_millis().expect("clock"))
            .expect("authenticated success clears terminal readiness");

        shutdown.send(()).expect("heartbeat shutdown");
        task.await
            .expect("heartbeat task")
            .expect("heartbeat result");
        store
            .release_supervised_collector_lease(lease)
            .expect("release lease");
    }

    #[tokio::test]
    async fn supervised_heartbeat_survives_temporary_catalogue_write_rejection() {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let lease = store
            .acquire_supervised_collector_lease(current_epoch_millis().expect("clock"))
            .expect("collector lease");
        let initial_heartbeat: i64 = store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT heartbeat_at_ms FROM supervised_collector_lease",
                [],
                |row| row.get(0),
            )
            .expect("initial heartbeat");
        let blocker = store.open().expect("catalogue blocker");
        blocker
            .execute_batch(
                "CREATE TRIGGER reject_supervised_collector_heartbeat
                 BEFORE UPDATE OF heartbeat_at_ms ON supervised_collector_lease
                 BEGIN SELECT RAISE(ABORT, 'test heartbeat write rejection'); END;",
            )
            .expect("install temporary write rejection");

        let (_state, state_rx) = watch::channel(SupervisedCollectorState::Active);
        let (shutdown, stop) = oneshot::channel();
        let task = tokio::spawn(run_supervised_collector_heartbeat(
            store.clone(),
            lease,
            state_rx,
            stop,
            Duration::from_millis(20),
        ));
        tokio::time::sleep(Duration::from_millis(70)).await;
        assert!(
            !task.is_finished(),
            "temporary SQLite write failure must not stop collection"
        );

        blocker
            .execute_batch("DROP TRIGGER reject_supervised_collector_heartbeat")
            .expect("clear temporary write rejection");
        tokio::time::sleep(Duration::from_millis(70)).await;
        let renewed_heartbeat: i64 = store
            .open()
            .expect("catalogue")
            .query_row(
                "SELECT heartbeat_at_ms FROM supervised_collector_lease",
                [],
                |row| row.get(0),
            )
            .expect("renewed heartbeat");
        assert!(renewed_heartbeat > initial_heartbeat);

        shutdown.send(()).expect("heartbeat shutdown");
        task.await
            .expect("heartbeat task")
            .expect("heartbeat result");
        store
            .release_supervised_collector_lease(lease)
            .expect("release lease");
    }

    #[test]
    fn only_typed_terminal_auth_failures_trip_operational_readiness() {
        assert!(is_terminal_auth_failure(
            &CollectorError::SensitiveAccessUnavailable
        ));
        assert!(is_terminal_auth_failure(&CollectorError::OwnerApi(
            OwnerApiError::HttpStatus(401)
        )));
        assert!(!is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
            OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(LegacyAuthError::HttpStatus(
                403
            )))
        )));
        assert!(is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
            OwnerApiAuthError::NotSignedIn
        )));
        assert!(!is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
            OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401))
        )));
        assert!(is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
            OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(403))
        )));
        assert!(!is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
            OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
                LegacyAuthError::InvalidRefreshToken
            ))
        )));
        assert!(!is_terminal_auth_failure(&CollectorError::OwnerApiAuth(
            OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
                LegacyAuthError::RotationOutcomeUnknown
            ))
        )));
        assert!(!is_terminal_auth_failure(&CollectorError::OwnerApi(
            OwnerApiError::RateLimited {
                retry_after_seconds: 60,
            }
        )));
        assert!(!is_terminal_auth_failure(&CollectorError::OwnerApi(
            OwnerApiError::Transport
        )));
    }

    #[tokio::test]
    async fn stream_auth_rejection_fences_later_owner_api_success_until_healthy_stream() {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), Instant::now());
        scheduler.accept_discovery(vec![vehicle], Instant::now());
        let (events, receiver) = mpsc::channel(4);
        let (shutdown, stop) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = stop.await;
            Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
        });
        let mut streams = vec![VehicleStreamRuntime {
            vehicle_id,
            power_gate: Arc::new(StreamPowerGate::default()),
            sensitive_access_failure: Arc::new(AtomicBool::new(false)),
            events: receiver,
            _shutdown: Some(shutdown),
            task: Some(task),
        }];
        let (state, receiver) = watch::channel(SupervisedCollectorState::Active);
        let mut stream_authentication_rejected = false;

        events
            .send(StreamEvent::AuthRejected)
            .await
            .expect("auth rejection event");
        let transition = drain_stream_events(&store, &mut scheduler, &mut streams)
            .await
            .expect("drain rejection");
        assert_eq!(transition, StreamAuthenticationTransition::Rejected);
        report_stream_authentication_transition(
            &state,
            &mut stream_authentication_rejected,
            transition,
        );

        // A products 200 response says nothing about the Streaming API
        // credential. It must not clear an earlier stream 401/403.
        report_successful_owner_api_request(&state, stream_authentication_rejected);
        assert_eq!(
            *receiver.borrow(),
            SupervisedCollectorState::AuthenticationTerminal
        );

        let telemetry = crate::tesla_stream::parse_data_update(
            r#"{"msg_type":"data:update","tag":"9","value":"1,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#,
        )
        .expect("valid authenticated stream frame");
        events
            .send(StreamEvent::Telemetry(Box::new(telemetry)))
            .await
            .expect("authenticated telemetry event");
        let transition = drain_stream_events(&store, &mut scheduler, &mut streams)
            .await
            .expect("drain healthy stream");
        assert_eq!(transition, StreamAuthenticationTransition::Authenticated);
        report_stream_authentication_transition(
            &state,
            &mut stream_authentication_rejected,
            transition,
        );
        assert!(!stream_authentication_rejected);
        assert_eq!(*receiver.borrow(), SupervisedCollectorState::Active);

        stop_and_clear_manual_probe_streams(&mut streams).await;
        assert!(streams.is_empty());
    }

    #[tokio::test]
    async fn production_stream_queue_backpressures_and_drains_without_event_loss() {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let vehicle_id = VehicleId::from_test(19);
        let mut scheduler = VehicleScheduler::new(test_cadence(), Instant::now());
        scheduler.accept_discovery(
            vec![Vehicle::for_test(19, "5YJ3E1EA7KF000019", "online")],
            Instant::now(),
        );
        let (sender, receiver) = mpsc::channel(STREAM_EVENT_CHANNEL_CAPACITY);
        let (shutdown, stop) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = stop.await;
            Ok::<(), crate::tesla_stream::StreamSupervisorError>(())
        });
        let mut streams = vec![VehicleStreamRuntime {
            vehicle_id,
            power_gate: Arc::new(StreamPowerGate::default()),
            sensitive_access_failure: Arc::new(AtomicBool::new(false)),
            events: receiver,
            _shutdown: Some(shutdown),
            task: Some(task),
        }];
        let first_timestamp = current_epoch_millis().expect("clock")
            - i64::try_from((STREAM_EVENT_CHANNEL_CAPACITY + 2) * 200)
                .expect("production queue duration fits i64");
        let telemetry = |offset: usize| {
            StreamEvent::Telemetry(Box::new(crate::tesla_stream::StreamUpdate {
                tag: vehicle_id.to_string(),
                timestamp_ms: first_timestamp
                    + i64::try_from(offset * 200).expect("telemetry offset fits i64"),
                speed: Some(20),
                odometer: Some(100.0 + offset as f64 / 1_000.0),
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
            }))
        };
        for offset in 0..STREAM_EVENT_CHANNEL_CAPACITY {
            sender
                .try_send(telemetry(offset))
                .expect("fill production stream queue");
        }
        let final_sender = sender.clone();
        let final_event = telemetry(STREAM_EVENT_CHANNEL_CAPACITY);
        let mut blocked_sender = tokio::spawn(async move { final_sender.send(final_event).await });
        assert!(
            timeout(Duration::from_millis(20), &mut blocked_sender)
                .await
                .is_err(),
            "the bounded producer must backpressure while the queue is full"
        );

        let mut projection_car_ids = HashMap::new();
        let mut result = drain_stream_events_with_cache(
            &store,
            &mut scheduler,
            &mut streams,
            &mut projection_car_ids,
        )
        .await
        .expect("first bounded stream drain");
        assert!(result.backlog);
        timeout(Duration::from_secs(1), blocked_sender)
            .await
            .expect("backpressured sender resumes after drain")
            .expect("sender task")
            .expect("final event delivery");
        drop(sender);

        let mut drain_turns = 1;
        while result.backlog {
            tokio::task::yield_now().await;
            result = drain_stream_events_with_cache(
                &store,
                &mut scheduler,
                &mut streams,
                &mut projection_car_ids,
            )
            .await
            .expect("prioritized backlog drain");
            assert!(result.terminal_error.is_none());
            drain_turns += 1;
        }
        assert_eq!(
            drain_turns,
            (STREAM_EVENT_CHANNEL_CAPACITY + 1).div_ceil(MAX_STREAM_EVENTS_PER_DRAIN)
        );
        let registered = projection_car_ids[&vehicle_id].registered_vehicle_id;
        let positions: i64 = store
            .open()
            .expect("database")
            .query_row(
                "SELECT COUNT(*) FROM lifecycle_open_rows
                 WHERE vehicle_id = ?1 AND domain = 'position'",
                [registered.to_string()],
                |row| row.get(0),
            )
            .expect("stream positions");
        assert_eq!(positions, (STREAM_EVENT_CHANNEL_CAPACITY + 1) as i64);

        stop_and_clear_manual_probe_streams(&mut streams).await;
    }

    #[test]
    fn active_streams_bound_collection_sleep_below_queue_capacity() {
        assert_eq!(collection_sleep_cap(false), CONTROL_SETTINGS_REFRESH);
        assert_eq!(collection_sleep_cap(true), STREAM_EVENT_DRAIN_INTERVAL);
        assert!(STREAM_EVENT_DRAIN_INTERVAL < Duration::from_secs(1));
        const {
            assert!(STREAM_EVENT_CHANNEL_CAPACITY > MAX_STREAM_EVENTS_PER_DRAIN);
        }
    }

    #[derive(Clone)]
    struct LegacyRuntimeMock {
        unauthorized: Arc<AtomicUsize>,
        token_calls: Arc<AtomicUsize>,
        authorization: Arc<Mutex<Vec<String>>>,
    }

    #[derive(Clone)]
    struct Coordinated401Mock {
        owner_calls: Arc<AtomicUsize>,
        token_calls: Arc<AtomicUsize>,
        owner_pairs: Arc<Mutex<Vec<String>>>,
        refresh_pairs: Arc<Mutex<Vec<String>>>,
        token_entered: Arc<tokio::sync::Notify>,
        token_release: Arc<tokio::sync::Notify>,
    }

    #[derive(Clone)]
    struct ScriptedUnauthorizedMock {
        owner_calls: Arc<AtomicUsize>,
        token_calls: Arc<AtomicUsize>,
        owner_pairs: Arc<Mutex<Vec<String>>>,
        refresh_pairs: Arc<Mutex<Vec<String>>>,
        owner_statuses: Arc<Mutex<Vec<u16>>>,
        refresh_statuses: Arc<Mutex<Vec<u16>>>,
        owner_script: Arc<Mutex<Vec<u16>>>,
        refresh_script: Arc<Mutex<Vec<u16>>>,
    }

    fn label_owner_authorization(headers: &HeaderMap) -> &'static str {
        match headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
        {
            Some("Bearer old-access-secret") => "old_pair",
            _ => "unknown",
        }
    }

    fn label_refresh_body(body: &str) -> &'static str {
        if body.contains("old-refresh-secret") {
            "old_pair"
        } else {
            "unknown"
        }
    }

    async fn scripted_unauthorized_products(
        State(state): State<ScriptedUnauthorizedMock>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        state.owner_calls.fetch_add(1, Ordering::SeqCst);
        state
            .owner_pairs
            .lock()
            .unwrap()
            .push(label_owner_authorization(&headers).to_owned());
        let status = state.owner_script.lock().unwrap().remove(0);
        state.owner_statuses.lock().unwrap().push(status);
        (
            StatusCode::from_u16(status).unwrap(),
            "scripted owner response",
        )
    }

    async fn scripted_unauthorized_token(
        State(state): State<ScriptedUnauthorizedMock>,
        body: String,
    ) -> impl IntoResponse {
        state.token_calls.fetch_add(1, Ordering::SeqCst);
        state
            .refresh_pairs
            .lock()
            .unwrap()
            .push(label_refresh_body(&body).to_owned());
        let status = state.refresh_script.lock().unwrap().remove(0);
        state.refresh_statuses.lock().unwrap().push(status);
        (
            StatusCode::from_u16(status).unwrap(),
            "scripted token response",
        )
    }

    async fn coordinated_401_products(
        State(state): State<Coordinated401Mock>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        state.owner_calls.fetch_add(1, Ordering::SeqCst);
        let pair = match headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
        {
            Some("Bearer old-access-secret") => "old_pair",
            _ => "unknown",
        };
        state.owner_pairs.lock().unwrap().push(pair.to_owned());
        (StatusCode::UNAUTHORIZED, "unauthorized")
    }

    async fn coordinated_blocked_token(
        State(state): State<Coordinated401Mock>,
    ) -> impl IntoResponse {
        state.token_calls.fetch_add(1, Ordering::SeqCst);
        let release = state.token_release.notified();
        tokio::pin!(release);
        release.as_mut().enable();
        state.token_entered.notify_waiters();
        release.await;
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

    async fn coordinated_failed_token(
        State(state): State<Coordinated401Mock>,
        body: String,
    ) -> impl IntoResponse {
        state.token_calls.fetch_add(1, Ordering::SeqCst);
        let pair = if body.contains("old-refresh-secret") {
            "old_pair"
        } else {
            "unknown"
        };
        state.refresh_pairs.lock().unwrap().push(pair.to_owned());
        (StatusCode::SERVICE_UNAVAILABLE, "refresh failed")
    }

    async fn coordinated_success_token(
        State(state): State<Coordinated401Mock>,
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

    fn coordinated_legacy_auth(
        issuer: url::Url,
        persisted: Arc<Mutex<(String, String)>>,
    ) -> CollectionAuth {
        let persisted_for_callback = Arc::clone(&persisted);
        let auth = crate::legacy_auth::LegacyAuth::for_test(
            issuer,
            "old-access-secret",
            "old-refresh-secret",
        )
        .with_test_schedule(2_000_000_000, 1_900_000_000);
        CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
                auth,
                Arc::new(move |access, refresh| {
                    *persisted_for_callback.lock().expect("durable pair lock") =
                        (access.to_owned(), refresh.to_owned());
                    Ok(())
                }),
            ))),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region: StreamRegion::Global,
        }
    }

    fn coordinated_test_legacy_auth(issuer: url::Url) -> CollectionAuth {
        let auth = crate::legacy_auth::LegacyAuth::for_test(
            issuer,
            "old-access-secret",
            "old-refresh-secret",
        )
        .with_test_schedule(2_000_000_000, 1_900_000_000);
        let manager = LegacyAuthManager::for_test_with_active_pair(auth).expect("active test pair");
        CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region: StreamRegion::Global,
        }
    }

    pub(crate) async fn test_unauthorized_six_restart_facade(
        owner_script: &[u16],
        refresh_script: &[u16],
    ) -> Result<LegacyUnauthorizedFacadeObservation, String> {
        if owner_script.len() != 6 || refresh_script.len() != 6 {
            return Err("unauthorized fixture requires six owner and token statuses".to_owned());
        }
        if owner_script
            .iter()
            .any(|status| StatusCode::from_u16(*status).is_err())
            || refresh_script
                .iter()
                .any(|status| StatusCode::from_u16(*status).is_err())
        {
            return Err("unauthorized fixture contains invalid HTTP status".to_owned());
        }
        let state = ScriptedUnauthorizedMock {
            owner_calls: Arc::new(AtomicUsize::new(0)),
            token_calls: Arc::new(AtomicUsize::new(0)),
            owner_pairs: Arc::new(Mutex::new(Vec::new())),
            refresh_pairs: Arc::new(Mutex::new(Vec::new())),
            owner_statuses: Arc::new(Mutex::new(Vec::new())),
            refresh_statuses: Arc::new(Mutex::new(Vec::new())),
            owner_script: Arc::new(Mutex::new(owner_script.to_vec())),
            refresh_script: Arc::new(Mutex::new(refresh_script.to_vec())),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = url::Url::parse(&format!("http://{address}/")).unwrap();
        let issuer = base.join("oauth2/v3/").unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/api/1/products", get(scripted_unauthorized_products))
                    .route("/oauth2/v3/token", post(scripted_unauthorized_token))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });
        let auth = coordinated_test_legacy_auth(issuer);
        let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();
        for _ in 0..5 {
            let error = match list_vehicles_for_auth(&client, &auth).await {
                Err(error) => error,
                Ok(_) => {
                    shutdown_legacy_refresh(&auth).await;
                    server.abort();
                    return Err("scripted owner request unexpectedly succeeded".to_owned());
                }
            };
            if !is_wrapped_legacy_unauthorized(&error) {
                shutdown_legacy_refresh(&auth).await;
                server.abort();
                return Err(format!(
                    "scripted owner status did not yield wrapped 401: {error}"
                ));
            }
        }
        // The production coordinator runs refreshes asynchronously.  Drain the
        // fifth actual forced refresh before the sixth 401 melts the fuse.
        if let Err(error) = wait_for_legacy_refresh_before_owner(&auth).await {
            shutdown_legacy_refresh(&auth).await;
            server.abort();
            return Err(format!(
                "scripted refresh unexpectedly became terminal: {error}"
            ));
        }
        let sixth = match list_vehicles_for_auth(&client, &auth).await {
            Err(error) => error,
            Ok(_) => {
                shutdown_legacy_refresh(&auth).await;
                server.abort();
                return Err("sixth scripted owner request unexpectedly succeeded".to_owned());
            }
        };
        if !matches!(
            sixth,
            CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
        ) {
            shutdown_legacy_refresh(&auth).await;
            server.abort();
            return Err("sixth owner request did not melt the production fuse".to_owned());
        }
        let CollectionAuth::Legacy { manager, fuse, .. } = &auth;
        let fuse_blown = fuse.try_lock().unwrap().is_blown();
        if !fuse_blown {
            shutdown_legacy_refresh(&auth).await;
            server.abort();
            return Err("production fuse was not blown".to_owned());
        }
        let manager = manager.lock().await;
        let durable_pair = if manager.access_token() == "old-access-secret" {
            "old_pair".to_owned()
        } else {
            "unknown".to_owned()
        };
        let durable_matches = manager
            .test_pair_matches("old-access-secret", "old-refresh-secret")
            .map_err(|error| format!("managed pair read failed: {error}"))?;
        if !durable_matches {
            shutdown_legacy_refresh(&auth).await;
            server.abort();
            return Err("managed pair changed predecessor pair".to_owned());
        }
        drop(manager);

        let tls_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| format!("TLS bind failed: {error}"))?;
        let tls_address = tls_listener
            .local_addr()
            .map_err(|error| format!("TLS address failed: {error}"))?;
        crate::crypto::install_default_provider();
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["auth.tesla.com".to_owned()])
                .map_err(|error| format!("TLS certificate failed: {error}"))?;
        let certificate_pem = cert.pem();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            certificate_pem.as_bytes().to_vec(),
            signing_key.serialize_pem().into_bytes(),
        )
        .await
        .map_err(|error| format!("TLS config failed: {error}"))?;
        let tls_state = state.clone();
        let tls_server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(tls_listener.into_std().expect("std TLS listener"), tls)
                .expect("TLS server")
                .serve(
                    Router::new()
                        .route("/oauth2/v3/token", post(scripted_unauthorized_token))
                        .with_state(tls_state)
                        .into_make_service(),
                )
                .await
                .expect("TLS serve");
        });
        let certificate = Certificate::from_pem(certificate_pem.as_bytes())
            .map_err(|error| format!("TLS root failed: {error}"))?;
        let startup_client = Client::builder()
            .https_only(true)
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(2))
            .add_root_certificate(certificate)
            .resolve("auth.tesla.com", tls_address)
            .build()
            .map_err(|error| format!("TLS client failed: {error}"))?;
        let mut restarted = crate::legacy_auth::LegacyAuth::from_persisted_state(
            "old-access-secret",
            "old-refresh-secret",
            0,
            0,
        )
        .unwrap();
        let restart_epoch = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let restart_result = restarted
            .refresh_if_due_persisted(&startup_client, restart_epoch, |_, _, _, _| Ok(()))
            .await;
        if restart_result != Err(crate::legacy_auth::LegacyAuthError::HttpStatus(400)) {
            shutdown_legacy_refresh(&auth).await;
            server.abort();
            tls_server.abort();
            return Err("imported startup did not use sixth scripted HTTP 400".to_owned());
        }
        let restart_retry_ms = restarted
            .retry_at()
            .map(|retry_at| (retry_at - 1_700_000_000) * 1_000)
            .ok_or_else(|| "imported startup did not schedule retry".to_owned())?;
        let pre_restart_retry_ms =
            i64::try_from(crate::legacy_auth::REFRESH_RETRY_DELAY.as_millis())
                .map_err(|_| "production retry delay exceeds i64".to_owned())?;
        shutdown_legacy_refresh(&auth).await;
        server.abort();
        tls_server.abort();
        let owner_requests = state.owner_calls.load(Ordering::SeqCst);
        let refresh_requests = state.token_calls.load(Ordering::SeqCst);
        let owner_pairs = state.owner_pairs.lock().unwrap().clone();
        let refresh_pairs = state.refresh_pairs.lock().unwrap().clone();
        let owner_statuses = state.owner_statuses.lock().unwrap().clone();
        let refresh_statuses = state.refresh_statuses.lock().unwrap().clone();
        if !state.owner_script.lock().unwrap().is_empty()
            || !state.refresh_script.lock().unwrap().is_empty()
        {
            return Err("scripted owner or token responses were not exhausted".to_owned());
        }
        Ok(LegacyUnauthorizedFacadeObservation {
            owner_retries: owner_requests.saturating_sub(owner_statuses.len()),
            owner_requests,
            refresh_requests,
            owner_pairs,
            refresh_pairs,
            owner_statuses,
            refresh_statuses,
            durable_pair,
            logical_resident_pair: if fuse_blown { "none" } else { "unknown" }.to_owned(),
            attempts_before_signout: refresh_requests.saturating_sub(1),
            fuse_melts: state.owner_calls.load(Ordering::SeqCst),
            fuse_blown,
            pre_restart_retry_ms,
            restart_retry_ms,
        })
    }

    #[tokio::test]
    async fn unauthorized_facade_uses_scripted_responses_not_constants() {
        let result = test_unauthorized_six_restart_facade(&[401; 6], &[400; 6])
            .await
            .expect("fixture script must drive production facade");
        assert_eq!(result.owner_statuses, vec![401; 6]);
        assert_eq!(result.refresh_statuses, vec![400; 6]);
        assert_eq!(result.pre_restart_retry_ms, 300_000);
        assert_eq!(result.restart_retry_ms, 450_000);
        let result = test_unauthorized_six_restart_facade(&[500; 6], &[400; 6]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn legacy_401_returns_before_blocked_refresh_and_next_owner_waits_for_ticket() {
        let state = Coordinated401Mock {
            owner_calls: Arc::new(AtomicUsize::new(0)),
            token_calls: Arc::new(AtomicUsize::new(0)),
            owner_pairs: Arc::new(Mutex::new(Vec::new())),
            refresh_pairs: Arc::new(Mutex::new(Vec::new())),
            token_entered: Arc::new(tokio::sync::Notify::new()),
            token_release: Arc::new(tokio::sync::Notify::new()),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = url::Url::parse(&format!("http://{address}/")).unwrap();
        let issuer = base.join("oauth2/v3/").unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/api/1/products", get(coordinated_401_products))
                    .route("/oauth2/v3/token", post(coordinated_blocked_token))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });
        let durable = Arc::new(Mutex::new((
            "old-access".to_owned(),
            "old-refresh".to_owned(),
        )));
        let auth = coordinated_legacy_auth(issuer, durable);
        let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();

        let first = timeout(
            Duration::from_secs(1),
            list_vehicles_for_auth(&client, &auth),
        )
        .await
        .expect("first owner response must not wait for refresh");
        assert!(is_wrapped_legacy_unauthorized(first.as_ref().unwrap_err()));
        timeout(Duration::from_secs(1), async {
            while state.token_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("single refresh must start asynchronously");
        assert_eq!(state.owner_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);

        let second = list_vehicles_for_auth(&client, &auth);
        tokio::pin!(second);
        assert!(
            timeout(Duration::from_millis(50), &mut second)
                .await
                .is_err()
        );
        assert_eq!(state.owner_calls.load(Ordering::SeqCst), 1);
        assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);
        state.token_release.notify_one();
        let second = timeout(Duration::from_secs(1), &mut second)
            .await
            .expect("second logical request must resume after ticket");
        assert!(is_wrapped_legacy_unauthorized(&second.unwrap_err()));
        assert_eq!(state.owner_calls.load(Ordering::SeqCst), 2);

        timeout(Duration::from_secs(1), async {
            while state.token_calls.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second accepted refresh starts");
        let shutdown = shutdown_legacy_refresh(&auth);
        tokio::pin!(shutdown);
        assert!(
            timeout(Duration::from_millis(50), &mut shutdown)
                .await
                .is_err(),
            "normal shutdown must await an accepted refresh"
        );
        state.token_release.notify_one();
        timeout(Duration::from_secs(1), &mut shutdown)
            .await
            .expect("normal shutdown drains accepted refresh");
        assert_eq!(state.token_calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn one_shot_legacy_drain_waits_for_queued_refresh_before_shutdown() {
        let state = Coordinated401Mock {
            owner_calls: Arc::new(AtomicUsize::new(0)),
            token_calls: Arc::new(AtomicUsize::new(0)),
            owner_pairs: Arc::new(Mutex::new(Vec::new())),
            refresh_pairs: Arc::new(Mutex::new(Vec::new())),
            token_entered: Arc::new(tokio::sync::Notify::new()),
            token_release: Arc::new(tokio::sync::Notify::new()),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = url::Url::parse(&format!("http://{address}/")).unwrap();
        let issuer = base.join("oauth2/v3/").unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/api/1/products", get(coordinated_401_products))
                    .route("/oauth2/v3/token", post(coordinated_blocked_token))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });
        let durable = Arc::new(Mutex::new((
            "old-access".to_owned(),
            "old-refresh".to_owned(),
        )));
        let auth = coordinated_legacy_auth(issuer, durable);
        let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();
        let result = list_vehicles_for_auth(&client, &auth).await;
        let error = result.unwrap_err();
        assert!(is_wrapped_legacy_unauthorized(&error));
        timeout(Duration::from_secs(1), async {
            while state.token_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let drain = drain_and_shutdown_legacy_refresh(&auth);
        tokio::pin!(drain);
        assert!(
            timeout(Duration::from_millis(50), &mut drain)
                .await
                .is_err()
        );
        state.token_release.notify_one();
        timeout(Duration::from_secs(1), &mut drain)
            .await
            .expect("one-shot shutdown drains its queued refresh")
            .expect("non-sensitive refresh failure remains retryable");
        assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn legacy_six_401_failures_keep_old_pair_and_blow_fuse_without_owner_retry() {
        let state = Coordinated401Mock {
            owner_calls: Arc::new(AtomicUsize::new(0)),
            token_calls: Arc::new(AtomicUsize::new(0)),
            owner_pairs: Arc::new(Mutex::new(Vec::new())),
            refresh_pairs: Arc::new(Mutex::new(Vec::new())),
            token_entered: Arc::new(tokio::sync::Notify::new()),
            token_release: Arc::new(tokio::sync::Notify::new()),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = url::Url::parse(&format!("http://{address}/")).unwrap();
        let issuer = base.join("oauth2/v3/").unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/api/1/products", get(coordinated_401_products))
                    .route("/oauth2/v3/token", post(coordinated_failed_token))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });
        let auth = coordinated_test_legacy_auth(issuer);
        let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();

        for _ in 0..5 {
            let result = list_vehicles_for_auth(&client, &auth).await;
            let error = result.unwrap_err();
            assert!(is_wrapped_legacy_unauthorized(&error));
            assert!(!is_terminal_auth_failure(&error));
        }
        let sixth = list_vehicles_for_auth(&client, &auth).await.unwrap_err();
        assert!(matches!(
            sixth,
            CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn)
        ));
        assert!(is_terminal_auth_failure(&sixth));
        assert_eq!(state.owner_calls.load(Ordering::SeqCst), 6);
        assert_eq!(state.token_calls.load(Ordering::SeqCst), 5);
        assert!(matches!(
            &auth,
            CollectionAuth::Legacy { fuse, .. } if fuse.try_lock().unwrap().is_blown()
        ));
        let CollectionAuth::Legacy { manager, .. } = &auth;
        let manager = manager.lock().await;
        assert_eq!(manager.access_token(), "old-access-secret");
        assert_eq!(manager.refresh_token(), "old-refresh-secret");
        assert!(
            manager
                .test_pair_matches("old-access-secret", "old-refresh-secret")
                .expect("managed predecessor pair")
        );
        drop(manager);
        shutdown_legacy_refresh(&auth).await;
        server.abort();
    }

    #[tokio::test]
    async fn successful_coordinator_refresh_resets_the_legacy_401_fuse() {
        let state = Coordinated401Mock {
            owner_calls: Arc::new(AtomicUsize::new(0)),
            token_calls: Arc::new(AtomicUsize::new(0)),
            owner_pairs: Arc::new(Mutex::new(Vec::new())),
            refresh_pairs: Arc::new(Mutex::new(Vec::new())),
            token_entered: Arc::new(tokio::sync::Notify::new()),
            token_release: Arc::new(tokio::sync::Notify::new()),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let base = url::Url::parse(&format!("http://{address}/")).unwrap();
        let issuer = base.join("oauth2/v3/").unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/api/1/products", get(coordinated_401_products))
                    .route("/oauth2/v3/token", post(coordinated_success_token))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });
        let auth = coordinated_test_legacy_auth(issuer);
        let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();
        for _ in 0..6 {
            let error = list_vehicles_for_auth(&client, &auth).await.unwrap_err();
            assert!(is_wrapped_legacy_unauthorized(&error));
        }
        assert_eq!(state.owner_calls.load(Ordering::SeqCst), 6);
        assert!(state.token_calls.load(Ordering::SeqCst) >= 5);
        assert!(matches!(
            &auth,
            CollectionAuth::Legacy { fuse, .. } if !fuse.try_lock().unwrap().is_blown()
        ));
        shutdown_legacy_refresh(&auth).await;
        server.abort();
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

        let auth = crate::legacy_auth::LegacyAuth::for_test(
            issuer,
            "old-access-secret",
            "old-refresh-secret",
        )
        .with_test_schedule(2_000_000_000, 1_900_000_000);
        let manager = Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
            auth,
            Arc::new(|_, _| Ok(())),
        )));
        let collection_auth = CollectionAuth::Legacy {
            manager: Arc::clone(&manager),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region: StreamRegion::Global,
        };
        let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).unwrap();
        assert!(matches!(
            list_vehicles_for_auth(&client, &collection_auth).await,
            Err(CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(
                OwnerApiError::HttpStatus(401)
            )))
        ));
        let vehicles = list_vehicles_for_auth(&client, &collection_auth)
            .await
            .unwrap();
        assert_eq!(vehicles.len(), 1);
        assert_eq!(state.token_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            state.authorization.lock().unwrap().as_slice(),
            &["Bearer old-access-secret", "Bearer rotated-access"]
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
        let supervisor = TeslaStreamSupervisor::new_legacy_auth_for_test(
            VehicleId::from_test(9),
            StreamVehicleId::from_test(9),
            manager,
            StreamRegion::Global,
            ws_endpoint,
            client.legacy_auth_http_client(),
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
        let temp = crate::private_tempdir().expect("temporary store");
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
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        let published = publish_compatibility_snapshots(
            &store,
            &publication_gate,
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
            vec![
                crate::protocol::MirrorTable::Car,
                crate::protocol::MirrorTable::State,
                crate::protocol::MirrorTable::Update,
            ]
        );
        assert_eq!(store.published_vehicles().expect("published cars").len(), 1);
    }

    #[test]
    fn fleet_collection_round_trips_sanitized_provider_raw_json_without_duplication() {
        let temp = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let raw = json!({
            "response": {
                "drive_state": {"shift_state": "P", "timestamp": 1_800_000_000_000_i64},
                "charge_state": {
                    "battery_level": 80,
                    "charge_limit_soc": 90,
                    "future_secret_name": "secret"
                },
                "vehicle_state": {
                    "software_update": {
                        "status": "available",
                        "version": "2026.20",
                        "expected_duration_sec": 900
                    }
                },
                "unknown_group": {"battery_level": 1}
            },
            "provider_trace": "fleet-trace"
        });
        let collection = ManualCollection {
            vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
            snapshots: vec![
                VehicleData::from_provider_raw_json(VehicleId::from_test(9), raw.clone())
                    .expect("Fleet response"),
            ],
            failures: vec![],
        };

        persist_collection_atomic_for_provider(
            &store,
            &collection,
            1_800_000_000_001,
            CollectorProvider::Fleet,
        )
        .expect("Fleet raw observation persists");
        let vehicle_id = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("vehicle UUID");
        let observations = store
            .current_observations_for_vehicle(vehicle_id)
            .expect("current Fleet observation");
        let fleet = observations
            .iter()
            .find(|observation| observation.payload["record_type"] == "fleet_api_vehicle_data_v1")
            .expect("Fleet current observation");
        assert_eq!(
            fleet.payload["provider_raw_json"],
            json!({
                "response": {
                    "drive_state": {
                        "shift_state": "P",
                        "timestamp": 1_800_000_000_000_i64
                    },
                    "charge_state": {"battery_level": 80, "charge_limit_soc": 90},
                    "vehicle_state": {
                        "software_update": {
                            "status": "available",
                            "version": "2026.20"
                        }
                    }
                }
            })
        );
        let rendered = fleet.payload["provider_raw_json"].to_string();
        for rejected in [
            "provider_trace",
            "unknown_group",
            "future_secret_name",
            "expected_duration_sec",
            "fleet-trace",
            "secret",
        ] {
            assert!(!rendered.contains(rejected), "field survived: {rejected}");
        }
        assert!(fleet.payload.get("vehicle_data").is_none());
    }

    #[test]
    fn fleet_atomic_collection_closes_located_drive_after_restart_without_duplicates() {
        let temp = crate::private_tempdir().expect("temporary store");
        let t0 = 1_800_000_100_000_i64;
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
        let collection = |vehicle_data: serde_json::Value| ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![
                VehicleData::from_provider_raw_json(
                    VehicleId::from_test(9),
                    json!({"response": vehicle_data}),
                )
                .expect("Fleet response"),
            ],
            failures: vec![],
        };

        let store = HubStore::initialize(temp.path()).expect("store");
        let first = collection(json!({
            "drive_state": {
                "shift_state": "D",
                "speed": 20,
                "latitude": 47.5,
                "longitude": 19.0,
                "timestamp": t0
            },
            "vehicle_state": {"odometer": 1000.0}
        }));
        persist_collection_atomic_for_provider(&store, &first, t0, CollectorProvider::Fleet)
            .expect("open Fleet drive");
        drop(store);

        let store = HubStore::initialize(temp.path()).expect("restart store");
        let second = collection(json!({
            "drive_state": {
                "shift_state": "D",
                "speed": 30,
                "latitude": 47.51,
                "longitude": 19.01,
                "timestamp": t0 + 60_000
            },
            "vehicle_state": {"odometer": 1001.0}
        }));
        persist_collection_atomic_for_provider(
            &store,
            &second,
            t0 + 60_000,
            CollectorProvider::Fleet,
        )
        .expect("continue Fleet drive");

        let terminal = collection(json!({
            "drive_state": {
                "shift_state": null,
                "speed": null,
                "latitude": 47.52,
                "longitude": 19.02,
                "timestamp": t0 + 120_000
            },
            "vehicle_state": {"odometer": 1002.0}
        }));
        let report = persist_collection_atomic_for_provider(
            &store,
            &terminal,
            t0 + 120_000,
            CollectorProvider::Fleet,
        )
        .expect("close Fleet drive");
        assert_eq!(report.drives_closed, 1);
        assert_eq!(report.positions_materialised, 2);
        assert_eq!(report.lifecycle_quarantines, 0);

        let duplicate = persist_collection_atomic_for_provider(
            &store,
            &terminal,
            t0 + 120_001,
            CollectorProvider::Fleet,
        )
        .expect("repeat terminal sample");
        assert_eq!(duplicate.observations_already_present, 1);
        assert_eq!(duplicate.drives_closed, 0);

        let connection = store.open().expect("database");
        let vehicle_id = connection
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("vehicle UUID");
        let lifecycle = store
            .load_lifecycle_state(vehicle_id)
            .expect("lifecycle query")
            .expect("lifecycle state");
        let open = OpenSessionState::decode(&lifecycle.open_session_json).expect("open session");
        assert!(open.open_drive.is_none());
        assert!(!lifecycle.quarantined);
        let drive_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM materialised_drives", [], |row| {
                row.get(0)
            })
            .expect("drive count");
        assert_eq!(drive_count, 1);
        let position_counts: (i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COUNT(drive_id) FROM materialised_positions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("position counts");
        assert_eq!(position_counts, (4, 3));
    }

    #[test]
    fn fleet_atomic_collection_discards_one_position_drive_without_open_row_leak() {
        let temp = crate::private_tempdir().expect("temporary store");
        let t0 = 1_800_000_400_000_i64;
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
        let collection = |vehicle_data: serde_json::Value| ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![
                VehicleData::from_provider_raw_json(
                    VehicleId::from_test(9),
                    json!({"response": vehicle_data}),
                )
                .expect("Fleet response"),
            ],
            failures: vec![],
        };

        let store = HubStore::initialize(temp.path()).expect("store");
        let moving = collection(json!({
            "drive_state": {
                "shift_state": "D",
                "speed": 20,
                "latitude": 47.5,
                "longitude": 19.0,
                "timestamp": t0
            },
            "vehicle_state": {"odometer": 1000.0}
        }));
        persist_collection_atomic_for_provider(&store, &moving, t0, CollectorProvider::Fleet)
            .expect("open Fleet drive");
        drop(store);

        let store = HubStore::initialize(temp.path()).expect("restart store");
        let parked = collection(json!({
            "drive_state": {
                "shift_state": null,
                "speed": null,
                "timestamp": t0 + 60_000
            },
            "vehicle_state": {"odometer": 1000.1}
        }));
        let report = persist_collection_atomic_for_provider(
            &store,
            &parked,
            t0 + 60_000,
            CollectorProvider::Fleet,
        )
        .expect("discard incomplete Fleet drive");
        assert_eq!(report.drives_closed, 0);

        let connection = store.open().expect("database");
        let drive_row_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM lifecycle_open_rows
                 WHERE domain IN ('drive', 'position')",
                [],
                |row| row.get(0),
            )
            .expect("drive row count");
        assert_eq!(drive_row_count, 0);
        let completed: (i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM materialised_drives),
                    (SELECT COUNT(*) FROM materialised_positions)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("completed row counts");
        assert_eq!(completed, (0, 0));
    }

    #[test]
    fn non_atomic_batch_discards_short_drive_without_rows_after_restart() {
        let temp = crate::private_tempdir().expect("temporary store");
        let t0 = 1_800_000_500_000_i64;
        let collection = ManualCollection {
            vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
            snapshots: vec![
                VehicleData::for_test(
                    9,
                    json!({
                        "drive_state": {
                            "shift_state": "D",
                            "speed": 20,
                            "latitude": 47.5,
                            "longitude": 19.0,
                            "timestamp": t0
                        },
                        "vehicle_state": {"odometer": 1000.0}
                    }),
                ),
                VehicleData::for_test(
                    9,
                    json!({
                        "drive_state": {
                            "shift_state": "P",
                            "speed": 0,
                            "timestamp": t0 + 60_000
                        },
                        "vehicle_state": {"odometer": 1000.1}
                    }),
                ),
            ],
            failures: vec![],
        };
        let store = HubStore::initialize(temp.path()).expect("store");
        persist_collection(&store, &collection, t0 + 60_000).expect("persist observations");
        materialise_lifecycle_for_collection(&store, &collection, t0 + 60_000)
            .expect("discard incomplete drive");
        drop(store);

        let store = HubStore::initialize(temp.path()).expect("restart store");
        let connection = store.open().expect("database");
        let drive_row_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM lifecycle_open_rows
                 WHERE domain IN ('drive', 'position')",
                [],
                |row| row.get(0),
            )
            .expect("drive row count");
        assert_eq!(drive_row_count, 0);
        let completed: (i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM materialised_drives),
                    (SELECT COUNT(*) FROM materialised_positions)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("completed row counts");
        assert_eq!(completed, (0, 0));
    }

    #[test]
    fn oversized_compaction_defers_only_while_an_aggregate_slot_remains() {
        let limits = ProtocolLimits::default();
        let row_capacity = ProjectionPackError::TooManyRows;
        assert!(may_defer_compaction_capacity_error(
            &row_capacity,
            limits.max_chunks - 1,
            limits,
        ));
        assert!(!may_defer_compaction_capacity_error(
            &row_capacity,
            limits.max_chunks,
            limits,
        ));
        assert!(is_compaction_pack_capacity_error(
            &ProjectionPackError::Protocol(ProtocolError::UncompressedSizeOutOfBounds(
                limits.max_uncompressed_pack_bytes + 1,
            )),
        ));
        assert!(!may_defer_compaction_capacity_error(
            &ProjectionPackError::Invalid("malformed compaction payload".into()),
            limits.max_chunks - 1,
            limits,
        ));
        assert!(is_compaction_catalog_capacity_error(&StoreError::Manifest(
            ProtocolError::LineageAggregateLimitExceeded
        )));
    }

    #[test]
    fn near_limit_collection_compacts_live_suffix_before_consuming_the_next_slot() {
        let temp = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let cursor_key = CursorKey::from_bytes([18; 32]);
        let now = 1_800_000_000_000_i64;
        let collection = ManualCollection {
            vehicles: vec![Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online")],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {"timestamp": now},
                    "vehicle_config": {"car_type": "model3"},
                    "vehicle_state": {"car_version": "2026.20"}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &collection, now).expect("raw observation");
        materialise_lifecycle_for_collection(&store, &collection, now).expect("lifecycle");
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        publish_compatibility_snapshots(&store, &publication_gate, &cursor_key, &collection, now)
            .expect("base publication");
        drop(publication_gate);
        let vehicle_id = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("vehicle UUID");
        let base = store
            .lineage_manifest_for_vehicle(vehicle_id)
            .expect("base lineage")
            .expect("published base")
            .base;

        for (index, enabled) in [false, true].into_iter().enumerate() {
            store
                .upsert_car_settings(
                    vehicle_id,
                    9,
                    &crate::hub_pack::ProjectionCarSettings {
                        enabled,
                        ..crate::hub_pack::ProjectionCarSettings::default()
                    },
                )
                .expect("settings mutation");
            let claim = store
                .claim_sync_mutations(vehicle_id, now + index as i64 + 1, 100)
                .expect("claim mutation")
                .expect("pending live mutation");
            publish_v2_delta(&store, &cursor_key, &claim).expect("publish live delta");
        }
        assert_eq!(
            store.v2_lineage_pack_count(vehicle_id).expect("pack count"),
            3
        );

        let tiny_limit = ProtocolLimits {
            max_chunks: 4,
            ..ProtocolLimits::default()
        };
        compact_v2_lineage_if_needed_with_limits(&store, &cursor_key, vehicle_id, tiny_limit)
            .expect("compact before simulated fourth pack");
        let compacted = store
            .lineage_manifest_for_vehicle(vehicle_id)
            .expect("compacted lineage")
            .expect("published compacted lineage");
        compacted.validate().expect("compacted lineage validates");
        assert_eq!(compacted.base, base);
        assert_eq!(compacted.deltas.len(), 1);
        assert_eq!(
            store.v2_lineage_pack_count(vehicle_id).expect("pack count"),
            2
        );

        store
            .upsert_car_settings(
                vehicle_id,
                9,
                &crate::hub_pack::ProjectionCarSettings {
                    enabled: false,
                    ..crate::hub_pack::ProjectionCarSettings::default()
                },
            )
            .expect("post-compaction mutation");
        let claim = store
            .claim_sync_mutations(vehicle_id, now + 3, 100)
            .expect("post-compaction claim")
            .expect("pending post-compaction mutation");
        publish_v2_delta(&store, &cursor_key, &claim).expect("publish after compaction");
        let final_lineage = store
            .lineage_manifest_for_vehicle(vehicle_id)
            .expect("final lineage")
            .expect("published final lineage");
        final_lineage.validate().expect("final lineage validates");
        assert_eq!(final_lineage.base, base);
        assert_eq!(final_lineage.deltas.len(), 2);
        assert_eq!(
            store.v2_lineage_pack_count(vehicle_id).expect("pack count"),
            3
        );
    }

    #[test]
    #[ignore = "requires TESLATLAS_REAL_CORPUS_ROOT pointing to a disposable clone"]
    fn real_imported_corpus_crosses_the_production_compaction_trigger() {
        let root = std::env::var_os("TESLATLAS_REAL_CORPUS_ROOT")
            .map(std::path::PathBuf::from)
            .expect("set TESLATLAS_REAL_CORPUS_ROOT to a disposable store clone");
        let store = HubStore::initialize(&root).expect("open and migrate corpus clone");
        let connection = store.open().expect("catalogue");
        let (vehicle_key, car_id): (String, i64) = connection
            .query_row(
                "SELECT vehicles.vehicle_id, car_settings.car_id
                   FROM vehicles
                   JOIN car_settings USING (vehicle_id)
                  ORDER BY vehicles.vehicle_id
                  LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("one imported vehicle and settings row");
        drop(connection);
        let vehicle_id = vehicle_key.parse::<Uuid>().expect("vehicle UUID");
        let original = store
            .lineage_manifest_for_vehicle(vehicle_id)
            .expect("lineage query")
            .expect("published lineage");
        original.validate().expect("original lineage validates");
        let original_base = original.base.clone();
        let initial_pack_count = store
            .v2_lineage_pack_count(vehicle_id)
            .expect("initial pack count");
        assert!(
            initial_pack_count >= 400,
            "this acceptance seam requires a production-scale imported corpus"
        );

        let cursor_key = CursorKey::from_bytes([29; 32]);
        let mut previous_pack_count = initial_pack_count;
        let mut compacted = false;
        for index in 0..128_i64 {
            store
                .upsert_car_settings(
                    vehicle_id,
                    car_id,
                    &crate::hub_pack::ProjectionCarSettings {
                        enabled: index % 2 == 0,
                        ..crate::hub_pack::ProjectionCarSettings::default()
                    },
                )
                .expect("settings mutation");
            let claim = store
                .claim_sync_mutations(vehicle_id, 2_100_000_000_000 + index, 100)
                .expect("claim mutation")
                .expect("pending mutation");
            publish_v2_delta(&store, &cursor_key, &claim).expect("publish production-scale delta");
            let current_pack_count = store
                .v2_lineage_pack_count(vehicle_id)
                .expect("current pack count");
            if current_pack_count < previous_pack_count {
                compacted = true;
                break;
            }
            previous_pack_count = current_pack_count;
        }
        assert!(compacted, "production compaction trigger was not crossed");

        let final_lineage = store
            .lineage_manifest_for_vehicle(vehicle_id)
            .expect("final lineage query")
            .expect("final lineage");
        final_lineage.validate().expect("final lineage validates");
        assert_eq!(final_lineage.base, original_base);
        assert!(
            store
                .v2_lineage_pack_count(vehicle_id)
                .expect("final pack count")
                < previous_pack_count
        );
        store.catalogue_check().expect("final corpus integrity");
    }

    #[test]
    fn outbox_uses_sparse_delta_after_immutable_base_and_preserves_base_pack() {
        let temp = crate::private_tempdir().expect("temporary store");
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
        materialise_lifecycle_for_collection(&store, &first, first_time).expect("first lifecycle");
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        publish_compatibility_snapshots(&store, &publication_gate, &cursor_key, &first, first_time)
            .expect("base publication");
        drop(publication_gate);
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
            .block_on(replay_export_outbox(
                &store,
                &cursor_key,
                std::slice::from_ref(&vehicle),
                first_time,
            ))
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
            .block_on(replay_export_outbox(
                &store,
                &cursor_key,
                std::slice::from_ref(&vehicle),
                second_time,
            ))
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
        let lineage = store
            .lineage_manifest_for_vehicle(vehicle_id)
            .expect("lineage manifest")
            .expect("published lineage");
        lineage.validate().expect("valid published lineage");
        assert_eq!(lineage.base.digest.to_string(), base_digest);
        assert_eq!(lineage.deltas.len(), 1);
        assert_eq!(lineage.deltas[0].pack.snapshot_id, lineage.base.snapshot_id);
        assert!(lineage.deltas[0].pack.ordinal > lineage.base.packs[0].ordinal);
        assert_eq!(
            store
                .manifest_for_vehicle(vehicle_id)
                .expect("legacy fallback manifest")
                .expect("legacy fallback available")
                .head_sequence,
            lineage.base.sequence
        );

        let delta = lineage.deltas[0].clone();
        let mutation_count =
            usize::try_from(delta.to_sequence - delta.from_sequence).expect("delta mutation count");
        let connection = store.open().expect("database");
        let mut statement = connection
            .prepare(
                "SELECT vehicle_id, revision, entity, entity_id, car_id,
                        operation, payload_json
                 FROM sync_mutations
                 WHERE vehicle_id = ?1
                 ORDER BY revision DESC LIMIT ?2",
            )
            .expect("mutation query");
        let mut mutations: Vec<SyncMutation> = statement
            .query_map(
                rusqlite::params![vehicle_id.to_string(), mutation_count as i64],
                |row| {
                    Ok(SyncMutation {
                        vehicle_id: row.get::<_, String>(0)?.parse().map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        revision: row.get(1)?,
                        entity: row.get(2)?,
                        entity_id: row.get(3)?,
                        car_id: row.get(4)?,
                        operation: row.get(5)?,
                        payload_json: row.get(6)?,
                    })
                },
            )
            .expect("mutation rows")
            .map(|row| row.expect("mutation row"))
            .collect();
        drop(statement);
        drop(connection);
        mutations.reverse();
        assert_eq!(mutations.len(), mutation_count);
        let replay_claim = SyncMutationClaim {
            vehicle_id,
            from_revision: mutations.first().expect("first mutation").revision,
            to_revision: mutations.last().expect("last mutation").revision,
            mutations,
        };
        store
            .commit_v2_delta_claim(&replay_claim, &delta, &cursor_key, &lineage.terminal_cursor)
            .expect("idempotent delta replay");
        let head_before_conflict = store.v2_head(vehicle_id).expect("head before conflict");
        let binding = store
            .v2_projection_binding(vehicle_id)
            .expect("immutable binding");
        let bad_hmac_cursor = OpaqueCursor::issue(
            &CursorKey::from_bytes([18; 32]),
            CursorClaims {
                protocol: PROTOCOL_V1,
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: binding.installation_id,
                account_id: binding.account_id,
                vehicle_id: binding.vehicle_id,
                generation: binding.generation,
                sequence: delta.to_sequence,
            },
        )
        .expect("bad-HMAC cursor shape");
        assert!(matches!(
            store.commit_v2_delta_claim(&replay_claim, &delta, &cursor_key, &bad_hmac_cursor),
            Err(StoreError::Manifest(_))
        ));
        let wrong_claim_cursor = OpaqueCursor::issue(
            &cursor_key,
            CursorClaims {
                protocol: PROTOCOL_V1,
                schema: HUB_PROJECTION_SCHEMA_V2,
                installation_id: binding.installation_id,
                account_id: binding.account_id,
                vehicle_id: binding.vehicle_id,
                generation: binding.generation,
                sequence: delta.to_sequence + 1,
            },
        )
        .expect("wrong-claim cursor shape");
        assert!(matches!(
            store.commit_v2_delta_claim(&replay_claim, &delta, &cursor_key, &wrong_claim_cursor,),
            Err(StoreError::LineageCatalogConflict)
        ));
        assert_eq!(
            store
                .v2_head(vehicle_id)
                .expect("head after rejected cursors"),
            head_before_conflict
        );
        let mut conflicting_delta = delta;
        conflicting_delta.chain_digest = Sha256Digest::of_bytes(b"conflicting-replay");
        assert!(matches!(
            store.commit_v2_delta_claim(
                &replay_claim,
                &conflicting_delta,
                &cursor_key,
                &lineage.terminal_cursor,
            ),
            Err(StoreError::LineageCatalogConflict)
        ));
        assert_eq!(
            store.v2_head(vehicle_id).expect("head after conflict"),
            head_before_conflict
        );
    }

    #[test]
    fn outbox_remains_scheduled_until_every_bounded_mutation_batch_is_published() {
        let temp = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temp.path()).expect("store");
        let cursor_key = CursorKey::from_bytes([23; 32]);
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "online");
        let now = 1_800_000_000_000_i64;
        let collection = ManualCollection {
            vehicles: vec![vehicle.clone()],
            snapshots: vec![VehicleData::for_test(
                9,
                json!({
                    "drive_state": {
                        "shift_state": "P", "speed": 0, "latitude": 47.0,
                        "longitude": 19.0, "timestamp": now
                    },
                    "vehicle_config": {"car_type": "model3"},
                    "vehicle_state": {"car_version": "2026.20"}
                }),
            )],
            failures: vec![],
        };
        persist_collection(&store, &collection, now).expect("raw observation");
        materialise_lifecycle_for_collection(&store, &collection, now).expect("lifecycle");
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        publish_compatibility_snapshots(&store, &publication_gate, &cursor_key, &collection, now)
            .expect("base publication");
        drop(publication_gate);
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
            .block_on(replay_export_outbox(
                &store,
                &cursor_key,
                std::slice::from_ref(&vehicle),
                now,
            ))
            .expect("clear base outbox");

        let vehicle_id = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("UUID");
        let binding = store
            .v2_projection_binding(vehicle_id)
            .expect("immutable binding");
        let settings_payload =
            serde_json::to_string(&store.load_car_settings(vehicle_id).expect("car settings"))
                .expect("settings payload");
        let connection = store.open().expect("database");
        let transaction = connection.unchecked_transaction().expect("transaction");
        let first_revision: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(revision), 0) + 1 FROM sync_mutations
                 WHERE vehicle_id = ?1",
                rusqlite::params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .expect("next revision");
        {
            let mut insert = transaction
                .prepare_cached(
                    "INSERT INTO sync_mutations(
                        vehicle_id, revision, entity, entity_id, car_id,
                        operation, payload_json, published, claimed_until_ms
                     ) VALUES (?1, ?2, 'car_setting', ?3, ?3, 'upsert', ?4, 0, 0)",
                )
                .expect("mutation insert");
            for offset in 0..10_001_i64 {
                insert
                    .execute(rusqlite::params![
                        vehicle_id.to_string(),
                        first_revision + offset,
                        binding.selected_car_id,
                        settings_payload,
                    ])
                    .expect("mutation row");
            }
        }
        transaction
            .execute(
                "INSERT INTO sync_mutation_sequences(vehicle_id, next_revision)
                 VALUES (?1, ?2)
                 ON CONFLICT(vehicle_id) DO UPDATE SET next_revision = excluded.next_revision",
                rusqlite::params![vehicle_id.to_string(), first_revision + 10_001],
            )
            .expect("mutation sequence");
        transaction
            .execute(
                "INSERT INTO export_outbox(
                    vehicle_id, dirty_revision, attempts, next_attempt_ms,
                    claimed_until_ms, last_error
                 ) VALUES (?1, 1, 0, 0, 0, NULL)",
                rusqlite::params![vehicle_id.to_string()],
            )
            .expect("outbox row");
        transaction.commit().expect("commit synthetic backlog");
        drop(connection);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime");
        runtime
            .block_on(replay_export_outbox(
                &store,
                &cursor_key,
                std::slice::from_ref(&vehicle),
                now + 1,
            ))
            .expect("first bounded batch");
        let connection = store.open().expect("database");
        let (unpublished, outbox): (i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM sync_mutations
                      WHERE vehicle_id = ?1 AND published = 0),
                    (SELECT COUNT(*) FROM export_outbox WHERE vehicle_id = ?1)",
                rusqlite::params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("mid-backlog state");
        assert_eq!((unpublished, outbox), (1, 1));
        drop(connection);

        runtime
            .block_on(replay_export_outbox(
                &store,
                &cursor_key,
                std::slice::from_ref(&vehicle),
                now + 2,
            ))
            .expect("final bounded batch");
        let connection = store.open().expect("database");
        let (unpublished, outbox): (i64, i64) = connection
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM sync_mutations
                      WHERE vehicle_id = ?1 AND published = 0),
                    (SELECT COUNT(*) FROM export_outbox WHERE vehicle_id = ?1)",
                rusqlite::params![vehicle_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("completed backlog state");
        assert_eq!((unpublished, outbox), (0, 0));
    }

    #[test]
    fn sparse_live_metadata_preserves_durable_car_and_new_pack_metadata_after_restart() {
        let temp = crate::private_tempdir().expect("temporary store");
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
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        publish_compatibility_snapshots(
            &store,
            &publication_gate,
            &CursorKey::from_bytes([11; 32]),
            &full,
            1_800_000_000_000,
        )
        .expect("publish full");
        drop(publication_gate);
        let vehicle_id = store
            .open()
            .expect("db")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle id")
            .parse::<Uuid>()
            .expect("uuid");
        let before = store
            .materialised_history(vehicle_id)
            .expect("history")
            .car
            .expect("car");

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
        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        publish_compatibility_snapshots(
            &store,
            &publication_gate,
            &CursorKey::from_bytes([11; 32]),
            &sparse,
            1_800_000_060_000,
        )
        .expect("publish sparse");
        let after = store
            .materialised_history(vehicle_id)
            .expect("history")
            .car
            .expect("car");
        assert_eq!(before, after);
        let manifest = store
            .manifest_for_vehicle(vehicle_id)
            .expect("manifest")
            .expect("published");
        let pack = store
            .pack_for_digest(manifest.chunks[0].sha256)
            .expect("pack")
            .expect("pack file");
        let bytes = zstd::stream::decode_all(std::fs::File::open(pack.path).expect("pack open"))
            .expect("decode");
        let inspect = temp.path().join("metadata.sqlite");
        std::fs::write(&inspect, bytes).expect("write inspect");
        let connection = rusqlite::Connection::open(inspect).expect("inspect");
        let packed: (
            String,
            String,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
        ) = connection
            .query_row(
                "SELECT name, model, vin, source_eid, exterior_color, firmware_version FROM cars",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .expect("packed car");
        assert_eq!(
            packed,
            (
                after.name,
                after.model,
                after.vin,
                after.source_eid,
                after.exterior_color,
                after.firmware_version
            )
        );
    }

    #[test]
    fn live_publication_includes_v2_state_and_update_history() {
        let temp = crate::private_tempdir().expect("temporary store");
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

        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        publish_compatibility_snapshots(
            &store,
            &publication_gate,
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
        let temp = crate::private_tempdir().expect("temporary store");
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

        let publication_gate = store
            .try_acquire_publication_gate()
            .expect("publication gate");
        publish_compatibility_snapshots(
            &store,
            &publication_gate,
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
            driving: Duration::from_secs(5),
            charging: Duration::from_secs(10),
            online: Duration::from_secs(75),
            sleeping: Duration::from_secs(30),
            offline_drive_timeout: Duration::from_secs(15 * 60),
            idle_suspend_after: Duration::from_secs(15 * 60),
            suspended: Duration::from_secs(21 * 60),
            updating: Duration::from_secs(15),
            stream_health_timeout: Duration::from_secs(30),
            maximum_backoff: Duration::from_secs(900),
        }
    }

    fn supervised_restart_test_cadence() -> CollectorCadence {
        CollectorCadence {
            // Keep a run quiescent after its initial proof transaction. This
            // makes the competing-lease assertion distinguish its work from
            // a legitimate next scheduled poll.
            driving: Duration::from_secs(1),
            charging: Duration::from_secs(1),
            online: Duration::from_secs(1),
            sleeping: Duration::from_secs(1),
            offline_drive_timeout: Duration::from_secs(1),
            idle_suspend_after: Duration::from_secs(1),
            suspended: Duration::from_secs(1),
            updating: Duration::from_secs(1),
            // The fake sends its finite eight-frame burst then waits for the
            // collector's orderly unsubscribe. Keep this above initial setup
            // so a silence reconnect cannot race that proof.
            stream_health_timeout: Duration::from_secs(10),
            maximum_backoff: Duration::from_secs(1),
        }
    }

    fn supervised_restart_test_config(data_dir: &std::path::Path) -> HubConfig {
        HubConfig {
            data_dir: data_dir.to_path_buf(),
            bind: "127.0.0.1:39191".parse().expect("loopback bind"),
            tls: None,
            collector: crate::config::CollectorConfig::default(),
            geocoder: crate::config::GeocoderConfig {
                enabled: false,
                ..crate::config::GeocoderConfig::default()
            },
            teslamate: crate::config::TeslaMateConfig::default(),
            terrain: TerrainConfig {
                cache_dir: Some(data_dir.join("terrain-cache")),
                ..TerrainConfig::default()
            },
        }
    }

    fn seed_supervised_restart_import(store: &HubStore) {
        use crate::teslamate_import::{
            TeslaMateImportRequest, TeslaMateImportScope, publish_history,
        };
        use crate::teslamate_projection::{TeslaMateCar, TeslaMateHistory};

        let imported_at_ms = current_epoch_millis().expect("clock") - 60_000;
        let history = TeslaMateHistory {
            cars: vec![TeslaMateCar {
                id: 1,
                eid: crate::fake_tesla::FIXTURE_EID as i64,
                vid: Some(crate::fake_tesla::FIXTURE_VID as i64),
                vin: Some(crate::fake_tesla::FIXTURE_VIN.to_owned()),
                name: Some("Restart fixture".to_owned()),
                model: Some("3".to_owned()),
                trim_badging: Some("74d".to_owned()),
                marketing_name: None,
                exterior_color: None,
                wheel_type: None,
                spoiler_type: None,
                efficiency_wh_per_km: None,
                settings: crate::hub_pack::ProjectionCarSettings {
                    enabled: true,
                    use_streaming_api: true,
                    ..Default::default()
                },
            }],
            drives: Vec::new(),
            positions: Vec::new(),
            charging_processes: Vec::new(),
            charges: Vec::new(),
            addresses: Vec::new(),
            geofences: Vec::new(),
            states: Vec::new(),
            updates: Vec::new(),
        };
        publish_history(
            store,
            &CursorKey::from_bytes([0xD1; 32]),
            &TeslaMateImportRequest {
                source_key: "supervised-restart-fixture".to_owned(),
                scope: TeslaMateImportScope::Selected(1),
                imported_at_ms,
            },
            &history,
        )
        .expect("seed selected imported car");
    }

    async fn join_supervised_restart_task(
        label: &str,
        task: &mut JoinHandle<Result<(), CollectorError>>,
    ) -> Result<(), CollectorError> {
        match timeout(Duration::from_secs(5), &mut *task).await {
            Ok(result) => result.expect("supervised collector task join"),
            Err(_) => {
                task.abort();
                let _ = task.await;
                panic!("{label} timeout");
            }
        }
    }

    async fn wait_for_supervised_signal_or_abort<T>(
        label: &str,
        task: &mut JoinHandle<Result<(), CollectorError>>,
        signal: oneshot::Receiver<T>,
    ) -> T {
        match timeout(Duration::from_secs(5), signal).await {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => {
                task.abort();
                let _ = task.await;
                panic!("{label} dropped");
            }
            Err(_) => {
                task.abort();
                let _ = task.await;
                panic!("{label} timeout");
            }
        }
    }

    async fn wait_for_supervised_restart_condition(
        label: &str,
        mut condition: impl FnMut() -> bool,
    ) {
        timeout(Duration::from_secs(5), async {
            while !condition() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{label} timeout"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn imported_legacy_pair_refreshes_then_collects_one_car_and_survives_reopen() {
        use crate::{
            credentials::{LegacyAuthManager, OwnerTokens},
            fake_tesla::{AdvanceMode, FAKE_REFRESHED_ACCESS_TOKEN, FakeTeslaSource},
            owner_api::OwnerApi,
        };

        crate::crypto::install_default_provider();
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        seed_supervised_restart_import(&store);

        crate::teslamate_credentials::replace_key(temporary.path(), b"chain-cloak-key")
            .expect("0600 Cloak key");
        let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
        let initial = OwnerTokens::from_secret_parts(
            "initial-access".to_owned(),
            "initial-refresh".to_owned(),
        )
        .expect("initial pair");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
                .expect("Cloak initial pair");
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, 2, 1)
                    .expect("due refresh schedule"),
            )
            .expect("store encrypted pair");

        let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
            .await
            .expect("loopback Tesla");
        fake.set_step(crate::fake_tesla::ScenarioStep::UnchangedNoOp);
        fake.set_base_ts_ms(current_epoch_millis().expect("clock") - 900_000);
        let manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
            store.clone(),
            temporary.path(),
            fake.oauth_issuer_url(),
        )
        .expect("load encrypted legacy pair");
        let region = manager.region();
        let auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region,
        };
        let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
            .expect("owner client");
        let config = supervised_restart_test_config(temporary.path());
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        let (finished, resume) = seam.arm_paused_collection_completion().await;
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task_store = store.clone();
        let task_config = config.clone();
        let stream_endpoint = fake.stream_endpoint().to_owned();
        let mut task = tokio::spawn(async move {
            SUPERVISED_COLLECTOR_TEST_SEAM
                .scope(seam, async move {
                    run_supervised_with_access(
                        &task_store,
                        &task_config,
                        supervised_restart_test_cadence(),
                        client,
                        auth,
                        stream_endpoint,
                        CursorKey::from_bytes([0xC1; 32]),
                        Some(ready_tx),
                        None,
                        async move {
                            let _ = shutdown_rx.await;
                        },
                    )
                    .await
                })
                .await
        });
        let _cursor =
            wait_for_supervised_signal_or_abort("collector readiness", &mut task, ready_rx).await;
        let _report =
            wait_for_supervised_signal_or_abort("first collection", &mut task, finished).await;
        assert_eq!(
            fake.token_refresh_request_count(),
            1,
            "one startup OAuth refresh"
        );
        assert!(fake.audited_requests().iter().any(|request| {
            request.path
                == format!(
                    "/api/1/vehicles/{}/vehicle_data",
                    crate::fake_tesla::FIXTURE_EID
                )
        }));

        resume.send(()).expect("resume stream drain");
        wait_for_supervised_restart_condition("numeric stream persistence", || {
            store
                .open()
                .expect("catalogue")
                .query_row(
                    "SELECT COUNT(*) FROM current_observations
                     WHERE record_type = 'tesla_stream_update_v1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("stream count")
                > 0
        })
        .await;
        shutdown_tx.send(()).expect("collector shutdown");
        join_supervised_restart_task("collector join", &mut task)
            .await
            .expect("collector result");

        let stored = store
            .load_teslamate_legacy_tokens()
            .expect("stored pair")
            .expect("stored pair exists");
        let successor = crate::teslamate_token::decrypt_legacy_owner_tokens(
            key.as_bytes(),
            stored.access(),
            stored.refresh(),
        )
        .expect("decrypt Cloak successor");
        assert_eq!(successor.access_token(), FAKE_REFRESHED_ACCESS_TOKEN);
        assert!(stored.next_refresh_at() > 0 && stored.next_refresh_at() < stored.expires_at());

        drop(store);
        let reopened_store = HubStore::initialize(temporary.path()).expect("reopen Hub");
        let vehicle_count: i64 = reopened_store
            .open()
            .expect("reopened catalogue")
            .query_row("SELECT COUNT(*) FROM vehicles", [], |row| row.get(0))
            .expect("one selected vehicle");
        assert_eq!(vehicle_count, 1);
        let reopened = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
            reopened_store,
            temporary.path(),
            fake.oauth_issuer_url(),
        )
        .expect("reopen latest pair");
        assert_eq!(reopened.access_token(), FAKE_REFRESHED_ACCESS_TOKEN);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn observer_collects_and_reconnects_without_refreshing() {
        use crate::{
            credentials::{LegacyAuthManager, OwnerTokens},
            fake_tesla::{AdvanceMode, FakeTeslaSource},
            owner_api::OwnerApi,
        };

        crate::crypto::install_default_provider();
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(temporary.path()).expect("Hub store");
        let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
            .await
            .expect("loopback Tesla");
        let setup_auth = LegacyAuth::for_test(
            fake.oauth_issuer_url(),
            "observer-access",
            "observer-refresh",
        );
        let setup_client =
            OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
                .expect("setup Owner client");
        setup_native_vehicle_with_client(
            &store,
            temporary.path(),
            &setup_client,
            &setup_auth,
            None,
        )
        .await
        .expect("native setup");
        crate::teslamate_credentials::replace_key(temporary.path(), b"observer-cloak-key")
            .expect("0600 Cloak key");
        let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
        let initial = OwnerTokens::from_secret_parts(
            "observer-access".to_owned(),
            "observer-refresh".to_owned(),
        )
        .expect("initial pair");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
                .expect("Cloak initial pair");
        store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, 2, 1)
                    .expect("due refresh schedule"),
            )
            .expect("store encrypted pair");

        fake.set_step(crate::fake_tesla::ScenarioStep::UnchangedNoOp);
        fake.set_base_ts_ms(current_epoch_millis().expect("clock") - 900_000);
        let manager = LegacyAuthManager::from_hub_teslamate_store_observer_with_issuer(
            store.clone(),
            temporary.path(),
            fake.oauth_issuer_url(),
        )
        .expect("load observer pair");
        let auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: false,
            region: StreamRegion::Global,
        };
        let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
            .expect("Owner client");
        let config = supervised_restart_test_config(temporary.path());
        let seam = Arc::new(SupervisedCollectorTestSeam::default());
        let (finished, resume) = seam.arm_paused_collection_completion().await;
        let (ready_tx, ready_rx) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let task_store = store.clone();
        let task_config = config.clone();
        let stream_endpoint = fake.stream_endpoint().to_owned();
        let mut task = tokio::spawn(async move {
            SUPERVISED_COLLECTOR_TEST_SEAM
                .scope(seam, async move {
                    run_supervised_with_access(
                        &task_store,
                        &task_config,
                        supervised_restart_test_cadence(),
                        client,
                        auth,
                        stream_endpoint,
                        CursorKey::from_bytes([0xC2; 32]),
                        Some(ready_tx),
                        None,
                        async move {
                            let _ = shutdown_rx.await;
                        },
                    )
                    .await
                })
                .await
        });
        let _cursor =
            wait_for_supervised_signal_or_abort("observer readiness", &mut task, ready_rx).await;
        let _report =
            wait_for_supervised_signal_or_abort("observer first collection", &mut task, finished)
                .await;
        assert_eq!(fake.token_refresh_request_count(), 0);
        assert!(fake.audited_requests().iter().any(|request| {
            request.path
                == format!(
                    "/api/1/vehicles/{}/vehicle_data",
                    crate::fake_tesla::FIXTURE_EID
                )
        }));

        resume.send(()).expect("resume observer stream drain");
        wait_for_supervised_restart_condition("observer stream persistence", || {
            store
                .open()
                .expect("catalogue")
                .query_row(
                    "SELECT COUNT(*) FROM current_observations
                     WHERE record_type = 'tesla_stream_update_v1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("stream count")
                > 0
        })
        .await;
        fake.set_stream_available(false);
        wait_for_supervised_restart_condition("observer stream reconnect attempt", || {
            fake.stream_session_stats().connection_attempts >= 2
        })
        .await;
        fake.set_stream_available(true);
        assert_eq!(fake.token_refresh_request_count(), 0);
        shutdown_tx.send(()).expect("observer shutdown");
        join_supervised_restart_task("observer collector join", &mut task)
            .await
            .expect("observer collector result");
        assert_eq!(fake.token_refresh_request_count(), 0);
    }

    #[tokio::test]
    async fn observer_stream_rejection_does_not_enqueue_refresh() {
        use crate::fake_tesla::{AdvanceMode, FakeTeslaSource};

        let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
            .await
            .expect("loopback Tesla");
        let auth = crate::legacy_auth::LegacyAuth::for_test(
            fake.oauth_issuer_url(),
            "observer-access",
            "observer-refresh",
        )
        .with_test_schedule(2_000_000_000, 1_900_000_000);
        let collection_auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
                auth,
                Arc::new(|_, _| Ok(())),
            ))),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: false,
            region: StreamRegion::Global,
        };
        let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
            .expect("Owner client");
        assert!(matches!(
            refresh_after_stream_authentication_rejection(
                &client,
                &collection_auth,
                StreamAuthenticationTransition::Rejected,
            )
            .await,
            Err(CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(
                OwnerApiError::HttpStatus(401)
            )))
        ));
        sleep(Duration::from_millis(25)).await;
        assert_eq!(fake.token_refresh_request_count(), 0);
        shutdown_legacy_refresh(&collection_auth).await;
    }

    #[tokio::test]
    async fn managed_stream_rejection_enqueues_legacy_refresh() {
        use crate::fake_tesla::{AdvanceMode, FakeTeslaSource};

        crate::crypto::install_default_provider();
        let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
            .await
            .expect("loopback Tesla");
        let auth = crate::legacy_auth::LegacyAuth::for_test(
            fake.oauth_issuer_url(),
            "stream-access",
            "stream-refresh",
        )
        .with_test_schedule(2_000_000_000, 1_900_000_000);
        let collection_auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
                auth,
                Arc::new(|_, _| Ok(())),
            ))),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region: StreamRegion::Global,
        };
        let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
            .expect("Owner client");

        refresh_after_stream_authentication_rejection(
            &client,
            &collection_auth,
            StreamAuthenticationTransition::Rejected,
        )
        .await
        .expect("managed stream rejection queues refresh");
        timeout(Duration::from_secs(1), async {
            while fake.token_refresh_request_count() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("one refresh request");
        wait_for_legacy_refresh_before_owner(&collection_auth)
            .await
            .expect("durably refreshed");
        shutdown_legacy_refresh(&collection_auth).await;
        fake.shutdown().await;
    }

    #[tokio::test]
    async fn persisted_refresh_failure_fences_later_collection() {
        use crate::fake_tesla::{AdvanceMode, FakeTeslaSource};

        crate::crypto::install_default_provider();
        let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
            .await
            .expect("loopback Tesla");
        let auth = crate::legacy_auth::LegacyAuth::for_test(
            fake.oauth_issuer_url(),
            "stream-access",
            "stream-refresh",
        )
        .with_test_schedule(2_000_000_000, 1_900_000_000);
        let collection_auth = CollectionAuth::Legacy {
            manager: Arc::new(tokio::sync::Mutex::new(LegacyAuthManager::for_test(
                auth,
                Arc::new(|_, _| Err(CredentialError::LegacyTokenStateWrite)),
            ))),
            fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
            refresh: Arc::new(LegacyRefreshCoordinator::default()),
            allow_refresh: true,
            region: StreamRegion::Global,
        };
        let client = OwnerApi::for_fake_http(fake.http_base_url().clone(), Duration::from_secs(2))
            .expect("Owner client");

        refresh_after_stream_authentication_rejection(
            &client,
            &collection_auth,
            StreamAuthenticationTransition::Rejected,
        )
        .await
        .expect("queue refresh");
        assert!(matches!(
            wait_for_legacy_refresh_before_owner(&collection_auth).await,
            Err(CollectorError::SensitiveAccessUnavailable)
        ));
        assert_eq!(fake.token_refresh_request_count(), 1);
        shutdown_legacy_refresh(&collection_auth).await;
        fake.shutdown().await;
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

        let speed_without_shift =
            VehicleData::for_test(1, json!({"drive_state":{"shift_state":null,"speed":25}}));
        let parked_with_speed =
            VehicleData::for_test(1, json!({"drive_state":{"shift_state":"P","speed":40}}));
        assert_eq!(poll_phase(&speed_without_shift), PollPhase::Online);
        assert_eq!(poll_phase(&parked_with_speed), PollPhase::Online);
        let reverse =
            VehicleData::for_test(1, json!({"drive_state":{"shift_state":"R","speed":0}}));
        assert_eq!(poll_phase(&reverse), PollPhase::Driving);
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
            scheduler.due_vehicles(now + Duration::from_secs(5)),
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
        assert!(scheduler.requires_live_stream_power_gate(online_id));
    }

    #[test]
    fn silent_pre_online_stream_falls_back_to_vehicle_data_at_deadline() {
        let now = Instant::now();
        let asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
        let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let id = online.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![asleep], now);
        scheduler.accept_discovery(vec![online], now + Duration::from_secs(30));

        scheduler.stream_unhealthy(id, now + Duration::from_secs(31));
        assert!(
            scheduler
                .due_vehicles(now + Duration::from_secs(59))
                .is_empty()
        );
        assert_eq!(
            scheduler.due_vehicles(now + Duration::from_secs(60)),
            vec![id]
        );
        assert!(
            !scheduler.requires_live_stream_power_gate(id),
            "the bounded silent-stream fallback must not demand absent power"
        );
    }

    #[test]
    fn nil_power_pre_online_stream_remains_gated_after_deadline() {
        let now = Instant::now();
        let asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
        let online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let id = online.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![asleep], now);
        scheduler.accept_discovery(vec![online], now + Duration::from_secs(30));

        scheduler.pre_online_power(id, None, now + Duration::from_secs(31));
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
        let temp = crate::private_tempdir().expect("temporary store");
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
            .block_on(persist_discovery_events(
                &store,
                &CursorKey::from_bytes([4; 32]),
                &[offline],
            ))
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
            .current_observations_for_vehicle(vehicle_id)
            .expect("current observations");
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
    fn stream_disabled_vehicle_polls_immediately_after_waking_then_uses_drive_cadence() {
        let now = Instant::now();
        let mut asleep = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "asleep");
        asleep.settings.use_streaming_api = false;
        let vehicle_id = asleep.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![asleep], now);
        assert!(scheduler.due_vehicles(now).is_empty());

        let woke_at = now + Duration::from_secs(30);
        let mut online = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        online.settings.use_streaming_api = false;
        scheduler.accept_discovery(vec![online], woke_at);
        assert_eq!(scheduler.due_vehicles(woke_at), vec![vehicle_id]);

        scheduler.vehicle_succeeded(vehicle_id, PollPhase::Driving, false, woke_at);
        assert_eq!(
            scheduler.vehicles[&vehicle_id].next_poll,
            woke_at + test_cadence().driving
        );
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
        scheduler.pre_online_power(vehicle_id, Some(1), now);
        scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, true, now);
        assert_eq!(
            scheduler.vehicles[&vehicle_id].next_poll,
            now + Duration::from_secs(75)
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
        assert!(scheduler.due_vehicles(fallback_at).is_empty());
        scheduler.pre_online_power(vehicle_id, Some(1), fallback_at + Duration::from_secs(1));
        assert!(
            scheduler
                .due_vehicles(fallback_at + Duration::from_secs(1))
                .contains(&vehicle_id)
        );
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
            recovered_at + Duration::from_secs(15)
        );
    }

    #[test]
    fn repeated_stream_telemetry_preserves_owner_api_retry_deadline() {
        let now = Instant::now();
        let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);
        scheduler.stream_healthy(vehicle_id, now);

        let failed_at = now + Duration::from_secs(1);
        scheduler.vehicle_failed_for_error(
            vehicle_id,
            &CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(OwnerApiError::HttpStatus(401))),
            failed_at,
        );
        let retry_at = scheduler.vehicles[&vehicle_id].next_poll;

        scheduler.stream_healthy(vehicle_id, failed_at + Duration::from_millis(100));
        assert_eq!(scheduler.vehicles[&vehicle_id].next_poll, retry_at);
        assert!(retry_at > failed_at);
    }

    #[test]
    fn negative_stream_power_schedules_one_charging_refresh() {
        let now = Instant::now();
        let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);
        scheduler.pre_online_power(vehicle_id, Some(0), now);
        scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, true, now);

        let ordinary_poll = scheduler.vehicles[&vehicle_id].next_poll;
        scheduler.schedule_stream_charging_poll(
            vehicle_id,
            Some("P"),
            Some(-3),
            now + Duration::from_millis(500),
        );
        assert_eq!(scheduler.vehicles[&vehicle_id].next_poll, ordinary_poll);

        let charging_at = now + Duration::from_secs(1);
        scheduler.schedule_stream_charging_poll(vehicle_id, None, Some(-3), charging_at);
        assert_eq!(scheduler.due_vehicles(charging_at), vec![vehicle_id]);
        assert_eq!(
            scheduler.vehicles[&vehicle_id].last_phase,
            PollPhase::Charging
        );

        scheduler.vehicle_failed(vehicle_id, charging_at);
        let retry_at = scheduler.vehicles[&vehicle_id].next_poll;
        scheduler.schedule_stream_charging_poll(
            vehicle_id,
            None,
            Some(-3),
            charging_at + Duration::from_millis(100),
        );
        assert_eq!(scheduler.vehicles[&vehicle_id].next_poll, retry_at);
        assert!(retry_at > charging_at);
    }

    #[test]
    fn suspended_parked_negative_power_schedules_charging_refresh() {
        let now = Instant::now();
        let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);
        scheduler.pre_online_power(vehicle_id, Some(0), now);
        scheduler.stream_healthy(vehicle_id, now);
        scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, true, now);
        let suspended_at = now + Duration::from_secs(3 * 60);
        scheduler.vehicle_succeeded(vehicle_id, PollPhase::Online, true, suspended_at);
        assert!(scheduler.vehicles[&vehicle_id].suspended);

        let charging_at = suspended_at + Duration::from_secs(1);
        scheduler.schedule_stream_charging_poll(vehicle_id, Some("P"), Some(-3), charging_at);
        assert_eq!(scheduler.due_vehicles(charging_at), vec![vehicle_id]);
        assert!(!scheduler.vehicles[&vehicle_id].suspended);
        assert_eq!(
            scheduler.vehicles[&vehicle_id].last_phase,
            PollPhase::Charging
        );
    }

    #[test]
    fn driving_cadence_uses_healthy_stream_interval() {
        let now = Instant::now();
        let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "driving");
        let vehicle_id = vehicle.id;
        let mut cadence = test_cadence();
        cadence.driving = Duration::from_millis(2_500);
        let mut scheduler = VehicleScheduler::new(cadence, now);
        scheduler.accept_discovery(vec![vehicle], now);

        scheduler.vehicle_succeeded(vehicle_id, PollPhase::Driving, false, now);
        assert_eq!(
            scheduler.vehicles[&vehicle_id].next_poll,
            now + Duration::from_millis(2_500)
        );

        scheduler.stream_healthy(vehicle_id, now + Duration::from_secs(1));
        let healthy_at = now + Duration::from_secs(2);
        scheduler.vehicle_succeeded(vehicle_id, PollPhase::Driving, false, healthy_at);
        assert_eq!(
            scheduler.vehicles[&vehicle_id].next_poll,
            healthy_at + Duration::from_secs(15)
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
        let mut stream_disabled = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
        stream_disabled.settings.use_streaming_api = false;
        stream_disabled.settings.suspend_after_idle_min = 2;
        stream_disabled.settings.suspend_min = 7;
        let mut streaming = Vehicle::for_test(3, "5YJ3E1EA7KF000003", "online");
        streaming.settings.use_streaming_api = true;
        streaming.settings.suspend_after_idle_min = 2;
        streaming.settings.suspend_min = 7;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(
            vec![disabled, stream_disabled.clone(), streaming.clone()],
            now,
        );

        // Stream-disabled cars poll normally; disabled cars do not.
        assert_eq!(scheduler.due_vehicles(now), vec![stream_disabled.id]);
        // Confirmed stream power opens the gate only for streaming-enabled cars.
        scheduler.pre_online_power(streaming.id, Some(1), now);
        assert_eq!(
            scheduler.due_vehicles(now),
            vec![stream_disabled.id, streaming.id]
        );
        scheduler.vehicle_succeeded(streaming.id, PollPhase::Online, true, now);
        assert!(
            !scheduler
                .due_vehicles(now + Duration::from_secs(60))
                .contains(&VehicleId::from_test(1))
        );
        assert!(
            scheduler
                .due_vehicles(now + Duration::from_secs(60))
                .contains(&stream_disabled.id),
            "stream-disabled cars remain normally pollable"
        );
        let transition = scheduler
            .vehicle_succeeded(
                streaming.id,
                PollPhase::Online,
                true,
                now + Duration::from_secs(2 * 60),
            )
            .expect("streaming car suspends");
        assert_eq!(transition.state, "suspended");
        assert_eq!(
            scheduler.vehicles[&streaming.id].next_poll,
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
    fn live_control_settings_pause_streams_and_resume_discovery() {
        let now = Instant::now();
        let vehicle = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);
        let paused = crate::hub_pack::ProjectionCarSettings {
            enabled: false,
            ..crate::hub_pack::ProjectionCarSettings::default()
        };
        let paused_targets = vec![(uuid::Uuid::nil(), 1, paused)];

        assert_eq!(
            scheduler.apply_control_settings(&paused_targets, now + Duration::from_secs(1)),
            vec![vehicle_id]
        );
        assert!(
            scheduler
                .due_vehicles(now + Duration::from_secs(1))
                .is_empty()
        );
        assert!(!scheduler.should_start_stream(vehicle_id));

        let resumed = crate::hub_pack::ProjectionCarSettings::default();
        let resumed_targets = vec![(uuid::Uuid::nil(), 1, resumed)];
        let resumed_at = now + Duration::from_secs(2);
        assert!(
            scheduler
                .apply_control_settings(&resumed_targets, resumed_at)
                .is_empty()
        );
        assert!(scheduler.discovery_due(resumed_at));
        assert!(scheduler.vehicles[&vehicle_id].settings.enabled);
    }

    #[test]
    fn discovery_keeps_all_configured_vehicles_and_their_settings() {
        let first = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let second = Vehicle::for_test(2, "5YJ3E1EA7KF000002", "online");
        let first_settings = crate::hub_pack::ProjectionCarSettings {
            enabled: true,
            use_streaming_api: false,
            suspend_min: 11,
            ..crate::hub_pack::ProjectionCarSettings::default()
        };
        let second_settings = crate::hub_pack::ProjectionCarSettings {
            enabled: false,
            suspend_min: 22,
            ..crate::hub_pack::ProjectionCarSettings::default()
        };
        let ignored = Vehicle::for_test(3, "5YJ3E1EA7KF000003", "online");

        let selected = filter_configured_vehicles(
            vec![first, second, ignored],
            &[
                (uuid::Uuid::nil(), 1, first_settings.clone()),
                (uuid::Uuid::nil(), 2, second_settings.clone()),
            ],
        );
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].id.get(), 1);
        assert_eq!(selected[0].settings, first_settings);
        assert_eq!(selected[1].id.get(), 2);
        assert_eq!(selected[1].settings, second_settings);
    }

    #[test]
    fn missing_configured_vehicle_waits_for_normal_discovery_cadence() {
        let now = Instant::now();
        let first = Vehicle::for_test(1, "5YJ3E1EA7KF000001", "online");
        let settings = crate::hub_pack::ProjectionCarSettings::default();
        let configured = vec![
            (uuid::Uuid::new_v4(), 1, settings.clone()),
            (uuid::Uuid::new_v4(), 2, settings),
        ];
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![first], now);

        let checked_at = now + Duration::from_secs(1);
        assert!(
            scheduler
                .apply_control_settings(&configured, checked_at)
                .is_empty()
        );
        assert!(!scheduler.discovery_due(checked_at));
    }

    #[test]
    fn stream_watermark_rejects_duplicate_and_old_frames_after_restart() {
        let temp = crate::private_tempdir().expect("temporary store");
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
        assert!(
            store
                .observations_for_vehicle(registered, crate::db::ObservationQuery::from_start(10),)
                .expect("pruned stream observations")
                .is_empty()
        );
        let observations = store
            .current_observations_for_vehicle(registered)
            .expect("current stream observation");
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].observed_at_ms, first_timestamp + 1_000);
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
    fn asleep_stream_frame_without_power_only_updates_pre_online_state() {
        let temporary = crate::private_tempdir().expect("temporary store");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let now = Instant::now();
        let vehicle = Vehicle::for_test(9, "5YJ3E1EA7KF000001", "asleep");
        let vehicle_id = vehicle.id;
        let mut scheduler = VehicleScheduler::new(test_cadence(), now);
        scheduler.accept_discovery(vec![vehicle], now);
        let update = |power| crate::tesla_stream::StreamUpdate {
            tag: vehicle_id.to_string(),
            timestamp_ms: current_epoch_millis().expect("clock") - 1_000,
            speed: Some(20),
            odometer: Some(100.0),
            soc: Some(80),
            elevation: Some(25),
            est_heading: Some(180),
            est_lat: Some(51.5),
            est_lng: Some(-0.1),
            power,
            shift_state: Some("D".to_owned()),
            range: Some(200),
            est_range: Some(210),
            heading: Some(180),
        };

        assert!(
            !process_stream_telemetry(&store, &mut scheduler, vehicle_id, &update(None))
                .expect("powerless asleep frame")
        );
        assert!(matches!(
            scheduler.vehicles[&vehicle_id].pre_online,
            PreOnlineCheck::ConfirmedFake { .. }
        ));
        let raw_count: i64 = store
            .open()
            .expect("database")
            .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
                row.get(0)
            })
            .expect("raw count");
        assert_eq!(
            raw_count, 0,
            "powerless asleep frame must not create lifecycle input"
        );

        assert!(
            process_stream_telemetry(&store, &mut scheduler, vehicle_id, &update(Some(12)))
                .expect("powered asleep frame")
        );
        assert!(matches!(
            scheduler.vehicles[&vehicle_id].pre_online,
            PreOnlineCheck::ConfirmedReal
        ));
        assert!(scheduler.vehicles[&vehicle_id].next_poll <= Instant::now());
        let raw_count: i64 = store
            .open()
            .expect("database")
            .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
                row.get(0)
            })
            .expect("raw count");
        assert_eq!(raw_count, 0);
        assert!(
            !process_stream_telemetry(
                &store,
                &mut scheduler,
                VehicleId::from_test(99),
                &update(Some(12)),
            )
            .expect("removed vehicle stream frame")
        );
        assert_eq!(
            store
                .current_observations_for_vehicle(
                    store
                        .open()
                        .expect("database")
                        .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                            row.get::<_, String>(0)
                        })
                        .expect("vehicle")
                        .parse()
                        .expect("vehicle UUID"),
                )
                .expect("current powered stream observation")
                .len(),
            1
        );
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
            let temp = crate::private_tempdir().expect("temporary store");
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
            for table in [
                "raw_observations",
                "current_observations",
                "stream_watermarks",
                "vehicle_lifecycle_state",
            ] {
                let count: i64 = connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .expect("fault count");
                assert_eq!(count, 0, "fault point {point:?} left {table}");
            }
            drop(connection);

            assert!(persist_stream_update(&store, vehicle_id, &update).expect("retry"));
            assert!(!persist_stream_update(&store, vehicle_id, &update).expect("duplicate"));
            let connection = store.open().expect("database");
            let raw: i64 = connection
                .query_row("SELECT COUNT(*) FROM raw_observations", [], |row| {
                    row.get(0)
                })
                .expect("raw count");
            assert_eq!(raw, 0);
            for table in [
                "current_observations",
                "stream_watermarks",
                "vehicle_lifecycle_state",
            ] {
                let count: i64 = connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .expect("committed count");
                assert_eq!(count, 1, "missing committed {table}");
            }
        }
    }

    #[test]
    fn concurrent_same_timestamp_has_one_committed_winner_and_restart_is_idempotent() {
        let temp = crate::private_tempdir().expect("temporary store");
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
        let right =
            std::thread::spawn(move || persist_stream_update(&second, vehicle_id, &right_update));
        let results = [
            left.join().expect("left").expect("left result"),
            right.join().expect("right").expect("right result"),
        ];
        assert_eq!(results.iter().filter(|value| **value).count(), 1);
        assert_eq!(results.iter().filter(|value| !**value).count(), 1);
        let registered = store
            .open()
            .expect("database")
            .query_row("SELECT vehicle_id FROM vehicles", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("vehicle")
            .parse::<Uuid>()
            .expect("uuid");
        assert!(
            store
                .observations_for_vehicle(registered, crate::db::ObservationQuery::from_start(10),)
                .expect("pruned observations")
                .is_empty()
        );
        assert_eq!(
            store
                .current_observations_for_vehicle(registered)
                .expect("current observation")
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
            assert_eq!(
                scheduler.vehicles[&id].next_poll,
                now + expected,
                "state={state}"
            );
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
        assert_eq!(
            scheduler.vehicles[&first_id].next_poll,
            now + Duration::from_secs(17)
        );

        scheduler.vehicle_failed_for_error(
            second_id,
            &CollectorError::OwnerApiAuth(OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
                crate::legacy_auth::LegacyAuthError::Transport,
            ))),
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
        assert_eq!(
            scheduler.vehicles[&first_id].next_poll,
            now + test_cadence().online
        );

        scheduler.vehicle_failed_for_error(
            second_id,
            &CollectorError::OwnerApi(OwnerApiError::VehicleInService),
            now,
        );
        assert_eq!(
            scheduler.vehicles[&second_id].next_poll,
            now + test_cadence().online
        );

        scheduler.vehicle_failed_for_error(
            first_id,
            &CollectorError::FleetApi(FleetApiError::RateLimited {
                retry_after_seconds: 23,
            }),
            now,
        );
        assert_eq!(
            scheduler.vehicles[&first_id].next_poll,
            now + Duration::from_secs(23)
        );
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
        assert!(
            scheduler
                .due_vehicles(now + Duration::from_secs(16))
                .is_empty()
        );
        assert_eq!(
            scheduler.due_vehicles(now + Duration::from_secs(17)),
            vec![id]
        );

        for offset in 0..8 {
            scheduler.vehicle_failed_for_error(
                id,
                &CollectorError::OwnerApi(OwnerApiError::VehicleNotFound),
                now + Duration::from_secs(offset),
            );
        }
        assert!(
            scheduler
                .due_vehicles(now + Duration::from_secs(9 * 60))
                .is_empty()
        );
        assert!(scheduler.vehicle_fuse_healthy(id, now + Duration::from_secs(11 * 60)));
    }

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
}

#[derive(Default)]
struct TerrainFuse {
    failures: Vec<Instant>,
    blown_until: Option<Instant>,
}
