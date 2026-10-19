// SPDX-License-Identifier: AGPL-3.0-only

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
    cursor_key: &CursorKey,
    vehicles: &[Vehicle],
    now_ms: i64,
    runtime_admission: Option<&Arc<crate::hub_user_process::AdmittedUserHub>>,
) -> Result<bool, CollectorError> {
    run_address_enrichment_once_guarded(
        store,
        config,
        cursor_key,
        vehicles,
        now_ms,
        runtime_admission,
        None,
    )
    .await
}

/// One enrichment step of a supervised run (B2 fix round, P3b fix 2, R-P3bF2-1): the
/// job's claim, the geocoder's cache write and the job's completion or retry are
/// separate store mutations, each preceded by the run's write guard, so a loss the
/// run observed during the lookup leaves the claimed job and the cache untouched.
async fn run_address_enrichment_once_guarded(
    store: &HubStore,
    config: &HubConfig,
    _cursor_key: &CursorKey,
    _vehicles: &[Vehicle],
    now_ms: i64,
    runtime_admission: Option<&Arc<crate::hub_user_process::AdmittedUserHub>>,
    write_guard: Option<&RunWriteGuard>,
) -> Result<bool, CollectorError> {
    #[cfg(test)]
    supervised_test_enrichment_gate().await;
    if !config.geocoder.enabled {
        return Ok(false);
    }
    check_optional_write(write_guard, "enrichment-claim")?;
    let Some(job) = store.claim_address_enrichment_job(now_ms)? else {
        return Ok(false);
    };
    let result = match Geocoder::new(&config.geocoder) {
        Ok(geocoder) => {
            let point = Wgs84Point::new(job.latitude, job.longitude)
                .map_err(|_| GeocoderError::MalformedResponse)?;
            if let Some(write_guard) = write_guard {
                // The geocoder consults the guard before it sends and before it
                // caches the answer.
                geocoder
                    .reverse_cached_with_egress_guard(store, point, now_ms, write_guard)
                    .await
            } else if let Some(admission) = runtime_admission {
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
            check_optional_write(write_guard, "enrichment-complete")?;
            store.complete_address_enrichment(&job, None, now_ms)?;
            return Ok(false);
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(address) => {
            check_optional_write(write_guard, "enrichment-complete")?;
            store.complete_address_enrichment(&job, Some(&address.display_name), now_ms)?;
            Ok(true)
        }
        Err(
            GeocoderError::MalformedResponse | GeocoderError::NoResult | GeocoderError::Disabled,
        ) => {
            check_optional_write(write_guard, "enrichment-complete")?;
            store.complete_address_enrichment(&job, None, now_ms)?;
            Ok(false)
        }
        Err(GeocoderError::EgressDenied) => Err(CollectorError::SensitiveAccessUnavailable),
        Err(error) => {
            check_optional_write(write_guard, "enrichment-retry")?;
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
            // The third production send of a stop signal (B2 fix round, P2, fourth
            // pass): witnessed like the others, so a path that must not signal
            // cannot do it through a drop.
            #[cfg(test)]
            supervised_test_log(LoopEvent::StopSignalSent {
                vehicle: self.vehicle_id.get(),
            });
        }
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
    }
}

/// The event channel of one stream run (C3 R28; C4 R49, R56 g): its capacity is
/// `STREAM_EVENT_CHANNEL_CAPACITY`, which is `K_STREAM_EVENT_CAPACITY` (40) and
/// replaces the retained 256, and the loop drains it at least every
/// `K_STREAM_DRAIN_CAP`.
fn new_stream_event_channel() -> (mpsc::Sender<StreamEvent>, mpsc::Receiver<StreamEvent>) {
    mpsc::channel(STREAM_EVENT_CHANNEL_CAPACITY)
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
    write_guard: Option<&RunWriteGuard>,
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
    let (events, receiver) = new_stream_event_channel();
    let power_gate = Arc::new(StreamPowerGate::default());
    let (shutdown, stop) = oneshot::channel();
    // Under test a run may be started by the test's own factory, after the checks
    // above: the loop's drains, stops and restarts then act on it as on any run.
    #[cfg(test)]
    let stop = match supervised_test_stream_run(vehicle_id, &events, stop) {
        Ok(task) => {
            streams.push(VehicleStreamRuntime {
                vehicle_id,
                power_gate,
                sensitive_access_failure: Arc::new(AtomicBool::new(false)),
                events: receiver,
                _shutdown: Some(shutdown),
                task: Some(task),
            });
            return true;
        }
        Err(stop) => stop,
    };
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
            // The log never carries a Tesla id (C3 R9).
            tracing::warn!(error = %error, "vehicle stream unavailable");
            return false;
        }
    };
    let mut supervisor = supervisor
        .with_health_timeout(health_timeout)
        .with_power_gate(Arc::clone(&power_gate));
    // The run's audit rows are store mutations too (B2 fix round, P3b fix 2,
    // R-P3bF2-1): the stream's session and request receipts are written only while
    // the run's write guard allows.
    if let Some(guard) = write_guard {
        supervisor = supervisor.with_write_guard(guard.observer("stream-audit"));
    }
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

/// Stop one car's stream (shutdown signal, at most 5 s for the run to end, then an
/// abort) and remove its runtime. It returns only once the task has ended, so no
/// second run for the car starts before its previous run has. The events still
/// queued when the task ended are returned, never dropped: the caller stores the
/// frames among them (C3 R36, lossless storage).
async fn disconnect_vehicle_stream(
    streams: &mut Vec<VehicleStreamRuntime>,
    vehicle_id: VehicleId,
) -> Vec<StreamEvent> {
    for stream in streams
        .iter_mut()
        .filter(|stream| stream.vehicle_id == vehicle_id)
    {
        if let Some(shutdown) = stream._shutdown.take() {
            let _ = shutdown.send(());
            #[cfg(test)]
            supervised_test_log(LoopEvent::StopSignalSent {
                vehicle: stream.vehicle_id.get(),
            });
        }
    }
    let deadline = Instant::now() + STREAM_SHUTDOWN_TIMEOUT;
    join_or_abort_selected_at(streams, &[vehicle_id], deadline).await;
    // The task has ended, so its bounded queue is stable: take what it holds.
    let mut queued = Vec::new();
    for stream in streams
        .iter_mut()
        .filter(|stream| stream.vehicle_id == vehicle_id)
    {
        while let Ok(event) = stream.events.try_recv() {
            queued.push(event);
        }
    }
    streams.retain(|stream| stream.vehicle_id != vehicle_id);
    queued
}

/// Store the frames among events a stopped stream left queued (C3 R36: frames
/// already queued when a stream is stopped are stored too). The control events
/// among them (healthy, outage, offline, rejection) describe a run that is over
/// and change nothing.
///
/// Every frame is its own transaction, so a run's write guard (`write_fence`, set by the
/// final stop's budget) is looked at immediately before each one: the first frame that
/// finds it refuse is not written, nor is any later one, and the stop is the sensitive
/// failure (B2 fix round, P2, fourth pass; P3b fix 2: the guard is the fence and the
/// retained admission).
fn store_queued_stream_frames(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    vehicle_id: VehicleId,
    queued: Vec<StreamEvent>,
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
    write_fence: Option<&RunWriteGuard>,
) -> Result<(), CollectorError> {
    for event in queued {
        if let StreamEvent::Telemetry { update, queued_at } = event {
            if frame_writes_fenced(write_fence) {
                return Err(CollectorError::SensitiveAccessUnavailable);
            }
            let queue_lag = scheduler.clock.mono_now().saturating_duration_since(queued_at);
            process_stream_telemetry_inner(
                store,
                scheduler,
                vehicle_id,
                &update,
                queue_lag,
                projection_car_ids,
                false,
            )?;
        }
    }
    Ok(())
}

/// Stop one car's stream and store the frames it left queued (C3 R28, R36).
async fn stop_vehicle_stream(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut Vec<VehicleStreamRuntime>,
    vehicle_id: VehicleId,
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
) -> Result<(), CollectorError> {
    let queued = disconnect_vehicle_stream(streams, vehicle_id).await;
    store_queued_stream_frames(store, scheduler, vehicle_id, queued, projection_car_ids, None)
}

/// Poll the tasks of the `selected` cars' runs where they are, and be ready once none
/// is left. A task's handle stays in its runtime until the task has ended (it is
/// cleared only when the join returned), so whoever drops this wait, or the future that
/// holds it, leaves the handle to [`VehicleStreamRuntime`]'s own `Drop`, which aborts
/// the task: a stopped run never outlives the collector with the credential manager,
/// whatever cancels the stop (B2 fix round, P2).
fn poll_selected_tasks(
    streams: &mut [VehicleStreamRuntime],
    selected: &[VehicleId],
    context: &mut std::task::Context<'_>,
) -> std::task::Poll<()> {
    let mut pending = false;
    for stream in streams
        .iter_mut()
        .filter(|stream| selected.contains(&stream.vehicle_id))
    {
        let Some(task) = stream.task.as_mut() else {
            continue;
        };
        if std::pin::Pin::new(task).poll(context).is_ready() {
            // The run's own result is not read: a stop the collector asked for is
            // not an error of the run.
            stream.task = None;
        } else {
            pending = true;
        }
    }
    if pending {
        std::task::Poll::Pending
    } else {
        std::task::Poll::Ready(())
    }
}

/// Wait until every `selected` run's task has ended.
async fn join_selected_tasks(streams: &mut [VehicleStreamRuntime], selected: &[VehicleId]) {
    std::future::poll_fn(|context| poll_selected_tasks(streams, selected, context)).await;
}

/// Ask every `selected` run's task to abort (it has ended once its handle is joined).
fn abort_selected_tasks(streams: &[VehicleStreamRuntime], selected: &[VehicleId]) {
    for stream in streams
        .iter()
        .filter(|stream| selected.contains(&stream.vehicle_id))
    {
        if let Some(task) = stream.task.as_ref() {
            task.abort();
        }
    }
}

/// Wait for the `selected` runs' tasks to end by `deadline` and abort the ones that
/// have not, then join the abort: every task has ended when this returns, so no second
/// run of its car can overlap it. The handles stay owned by their runtimes meanwhile.
async fn join_or_abort_selected_at(
    streams: &mut [VehicleStreamRuntime],
    selected: &[VehicleId],
    deadline: Instant,
) {
    if tokio::time::timeout_at(deadline, join_selected_tasks(streams, selected))
        .await
        .is_err()
    {
        abort_selected_tasks(streams, selected);
        join_selected_tasks(streams, selected).await;
    }
}

/// Whether any `selected` run still holds a task that has not been joined.
fn selected_tasks_remain(streams: &[VehicleStreamRuntime], selected: &[VehicleId]) -> bool {
    streams
        .iter()
        .any(|stream| selected.contains(&stream.vehicle_id) && stream.task.is_some())
}

/// Stop the runs of `vehicle_ids` together and store the frames they left (C3 R28,
/// R36, B2 clarification 3). Every selected run is signalled before any is joined, and
/// their five-second deadlines run concurrently, so n stuck runs cost five seconds,
/// not 5n. While they wind down the loop keeps draining: the continuing streams
/// completely, and the selected ones for their telemetry only (their control events
/// describe a run that is ending and must not re-admit it). A run that is still
/// alive at its deadline is aborted and joined, and a runtime is removed only after
/// its task has ended, so no replacement of it starts early. What the drains found
/// (a rejection, an ended run, a terminal error) goes to `pending` for the loop's
/// next settlement.
async fn stop_vehicle_streams(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut Vec<VehicleStreamRuntime>,
    vehicle_ids: &[VehicleId],
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
    pending: &mut StreamDrainResult,
    budget: &mut DrainBudget,
) -> Result<(), CollectorError> {
    let selected: Vec<VehicleId> = {
        let mut selected = Vec::new();
        for id in vehicle_ids {
            if !selected.contains(id) && streams.iter().any(|stream| stream.vehicle_id == *id) {
                selected.push(*id);
            }
        }
        selected
    };
    if selected.is_empty() {
        return Ok(());
    }
    // Every signal goes out before any join.
    for stream in streams
        .iter_mut()
        .filter(|stream| selected.contains(&stream.vehicle_id))
    {
        if let Some(shutdown) = stream._shutdown.take() {
            let _ = shutdown.send(());
            #[cfg(test)]
            supervised_test_log(LoopEvent::StopSignalSent {
                vehicle: stream.vehicle_id.get(),
            });
        }
    }
    let deadline = Instant::now() + STREAM_SHUTDOWN_TIMEOUT;
    // The handles stay in their runtimes while the runs wind down, so a stop that is
    // dropped half way leaves them to the runtimes' `Drop` (abort) and not detached.
    let mut overdue_handled = false;
    while selected_tasks_remain(streams, &selected) {
        let drain_at = budget.deadline();
        enum Step {
            Drain,
            Joined,
            Overdue,
        }
        let step = {
            let joined = join_selected_tasks(streams, &selected);
            tokio::pin!(joined);
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(drain_at) => Step::Drain,
                () = &mut joined => Step::Joined,
                () = tokio::time::sleep_until(deadline), if !overdue_handled => Step::Overdue,
            }
        };
        match step {
            Step::Joined => {}
            Step::Overdue => {
                // Abort what is still alive; the loop then joins the aborts, still
                // draining at the shared deadline.
                overdue_handled = true;
                abort_selected_tasks(streams, &selected);
            }
            Step::Drain => {
                // The test seam may hold a stop's drain before it writes anything.
                supervised_test_gate("stop-drain").await;
                if budget.writes_fenced() {
                    // The run's admission was lost while the stop waited: no frame
                    // is written, the runs this call is stopping end here.
                    abort_selected_tasks(streams, &selected);
                    join_selected_tasks(streams, &selected).await;
                    return Err(CollectorError::SensitiveAccessUnavailable);
                }
                budget.mark();
                let drained = match drain_stream_events_while_stopping(
                    store,
                    scheduler,
                    streams,
                    projection_car_ids,
                    &selected,
                    budget.write_fence.as_ref(),
                )
                .await
                {
                    Ok(drained) => drained,
                    Err(error) => {
                        // The runs this call is stopping are aborted and joined: none
                        // may outlive it with the credential manager.
                        abort_selected_tasks(streams, &selected);
                        join_selected_tasks(streams, &selected).await;
                        return Err(error);
                    }
                };
                apply_gate_revocations(scheduler, streams);
                budget.note_backlog(drained.backlog);
                pending.absorb(drained);
            }
        }
    }
    // The tasks have ended, so their bounded queues are stable: store what they hold,
    // unless the run's admission was lost since (the queued frames are dropped with
    // their runtimes).
    if budget.writes_fenced() {
        streams.retain(|stream| !selected.contains(&stream.vehicle_id));
        return Err(CollectorError::SensitiveAccessUnavailable);
    }
    let mut result = Ok(());
    for id in &selected {
        let mut queued = Vec::new();
        for stream in streams.iter_mut().filter(|stream| stream.vehicle_id == *id) {
            while let Ok(event) = stream.events.try_recv() {
                queued.push(event);
            }
        }
        let stored = store_queued_stream_frames(
            store,
            scheduler,
            *id,
            queued,
            projection_car_ids,
            budget.write_fence.as_ref(),
        );
        if result.is_ok() {
            result = stored;
        }
    }
    streams.retain(|stream| !selected.contains(&stream.vehicle_id));
    result
}

