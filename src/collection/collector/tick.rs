// SPDX-License-Identifier: AGPL-3.0-only

// The tick transaction (C3 part K3: R5, R6, R14, section 8 item 11). Every
// durable write a tick's results cause commits in one write transaction: the
// discovery observations of R14, the `vehicle_data` observations and their
// lifecycle deltas, the Hub events of R40 and R52, the scheduler rows of R4 and
// the gaps of R8. The commit happens when every request of the tick has
// completed or when the oldest uncommitted result is `K_TICK_COMMIT_AGE` old,
// whichever comes first. A tick whose requests produced nothing to store writes
// nothing.
//
// The scheduler keeps its decisions in memory and reports the outcome:
// `commit_succeeded` moves its persisted views, `commit_failed` returns it to
// the state before the tick (R6). Nothing here decides cadence or errors; later
// parts fill the results this module stores.

/// The longest a result waits for its transaction (C3 R5, `K_TICK_COMMIT_AGE`,
/// **provisional**, gate M-I1): the plan's intake target is 250 ms from an
/// admitted observation to its durable commit, and without a bound a received
/// observation would wait for the slowest of up to `K_MAX_IN_FLIGHT` requests
/// (each up to `request_timeout_seconds`). 100 ms leaves 150 ms of the target
/// for the commit itself.
const K_TICK_COMMIT_AGE: Duration = Duration::from_millis(100);

/// What the store knows of a car's identity for one tick (C3 R5): its Hub id,
/// the source it belongs to and its pack-local car id.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Registered {
    hub_id: Uuid,
    source_id: Uuid,
    pack_car_id: i64,
}

/// A registered identity with the descriptor facts it was registered under, so a
/// change of VIN or display name registers again.
#[derive(Clone, Debug)]
struct CachedIdentity {
    vin: Option<String>,
    display_name: Option<String>,
    registered: Registered,
}

/// The cars' store identities, resolved once per process and per change (C3 R5).
/// Registering a car refreshes its `last_seen_at_ms`, a durable write, so it is
/// done before the tick transaction and only when the identity is new or its
/// descriptor changed; an unchanged poll writes nothing (R14, the Idle target).
#[derive(Default)]
struct IdentityCache {
    entries: HashMap<VehicleId, CachedIdentity>,
}

impl IdentityCache {
    #[cfg(test)]
    fn resolve(
        &mut self,
        store: &HubStore,
        provider: CollectorProvider,
        vehicle: &Vehicle,
        now_ms: i64,
    ) -> Result<Registered, CollectorError> {
        self.resolve_guarded(store, provider, vehicle, now_ms, None)
    }

    /// [`Self::resolve`] for a supervised run: the source and the vehicle are two
    /// independent store mutations, each preceded by the run's write guard (B2 fix
    /// round, P3b fix 2, R-P3bF2-1). A cached identity writes nothing.
    fn resolve_guarded(
        &mut self,
        store: &HubStore,
        provider: CollectorProvider,
        vehicle: &Vehicle,
        now_ms: i64,
        guard: Option<&RunWriteGuard>,
    ) -> Result<Registered, CollectorError> {
        let vin = clean_optional_text(Some(&vehicle.vin));
        let display_name = clean_optional_text(vehicle.display_name.as_deref());
        if let Some(entry) = self.entries.get(&vehicle.id)
            && entry.vin == vin
            && entry.display_name == display_name
        {
            return Ok(entry.registered.clone());
        }
        check_optional_write(guard, "identity-source")?;
        let source = store.register_source(&provider_source(provider), now_ms)?;
        let mut descriptor = VehicleDescriptor::new(source.source_id, vehicle.id.get().to_string())
            .with_tesla_identity(Some(vehicle.id.get() as i64), None);
        descriptor.vin = vin.clone();
        descriptor.display_name = display_name.clone();
        check_optional_write(guard, "identity-vehicle")?;
        let record = store.register_vehicle(&descriptor, now_ms)?;
        check_optional_write(guard, "identity-car-id")?;
        let pack_car_id =
            projection_car_id_for_vehicle(store, record.vehicle_id, vehicle.id.get())?;
        let registered = Registered {
            hub_id: record.vehicle_id,
            source_id: source.source_id,
            pack_car_id,
        };
        self.entries.insert(
            vehicle.id,
            CachedIdentity {
                vin,
                display_name,
                registered: registered.clone(),
            },
        );
        Ok(registered)
    }
}

/// A listed car as a discovery candidate (C3 R14): Tesla's state as received,
/// and the Hub wall time at which the response was received (C1 R7: discovery
/// is on the Hub clock).
#[derive(Clone, Debug)]
struct DiscoveryItem {
    vehicle: Vehicle,
    received_at_ms: i64,
}

/// A successful read with the car as the scheduler held it when the response
/// arrived and the Hub wall time of the response (C1: `received_at_ms`).
#[derive(Clone, Debug)]
struct SnapshotItem {
    vehicle: Vehicle,
    snapshot: VehicleData,
    received_at_ms: i64,
}

/// A Hub lifecycle event to store (C3 R40, R52, C1 R15): `no_data` for a drive
/// that ended while the car was unreachable, `service_refused` for a read Tesla
/// refused as in service. A `no_data` item is only a candidate (R40, B2
/// clarification 3): the tick transaction decides it after the tick's own writes and
/// stamps the event with that decision's time, so `observed_at_ms` of a candidate is
/// the time it was planned, never the time it is stored.
#[derive(Clone, Debug)]
struct HubEventItem {
    vehicle: Vehicle,
    event: &'static str,
    observed_at_ms: i64,
}

/// The results of a tick not yet stored (C3 R5).
#[derive(Debug, Default)]
struct TickBatch {
    discoveries: Vec<DiscoveryItem>,
    snapshots: Vec<SnapshotItem>,
    hub_events: Vec<HubEventItem>,
    failures: Vec<VehicleCollectionFailure>,
    /// When the oldest result of the batch arrived: its transaction is due
    /// `K_TICK_COMMIT_AGE` later.
    first_result_at: Option<Instant>,
}

impl TickBatch {
    /// Nothing is waiting for a transaction (C3 R5: a reservation commit made with
    /// an empty batch is a lone one).
    fn is_empty(&self) -> bool {
        self.discoveries.is_empty()
            && self.snapshots.is_empty()
            && self.hub_events.is_empty()
            && self.failures.is_empty()
    }

    /// Note a result arriving at `now`: the first one starts the commit clock.
    fn note_result(&mut self, now: Instant) {
        self.first_result_at.get_or_insert(now);
    }

    /// When the batch must commit at the latest (C3 R5), if it holds a result.
    fn commit_deadline(&self) -> Option<Instant> {
        self.first_result_at
            .map(|first| deadline_after(first, K_TICK_COMMIT_AGE))
    }
}

/// What one tick transaction did (C3 R5), for the scheduler and the log.
#[derive(Debug, Default)]
struct TickTxOutput {
    /// Anything was written.
    wrote: bool,
    /// The cars whose lifecycle snapshot the transaction read (C3 R14).
    views_read: Vec<(VehicleId, crate::db::CollectorLifecycleSnapshot)>,
    /// The cars that got a current discovery row only.
    current_only: Vec<VehicleId>,
    /// The cars with an observation stored: `(car, received_at_ms)`. A Hub event
    /// is not one (C3 R40: evidence is the car's own observations).
    stored: Vec<(VehicleId, i64)>,
    /// The cars a Hub event was stored for; their lifecycle view is stale.
    events: Vec<VehicleId>,
    /// The cars a `no_data` candidate was appended for, with the decision time it
    /// was stamped with (C3 R40): only these are marked closed, and only after the
    /// commit.
    no_data_appended: Vec<(VehicleId, i64)>,
    report: Option<ManualCollectionReport>,
}

