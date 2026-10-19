// SPDX-License-Identifier: AGPL-3.0-only

// The self-hosted telemetry receiver's intake (G2-6 6A S5). One POST follows
// this order: the envelope is parsed and validated whole; an alert or error is
// turned into its stored event and size-checked, all before any clock read, store
// access or identity resolution; the Hub wall clock is read once as the POST's
// admission time; the configured vehicle is resolved; its store identity comes
// from the receiver's own cache, filled by a read-only lookup (an unchanged
// identity is never registered, including on the first POST after a restart);
// then either the event is appended as one raw observation, or the accumulator
// is applied to a clone and the snapshot, the lifecycle and the complete
// accumulator state commit in one write transaction. Only after that commit are
// the candidate and its bytes installed and the pending derived work published,
// with the identity already resolved.

/// The `provider_feed` marker of a stored receiver snapshot (C1 R76).
const RECEIVER_PROVIDER_FEED: &str = "fleet_telemetry";

/// Why one POST was not accepted. The route maps each to a status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiverRejection {
    /// The envelope, or the record stored from it, is invalid: 400.
    Invalid,
    /// No enabled configured vehicle has this VIN, or more than one has: 422.
    UnknownVehicle,
    /// The Hub could not commit or publish; the receiver retries: 503.
    Unavailable,
}

/// A vehicle's accumulator beside the exact state bytes last written for it.
struct ReceiverVehicleState {
    accumulator: FleetTelemetryAccumulator,
    /// The complete state last committed, empty when none is stored (or the
    /// stored one was unusable), so the first commit writes the state.
    last_persisted: Vec<u8>,
}

/// A store identity with the descriptor facts it was resolved under, keyed by the
/// Hub vehicle UUID: a changed EID or VIN resolves again.
struct CachedReceiverIdentity {
    eid: i64,
    vin: String,
    registered: Registered,
}

#[derive(Default)]
struct ReceiverInner {
    vehicles: HashMap<Uuid, ReceiverVehicleState>,
    identities: HashMap<Uuid, CachedReceiverIdentity>,
}

/// Test-only seams of the receiver (G2-6 6A S5): an injected admission clock with
/// a read counter, and a one-shot fault just before the intake commit.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct ReceiverTestHooks {
    clock: std::sync::Mutex<Option<Arc<dyn Fn() -> i64 + Send + Sync>>>,
    clock_reads: AtomicUsize,
    commit_fault: AtomicBool,
}

#[cfg(test)]
impl ReceiverTestHooks {
    /// Replace the wall clock with `clock` for later POSTs.
    pub(crate) fn set_clock(&self, clock: impl Fn() -> i64 + Send + Sync + 'static) {
        *self.clock.lock().expect("receiver clock hook") = Some(Arc::new(clock));
    }

    /// How many times the admission clock has been read.
    pub(crate) fn clock_reads(&self) -> usize {
        self.clock_reads.load(Ordering::SeqCst)
    }

    /// Make the next intake transaction fail just before its commit.
    pub(crate) fn fail_next_commit(&self) {
        self.commit_fault.store(true, Ordering::SeqCst);
    }
}

/// The receiver's process state: the ingress lock over the accumulators and the
/// identity cache, and the configured `T_gap`.
pub(crate) struct FleetReceiver {
    offline_drive_timeout: Duration,
    inner: tokio::sync::Mutex<ReceiverInner>,
    #[cfg(test)]
    hooks: ReceiverTestHooks,
}