/// Stop every stream (the orderly stop, or a credential stop) and store the frames
/// they left queued (C3 R28, R36, B2 clarification 3). It is the same stop as a
/// settings stop: every signal first, concurrent deadlines, and the loop keeps its
/// drain deadline while the runs wind down (`stop_vehicle_streams`).
async fn stop_all_streams_and_store(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut Vec<VehicleStreamRuntime>,
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
    pending: &mut StreamDrainResult,
    budget: &mut DrainBudget,
) -> Result<(), CollectorError> {
    let ids: Vec<VehicleId> = streams.iter().map(|stream| stream.vehicle_id).collect();
    let result = stop_vehicle_streams(
        store,
        scheduler,
        streams,
        &ids,
        projection_car_ids,
        pending,
        budget,
    )
    .await;
    // A runtime that held no task is gone with the rest.
    streams.clear();
    result
}

/// The streams of cars that are not among `configured` (the cars the scheduler
/// holds), which the caller stops. The caller reads the set before this awaits, so
/// no borrow of the scheduler is held across the stops.
fn streams_not_in_scheduler(
    streams: &[VehicleStreamRuntime],
    configured: &HashSet<VehicleId>,
) -> Vec<VehicleId> {
    streams
        .iter()
        .map(|stream| stream.vehicle_id)
        .filter(|vehicle_id| !configured.contains(vehicle_id))
        .collect()
}