/// How a tick's commit ended (C3 R5, R6).
#[derive(Debug)]
enum TickOutcome {
    /// There was nothing to store and the store was not touched.
    Idle,
    /// The transaction ran. `wrote` says whether it committed anything (a
    /// transaction that found nothing to write rolls back); `report` is set when
    /// snapshots were stored.
    Committed {
        wrote: bool,
        report: Option<ManualCollectionReport>,
    },
    /// The transaction failed and rolled back; the scheduler is back at its
    /// baseline and waits (R6).
    Failed,
}

/// What a tick's commit needs besides the scheduler and the batch.
struct TickCommit<'a> {
    store: &'a HubStore,
    cursor_key: &'a CursorKey,
    provider: CollectorProvider,
    /// `T_gap`, the configured offline drive timeout, passed to every intake.
    offline_drive_timeout: Duration,
}

/// C3 R14 for one discovery candidate, inside the tick transaction. The cached
/// lifecycle facts decide when they are current; otherwise the transaction reads
/// them once. Returns the vehicle's identity use through `output`.
fn store_discovery_in_transaction(
    store: &HubStore,
    transaction: &rusqlite::Transaction<'_>,
    commit: &TickCommit<'_>,
    registered: &Registered,
    item: &DiscoveryItem,
    cached: Option<(Option<String>, Option<String>)>,
    output: &mut TickTxOutput,
) -> Result<(), CollectorError> {
    let vehicle = &item.vehicle;
    let (last_reach, current, read) = match cached {
        Some((last_reach, current)) => (last_reach, current, None),
        None => {
            let snapshot = HubStore::collector_lifecycle_snapshot_in_transaction(
                transaction,
                registered.hub_id,
            )?;
            (
                snapshot.last_reach.clone(),
                snapshot.current_discovery_state.clone(),
                Some(snapshot),
            )
        }
    };
    let action =
        crate::db::discovery_action(&vehicle.state, last_reach.as_deref(), current.as_deref());
    let input = ObservationInput {
        source_id: registered.source_id,
        vehicle_id: registered.hub_id,
        observed_at_ms: item.received_at_ms,
        payload: discovery_payload(vehicle, commit.provider),
    };
    let mut read = read;
    match action {
        crate::db::DiscoveryAction::Skip => {}
        crate::db::DiscoveryAction::WriteCurrentOnly => {
            HubStore::write_current_discovery_in_transaction(
                transaction,
                &input,
                item.received_at_ms,
            )?;
            if let Some(snapshot) = read.as_mut() {
                snapshot.current_discovery_state = Some(vehicle.state.clone());
            }
            output.current_only.push(vehicle.id);
            output.wrote = true;
        }
        crate::db::DiscoveryAction::Deliver => {
            let mut live = vehicle.settings.clone();
            // A live car's suspend length is final (C3 R60): no model lookup. A
            // row an earlier Hub left unresolved gets the Hub default first, since
            // the upsert below marks the row resolved and would otherwise keep
            // that row's own value for good.
            HubStore::resolve_car_suspend_min_in_transaction(transaction, registered.hub_id)?;
            live.suspend_min_resolved = true;
            HubStore::upsert_car_settings_in_transaction(
                transaction,
                registered.hub_id,
                registered.pack_car_id,
                &live,
            )?;
            store.accept_owner_observation_and_lifecycle_in_transaction(
                transaction,
                &input,
                item.received_at_ms,
                registered.pack_car_id,
                commit.offline_drive_timeout,
            )?;
            output.stored.push((vehicle.id, item.received_at_ms));
            output.wrote = true;
        }
    }
    if let Some(snapshot) = read {
        output.views_read.push((vehicle.id, snapshot));
    }
    Ok(())
}

/// Store the batch's snapshots inside the tick transaction (C3 R5, section 8
/// item 11: `finish_collection[_for_provider]` through its `_in_transaction`
/// variant): each snapshot with its lifecycle delta, then each car's settings,
/// materialised car and lifecycle catch-up, as `persist_collection_mode` and
/// `materialise_lifecycle_for_collection_provider` do on their own transactions.
fn finish_collection_in_transaction(
    store: &HubStore,
    transaction: &rusqlite::Transaction<'_>,
    commit: &TickCommit<'_>,
    registered: &HashMap<VehicleId, Registered>,
    snapshots: &[SnapshotItem],
    failures: usize,
    output: &mut TickTxOutput,
) -> Result<(), CollectorError> {
    if snapshots.is_empty() {
        return Ok(());
    }
    let mut report = ManualCollectionReport {
        source_id: Uuid::nil(),
        request_audit_correlation_id: Uuid::nil(),
        vehicles_seen: 0,
        online_vehicles_seen: 0,
        snapshots_received: snapshots.len(),
        observations_inserted: 0,
        observations_already_present: 0,
        snapshots_published: 0,
        vehicle_failures: failures,
        drives_closed: 0,
        charges_closed: 0,
        positions_materialised: 0,
        charge_samples_materialised: 0,
        lifecycle_quarantines: 0,
    };
    let mut seen: Vec<VehicleId> = Vec::new();
    for item in snapshots {
        let identity = registered
            .get(&item.vehicle.id)
            .ok_or(CollectorError::SnapshotWithoutListedVehicle)?;
        report.source_id = identity.source_id;
        if !seen.contains(&item.vehicle.id) {
            seen.push(item.vehicle.id);
            if item.vehicle.is_online() {
                report.online_vehicles_seen += 1;
            }
        }
        if item.received_at_ms < 0 {
            return Err(CollectorError::InvalidReceiptTimestamp);
        }
        let input = ObservationInput {
            source_id: identity.source_id,
            vehicle_id: identity.hub_id,
            observed_at_ms: observation_timestamp(&item.snapshot, item.received_at_ms),
            payload: observation_payload(&item.snapshot, &item.vehicle.state, commit.provider),
        };
        let result = store.accept_owner_observation_and_lifecycle_in_transaction(
            transaction,
            &input,
            item.received_at_ms,
            identity.pack_car_id,
            commit.offline_drive_timeout,
        )?;
        if result.append.inserted {
            report.observations_inserted += 1;
        } else {
            report.observations_already_present += 1;
        }
        report.drives_closed += result.drives_closed;
        report.charges_closed += result.charges_closed;
        report.positions_materialised += result.positions_materialised;
        report.charge_samples_materialised += result.charge_samples_materialised;
        report.lifecycle_quarantines += usize::from(result.lifecycle_quarantined);
        output.stored.push((item.vehicle.id, item.received_at_ms));
    }
    report.vehicles_seen = seen.len();
    for id in seen {
        let Some(latest) = snapshots.iter().rev().find(|item| item.vehicle.id == id) else {
            continue;
        };
        let identity = &registered[&id];
        let mut live = latest.vehicle.settings.clone();
        // A live car's suspend length is final (C3 R60): no model lookup. A row an
        // earlier Hub version left unresolved gets the Hub default once, before
        // the upsert marks it resolved (R60); imported rows and owner-set values
        // are never changed.
        HubStore::resolve_car_suspend_min_in_transaction(transaction, identity.hub_id)?;
        live.suspend_min_resolved = true;
        HubStore::upsert_car_settings_in_transaction(
            transaction,
            identity.hub_id,
            identity.pack_car_id,
            &live,
        )?;
        let seed_car = compatibility_car(
            &latest.vehicle,
            Some(&latest.snapshot),
            identity.pack_car_id,
        );
        HubStore::persist_materialised_car_if_absent_in_transaction(
            transaction,
            identity.hub_id,
            &seed_car,
        )?;
        let materialised = materialise_vehicle_lifecycle_with(
            store,
            Some(transaction),
            identity.hub_id,
            identity.pack_car_id,
            latest.received_at_ms,
        )?;
        report.drives_closed += materialised.drives_closed;
        report.charges_closed += materialised.charges_closed;
        report.positions_materialised += materialised.positions_materialised;
        report.charge_samples_materialised += materialised.charge_samples_materialised;
        report.lifecycle_quarantines += materialised.lifecycle_quarantines;
    }
    output.wrote = true;
    output.report = Some(report);
    Ok(())
}

