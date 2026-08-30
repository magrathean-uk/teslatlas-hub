// SPDX-License-Identifier: AGPL-3.0-only

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
const STREAM_SOURCE_GAP_WARN_MS: i64 = 5_000;
const STREAM_QUEUE_LAG_WARN_MS: i64 = 5_000;
const STREAM_QUEUE_LAG_RECOVERED_MS: i64 = 1_000;
// Raw stream observations commit immediately. Derived client sync packs can
// coalesce briefly so a high-rate stream does not create one immutable pack
// per frame and repeatedly compact a nearly-full imported lineage.
const STREAM_EXPORT_REPLAY_INTERVAL: Duration = Duration::from_secs(60);
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