impl FleetReceiver {
    pub(crate) fn new(offline_drive_timeout: Duration) -> Self {
        Self {
            offline_drive_timeout,
            inner: tokio::sync::Mutex::new(ReceiverInner::default()),
            #[cfg(test)]
            hooks: ReceiverTestHooks::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn hooks(&self) -> &ReceiverTestHooks {
        &self.hooks
    }

    /// The POST's one admission time, epoch milliseconds, on the Hub clock.
    fn admission_time_ms(&self) -> Result<i64, CollectorError> {
        #[cfg(test)]
        {
            self.hooks.clock_reads.fetch_add(1, Ordering::SeqCst);
            let injected = self.hooks.clock.lock().expect("receiver clock hook").clone();
            if let Some(clock) = injected {
                return Ok(clock());
            }
        }
        current_epoch_millis()
    }

    #[cfg(not(test))]
    fn commit_hook(&self) -> Result<(), CollectorError> {
        Ok(())
    }

    #[cfg(test)]
    fn commit_hook(&self) -> Result<(), CollectorError> {
        if self.hooks.commit_fault.swap(false, Ordering::SeqCst) {
            return Err(StoreError::InjectedStreamFault("receiver-commit").into());
        }
        Ok(())
    }

    /// Accept one POST body. `Ok` is permission to acknowledge the record.
    pub(crate) async fn ingest(
        &self,
        store: &HubStore,
        cursor_key: &CursorKey,
        body: &[u8],
    ) -> Result<(), ReceiverRejection> {
        // 1. The whole envelope validates first; nothing below runs otherwise.
        let envelope = parse_envelope(body).map_err(|_| ReceiverRejection::Invalid)?;
        // 2. An event is built and size-checked here, still before any clock,
        // store or identity access.
        let event = match envelope.kind {
            ReceiverKind::Event => Some(event_record(&envelope)?),
            ReceiverKind::Vehicle | ReceiverKind::Connectivity => None,
        };
        // 3. The admission time, read once.
        let admitted_at_ms = self.admission_time_ms().map_err(|error| {
            tracing::error!(target: "collector.receiver", error = %error, "receiver clock failed");
            ReceiverRejection::Unavailable
        })?;
        // 4. The configured enabled vehicle of this VIN.
        let (vehicle_id, eid, settings) = match configured_fleet_vehicle_for_vin(store, &envelope.vin)
        {
            Ok(configured) => configured,
            Err(CollectorError::SelectedVehicleMissing) => {
                return Err(ReceiverRejection::UnknownVehicle);
            }
            Err(error) => {
                tracing::error!(
                    target: "collector.receiver",
                    error = %error,
                    "receiver vehicle lookup failed"
                );
                return Err(ReceiverRejection::Unavailable);
            }
        };
        let source_vehicle_id =
            VehicleId::try_from_i64(eid).ok_or(ReceiverRejection::UnknownVehicle)?;

        // Serialize resolution, apply and commit per process.
        let mut inner = self.inner.lock().await;
        match event {
            Some(record) => {
                let identity = self
                    .resolve_identity(
                        &mut inner,
                        store,
                        vehicle_id,
                        source_vehicle_id,
                        &envelope.vin,
                        admitted_at_ms,
                    )
                    .map_err(|error| unavailable("receiver identity", &error))?;
                self.store_event(
                    store,
                    &identity,
                    record,
                    envelope.timestamp_ms,
                    admitted_at_ms,
                )
            }
            None => {
                self.apply_record(
                    &mut inner,
                    store,
                    cursor_key,
                    envelope,
                    vehicle_id,
                    source_vehicle_id,
                    settings,
                    admitted_at_ms,
                )
                .await
            }
        }
    }

    /// The store identity of the configured vehicle: from the cache when its
    /// descriptor is unchanged, otherwise from a read-only lookup; only an absent
    /// or changed identity is registered, once.
    fn resolve_identity(
        &self,
        inner: &mut ReceiverInner,
        store: &HubStore,
        vehicle_id: Uuid,
        source_vehicle_id: VehicleId,
        vin: &str,
        now_ms: i64,
    ) -> Result<Registered, CollectorError> {
        let eid = source_vehicle_id.get() as i64;
        if let Some(cached) = inner.identities.get(&vehicle_id)
            && cached.eid == eid
            && cached.vin == vin
        {
            return Ok(cached.registered.clone());
        }
        let source = provider_source(CollectorProvider::Fleet);
        let mut descriptor = VehicleDescriptor::new(Uuid::nil(), source_vehicle_id.get().to_string())
            .with_tesla_identity(Some(eid), None);
        descriptor.vin = clean_optional_text(Some(vin));
        let registered = match store.registered_vehicle_unchanged(&source, &descriptor)? {
            Some(unchanged) => Registered {
                hub_id: unchanged.vehicle_id,
                source_id: unchanged.source_id,
                pack_car_id: projection_car_id_for_vehicle(
                    store,
                    unchanged.vehicle_id,
                    source_vehicle_id.get(),
                )?,
                generation: unchanged.source_generation,
            },
            None => {
                let source = store.register_source(&source, now_ms)?;
                descriptor.source_id = source.source_id;
                let record = store.register_vehicle(&descriptor, now_ms)?;
                Registered {
                    hub_id: record.vehicle_id,
                    source_id: source.source_id,
                    pack_car_id: projection_car_id_for_vehicle(
                        store,
                        record.vehicle_id,
                        source_vehicle_id.get(),
                    )?,
                    generation: source.generation,
                }
            }
        };
        if registered.hub_id != vehicle_id {
            return Err(StoreError::VehicleIdentityConflict.into());
        }
        inner.identities.insert(
            vehicle_id,
            CachedReceiverIdentity {
                eid,
                vin: vin.to_owned(),
                registered: registered.clone(),
            },
        );
        Ok(registered)
    }

    /// Append one alert or error as a raw observation: the event time is its
    /// observation time, the admission time its receipt time. No current row, no
    /// accumulator, no lifecycle.
    fn store_event(
        &self,
        store: &HubStore,
        identity: &Registered,
        record: Value,
        event_ms: i64,
        admitted_at_ms: i64,
    ) -> Result<(), ReceiverRejection> {
        let input = ObservationInput {
            source_id: identity.source_id,
            vehicle_id: identity.hub_id,
            observed_at_ms: event_ms,
            payload: record,
        };
        store
            .collector_write_transaction(
                |transaction| {
                    // A replay inserts nothing; the transaction still commits once,
                    // so every accepted POST is exactly one successful intake.
                    HubStore::append_event_observation_in_transaction(
                        transaction,
                        &input,
                        admitted_at_ms,
                    )?;
                    Ok::<_, CollectorError>(((), true))
                },
                || self.commit_hook(),
            )
            .map_err(|error| unavailable("receiver event commit", &error))
    }

    /// Apply a vehicle or connectivity record: the snapshot, the lifecycle and the
    /// complete state commit in one transaction; the candidate is installed only
    /// after it; then the pending derived work is published.
    #[allow(clippy::too_many_arguments)]
    async fn apply_record(
        &self,
        inner: &mut ReceiverInner,
        store: &HubStore,
        cursor_key: &CursorKey,
        envelope: ReceiverEnvelope,
        vehicle_id: Uuid,
        source_vehicle_id: VehicleId,
        settings: crate::hub_pack::ProjectionCarSettings,
        admitted_at_ms: i64,
    ) -> Result<(), ReceiverRejection> {
        let vin = envelope.vin.clone();
        let stream_id = StreamVehicleId::try_from_i64(source_vehicle_id.get() as i64)
            .ok_or(ReceiverRejection::UnknownVehicle)?;
        let publication_gate = store
            .acquire_publication_gate()
            .await
            .map_err(|error| unavailable("receiver publication gate", &CollectorError::from(error)))?;
        let identity = self
            .resolve_identity(
                inner,
                store,
                vehicle_id,
                source_vehicle_id,
                &vin,
                admitted_at_ms,
            )
            .map_err(|error| unavailable("receiver identity", &error))?;

        // First use of this vehicle in the process: its stored state, else the
        // current seed, else empty. The loaded state is installed at once, so an
        // unusable one is reported once even if the first commit fails.
        if let std::collections::hash_map::Entry::Vacant(slot) = inner.vehicles.entry(vehicle_id) {
            let loaded = load_receiver_vehicle_state(store, vehicle_id, &vin)
                .map_err(|error| unavailable("receiver state load", &error))?;
            slot.insert(loaded);
        }
        let current = inner
            .vehicles
            .get(&vehicle_id)
            .ok_or(ReceiverRejection::Unavailable)?;
        let mut candidate = current.accumulator.clone();
        let snapshot = candidate
            .apply_envelope(envelope)
            .map_err(|_| ReceiverRejection::Invalid)?;
        let encoded = candidate
            .encode_complete()
            .map_err(|_| ReceiverRejection::Invalid)?;
        let write_state = encoded != current.last_persisted;

        let vehicle = Vehicle {
            id: source_vehicle_id,
            stream_id,
            vin: vin.clone(),
            state: snapshot
                .owner_data
                .get("state")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty() && value.len() <= 64)
                .unwrap_or("unknown")
                .to_owned(),
            display_name: None,
            settings,
        };
        // Fleet Telemetry arrives from the vehicle fleet over the receiver, not as
        // the answer to a bearer request, and no credential is in scope here: the
        // scrub gets an empty secret set and is a no-op.
        let data = VehicleData::from_provider_raw_json(
            source_vehicle_id,
            serde_json::json!({"response": snapshot.owner_data}),
            &ProviderSecrets::none(),
        )
        .map_err(|error| unavailable("receiver snapshot", &CollectorError::from(error)))?;
        let item = SnapshotItem {
            vehicle,
            snapshot: data,
            received_at_ms: admitted_at_ms,
        };
        let registered = HashMap::from([(source_vehicle_id, identity.clone())]);
        let intake = SnapshotIntake {
            provider: CollectorProvider::Fleet,
            provider_feed: Some(RECEIVER_PROVIDER_FEED),
            // Merged snapshots are ordered by arrival, so the order key must never
            // move backwards: the accumulator's newest event time does not, a
            // replay leaves it alone, and a drive timestamp can reappear older
            // than an earlier snapshot's receipt time.
            observed_at_ms: Some(snapshot.created_at_ms),
            offline_drive_timeout: self.offline_drive_timeout,
        };
        store
            .collector_write_transaction(
                |transaction| {
                    let mut output = TickTxOutput::default();
                    intake_snapshots_in_transaction(
                        store,
                        transaction,
                        &intake,
                        &registered,
                        std::slice::from_ref(&item),
                        0,
                        &mut output,
                    )?;
                    if write_state {
                        HubStore::put_receiver_state_in_transaction(
                            transaction,
                            identity.hub_id,
                            &encoded,
                            admitted_at_ms,
                        )?;
                    }
                    Ok::<_, CollectorError>(((), true))
                },
                || self.commit_hook(),
            )
            .map_err(|error| unavailable("receiver intake commit", &error))?;

        // Committed: the candidate and its bytes replace the prior pair.
        if let Some(installed) = inner.vehicles.get_mut(&vehicle_id) {
            installed.accumulator = candidate;
            installed.last_persisted = encoded;
        }
        // Publish with the identity already resolved. A failure is a 503; the
        // retry is idempotent because the intake has committed.
        publish_compatibility_snapshots_with_identities(
            store,
            &publication_gate,
            cursor_key,
            std::slice::from_ref(&identity),
            admitted_at_ms,
            None,
        )
        .map_err(|error| unavailable("receiver publication", &error))?;
        Ok(())
    }
}

/// Log a failed step without the VIN, the payload or the token, and answer 503.
fn unavailable(step: &str, error: &CollectorError) -> ReceiverRejection {
    tracing::error!(target: "collector.receiver", step, error = %error, "receiver step failed");
    ReceiverRejection::Unavailable
}

/// The stored record of one alert or error, bounded by the raw-observation limit.
fn event_record(envelope: &ReceiverEnvelope) -> Result<Value, ReceiverRejection> {
    let payload = envelope
        .event_payload
        .clone()
        .ok_or(ReceiverRejection::Invalid)?;
    let record = serde_json::json!({
        "record_type": crate::db::RECEIVER_EVENT_RECORD_TYPE,
        "tx_type": envelope.tx_type,
        "txid": envelope.txid,
        "envelope_received_at_ms": envelope.received_at_ms,
        "timestamp_ms": envelope.timestamp_ms,
        "payload": payload,
    });
    let size = serde_json::to_vec(&record)
        .map_err(|_| ReceiverRejection::Invalid)?
        .len();
    if size > crate::db::MAX_RAW_OBSERVATION_BYTES {
        return Err(ReceiverRejection::Invalid);
    }
    Ok(record)
}

/// The accumulator of a vehicle's first record in this process: the stored
/// complete state bound to the VIN; if it is unusable, one WARN naming the Hub
/// vehicle UUID only, then the current Fleet seed, then empty state.
fn load_receiver_vehicle_state(
    store: &HubStore,
    vehicle_id: Uuid,
    vin: &str,
) -> Result<ReceiverVehicleState, CollectorError> {
    if let Some(bytes) = store.load_receiver_state(vehicle_id)? {
        match FleetTelemetryAccumulator::restore_complete_for_vin(&bytes, vin) {
            Ok(accumulator) => {
                return Ok(ReceiverVehicleState {
                    accumulator,
                    last_persisted: bytes,
                });
            }
            Err(_) => tracing::warn!(
                target: "collector.receiver",
                reason = "accumulator_state_discarded",
                vehicle_id = %vehicle_id,
                "stored accumulator state is unusable and was discarded"
            ),
        }
    }
    let seeded = match fleet_telemetry_seed_for_vin(store, vin)? {
        Some(owner_data) => FleetTelemetryAccumulator::restore(vin, &owner_data).ok(),
        None => None,
    };
    let accumulator = match seeded {
        Some(accumulator) => accumulator,
        None => FleetTelemetryAccumulator::empty(vin).map_err(|_| CollectorError::SelectedVehicleMissing)?,
    };
    Ok(ReceiverVehicleState {
        accumulator,
        last_persisted: Vec::new(),
    })
}