/// Everything the tick transaction writes (C3 R5), in the order the evidence
/// arose: the list's discoveries, the reads' snapshots, the Hub's own events and
/// then the scheduler's rows.
///
/// A `no_data` event (C3 R40, B2 clarification 3) is a candidate until this
/// transaction decides it, after the discoveries and reads it stores: it inspects
/// the car's drive in the transaction's own lifecycle state and the newest receipt
/// among the committed rows read before the transaction wrote, the scheduler's cache
/// and the transaction's earlier observations, samples one decision time on the
/// scheduler's clock, and appends only an open drive that no force flag has closed
/// and that has gone strictly longer than `T_gap` without evidence, stamped with that
/// time. A discovery that only refreshes the car's current row is no evidence.
fn write_tick_in_transaction(
    store: &HubStore,
    transaction: &rusqlite::Transaction<'_>,
    commit: &TickCommit<'_>,
    scheduler: &VehicleScheduler,
    registered: &HashMap<VehicleId, Registered>,
    batch: &TickBatch,
    rows: &crate::db::CollectorRowsWrite,
) -> Result<TickTxOutput, CollectorError> {
    let mut output = TickTxOutput::default();
    // The committed evidence of each candidate car, read before the transaction's
    // own writes: a current-only discovery row it makes below is not evidence.
    let mut committed_evidence: HashMap<VehicleId, Option<i64>> = HashMap::new();
    for event in batch
        .hub_events
        .iter()
        .filter(|event| event.event == HUB_EVENT_NO_DATA)
    {
        let identity = registered
            .get(&event.vehicle.id)
            .ok_or(CollectorError::SnapshotWithoutListedVehicle)?;
        let snapshot = HubStore::collector_lifecycle_snapshot_in_transaction(
            transaction,
            identity.hub_id,
        )?;
        committed_evidence.insert(event.vehicle.id, snapshot.last_stored_obs_ms);
    }
    for item in &batch.discoveries {
        let identity = registered
            .get(&item.vehicle.id)
            .ok_or(CollectorError::SnapshotWithoutListedVehicle)?;
        let cached = scheduler.cached_discovery_view(item.vehicle.id);
        store_discovery_in_transaction(
            store,
            transaction,
            commit,
            identity,
            item,
            cached,
            &mut output,
        )?;
    }
    finish_collection_in_transaction(
        store,
        transaction,
        commit,
        registered,
        &batch.snapshots,
        batch.failures.len(),
        &mut output,
    )?;
    // One decision time for the transaction, sampled after its writes.
    let mut decision_ms: Option<i64> = None;
    for event in &batch.hub_events {
        let identity = registered
            .get(&event.vehicle.id)
            .ok_or(CollectorError::SnapshotWithoutListedVehicle)?;
        let is_no_data = event.event == HUB_EVENT_NO_DATA;
        let observed_at_ms = if is_no_data {
            let decided = *decision_ms.get_or_insert_with(|| scheduler.wall_or_last());
            if !no_data_is_due_in_transaction(
                transaction,
                commit,
                scheduler,
                identity,
                event,
                committed_evidence
                    .get(&event.vehicle.id)
                    .copied()
                    .flatten(),
                &output,
                decided,
            )? {
                continue;
            }
            decided
        } else {
            event.observed_at_ms
        };
        append_hub_lifecycle_event_in_transaction(
            store,
            transaction,
            identity,
            observed_at_ms,
            event.event,
            commit.offline_drive_timeout,
        )?;
        if is_no_data {
            output.no_data_appended.push((event.vehicle.id, observed_at_ms));
        }
        output.events.push(event.vehicle.id);
        output.wrote = true;
    }
    if HubStore::write_collector_rows_in_transaction(transaction, rows)? {
        output.wrote = true;
    }
    Ok(output)
}

/// The decision of a `no_data` candidate (C3 R40, B2 clarification 3), inside the
/// transaction that would store it. All of these must hold: no force flag has
/// closed the car already; its lifecycle in this transaction still has an open drive
/// (an intake of this transaction may have closed or ended it, whatever the cached
/// view says); it has some receipt evidence; and `decision_ms` is strictly more
/// than `T_gap` after the newest of that evidence. The evidence is the newest of the
/// committed rows read before the transaction wrote, the scheduler's cached time of
/// the car's last stored observation and the receipt times of the discoveries and
/// reads this transaction stored for the car.
#[allow(clippy::too_many_arguments)]
fn no_data_is_due_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    commit: &TickCommit<'_>,
    scheduler: &VehicleScheduler,
    identity: &Registered,
    event: &HubEventItem,
    committed_evidence_ms: Option<i64>,
    output: &TickTxOutput,
    decision_ms: i64,
) -> Result<bool, CollectorError> {
    let record = scheduler.vehicles.get(&event.vehicle.id);
    if record.is_none_or(|vehicle| vehicle.force_closed_at_gap) {
        return Ok(false);
    }
    let state = HubStore::collector_lifecycle_snapshot_in_transaction(
        transaction,
        identity.hub_id,
    )?;
    if !state.drive_open {
        return Ok(false);
    }
    let evidence_ms = committed_evidence_ms
        .into_iter()
        .chain(record.and_then(|vehicle| vehicle.last_stored_obs_ms))
        .chain(
            output
                .stored
                .iter()
                .filter(|(id, _)| *id == event.vehicle.id)
                .map(|(_, received_at_ms)| *received_at_ms),
        )
        .max();
    let Some(evidence_ms) = evidence_ms else {
        return Ok(false);
    };
    let gap_ms = i64::try_from(commit.offline_drive_timeout.as_millis()).unwrap_or(i64::MAX);
    Ok(decision_ms.saturating_sub(evidence_ms) > gap_ms)
}

/// The vehicles a batch needs an identity for: every car it stores something
/// for or about.
fn batch_vehicles(batch: &TickBatch) -> Vec<Vehicle> {
    let mut vehicles: Vec<Vehicle> = Vec::new();
    let all = batch
        .discoveries
        .iter()
        .map(|item| &item.vehicle)
        .chain(batch.snapshots.iter().map(|item| &item.vehicle))
        .chain(batch.hub_events.iter().map(|item| &item.vehicle));
    for vehicle in all {
        if !vehicles.iter().any(|known| known.id == vehicle.id) {
            vehicles.push(vehicle.clone());
        }
    }
    vehicles
}

