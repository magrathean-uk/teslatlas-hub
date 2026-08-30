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

fn report_stream_outage(vehicle_id: VehicleId, reason: &'static str, outage: StreamOutage) {
    let StreamOutage::Active(outage) = outage else {
        return;
    };
    tracing::warn!(
        vehicle_id = vehicle_id.get(),
        reason,
        consecutive_failures = outage.consecutive_failures,
        outage_ms = u64::try_from(outage.outage_duration.as_millis()).unwrap_or(u64::MAX),
        phase = ?outage.phase,
        owner_api_fallback_scheduled = outage.owner_api_fallback_scheduled,
        live_power_gate = outage.live_power_gate,
        recovery = "stream_reconnect_and_owner_api_fallback",
        "vehicle stream degraded; bounded recovery active"
    );
}

fn report_stream_recovery(vehicle_id: VehicleId, recovery: StreamRecovery) {
    let StreamRecovery::Recovered(recovery) = recovery else {
        return;
    };
    tracing::info!(
        vehicle_id = vehicle_id.get(),
        failures = recovery.failures,
        outage_ms = u64::try_from(recovery.outage_duration.as_millis()).unwrap_or(u64::MAX),
        "vehicle stream recovered"
    );
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
            report_stream_recovery(
                stream.vehicle_id,
                scheduler.stream_healthy(stream.vehicle_id, Instant::now()),
            );
        }
        StreamEvent::Telemetry { update, queued_at } => {
            *authenticated = true;
            report_stream_recovery(
                stream.vehicle_id,
                scheduler.stream_healthy(stream.vehicle_id, Instant::now()),
            );
            process_stream_telemetry_with_cache(
                store,
                scheduler,
                stream.vehicle_id,
                &update,
                queued_at.elapsed(),
                projection_car_ids,
            )?;
        }
        StreamEvent::VehicleOffline => {
            let now = Instant::now();
            report_stream_outage(
                stream.vehicle_id,
                "vehicle_offline",
                scheduler.stream_unhealthy(stream.vehicle_id, now),
            );
            scheduler.schedule_offline_state_fetch(stream.vehicle_id, now);
        }
        StreamEvent::AuthRejected => {
            *authentication_rejected = true;
            report_stream_outage(
                stream.vehicle_id,
                "authentication_rejected",
                scheduler.stream_unhealthy(stream.vehicle_id, Instant::now()),
            );
            tracing::warn!(
                vehicle_id = stream.vehicle_id.get(),
                "vehicle stream authentication rejected"
            );
        }
        StreamEvent::TransportUnavailable => {
            report_stream_outage(
                stream.vehicle_id,
                "transport_unavailable",
                scheduler.stream_unhealthy(stream.vehicle_id, Instant::now()),
            );
        }
        StreamEvent::ProtocolViolation => {
            report_stream_outage(
                stream.vehicle_id,
                "protocol_violation",
                scheduler.stream_unhealthy(stream.vehicle_id, Instant::now()),
            );
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
    queue_lag: Duration,
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
    if let std::collections::hash_map::Entry::Vacant(entry) = projection_car_ids.entry(vehicle_id) {
        let context = stream_context(store, vehicle_id)?;
        entry.insert(context);
    }
    let context = projection_car_ids
        .get_mut(&vehicle_id)
        .expect("stream context is present after insertion");
    persist_stream_update_with_projection(store, vehicle_id, update, Some((context, queue_lag)))
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
}

impl StreamContext {
    fn report_ingestion_health(
        &mut self,
        vehicle_id: VehicleId,
        observed_at_ms: i64,
        queue_lag: Duration,
    ) {
        if let Some(previous) = self.last_stream_timestamp_ms {
            let source_gap_ms = observed_at_ms.saturating_sub(previous);
            if source_gap_ms >= STREAM_SOURCE_GAP_WARN_MS {
                tracing::warn!(
                    vehicle_id = vehicle_id.get(),
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
                vehicle_id = vehicle_id.get(),
                queue_lag_ms,
                diagnosis = "local_stream_processing_lag",
                "Tesla stream ingestion queue is falling behind"
            );
        } else if queue_lag_ms <= STREAM_QUEUE_LAG_RECOVERED_MS && self.queue_lagging {
            self.queue_lagging = false;
            tracing::info!(
                vehicle_id = vehicle_id.get(),
                queue_lag_ms,
                "Tesla stream ingestion processing lag recovered"
            );
        }
    }
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
    let last_stream_timestamp_ms = store.stream_watermark(registered.vehicle_id)?;
    Ok(StreamContext {
        source_id: source.source_id,
        registered_vehicle_id: registered.vehicle_id,
        selected_car_id: projection_car_id_for_vehicle(
            store,
            registered.vehicle_id,
            vehicle_id.get(),
        )?,
        writer: store.stream_observation_writer()?,
        last_stream_timestamp_ms,
        queue_lagging: false,
    })
}

fn persist_stream_update_with_projection(
    store: &HubStore,
    vehicle_id: VehicleId,
    update: &crate::tesla_stream::StreamUpdate,
    mut context: Option<(&mut StreamContext, Duration)>,
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
            context.report_ingestion_health(vehicle_id, update.timestamp_ms, *queue_lag);
        }
        result
    } else {
        store.accept_stream_observation_and_lifecycle(&input, received_at_ms, pack_car_id)?
    };
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