/// Whether an error is Tesla saying the car is in service (C3 R42, R52): a 405
/// whose error text says so, on either wire. It is the `InService` class of the
/// one classifier, so the rule has one home.
fn is_vehicle_in_service(error: &CollectorError) -> bool {
    classify(
        error,
        RequestKind::Read,
        crate::config::CollectorMode::Legacy,
    ) == ErrorClass::InService
}

/// The Owner API error inside a collector error, if it holds one (C3 R42): the
/// bare error and the one a credential-fused request wraps.
fn owner_api_error(error: &CollectorError) -> Option<&OwnerApiError> {
    match error {
        CollectorError::OwnerApi(error)
        | CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(error)) => Some(error),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum StreamAuthenticationTransition {
    #[default]
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

#[derive(Default)]
struct StreamDrainResult {
    transition: StreamAuthenticationTransition,
    backlog: bool,
    terminal_error: Option<CollectorError>,
    /// The cars whose stream run ended with an error that is not terminal (C3
    /// R28, C4 R56 e): the scheduler has counted it and set the run spacing, and the
    /// loop removes the ended runtime so a later run can start.
    ended: Vec<VehicleId>,
}

impl StreamDrainResult {
    /// Folds the result of a later drain into this one: a rejection outranks an
    /// authentication, the ended runs add up, the first terminal error stays, and
    /// the backlog is the later drain's.
    fn absorb(&mut self, later: StreamDrainResult) {
        use StreamAuthenticationTransition::{Authenticated, NoChange, Rejected};
        self.transition = match (self.transition, later.transition) {
            (Rejected, _) | (_, Rejected) => Rejected,
            (Authenticated, _) | (_, Authenticated) => Authenticated,
            (NoChange, NoChange) => NoChange,
        };
        self.ended.extend(later.ended);
        if self.terminal_error.is_none() {
            self.terminal_error = later.terminal_error;
        }
        self.backlog = later.backlog;
    }
}

/// The stream queues, lent to a wait on Owner API or credential work so the events
/// keep being committed meanwhile (C3 R11; C4 R56 g). C4 waits 10 s for room in a
/// queue of 40 and then ends the run, so a REST or authentication wait that
/// lasted tens of seconds (a list, a refresh, a confirmation read) would lose
/// the stream's telemetry if nothing drained meanwhile. Each drain stores the
/// telemetry and notes the evidence exactly as the loop's own drain does; what
/// the loop must act on (an ended run, a rejection, a terminal error) is kept in
/// `pending` and settled at the loop's next drain.
struct StreamPump<'a> {
    store: &'a HubStore,
    streams: &'a mut [VehicleStreamRuntime],
    projection_car_ids: &'a mut HashMap<VehicleId, StreamContext>,
    pending: &'a mut StreamDrainResult,
    /// The instant of the previous drain, shared by every wait of the loop.
    budget: &'a mut DrainBudget,
}

/// The shared drain deadline (C3 R2, R11, C4 R56 g, B2 clarification 3). The bound
/// of `K_STREAM_DRAIN_CAP` is measured between consecutive drains, whatever the loop
/// waited for in between: a list, a read, a credential, enrichment, export replay, a
/// stream stop or the ordinary sleep. Every path records the instant just before it
/// drains, and the next drain is due at that instant plus the cap, so one wait
/// starting late in the interval cannot push the next drain a whole cap further out.
/// An expired deadline is serviced at once, before any ready future is handled.
///
/// It reads the async runtime's monotonic timer, which paused test time moves, and
/// not the scheduler's clock: a narrow exception, because only the runtime's timer
/// can wake the loop. Due times, persisted values and the R40 evidence ages still
/// read the scheduler's clock.
#[derive(Clone, Debug)]
struct DrainBudget {
    last: Instant,
    /// A backlog stayed in a queue: the next drain is due now.
    backlog: bool,
    /// The run's write guard, once the loop guards its frame writes with it (B2 fix
    /// round, P2, third pass; P3b fix 2: the fence and the retained admission): a
    /// drain that finds it refuse writes no frame.
    write_fence: Option<RunWriteGuard>,
    /// The instant of every drain, for the tests that prove the interval between
    /// two of them.
    #[cfg(test)]
    marks: Vec<Instant>,
}

/// Whether a drain's write `guard`, if it has one, refuses: the frame about to be
/// written must not be (every frame is a transaction of its own).
fn frame_writes_fenced(guard: Option<&RunWriteGuard>) -> bool {
    guard.is_some_and(|guard| guard.refused("stream-frame"))
}

impl DrainBudget {
    fn new(now: Instant) -> Self {
        Self {
            last: now,
            backlog: false,
            write_fence: None,
            #[cfg(test)]
            marks: Vec::new(),
        }
    }

    /// Guard the frame writes of every later drain with the run's write guard.
    fn guard_writes_with(&mut self, guard: impl Into<RunWriteGuard>) {
        self.write_fence = Some(guard.into());
    }

    /// Whether the write guard refuses: no frame may be written after it.
    fn writes_fenced(&self) -> bool {
        frame_writes_fenced(self.write_fence.as_ref())
    }

    /// A drain starts now: call it immediately before draining.
    fn mark(&mut self) {
        self.last = Instant::now();
        self.backlog = false;
        #[cfg(test)]
        {
            self.marks.push(self.last);
            supervised_test_log(LoopEvent::Drain(self.last));
        }
    }

    /// Record a drain as having begun at `at` (the tests place one in the past).
    #[cfg(test)]
    fn mark_at_for_test(&mut self, at: Instant) {
        self.last = at;
        self.backlog = false;
        self.marks.push(at);
    }

    /// What the drain that just ran left: a backlog needs the next drain at once.
    fn note_backlog(&mut self, backlog: bool) {
        self.backlog = backlog;
    }

    /// When the next drain is due.
    fn deadline(&self) -> Instant {
        if self.backlog {
            self.last
        } else {
            deadline_after(self.last, K_STREAM_DRAIN_CAP)
        }
    }

    /// How much of the interval is left at `now`; zero once the deadline has passed.
    fn remaining(&self, now: Instant) -> Duration {
        self.deadline().saturating_duration_since(now)
    }
}

/// How long the loop's ordinary sleep lasts (C3 R2, R11, B2 clarification 3): the
/// planned wait, ended by the shared drain deadline when streams are running, so a
/// tail of enrichment and replay cannot stretch the interval between two drains. A
/// shorter planned wait is kept, an expired deadline gives zero and no stream leaves
/// the wait unchanged.
fn drain_bounded_sleep(planned: Duration, remaining: Duration, has_streams: bool) -> Duration {
    if has_streams {
        planned.min(remaining)
    } else {
        planned
    }
}

/// What is left of the loop floor in an iteration that did nothing (C3 R11, B2
/// clarification 3): two such iterations start at least `K_LOOP_FLOOR` apart, whatever
/// drained or ended the sleep meanwhile. An iteration that did work has no floor.
fn idle_floor_remaining(started: Instant, now: Instant, idle: bool) -> Duration {
    if idle {
        K_LOOP_FLOOR.saturating_sub(now.saturating_duration_since(started))
    } else {
        Duration::ZERO
    }
}

impl StreamPump<'_> {
    /// Whether there is any queue to drain.
    fn armed(&self) -> bool {
        !self.streams.is_empty()
    }

    /// When the next drain is due (see [`DrainBudget`]).
    fn deadline(&self) -> Instant {
        self.budget.deadline()
    }

    /// Whether a backlog asks for the next drain at once.
    fn backlog(&self) -> bool {
        self.budget.backlog
    }

    /// One drain, with the gate revocations it asks for applied at once. The
    /// budget records the instant first, and what the drain left decides when the
    /// next one is due: at once while a backlog remains, else at the cap.
    async fn drain(&mut self, scheduler: &mut VehicleScheduler) -> Result<(), CollectorError> {
        self.budget.mark();
        let drained = drain_stream_events_with_cache(
            self.store,
            scheduler,
            self.streams,
            self.projection_car_ids,
        )
        .await?;
        apply_gate_revocations(scheduler, self.streams);
        self.budget.note_backlog(drained.backlog);
        self.pending.absorb(drained);
        Ok(())
    }

    /// Waits for `work` and drains the queues at the shared deadline meanwhile, so
    /// the interval between two drains is at most `K_STREAM_DRAIN_CAP` however long
    /// the work lasts and however early in the interval it began. An expired
    /// deadline is serviced before `work`, even when `work` is ready. `work` must not
    /// use the scheduler. A store fault of a drain ends the wait with that error and
    /// drops `work`; the credential unit finishes what it started in its own tasks
    /// (C5 R15a).
    async fn while_waiting<T>(
        &mut self,
        scheduler: &mut VehicleScheduler,
        work: impl Future<Output = T>,
    ) -> Result<T, CollectorError> {
        if !self.armed() {
            return Ok(work.await);
        }
        tokio::pin!(work);
        // The test seam logs the first time this work is left pending: a wait that
        // reaches its work before its drain has not serviced the deadline first.
        #[cfg(test)]
        let mut work = PendingLogged::new(work);
        loop {
            let drain_at = self.deadline();
            tokio::select! {
                biased;
                () = tokio::time::sleep_until(drain_at) => {}
                output = &mut work => return Ok(output),
            }
            self.drain(scheduler).await?;
            if self.backlog() {
                tokio::task::yield_now().await;
            }
        }
    }
}