/// Whether the batch needs a transaction at all (C3 R5, R14): it does when there
/// is anything to store, or when a discovery's decision needs facts the
/// scheduler has not cached. A list whose states equal what the cached lifecycle
/// view holds is decided in memory and touches no file.
fn tick_needs_transaction(
    scheduler: &VehicleScheduler,
    batch: &TickBatch,
    tick: &PendingTick,
) -> bool {
    if !tick.is_empty() || !batch.snapshots.is_empty() || !batch.hub_events.is_empty() {
        return true;
    }
    batch.discoveries.iter().any(
        |item| match scheduler.cached_discovery_view(item.vehicle.id) {
            None => true,
            Some((last_reach, current)) => {
                crate::db::discovery_action(
                    &item.vehicle.state,
                    last_reach.as_deref(),
                    current.as_deref(),
                ) != crate::db::DiscoveryAction::Skip
            }
        },
    )
}

/// Commit one batch of a tick (C3 R5, R6). Returns [`TickOutcome::Idle`] when
/// nothing needed the store, [`TickOutcome::Committed`] when the transaction ran
/// (even if it rolled back for lack of writes) and [`TickOutcome::Failed`] when
/// it failed: the scheduler has then returned to its baseline and waits.
///
/// After a commit that stored a snapshot the compatibility snapshots are
/// published, unchanged (section 8 item 1); a failure of that publication is the
/// collector's error, as before.
async fn commit_tick(
    commit: &TickCommit<'_>,
    identities: &mut IdentityCache,
    scheduler: &mut VehicleScheduler,
    batch: TickBatch,
    now: Instant,
) -> Result<TickOutcome, CollectorError> {
    // The test seam may hold the commit before anything is written.
    supervised_test_gate("tick-write").await;
    // A loss the supervisor or the renewal observed stays decisive when the admission
    // passes again (B2 fix round, P3b fix 1, R-P3bF-1): no transaction is begun, the
    // batch is dropped with the run, and the loop ends with the sensitive failure.
    scheduler.check_write("tick-commit")?;
    let mut tick = scheduler.take_tick();
    let other_writes = tick_needs_transaction(scheduler, &batch, &tick);
    // The request counters join the tick (C3 R5, R25): Fleet polling writes them in
    // every commit that has any change, the Owner-wire counts ride on a commit that
    // happens anyway or wait for their hourly flush.
    scheduler.attach_counters(&mut tick, other_writes, false);
    if !other_writes && tick.counters.is_empty() {
        // Nothing to write: the persisted views already match.
        return Ok(TickOutcome::Idle);
    }
    // The publication gate serialises the lineage and its publication with what
    // the collector stores for it. Rows of the collector's own tables touch
    // neither, so a tick of scheduler rows alone does not wait for it.
    // A fault of the store before the transaction (the gate, an identity's own
    // write) is a store fault like the transaction's (R6): the tick's transitions
    // are dropped, the scheduler waits, and the loop goes on.
    let publication_gate = if batch.discoveries.is_empty()
        && batch.snapshots.is_empty()
        && batch.hub_events.is_empty()
    {
        None
    } else {
        match commit.store.acquire_publication_gate().await {
            Ok(gate) => Some(gate),
            Err(error) => {
                log_commit_failure(&CollectorError::from(error));
                scheduler.commit_failed(tick, now);
                return Ok(TickOutcome::Failed);
            }
        }
    };
    // The gate may have been waited for: a loss the run met meanwhile is seen before
    // any identity or tick write (B2 fix round, P3b fix 2, Opus N1).
    scheduler.check_write("tick-after-gate")?;
    let now_ms = scheduler.wall_or_last();
    let mut registered: HashMap<VehicleId, Registered> = HashMap::new();
    for vehicle in batch_vehicles(&batch) {
        match identities.resolve_guarded(
            commit.store,
            commit.provider,
            &vehicle,
            now_ms,
            scheduler.write_guard(),
        ) {
            Ok(identity) => {
                registered.insert(vehicle.id, identity);
            }
            // A refused write is the run's loss, not a store fault of the tick.
            Err(error) if loses_sensitive_access(&error) => return Err(error),
            Err(error) => {
                log_commit_failure(&error);
                scheduler.commit_failed(tick, now);
                return Ok(TickOutcome::Failed);
            }
        }
    }
    let rows = tick.to_rows_write(now_ms);
    scheduler.check_write("tick-transaction")?;
    let result = commit.store.collector_write_transaction(
        |transaction| {
            let output = write_tick_in_transaction(
                commit.store,
                transaction,
                commit,
                scheduler,
                &registered,
                &batch,
                &rows,
            )?;
            let wrote = output.wrote;
            Ok::<_, CollectorError>((output, wrote))
        },
        supervised_tick_commit_hook,
    );
    let output = match result {
        Ok(output) => output,
        Err(error) => {
            log_commit_failure(&error);
            scheduler.commit_failed(tick, now);
            return Ok(TickOutcome::Failed);
        }
    };
    let wrote = output.wrote;
    #[cfg(test)]
    if wrote {
        supervised_tick_committed();
    }
    if wrote {
        scheduler.commit_succeeded(tick, now);
    } else {
        // A transaction that only read proves nothing about the store's ability to
        // write, so it does not end a store back-off (C3 R6); the tick was empty.
        scheduler.renew_baseline();
    }
    for (id, mut snapshot) in output.views_read {
        if output.current_only.contains(&id) {
            snapshot.current_discovery_state = scheduler
                .vehicles
                .get(&id)
                .map(|vehicle| vehicle.vehicle.state.clone());
        }
        scheduler.set_lifecycle_snapshot(id, &snapshot);
    }
    for (id, received_at_ms) in output.stored {
        scheduler.note_observation_stored(id, received_at_ms);
    }
    for id in output.events {
        scheduler.lifecycle_changed(id);
    }
    // A `no_data` event marks its car closed (C3 R40) only now that it is durable,
    // so a failed commit plans it again, and only for the candidates the transaction
    // appended: a rejected candidate sets no flag.
    for (id, decided_at_ms) in output.no_data_appended {
        scheduler.note_force_closed(id);
        #[cfg(test)]
        supervised_test_no_data_stored(decided_at_ms);
        #[cfg(not(test))]
        let _ = decided_at_ms;
    }
    let mut report = output.report;
    if let Some(report) = report.as_mut()
        && let Some(publication_gate) = publication_gate.as_ref()
    {
        let collection = ManualCollection {
            vehicles: scheduler.vehicles(),
            snapshots: batch
                .snapshots
                .iter()
                .map(|item| item.snapshot.clone())
                .collect(),
            failures: batch.failures.clone(),
        };
        report.snapshots_published = publish_compatibility_snapshots_for_provider_guarded(
            commit.store,
            publication_gate,
            commit.cursor_key,
            &collection,
            now_ms,
            commit.provider,
            scheduler.write_guard(),
        )?;
    }
    drop(publication_gate);
    Ok(TickOutcome::Committed { wrote, report })
}

/// One log line for a tick that could not be stored (C3 R6, R9): the class is
/// `store`, and the error text carries no vehicle, token or location.
fn log_commit_failure(error: &CollectorError) {
    tracing::warn!(
        target: "collector.decision",
        class = "store",
        error = %error,
        "collector tick commit failed; the tick's transitions are dropped"
    );
}

/// The tick-commit hook of the tests (section 8 item 9): in production a no-op,
/// in tests it injects a commit fault.
#[cfg(not(test))]
fn supervised_tick_commit_hook() -> Result<(), CollectorError> {
    Ok(())
}

#[cfg(test)]
fn supervised_tick_commit_hook() -> Result<(), CollectorError> {
    SUPERVISED_COLLECTOR_TEST_SEAM
        .try_with(|seam| seam.tick_commit_gate())
        .unwrap_or(Ok(()))
}

/// What the loop does with a result it has handled (C3 R5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadFlow {
    Continue,
    /// The credential unit signed collection out: stop sending the rest of the
    /// tick and restart the loop's iteration.
    SignedOut,
}

/// The state of one tick while its requests run (C3 R5): the scheduler, the
/// identities, the commit context and the results not yet stored.
struct TickRun<'a> {
    scheduler: &'a mut VehicleScheduler,
    identities: &'a mut IdentityCache,
    commit: TickCommit<'a>,
    batch: TickBatch,
    /// Woken when a snapshot was stored, so terrain enrichment can run.
    terrain_wake: &'a mpsc::Sender<()>,
    /// Some transaction of the tick committed something (C3 R11: an iteration
    /// that wrote is not idle).
    any_wrote: bool,
}

impl TickRun<'_> {
    /// Commit a raised reservation before the requests it covers are sent (C3 R27).
    /// With no result waiting the transaction is a lone reservation commit and is
    /// counted (R5, R13); with results waiting the raise rides on their transaction.
    /// A store failure is the scheduler's wait (R6): the requests are then withheld,
    /// because only a committed reservation may be relied on.
    async fn commit_reservation(&mut self, now: Instant) -> Result<TickOutcome, CollectorError> {
        let lone = self.batch.is_empty();
        let outcome = self.commit_now(now).await?;
        if lone && matches!(outcome, TickOutcome::Committed { wrote: true, .. }) {
            self.scheduler.note_reservation_commit();
            #[cfg(test)]
            supervised_test_lone_reservation_commit();
        }
        Ok(outcome)
    }

    /// Commit what the batch holds now and start a new batch (C3 R5). A store
    /// failure is not an error of the loop: the scheduler waits (R6).
    async fn commit_now(&mut self, now: Instant) -> Result<TickOutcome, CollectorError> {
        let batch = std::mem::take(&mut self.batch);
        let outcome =
            commit_tick(&self.commit, self.identities, self.scheduler, batch, now).await?;
        if let TickOutcome::Committed { wrote, report } = &outcome {
            self.any_wrote |= *wrote;
            if let Some(report) = report {
                #[cfg(test)]
                supervised_test_collection_finished(report).await;
                let _ = self.terrain_wake.try_send(());
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
        }
        Ok(outcome)
    }
}

/// Raise the reservation for the requests `plan` names and commit it before they
/// are sent (C3 R27). Nothing happens in the Owner-wire modes, when the persisted
/// reservation already covers the plan, or while the account may not send.
async fn reserve_for(
    run: &mut TickRun<'_>,
    plan: &[(RequestCategory, u64)],
) -> Result<(), CollectorError> {
    let now = run.scheduler.clock.mono_now();
    if run.scheduler.plan_reservation(plan, now) {
        let now = run.scheduler.clock.mono_now();
        run.commit_reservation(now).await?;
    }
    Ok(())
}

/// What a request of a tick is for (C3 R5): a targeted record check, a
/// `vehicle_data` read or the probe of a held car (R51, R52). The key names the car
/// the result belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TickKey {
    Check(VehicleId),
    Read(VehicleId),
    Probe(VehicleId, ProbeKind),
}

/// What a request of a tick returns.
#[derive(Debug)]
enum TickPayload {
    State(String),
    Data(VehicleData),
    /// An Owner-wire record probe's answer: whether the car is in service (R52).
    InService(bool),
}

/// What a request's log event needs (C3 R9): the clock the duration is read
/// through (R2), the car's Hub id (`account` for a list) and the mode, which the
/// classifier reads.
struct RequestLog {
    clock: Arc<dyn CollectorClock>,
    hub: String,
    mode: crate::config::CollectorMode,
}

impl RequestLog {
    fn for_vehicle(scheduler: &VehicleScheduler, id: VehicleId) -> Self {
        Self {
            clock: Arc::clone(&scheduler.clock),
            hub: scheduler.hub_label(id),
            mode: scheduler.mode,
        }
    }

    fn for_account(scheduler: &VehicleScheduler) -> Self {
        Self {
            clock: Arc::clone(&scheduler.clock),
            hub: "account".to_owned(),
            mode: scheduler.mode,
        }
    }

    /// One DEBUG event per scheduled request, at its completion (C3 R9, R5: every
    /// attempt is in the request log): the operation, the class the classifier
    /// gives the outcome (`ok` for a success), the numeric status, the duration
    /// in milliseconds on the scheduler's clock and the Hub id. Never the VIN, a
    /// Tesla id, a URL or path, a token or a body: the error is reduced to its
    /// class and status before anything is logged. Gate M-T1 reads `duration_ms`.
    fn emit<T>(
        &self,
        operation: &'static str,
        kind: RequestKind,
        started: Instant,
        result: &Result<T, CollectorError>,
    ) {
        let duration_ms = u64::try_from(
            self.clock
                .mono_now()
                .saturating_duration_since(started)
                .as_millis(),
        )
        .unwrap_or(u64::MAX);
        let (class, status) = match result {
            Ok(_) => ("ok".to_owned(), Some(200)),
            // A resend the reservation did not cover (R27, R47): the first request
            // went out and was answered 401, and the resend was not sent.
            Err(CollectorError::FleetResendWithheld) => ("resend_withheld".to_owned(), Some(401)),
            Err(error) => (
                format!("{:?}", classify(error, kind, self.mode)),
                http_status_of(error),
            ),
        };
        tracing::debug!(
            target: "collector.request",
            vehicle = %self.hub,
            operation,
            class = %class,
            status,
            duration_ms,
            "collector request"
        );
    }
}

/// Run one request of a tick and log it (C3 R9). `operation` is `record`, `read`
/// or `probe`; the request's own future returns its key and result.
async fn logged_request<'a>(
    log: RequestLog,
    request: impl Future<Output = (TickKey, Result<TickPayload, CollectorError>)> + Send + 'a,
) -> (TickKey, Result<TickPayload, CollectorError>) {
    let started = log.clock.mono_now();
    let (key, result) = request.await;
    let (operation, kind) = match key {
        TickKey::Check(_) => ("record", RequestKind::Record),
        TickKey::Read(_) => ("read", RequestKind::Read),
        TickKey::Probe(..) => ("probe", RequestKind::Probe),
    };
    // A read the stream's power gate refused was never sent: no request, no log.
    if !is_stream_power_not_confirmed_result(&result) {
        log.emit(operation, kind, started, &result);
    }
    (key, result)
}

/// Whether a request's result is the power gate's refusal, which means nothing
/// was sent (C3 R36).
fn is_stream_power_not_confirmed_result<T>(result: &Result<T, CollectorError>) -> bool {
    result.as_ref().is_err_and(is_stream_power_not_confirmed)
}

/// One request of a tick, ready to run.
type TickFuture<'a> =
    futures_util::future::BoxFuture<'a, (TickKey, Result<TickPayload, CollectorError>)>;