/// The sleep that ends an iteration (C3 R2, R11, B2 clarification 3): the planned
/// wait, ended by the shared drain deadline while streams run, so what the tail of the
/// iteration took does not stretch the interval between two drains; and, after an
/// iteration that did nothing, the rest of the loop floor, waited with the queues
/// drained at their deadline. Earlier draining therefore never lets an empty
/// iteration start closer than `K_LOOP_FLOOR` to the previous one.
async fn end_iteration_sleep(
    mut pump: StreamPump<'_>,
    scheduler: &mut VehicleScheduler,
    planned: Duration,
    started: Instant,
    idle: bool,
) -> Result<(), CollectorError> {
    let began = loop_now();
    let sleep_for = drain_bounded_sleep(planned, pump.budget.remaining(began), pump.armed());
    if !sleep_for.is_zero() {
        cadence_sleep(sleep_for).await;
    }
    #[cfg(test)]
    {
        let ended = loop_now();
        supervised_test_iteration_slept(IterationSleep {
            planned,
            armed: pump.armed(),
            drain_mark: pump.budget.last,
            began,
            slept_for: sleep_for,
            ended,
            queued: pump.streams.iter().map(|stream| stream.events.len()).sum(),
        });
        supervised_test_log(LoopEvent::SleepEnded(ended));
    }
    let floor_left = idle_floor_remaining(started, loop_now(), idle);
    if !floor_left.is_zero() {
        pump.while_waiting(scheduler, cadence_sleep(floor_left)).await?;
    }
    Ok(())
}