/// Run a tick's requests concurrently and commit under the bound of C3 R5. Each
/// request resolves to a key and a result; `handle` applies one result to the
/// scheduler and the batch (the caller's code: its effects on streams and state
/// are its own) and returns whether to go on. The batch commits when the last
/// request has completed or when its oldest result is `K_TICK_COMMIT_AGE` old,
/// and the results of requests still in flight then go into a further
/// transaction of the same tick. With no requests the batch commits at once, so
/// a list's result and the scheduler's own changes are stored.
async fn drive_requests<Handle>(
    run: &mut TickRun<'_>,
    requests: Vec<TickFuture<'_>>,
    mut pump: Option<StreamPump<'_>>,
    mut handle: Handle,
) -> Result<ReadFlow, CollectorError>
where
    Handle: FnMut(
        &mut TickRun<'_>,
        TickKey,
        Result<TickPayload, CollectorError>,
        Instant,
    ) -> Result<ReadFlow, CollectorError>,
{
    use futures_util::StreamExt;
    let mut pending: futures_util::stream::FuturesUnordered<TickFuture<'_>> =
        requests.into_iter().collect();
    // C3 R11, C4 R56 g: the stream queues are drained at least every
    // `K_STREAM_DRAIN_CAP` while requests are pending, so a slow request or an
    // authentication wait cannot fill a queue and end the stream. The interval is
    // measured from the previous drain, whatever the loop did before this tick, and
    // an expired deadline is serviced before any ready request (B2 clarification 3).
    let pump_armed = pump.as_ref().is_some_and(StreamPump::armed);
    while !pending.is_empty() {
        let deadline = run.batch.commit_deadline();
        let drain_at = pump.as_ref().map_or_else(Instant::now, StreamPump::deadline);
        tokio::select! {
            biased;
            () = async move {
                if pump_armed {
                    tokio::time::sleep_until(drain_at).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                if let Some(pump) = pump.as_mut() {
                    pump.drain(run.scheduler).await?;
                    if pump.backlog() {
                        tokio::task::yield_now().await;
                    }
                }
            }
            Some((key, result)) = pending.next() => {
                // C3 R2: time is read through the scheduler's clock seam.
                let now = run.scheduler.clock.mono_now();
                run.batch.note_result(now);
                match handle(run, key, result, now) {
                    Ok(ReadFlow::Continue) => {}
                    Ok(ReadFlow::SignedOut) => {
                        // The rest of the tick's requests are not sent (R47), but
                        // what the tick already received is stored first, with the
                        // account's stop and its gap (R5, R8). The requests still
                        // pending are dropped, which settles what they counted
                        // (a request the credential unit never sent counted nothing;
                        // a Fleet request's guard, taken when it was built, takes
                        // its start back though it was never polled), and no car
                        // stays marked in flight: it would otherwise be hidden from
                        // every later tick, after the stop is lifted too.
                        run.commit_now(now).await?;
                        drop(pending);
                        run.scheduler.release_in_flight();
                        return Ok(ReadFlow::SignedOut);
                    }
                    Err(error) => {
                        // A result that ends the run must not hide the error that
                        // ended it. The loss of the process's sensitive access (R47,
                        // B2 clarification) writes nothing: the admission that
                        // guards the store is what failed. Any other error that ends
                        // a run keeps the snapshots already received in this tick:
                        // they are stored, best effort, and the error that ended
                        // collection is reported, not a store fault met on the way
                        // out.
                        if !loses_sensitive_access(&error) {
                            let _ = run.commit_now(now).await;
                        }
                        return Err(error);
                    }
                }
            }
            () = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                let now = run.scheduler.clock.mono_now();
                run.commit_now(now).await?;
            }
        }
    }
    let now = run.scheduler.clock.mono_now();
    run.commit_now(now).await?;
    Ok(ReadFlow::Continue)
}

// ---------------------------------------------------------------------------
// Start path (C3 R4, R7, R8) and status (C3 R13).
// ---------------------------------------------------------------------------

/// The configuration fingerprint (C3 R4): lowercase hex SHA-256 over the mode,
/// the normalised origin of the configured Owner API base, the stream endpoint
/// and the credential identity, each prefixed by its length so no two inputs
/// share a digest. A provider URL may carry a key (C4 R52), so only the digest is
/// stored. A change of any input resets a stored refusal at the next start (R7).
fn collector_config_fingerprint(
    mode: crate::config::CollectorMode,
    origin: &str,
    stream_endpoint: &str,
    credential_identity: &str,
) -> String {
    let mut hasher = Sha256::new();
    for field in [mode.code(), origin, stream_endpoint, credential_identity] {
        hasher.update(u64::try_from(field.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// What the start path of a supervised run reads and commits before anything
/// else starts (C3 R7 step 1, R8): the mode, the fingerprint, the account reset
/// and the start gaps, then the one store read the scheduler loads from.
struct CollectorStart {
    mode: crate::config::CollectorMode,
    snapshot: CollectorStateSnapshot,
    /// C3 reset a stored account state for a mode or fingerprint change (R7): the
    /// start log reads it from the store's outcome, and the tests from here. It is
    /// not the refusal decision.
    #[cfg(test)]
    account_reset: bool,
    /// The one refusal decision of the start transaction (C3 R7, B2
    /// clarification 3): the durable C5 refusal was lifted with it, and the
    /// credential unit's view clears its own refusal by it.
    refusal_reset: bool,
    /// `(mode, state, reason_code)` of the account row as the reset left it.
    account_row: Option<(String, String, String)>,
}

impl CollectorStart {
    /// What the credential unit is told about this start (C5 R57 step 6).
    fn credential_view(&self) -> CollectorStartView<'_> {
        CollectorStartView {
            reset_refusal: self.refusal_reset,
            account: self
                .account_row
                .as_ref()
                .map(|(mode, state, reason)| (mode.as_str(), state.as_str(), reason.as_str())),
        }
    }
}

/// Commit the start of a collector run and read its state (C3 R7, R8). The
/// previous run's lease heartbeat must be read before this run's lease replaces
/// it, so the caller passes it in. The reset and the refusal lift commit here, in
/// one transaction. The credential unit may already be built; it applies this
/// outcome after the commit and before its first request, and binds an unbound
/// pair's generation before the fingerprint is read (C3 R7, B2 clarification 3).
fn start_collector_run(
    store: &HubStore,
    config: &HubConfig,
    cadence: &CollectorCadence,
    stream_endpoint: &str,
    previous_heartbeat_ms: Option<i64>,
) -> Result<CollectorStart, CollectorError> {
    let mode = crate::config::collector_mode(&config.collector);
    let fleet = mode == crate::config::CollectorMode::FleetPolling;
    let fingerprint = collector_config_fingerprint(
        mode,
        &crate::config::owner_api_origin_text(&config.collector),
        if fleet { "" } else { stream_endpoint },
        &store.collector_credential_identity(fleet)?,
    );
    let alert_ms = i64::try_from(cadence.offline_drive_timeout.as_millis()).unwrap_or(i64::MAX);
    let started = store.begin_collector_run(&crate::db::CollectorRunStartRequest {
        mode: mode.code(),
        config_fingerprint: &fingerprint,
        start_ms: current_epoch_millis()?,
        previous_heartbeat_ms,
        gap_min_ms: K_GAP_MIN_MS,
        gap_alert_ms: alert_ms,
    })?;
    if started.account_reset
        || started.refusal_reset
        || started.hub_down_inserted
        || started.stopped_gap_closed
    {
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            reason = if started.account_reset || started.refusal_reset {
                "config_changed"
            } else {
                "gap_closed"
            },
            account_reset = started.account_reset,
            refusal_reset = started.refusal_reset,
            holds_cleared = started.holds_cleared,
            hub_down = started.hub_down_inserted,
            stopped_gap_closed = started.stopped_gap_closed,
            "collector start"
        );
    }
    supervised_test_after_start_commit()?;
    // Unapplied owner actions (R7 step 5) are read at each settings refresh by
    // the loops, so the start load leaves them in the store.
    let configured = store.configured_tesla_vehicles()?;
    let rows = store.load_collector_state_rows()?;
    let account_row = rows.account.as_ref().map(|account| {
        (
            account.mode.clone(),
            account.state.clone(),
            account.reason_code.clone(),
        )
    });
    Ok(CollectorStart {
        mode,
        snapshot: CollectorStateSnapshot::from_rows(rows, fingerprint, &configured),
        #[cfg(test)]
        account_reset: started.account_reset,
        refusal_reset: started.refusal_reset,
        account_row,
    })
}

/// The in-process status publication (C3 R13): the latest `CollectorStatus` of
/// the running collector, for in-process readers. Other processes read the R4
/// tables (R66). The collector holds the process's one lease, so one value
/// suffices.
static COLLECTOR_STATUS: std::sync::OnceLock<watch::Sender<Option<CollectorStatus>>> =
    std::sync::OnceLock::new();

fn collector_status_sender() -> &'static watch::Sender<Option<CollectorStatus>> {
    COLLECTOR_STATUS.get_or_init(|| watch::channel(None).0)
}

/// Publish the status (C3 R13).
fn publish_collector_status(status: CollectorStatus) {
    collector_status_sender().send_replace(Some(status));
}

/// A reader of the published status (C3 R13).
#[cfg(test)]
fn collector_status_receiver() -> watch::Receiver<Option<CollectorStatus>> {
    collector_status_sender().subscribe()
}

/// Whether the run's last transaction was recorded (C3 R8, B2 clarification 3).
/// Only a recorded stop lets the lease be released: a skipped or failed one leaves
/// no `hub_stopped` gap, so the lease must expire on its own and the next admitted
/// start reads its last heartbeat and records the downtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopRecord {
    Recorded,
    Unrecorded,
}