/// The loop's own drain at the top of an iteration (C3 R11): it records the shared
/// instant like every other drain.
async fn drain_streams_now(
    budget: &mut DrainBudget,
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut [VehicleStreamRuntime],
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
) -> Result<StreamDrainResult, CollectorError> {
    budget.mark();
    let drained =
        drain_stream_events_with_cache(store, scheduler, streams, projection_car_ids).await?;
    budget.note_backlog(drained.backlog);
    Ok(drained)
}

/// Settles what the drains made while requests were pending (C3 R28): a run that
/// ended is removed, so a later run can start once the scheduler's spacing allows,
/// and a terminal error ends the collector. A rejection stays in `pending`: the
/// loop's next drain folds it in and sends it to the credential unit.
async fn settle_pending_stream_drain(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut Vec<VehicleStreamRuntime>,
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
    pending: &mut StreamDrainResult,
) -> Result<(), CollectorError> {
    for vehicle_id in std::mem::take(&mut pending.ended) {
        stop_vehicle_stream(store, scheduler, streams, vehicle_id, projection_car_ids).await?;
    }
    match pending.terminal_error.take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The decision-log event of a stream outage (C3 R9, R10, R28): one INFO event at
/// the first unhealthy event of an outage, with the car's Hub id (never a Tesla
/// id), the phase, the time to the car's next action and the facts of the report.
/// `source` names what ended the stream's health: an event, the end of a run, or
/// silence while Driving.
fn report_stream_outage(
    scheduler: &VehicleScheduler,
    vehicle_id: VehicleId,
    source: &'static str,
    outage: StreamOutage,
    now: Instant,
) {
    let StreamOutage::Active(outage) = outage else {
        return;
    };
    tracing::info!(
        target: "collector.decision",
        vehicle = %scheduler.hub_label(vehicle_id),
        from = outage.phase.code(),
        to = outage.phase.code(),
        reason = "stream_outage",
        next_in_ms = scheduler.next_action_in_ms(vehicle_id, now),
        gated = outage.live_power_gate,
        source,
        consecutive_failures = outage.consecutive_failures,
        outage_ms = u64::try_from(outage.outage_duration.as_millis()).unwrap_or(u64::MAX),
        owner_api_fallback_scheduled = outage.owner_api_fallback_scheduled,
        owner_api_fallback_after_ms = ?outage.owner_api_fallback_after
            .map(|delay| u64::try_from(delay.as_millis()).unwrap_or(u64::MAX)),
        suspended = outage.suspended,
        "collector transition"
    );
}

/// The decision-log event of a stream recovery (C3 R9, R10): one INFO event at the
/// first healthy event after an outage.
fn report_stream_recovery(
    scheduler: &VehicleScheduler,
    vehicle_id: VehicleId,
    recovery: StreamRecovery,
    now: Instant,
) {
    let StreamRecovery::Recovered(recovery) = recovery else {
        return;
    };
    let phase = scheduler
        .vehicles
        .get(&vehicle_id)
        .map_or(PollPhase::Unknown, |vehicle| vehicle.phase);
    tracing::info!(
        target: "collector.decision",
        vehicle = %scheduler.hub_label(vehicle_id),
        from = phase.code(),
        to = phase.code(),
        reason = "stream_recovered",
        next_in_ms = scheduler.next_action_in_ms(vehicle_id, now),
        gated = false,
        failures = recovery.failures,
        outage_ms = u64::try_from(recovery.outage_duration.as_millis()).unwrap_or(u64::MAX),
        "collector transition"
    );
}

/// The stream of a car is unhealthy (C3 R28, R36): its power gate is revoked and
/// the outage, if it is the first of its run, is reported once.
fn note_stream_unhealthy(
    scheduler: &mut VehicleScheduler,
    vehicle_id: VehicleId,
    source: &'static str,
    now: Instant,
) {
    scheduler.revoke_gate(vehicle_id);
    let outage = scheduler.stream_unhealthy(vehicle_id, now);
    report_stream_outage(scheduler, vehicle_id, source, outage, now);
}

/// One stream event (C3 R36), the scheduler's side of the stream:
/// - `Healthy`: the stream is healthy, and the first one after an outage reports
///   the recovery; it is not use evidence by itself (the `Telemetry` that follows
///   is judged), and it proves the stream authenticated;
/// - `Telemetry`: the stream is healthy, the frame is stored for every car except a
///   Disabled or Removed one, and it is judged as evidence (motion, parked power);
///   a frame also proves the stream authenticated;
/// - `VehicleOffline`: the power gate is revoked and a targeted non-waking state
///   check is due now;
/// - `TransportUnavailable`, `ProtocolViolation`, `AuthRejected`: the power gate is
///   revoked and the stream is in outage; `AuthRejected` also tells the caller
///   through `authentication_rejected`, which goes to the credential unit as
///   evidence, not a verdict (C4 R46, R56 c).
///
/// The scheduler only records which gates must be revoked; the loop applies them
/// before the next request is built (`apply_gate_revocations`). Every event of
/// every kind is idempotent (C3 R10, C4 R56 f).
fn process_stream_event(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    stream: &VehicleStreamRuntime,
    event: StreamEvent,
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
    authentication_rejected: &mut bool,
    authenticated: &mut bool,
) -> Result<(), CollectorError> {
    let vehicle_id = stream.vehicle_id;
    let now = scheduler.clock.mono_now();
    match event {
        StreamEvent::Healthy => {
            *authenticated = true;
            let recovery = scheduler.stream_healthy(vehicle_id, now);
            report_stream_recovery(scheduler, vehicle_id, recovery, now);
            #[cfg(test)]
            supervised_test_log(LoopEvent::HealthyProcessed(Instant::now()));
        }
        StreamEvent::Telemetry { update, queued_at } => {
            *authenticated = true;
            let recovery = scheduler.stream_healthy(vehicle_id, now);
            report_stream_recovery(scheduler, vehicle_id, recovery, now);
            let queue_lag = now.saturating_duration_since(queued_at);
            process_stream_telemetry_with_cache(
                store,
                scheduler,
                vehicle_id,
                &update,
                queue_lag,
                projection_car_ids,
            )?;
        }
        StreamEvent::VehicleOffline => {
            scheduler.revoke_gate(vehicle_id);
            scheduler.schedule_offline_state_fetch(vehicle_id, now);
        }
        StreamEvent::TransportUnavailable => {
            note_stream_unhealthy(scheduler, vehicle_id, "transport_unavailable", now);
        }
        StreamEvent::ProtocolViolation => {
            note_stream_unhealthy(scheduler, vehicle_id, "protocol_violation", now);
        }
        StreamEvent::AuthRejected => {
            *authentication_rejected = true;
            note_stream_unhealthy(scheduler, vehicle_id, "authentication_rejected", now);
            #[cfg(test)]
            supervised_test_log(LoopEvent::RejectionProcessed);
        }
    }
    Ok(())
}

/// How a stream run's end is handled (C3 R28; C4 R12, R56 e).
enum StreamRunEnd {
    /// The credential authority is unavailable: the collector stops, as before.
    Terminal(CollectorError),
    /// Any other end: the next run waits out the run spacing; `audit` says the run
    /// ended because a receipt could not be written.
    Spaced { audit: bool },
}

/// Classify the end of a stream run's task. Only `CredentialAuthorityUnavailable`
/// is terminal (the sensitive-access flag keeps stopping the collector, IF c1).
/// Every other end, an error of C4 R12 (`EventQueueFull`, `EventReceiverClosed`,
/// `Audit`), a panic, a cancellation or a normal return that C3 did not ask for,
/// is not cured by an immediate retry, so it is spaced: a normal return nobody
/// asked for would otherwise restart in a tight loop.
fn classify_stream_run_end(
    result: Result<Result<(), crate::tesla_stream::StreamSupervisorError>, JoinError>,
) -> StreamRunEnd {
    match classify_stream_task_result(result) {
        CollectorError::StreamTask(StreamTaskOutcome::Supervisor(
            error @ crate::tesla_stream::StreamSupervisorError::CredentialAuthorityUnavailable,
        )) => StreamRunEnd::Terminal(CollectorError::StreamTask(StreamTaskOutcome::Supervisor(
            error,
        ))),
        CollectorError::StreamTask(StreamTaskOutcome::Supervisor(
            crate::tesla_stream::StreamSupervisorError::Audit(_),
        )) => StreamRunEnd::Spaced { audit: true },
        _ => StreamRunEnd::Spaced { audit: false },
    }
}

async fn drain_stream_events_with_cache(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut [VehicleStreamRuntime],
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
) -> Result<StreamDrainResult, CollectorError> {
    drain_stream_events_while_stopping(store, scheduler, streams, projection_car_ids, &[], None)
        .await
}

/// One drain while the runs of `stopping` wind down (C3 R28, R36, B2 clarification
/// 3): the other streams are drained as ever, and a stopping run contributes its
/// telemetry frames only, each stored once by the ordinary rule (so a Disabled or
/// Removed car's frames are not), while its control events change nothing: they
/// describe a run that is ending and must not mark it healthy or re-admit it.
async fn drain_stream_events_while_stopping(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut [VehicleStreamRuntime],
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
    stopping: &[VehicleId],
    write_fence: Option<&RunWriteGuard>,
) -> Result<StreamDrainResult, CollectorError> {
    let mut authentication_rejected = false;
    let mut authenticated = false;
    let mut terminal_error = None;
    let mut ended = Vec::new();
    for stream in streams.iter_mut() {
        let winding_down = stopping.contains(&stream.vehicle_id);
        for _ in 0..MAX_STREAM_EVENTS_PER_DRAIN {
            let Ok(event) = stream.events.try_recv() else {
                break;
            };
            // Each frame is its own transaction: the run's loss fence is looked at
            // immediately before each one (B2 fix round, P2, fourth pass).
            if matches!(event, StreamEvent::Telemetry { .. }) && frame_writes_fenced(write_fence) {
                return Err(CollectorError::SensitiveAccessUnavailable);
            }
            if winding_down {
                if let StreamEvent::Telemetry { update, queued_at } = event {
                    let queue_lag = scheduler
                        .clock
                        .mono_now()
                        .saturating_duration_since(queued_at);
                    process_stream_telemetry_inner(
                        store,
                        scheduler,
                        stream.vehicle_id,
                        &update,
                        queue_lag,
                        projection_car_ids,
                        false,
                    )?;
                }
                continue;
            }
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
        if !winding_down && stream.task.as_ref().is_some_and(JoinHandle::is_finished) {
            // Once the producer has finished, its bounded queue is stable.
            // Drain every final event before consuming the JoinHandle so the
            // last telemetry and authentication transition reach the caller.
            while let Ok(event) = stream.events.try_recv() {
                // The same per-frame look at the loss fence as above (R-P2F5-3).
                if matches!(event, StreamEvent::Telemetry { .. }) && frame_writes_fenced(write_fence) {
                    return Err(CollectorError::SensitiveAccessUnavailable);
                }
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
            match classify_stream_run_end(task.await) {
                StreamRunEnd::Terminal(error) => {
                    if terminal_error.is_none() {
                        terminal_error = Some(error);
                    }
                    #[cfg(test)]
                    supervised_test_log(LoopEvent::TerminalRunSettled);
                }
                StreamRunEnd::Spaced { audit } => {
                    // The run is over: it is an outage for cadence, its gate is
                    // revoked, and the next run waits (C3 R28, R36).
                    let now = scheduler.clock.mono_now();
                    note_stream_unhealthy(scheduler, stream.vehicle_id, "run_ended", now);
                    if audit {
                        let _ = scheduler.stream_audit_failed(stream.vehicle_id);
                    }
                    scheduler.stream_task_ended(stream.vehicle_id, now);
                    ended.push(stream.vehicle_id);
                }
            }
        }
        if stream.sensitive_access_failure.load(Ordering::Acquire) {
            terminal_error = Some(CollectorError::SensitiveAccessUnavailable);
        }
    }
    // A Driving stream that falls silent with no event is an outage for cadence,
    // reported once (C3 R28).
    let now = scheduler.clock.mono_now();
    for vehicle_id in scheduler.silent_drive_streams(now) {
        note_stream_unhealthy(scheduler, vehicle_id, "frame_silence", now);
    }
    // The in-process source of the audit readiness code (C3 R28).
    store.set_stream_audit_failed_cars(scheduler.stream_audit_failed_count());
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
        ended,
    })
}

/// Revoke the power gates the scheduler asked for (C3 R36), through `revoke`.
fn apply_gate_revocations_with(
    scheduler: &mut VehicleScheduler,
    mut revoke: impl FnMut(VehicleId),
) {
    for vehicle_id in scheduler.take_gate_revocations() {
        revoke(vehicle_id);
    }
}

/// Revoke the power gates of the streams the scheduler asked for (C3 R36). C3
/// calls exactly two things on a gate, `revoke` here and `is_confirmed` at the
/// send time of a gated read, and never `observe` (C4 R50, R56 i). The loop calls
/// it after every stream drain, after every result that may enter Suspended and
/// before it builds a read.
fn apply_gate_revocations(scheduler: &mut VehicleScheduler, streams: &[VehicleStreamRuntime]) {
    apply_gate_revocations_with(scheduler, |vehicle_id| {
        for stream in streams
            .iter()
            .filter(|stream| stream.vehicle_id == vehicle_id)
        {
            stream.power_gate.revoke();
        }
    });
}

/// The gates of the running streams, kept apart from the streams themselves so a
/// handler can revoke while the queues are lent out.
fn stream_gates(streams: &[VehicleStreamRuntime]) -> Vec<(VehicleId, Arc<StreamPowerGate>)> {
    streams
        .iter()
        .map(|stream| (stream.vehicle_id, Arc::clone(&stream.power_gate)))
        .collect()
}

/// [`apply_gate_revocations`] over [`stream_gates`].
fn revoke_requested_gates(
    scheduler: &mut VehicleScheduler,
    gates: &[(VehicleId, Arc<StreamPowerGate>)],
) {
    apply_gate_revocations_with(scheduler, |vehicle_id| {
        for (_, gate) in gates.iter().filter(|(id, _)| *id == vehicle_id) {
            gate.revoke();
        }
    });
}

/// Stop and start the streams as the scheduler says (C3 R28): the cars whose stream
/// is no longer wanted, or that have left the scheduler, are stopped together
/// (`stop_vehicle_streams`: every signal first, concurrent five-second deadlines,
/// abort and join before a runtime is removed, so no second run for a car starts
/// before its previous run has ended), and a stream is started for every car that
/// should have one and has none.
#[allow(clippy::too_many_arguments)]
async fn reconcile_streams(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    streams: &mut Vec<VehicleStreamRuntime>,
    auth: &CollectionAuth,
    client: &OwnerApi,
    health_timeout: Duration,
    stream_endpoint: &str,
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
    pending: &mut StreamDrainResult,
    budget: &mut DrainBudget,
) -> Result<(), CollectorError> {
    // A stream of a car that has left the scheduler is not wanted either.
    let configured = scheduler
        .vehicles()
        .into_iter()
        .map(|vehicle| vehicle.id)
        .collect::<HashSet<_>>();
    let mut stops = streams_not_in_scheduler(streams, &configured);
    stops.extend(scheduler.streams_to_stop());
    stop_vehicle_streams(
        store,
        scheduler,
        streams,
        &stops,
        projection_car_ids,
        pending,
        budget,
    )
    .await?;
    for vehicle in scheduler.vehicles() {
        if scheduler.should_start_stream(vehicle.id) {
            ensure_vehicle_stream(
                store,
                streams,
                vehicle.id,
                vehicle.stream_id,
                auth,
                client,
                health_timeout,
                Some(stream_endpoint),
                scheduler.write_guard(),
            );
        }
    }
    Ok(())
}

/// Store one stream frame and judge it as evidence (C3 R36): the scheduler first
/// says whether the frame is stored (every frame of a car that is not Disabled or
/// Removed, whatever its holds, account state or wait: a REST failure says nothing
/// about the stream's data, and frames already queued when a stream is stopped are
/// stored too) and notes the stream healthy, then the frame is judged as evidence
/// (motion, parked power, gear P after Driving). A stored frame is the car's latest
/// observation for R40.
fn process_stream_telemetry_with_cache(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    vehicle_id: VehicleId,
    update: &crate::tesla_stream::StreamUpdate,
    queue_lag: Duration,
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
) -> Result<bool, CollectorError> {
    process_stream_telemetry_inner(
        store,
        scheduler,
        vehicle_id,
        update,
        queue_lag,
        projection_car_ids,
        true,
    )
}

/// [`process_stream_telemetry_with_cache`], where `marks_healthy` says whether the
/// frame also shows the car's stream connected and fresh. The frames of a run that is
/// being stopped, and of the queue it left, are stored and judged but mark nothing
/// healthy (B2 round 3, C4R3-N2): their stream is over, and the scheduler would
/// otherwise plan the car's next read at the slower stream interval until the next
/// reconcile.
fn process_stream_telemetry_inner(
    store: &HubStore,
    scheduler: &mut VehicleScheduler,
    vehicle_id: VehicleId,
    update: &crate::tesla_stream::StreamUpdate,
    queue_lag: Duration,
    projection_car_ids: &mut HashMap<VehicleId, StreamContext>,
    marks_healthy: bool,
) -> Result<bool, CollectorError> {
    let now = scheduler.clock.mono_now();
    let persist = if marks_healthy {
        scheduler.should_persist_stream_telemetry(vehicle_id, update.power, now)
    } else {
        scheduler.should_persist_stopping_stream_telemetry(vehicle_id)
    };
    scheduler.schedule_stream_evidence(
        vehicle_id,
        update.shift_state.as_deref(),
        update.speed,
        update.power,
        now,
    );
    if !persist {
        return Ok(false);
    }
    if let std::collections::hash_map::Entry::Vacant(entry) = projection_car_ids.entry(vehicle_id) {
        // The car's identity is registered with its own store mutations: each is
        // preceded by the run's write guard (B2 fix round, P3b fix 2, R-P3bF2-1).
        let context = stream_context(store, vehicle_id, scheduler.write_guard())?;
        entry.insert(context);
    }
    // The frame itself is a transaction of its own.
    scheduler.check_write("stream-frame-intake")?;
    let context = projection_car_ids
        .get_mut(&vehicle_id)
        .expect("stream context is present after insertion");
    let stored = persist_stream_update_with_projection(
        store,
        vehicle_id,
        update,
        Some((context, queue_lag)),
    )?;
    if stored {
        scheduler.note_observation_stored(vehicle_id, scheduler.wall_or_last());
    }
    Ok(stored)
}

#[cfg(test)]
fn persist_stream_update(
    store: &HubStore,
    vehicle_id: VehicleId,
    update: &crate::tesla_stream::StreamUpdate,
) -> Result<bool, CollectorError> {
    persist_stream_update_with_projection(store, vehicle_id, update, None)
}

struct StreamContext {
    source_id: Uuid,
    registered_vehicle_id: Uuid,
    selected_car_id: i64,
    writer: StreamObservationWriter,
    last_stream_timestamp_ms: Option<i64>,
    queue_lagging: bool,
    /// The car's Hub id as text: the only id the logs carry (C3 R9).
    hub_label: String,
}

impl StreamContext {
    fn report_ingestion_health(&mut self, observed_at_ms: i64, queue_lag: Duration) {
        if let Some(previous) = self.last_stream_timestamp_ms {
            let source_gap_ms = observed_at_ms.saturating_sub(previous);
            if source_gap_ms >= STREAM_SOURCE_GAP_WARN_MS {
                tracing::warn!(
                    vehicle = %self.hub_label,
                    source_gap_ms,
                    previous_observed_at_ms = previous,
                    observed_at_ms,
                    diagnosis = "upstream_stream_gap",
                    "Tesla stream resumed after a source timestamp gap"
                );
            }
        }
        self.last_stream_timestamp_ms = Some(observed_at_ms);

        let queue_lag_ms = i64::try_from(queue_lag.as_millis()).unwrap_or(i64::MAX);
        if queue_lag_ms >= STREAM_QUEUE_LAG_WARN_MS && !self.queue_lagging {
            self.queue_lagging = true;
            tracing::warn!(
                vehicle = %self.hub_label,
                queue_lag_ms,
                diagnosis = "local_stream_processing_lag",
                "Tesla stream ingestion queue is falling behind"
            );
        } else if queue_lag_ms <= STREAM_QUEUE_LAG_RECOVERED_MS && self.queue_lagging {
            self.queue_lagging = false;
            tracing::info!(
                vehicle = %self.hub_label,
                queue_lag_ms,
                "Tesla stream ingestion processing lag recovered"
            );
        }
    }
}

fn stream_context(
    store: &HubStore,
    vehicle_id: VehicleId,
    guard: Option<&RunWriteGuard>,
) -> Result<StreamContext, CollectorError> {
    let received_at_ms = current_epoch_millis()?;
    check_optional_write(guard, "stream-context-source")?;
    let source = store.register_source(
        &SourceDescriptor::new(STREAM_SOURCE_KIND, STREAM_SOURCE_KEY),
        received_at_ms,
    )?;
    check_optional_write(guard, "stream-context-vehicle")?;
    let registered = store.register_vehicle(
        &VehicleDescriptor::new(source.source_id, vehicle_id.get().to_string())
            .with_tesla_identity(Some(vehicle_id.get() as i64), None),
        received_at_ms,
    )?;
    let last_stream_timestamp_ms = store.stream_watermark(registered.vehicle_id)?;
    check_optional_write(guard, "stream-context-car-id")?;
    let selected_car_id =
        projection_car_id_for_vehicle(store, registered.vehicle_id, vehicle_id.get())?;
    Ok(StreamContext {
        source_id: source.source_id,
        registered_vehicle_id: registered.vehicle_id,
        selected_car_id,
        writer: store.stream_observation_writer()?,
        last_stream_timestamp_ms,
        queue_lagging: false,
        hub_label: registered.vehicle_id.to_string(),
    })
}

fn persist_stream_update_with_projection(
    store: &HubStore,
    vehicle_id: VehicleId,
    update: &crate::tesla_stream::StreamUpdate,
    mut context: Option<(&mut StreamContext, Duration)>,
) -> Result<bool, CollectorError> {
    let received_at_ms = current_epoch_millis()?;
    let maximum = received_at_ms.saturating_add(K_SKEW_MS);
    if update.timestamp_ms < K_EARLIEST_MS || update.timestamp_ms > maximum {
        tracing::debug!(
            reason = "clock_or_future_timestamp",
            "stream frame rejected"
        );
        return Ok(false);
    }
    let (source_id, registered_vehicle_id, pack_car_id) =
        if let Some((context, _)) = context.as_ref() {
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
    let input = ObservationInput {
        source_id,
        vehicle_id: registered_vehicle_id,
        observed_at_ms: update.timestamp_ms,
        payload: stream_observation_payload(update),
    };
    let result = if let Some((context, queue_lag)) = context.as_mut() {
        let result = context.writer.accept(&input, received_at_ms, pack_car_id)?;
        if matches!(result, StreamObservationResult::Committed { .. }) {
            context.report_ingestion_health(update.timestamp_ms, *queue_lag);
        }
        result
    } else {
        store.accept_stream_observation_and_lifecycle(&input, received_at_ms, pack_car_id)?
    };
    #[cfg(test)]
    if matches!(result, StreamObservationResult::Committed { .. }) {
        supervised_test_log(LoopEvent::FrameCommitted {
            vehicle: vehicle_id.get(),
            observed_at_ms: update.timestamp_ms,
        });
        supervised_test_frame_committed(update.timestamp_ms);
    }
    if matches!(result, StreamObservationResult::IgnoredDuplicate) {
        tracing::debug!(
            vehicle = %registered_vehicle_id,
            reason = "non_monotonic_timestamp",
            "stream frame rejected"
        );
        return Ok(false);
    }
    Ok(true)
}