/// The last commit of a run (C3 R8): the scheduler's remaining rows and the
/// `hub_stopped` gap, in one transaction. A fault here is logged and does not
/// change how the run ends: the stop is [`StopRecord::Unrecorded`], and the next
/// start then finds no open gap and falls back to the heartbeat rule.
fn finish_collector_run(store: &HubStore, scheduler: &mut VehicleScheduler) -> StopRecord {
    #[cfg(test)]
    supervised_test_finish_entered();
    let mut tick = scheduler.take_tick();
    // The counters are flushed at a graceful stop whatever the mode (C3 R25).
    scheduler.attach_counters(&mut tick, true, true);
    let now_ms = scheduler.wall_or_last();
    let rows = tick.to_rows_write(now_ms);
    match store.end_collector_run(Some(&rows), now_ms) {
        Ok(()) => {
            scheduler.commit_succeeded(tick, scheduler.clock.mono_now());
            StopRecord::Recorded
        }
        Err(error) => {
            tracing::warn!(
                target: "collector.decision",
                class = "store",
                error = %error,
                "collector stop could not be recorded"
            );
            StopRecord::Unrecorded
        }
    }
}

/// The shared, irreversible half of [`AdmissionLoss`]: the renewal of the lease
/// consults it before every write, so a loss the supervisor observed fences the
/// renewal even when the renewal's own revalidation would pass again (a lock file
/// put back, a path restored). Cloning shares it; once set it is never cleared.
#[derive(Clone, Debug)]
struct AdmissionFence(Arc<watch::Sender<bool>>);

impl Default for AdmissionFence {
    fn default() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }
}

impl AdmissionFence {
    fn set(&self) {
        self.0.send_replace(true);
    }

    fn is_set(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once the fence is set (at once when it already is). The final stream
    /// stop watches it (B2 fix round, P2, third pass), so a loss the renewal met ends
    /// the stop even when the retained admission passes again.
    async fn wait_set(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|set| *set).await;
    }
}

/// The run's one synchronous write guard (B2 fix round, P3b fix 2, R-P3bF2-1): the
/// shared irreversible [`AdmissionFence`] together with the retained admission. It is
/// consulted immediately before every independent store mutation (or new
/// transaction) a supervised Owner or Fleet run can reach, with no await between the
/// check and the mutation. It refuses when the fence is set, whatever the admission
/// says now (a lock file put back, a path restored), and otherwise revalidates the
/// admission and, when that fails, SETS the fence, so the loss is irreversible from
/// the first boundary that met it. An already-started transaction may finish: the
/// check is a boundary protection and not atomic exclusion (C5 R23). Exempt are only
/// the lease renewal and release semantics that the work order and the specs fix
/// (C3 R8, R47; C5 R33a).
#[derive(Clone, Debug)]
struct RunWriteGuard {
    fence: AdmissionFence,
    admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
    /// Test only: which boundaries consulted the guard and what they were told.
    #[cfg(test)]
    audit: Arc<WriteGuardAudit>,
}

/// A guard that knows only the fence, for the tests of the final stop's frame writes.
/// It exists only in test builds (B2 round 3, X-dataloss N2): a production run has
/// only the guard `for_run` makes, which always carries the retained admission, so a
/// later caller cannot bring back a boundary that checks the fence and never the
/// admission.
#[cfg(test)]
impl From<AdmissionFence> for RunWriteGuard {
    fn from(fence: AdmissionFence) -> Self {
        Self::new(fence, None)
    }
}

impl RunWriteGuard {
    fn new(
        fence: AdmissionFence,
        admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>,
    ) -> Self {
        Self {
            fence,
            admission,
            #[cfg(test)]
            audit: Arc::default(),
        }
    }

    /// The one guard of a supervised run (B2 fix round, P3b fix 3, R-P3bF3-1): made by
    /// the entry point before the credential unit is built or the cursor key is
    /// created, then handed down to the unit's constructor, the terrain cache and the
    /// supervisor. The test seam learns of its fence and its audit here, once.
    fn for_run(admission: Option<Arc<crate::hub_user_process::AdmittedUserHub>>) -> Self {
        let guard = Self::new(AdmissionFence::default(), admission);
        supervised_test_register_fence(&guard.fence);
        supervised_test_register_guard(&guard);
        guard
    }

    #[cfg(test)]
    fn fence(&self) -> &AdmissionFence {
        &self.fence
    }

    /// The core: whether the write at `point` is refused. It is the only place where
    /// a boundary revalidates the admission and sets the fence.
    fn refused(&self, #[cfg_attr(not(test), allow(unused_variables))] point: &'static str) -> bool {
        // The test seam sets the fence at a chosen boundary, exactly when the run
        // reaches it, and records what the store held then; nothing in production.
        #[cfg(test)]
        self.audit.trip(point, &self.fence);
        let refused = self.fence.is_set()
            || assert_runtime_sensitive_access(self.admission.as_deref()).is_err();
        if refused {
            self.fence.set();
        }
        #[cfg(test)]
        self.audit.note(point, refused);
        refused
    }

    /// [`Self::refused`] as the result of the collector's own steps.
    fn check(&self, point: &'static str) -> Result<(), CollectorError> {
        if self.refused(point) {
            Err(CollectorError::SensitiveAccessUnavailable)
        } else {
            Ok(())
        }
    }

    /// The guard as the other units see it (the credential managers, the stream audit):
    /// `true` once a write is refused.
    fn observer(&self, point: &'static str) -> crate::credentials::LossObserver {
        let guard = self.clone();
        Arc::new(move || guard.refused(point))
    }

    /// Test only: the boundaries this guard has been consulted at.
    #[cfg(test)]
    fn audit(&self) -> &WriteGuardAudit {
        &self.audit
    }
}

impl crate::geocoder::EgressGuard for RunWriteGuard {
    /// The geocoder consults it before it sends and again before it caches the
    /// answer: both are refused once the run's writes are.
    fn assert_egress_allowed(&self) -> Result<(), crate::geocoder::EgressGuardError> {
        self.check("geocoder")
            .map_err(|_| crate::geocoder::EgressGuardError)
    }
}

/// The run's guard as the terrain cache sees it (B2 fix round, P3b fix 4, N3): every
/// permit the cache takes (its construction, the lock rechecks, each chunk, sync, staging,
/// rename, eviction and removal) is recorded at the audit point "terrain-cache", so the
/// audit tells a cache refusal from the geocoder's, and a test that arms the geocoder
/// does not trip at the cache.
struct TerrainCacheGuard<'a>(&'a RunWriteGuard);

impl crate::geocoder::EgressGuard for TerrainCacheGuard<'_> {
    fn assert_egress_allowed(&self) -> Result<(), crate::geocoder::EgressGuardError> {
        self.0
            .check("terrain-cache")
            .map_err(|_| crate::geocoder::EgressGuardError)
    }
}

/// `check` of an optional guard: a caller with no guard (a setup command, a one-shot
/// collection) is not a supervised run and is not fenced.
fn check_optional_write(
    guard: Option<&RunWriteGuard>,
    point: &'static str,
) -> Result<(), CollectorError> {
    guard.map_or(Ok(()), |guard| guard.check(point))
}

/// What the guards of a test were consulted for, per boundary.
#[cfg(test)]
#[derive(Default)]
struct WriteGuardAudit {
    passed: std::sync::Mutex<Vec<&'static str>>,
    refused: std::sync::Mutex<Vec<&'static str>>,
    /// The boundary at which the fence is set when the run reaches it (the `n`th time),
    /// and what is done at that very moment (the test reads the store there).
    trip: std::sync::Mutex<Option<(&'static str, usize, Box<dyn Fn() + Send + Sync>)>>,
    reached: AtomicUsize,
    tripped: AtomicUsize,
}

#[cfg(test)]
impl std::fmt::Debug for WriteGuardAudit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("WriteGuardAudit").finish_non_exhaustive()
    }
}

#[cfg(test)]
impl WriteGuardAudit {
    /// Set the fence when `point` is reached the `nth` time.
    fn trip_at_nth(
        &self,
        point: &'static str,
        nth: usize,
        action: impl Fn() + Send + Sync + 'static,
    ) {
        *self
            .trip
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((point, nth, Box::new(action)));
    }

    /// Whether the boundary the test armed was reached.
    fn tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst) > 0
    }

    fn trip(&self, point: &'static str, fence: &AdmissionFence) {
        let guard = self
            .trip
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((armed, nth, action)) = guard.as_ref()
            && *armed == point
            && self.reached.fetch_add(1, Ordering::SeqCst) + 1 == *nth
        {
            // The fence is set first, so that the action - the stamp a test takes of the
            // store - is read with every other thread's boundary already refused; and the
            // trip counts as reached only once the action is over, so a test that waits
            // for it holds a finished stamp (B2 fix round, P3b fix 5, R-P3bF5-4).
            fence.set();
            action();
            self.tripped.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn note(&self, point: &'static str, refused: bool) {
        let list = if refused { &self.refused } else { &self.passed };
        list.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(point);
    }

    /// How often `point` let a write through.
    fn passed(&self, point: &str) -> usize {
        self.passed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|seen| **seen == point)
            .count()
    }

    /// How often `point` refused a write.
    fn refused(&self, point: &str) -> usize {
        self.refused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|seen| **seen == point)
            .count()
    }

    /// How many writes of any boundary were let through.
    fn total_passed(&self) -> usize {
        self.passed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

/// Revalidate before the run's own loop writes (B2 fix round, P3b fix 1, R-P3bF-1):
/// the shared fence first, so a loss that only the renewal or the supervisor
/// observed stays decisive when the admission passes again, then the retained
/// admission itself (fix 2: the one guard core, which sets the fence when the
/// admission fails).
///
/// `point` names the boundary for the test seam, which may hold the loop there.
async fn assert_run_may_write(
    point: &'static str,
    guard: &RunWriteGuard,
) -> Result<(), CollectorError> {
    supervised_test_gate(point).await;
    guard.check(point)
}

/// The monotonic fact that this process lost the retained admission that guards
/// the store (C3 R47, R8, B2 clarification 3). It is set by any observation, the
/// run's own result, an outcome of the credential unit's shutdown or the check at a
/// write boundary, and it is never cleared: an earlier success or a later error
/// cannot erase it. After it, the process writes nothing more, its renewal is fenced
/// (through the shared [`AdmissionFence`]) and joined, and its lease is retained to
/// expire.
#[derive(Debug, Default)]
struct AdmissionLoss {
    observed: bool,
    fence: AdmissionFence,
}

impl AdmissionLoss {
    /// A loss fact whose fence the run's renewal already holds.
    fn with_fence(fence: AdmissionFence) -> Self {
        Self {
            observed: false,
            fence,
        }
    }

    /// Whether a loss is known: this fact's own, or the shared fence the run's
    /// renewal set on its own denial (B2 fix round, P2: a loss the renewal met stays
    /// decisive for the supervisor even when the admission passes again).
    fn observed(&self) -> bool {
        self.observed || self.fence.is_set()
    }

    fn note(&mut self) {
        self.observed = true;
        self.fence.set();
    }

    /// Fold in the run's result, or any outcome that came after it.
    fn note_result(&mut self, result: &Result<(), CollectorError>) {
        if result.as_ref().is_err_and(loses_sensitive_access) {
            self.note();
        }
    }

    /// Revalidate the retained admission at a write boundary.
    fn check(&mut self, admission: Option<&crate::hub_user_process::AdmittedUserHub>) -> bool {
        if self.fence.is_set() || assert_runtime_sensitive_access(admission).is_err() {
            self.note();
        }
        self.observed()
    }

    /// Merge an outcome of the shutdown into the run's result: a sensitive loss is
    /// kept, the first one observed wins, and it replaces an earlier success or
    /// ordinary error; any other error replaces only a success. The bare denial of
    /// the admission (what the renewal or a write boundary reports) says less than
    /// the credential unit's own sensitive outcome, so the unit's outcome replaces
    /// it: the run ends with what the unit found (C5 R33a, B2 clarification 3).
    fn merge(&mut self, result: &mut Result<(), CollectorError>, outcome: Result<(), CollectorError>) {
        let Err(error) = outcome else {
            return;
        };
        if loses_sensitive_access(&error) {
            self.note();
            let bare = |error: &CollectorError| matches!(error, CollectorError::SensitiveAccessUnavailable);
            let replaces = match result {
                Ok(()) => true,
                Err(held) if !loses_sensitive_access(held) => true,
                Err(held) => bare(held) && !bare(&error),
            };
            if replaces {
                *result = Err(error);
            }
        } else if result.is_ok() {
            *result = Err(error);
        }
    }
}
