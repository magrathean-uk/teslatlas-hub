// SPDX-License-Identifier: AGPL-3.0-only

// Durable collector state (C3 R4, R5, R7, R8, R14, R66): the five tables of
// the v70 step, their reads and writes, the start and stop gap rules, the
// owner-action and acknowledgement writes, and the readiness codes the tables
// give. Everything here is plain data; the scheduler turns its own state into
// these rows (`collector/scheduler.rs`) and the tick transaction
// (`collector/tick.rs`) hands them to `write_collector_rows_in_transaction`.
// No location, VIN, token, URL or Tesla identifier is stored (C3 R4).

/// Phases that count as "the car is awake" for a gap's alert (C3 R8). A
/// Suspended car is online that is being left alone, so a stored `suspended`
/// phase is awake: the reported-state refinement of the in-memory rule is not
/// stored, and an asleep car would have moved to `asleep` before the stop.
const COLLECTOR_AWAKE_PHASES_SQL: &str =
    "('online', 'driving', 'charging', 'updating', 'suspended')";

/// The most unapplied owner actions one read returns (C3 R65). The owner can
/// press a button a few times; a bound keeps a flood from stalling the loop,
/// and the remainder is read at the next refresh.
const COLLECTOR_ACTION_READ_LIMIT: i64 = 256;

/// The account row of `collector_account_state` (C3 R4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectorAccountRow {
    pub mode: String,
    pub config_fingerprint: String,
    pub state: String,
    pub reason_code: String,
    pub since_ms: i64,
    pub rate_limited_until_ms: Option<i64>,
    pub next_probe_ms: Option<i64>,
}

/// One row of `collector_vehicle_state` (C3 R4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectorVehicleRow {
    pub vehicle_id: Uuid,
    pub phase: String,
    pub hold: String,
    pub reason_code: String,
    pub phase_since_ms: i64,
    pub suspended_until_ms: Option<i64>,
    pub next_probe_ms: Option<i64>,
    pub last_wake_read_at_ms: Option<i64>,
}

/// One row of `collector_request_counters` (C3 R4, R25, R27).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectorCounterRow {
    pub month: String,
    pub mode: String,
    pub category: String,
    pub sent: i64,
    pub billable: i64,
    pub reserved: i64,
}

/// One row of `collection_gaps` (C3 R4, R8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectionGapRecord {
    pub gap_id: i64,
    pub vehicle_id: Option<Uuid>,
    pub kind: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub alert: bool,
    pub notified_at_ms: Option<i64>,
    pub created_at_ms: i64,
}

/// One row of `collector_owner_actions` (C3 R4, R65).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectorActionRecord {
    pub action_id: i64,
    pub action: String,
    pub vehicle_id: Option<Uuid>,
    pub requested_at_ms: i64,
}

/// Everything the scheduler loads before its first tick (C3 R7), read in one
/// store call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CollectorStateRows {
    pub account: Option<CollectorAccountRow>,
    pub vehicles: Vec<CollectorVehicleRow>,
    pub counters: Vec<CollectorCounterRow>,
    /// Gaps with no end. The graceful-stop and downtime gaps are closed by the
    /// start function before this read.
    pub open_gaps: Vec<CollectionGapRecord>,
    /// Owner actions not yet applied, oldest first.
    pub actions: Vec<CollectorActionRecord>,
}

/// A gap to insert (C3 R8). `ended_at_ms: None` opens it; at most one open gap
/// exists per kind and vehicle, so inserting an open gap that is already open
/// is a no-op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectorGapInsert {
    pub vehicle_id: Option<Uuid>,
    pub kind: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub alert: bool,
}

/// An open gap to close (C3 R8). The gap becomes an alert when its length
/// reaches `alert_at_or_over_ms`, the rule for gaps that began while every car
/// slept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CollectorGapClose {
    pub vehicle_id: Option<Uuid>,
    pub kind: String,
    pub ended_at_ms: i64,
    pub alert_at_or_over_ms: i64,
}

/// The scheduler's rows of one tick transaction (C3 R5). Rows are written only
/// when a persisted field changed, never per unchanged poll.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CollectorRowsWrite {
    pub now_ms: i64,
    pub account: Option<CollectorAccountRow>,
    pub vehicles: Vec<CollectorVehicleRow>,
    pub counters: Vec<CollectorCounterRow>,
    pub gap_inserts: Vec<CollectorGapInsert>,
    pub gap_closes: Vec<CollectorGapClose>,
    pub applied_actions: Vec<i64>,
}

impl CollectorRowsWrite {
    /// Nothing to write.
    pub(crate) fn is_empty(&self) -> bool {
        self.account.is_none()
            && self.vehicles.is_empty()
            && self.counters.is_empty()
            && self.gap_inserts.is_empty()
            && self.gap_closes.is_empty()
            && self.applied_actions.is_empty()
    }
}

/// What the start function changed (C3 R7, R8), for the start log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CollectorRunStart {
    /// A stored non-collecting state was reset because the mode or the
    /// configuration fingerprint differs from the stored one.
    pub account_reset: bool,
    /// The ordinary start lifts a refusal (C3 R4/R7, B2 clarification 3): a C3
    /// `owner_api_refused` or `provider_refused` account state whose mode or
    /// fingerprint changed, any existing C3 account row whose mode differs from
    /// the requested one, or, with no C3 account row, a C5 `api_refused` row
    /// stopped in another mode. The one decision lifts the durable C5 refusal in
    /// the same transaction and tells the credential unit's view; it is narrower
    /// than `account_reset`, which also covers a same-mode fingerprint change of
    /// any other non-collecting state.
    pub refusal_reset: bool,
    /// Vehicles whose `paused_access_refused` hold was cleared for the same
    /// reason.
    pub holds_cleared: usize,
    /// An open `hub_stopped` gap was closed.
    pub stopped_gap_closed: bool,
    /// A `hub_down` gap was inserted.
    pub hub_down_inserted: bool,
}

/// The inputs of [`HubStore::begin_cadence_wait`] (C3 R8, B2 clarification 3).
#[derive(Clone, Copy, Debug)]
pub(crate) struct CadenceWaitStartRequest {
    /// The wall time of the start.
    pub start_ms: i64,
    /// `heartbeat_at_ms` of the lease row the previous run left, read before
    /// the wait's lease replaced it.
    pub previous_heartbeat_ms: Option<i64>,
    /// Shortest silence worth recording as `hub_down` (`K_GAP_MIN`).
    pub gap_min_ms: i64,
    /// The length at which a gap alerts: the configured offline timeout in
    /// milliseconds, saturating, and zero meets the threshold at once.
    pub gap_alert_ms: i64,
}

/// What [`HubStore::begin_cadence_wait`] changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CadenceWaitStart {
    pub stopped_gap_closed: bool,
    pub hub_down_inserted: bool,
}

/// The inputs of [`HubStore::begin_collector_run`] (C3 R7, R8).
#[derive(Clone, Debug)]
pub(crate) struct CollectorRunStartRequest<'a> {
    /// `collector_account_state.mode` of the configured mode.
    pub mode: &'a str,
    /// The configuration fingerprint (C3 R4), 64 lowercase hex characters.
    pub config_fingerprint: &'a str,
    /// The wall time of the start.
    pub start_ms: i64,
    /// `heartbeat_at_ms` of the lease row the previous run left, read before
    /// this run's lease replaced it.
    pub previous_heartbeat_ms: Option<i64>,
    /// Shortest silence worth recording as `hub_down` (`K_GAP_MIN`).
    pub gap_min_ms: i64,
    /// A gap that began while every car slept alerts at this length
    /// (`K_GAP_ALERT`).
    pub gap_alert_ms: i64,
}

/// What the collector knows about one car's lifecycle at the moment of a read
/// (C3 R14, R40).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CollectorLifecycleSnapshot {
    pub last_reach: Option<String>,
    pub drive_open: bool,
    /// `source_vehicle_state` of the car's current discovery row, if it has one.
    pub current_discovery_state: Option<String>,
    /// The greatest `received_at_ms` over the car's current rows (C3 R40).
    pub last_stored_obs_ms: Option<i64>,
}

/// The outcome of C3 R14's rule for one discovery candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiscoveryAction {
    /// Store nothing: the state equals what the lifecycle already holds.
    Skip,
    /// Store only the car's current discovery row: the state equals the
    /// lifecycle's last reachability but the car has no such row.
    WriteCurrentOnly,
    /// Deliver the discovery to the lifecycle (raw row, current row, projection).
    Deliver,
}

/// C3 R14's storage rule. A reachability state (`online`, `asleep`, `offline`)
/// is delivered unless it equals the lifecycle's `last_reach`; with no
/// `last_reach` it is delivered, and an equal one still writes a missing
/// current row. A state that gives the lifecycle no reachability is delivered
/// only when it differs from the current discovery row's state: withholding such
/// a repeat changes nothing the lifecycle computes, so C1 R77's purpose (every
/// reachability change reaches the lifecycle) holds without a write per poll.
pub(crate) fn discovery_action(
    state: &str,
    last_reach: Option<&str>,
    current_discovery_state: Option<&str>,
) -> DiscoveryAction {
    if matches!(state, "online" | "asleep" | "offline") {
        if last_reach == Some(state) {
            if current_discovery_state.is_none() {
                DiscoveryAction::WriteCurrentOnly
            } else {
                DiscoveryAction::Skip
            }
        } else {
            DiscoveryAction::Deliver
        }
    } else if current_discovery_state == Some(state) {
        DiscoveryAction::Skip
    } else {
        DiscoveryAction::Deliver
    }
}

fn collector_vehicle_uuid(value: &str) -> Result<Uuid, StoreError> {
    parse_stored_uuid("collector vehicle", value)
}

impl HubStore {
    /// The credential identity that belongs in the configuration fingerprint
    /// (C3 R4, R7). Fleet polling: the stored application id and region, which
    /// stay the same when a token refreshes and change when the owner stores
    /// credentials of another application or region. The Owner wire: the
    /// generation of the last external credential write (C5, B2 clarification
    /// of 2026-10-12), which the credential unit's own refreshes never change.
    pub(crate) fn collector_credential_identity(&self, fleet: bool) -> Result<String, StoreError> {
        if !fleet {
            return self.owner_wire_credential_identity();
        }
        let connection = self.open_read_only_connection()?;
        let identity: Option<(String, String)> = connection
            .query_row(
                "SELECT client_id, region FROM fleet_tokens WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        Ok(identity.map_or_else(String::new, |(client, region)| format!("{client}|{region}")))
    }

    /// The account singleton's `(mode, state, reason_code)`, one indexed row read
    /// and nothing else (C3 R48, R49: the resident action checks a standing
    /// refusal before every admission, so it must not load vehicles, counters,
    /// gaps or actions). `None` before the collector first started.
    pub(crate) fn collector_account_stop_view(
        &self,
    ) -> Result<Option<(String, String, String)>, StoreError> {
        let connection = self.open_read_only_connection()?;
        connection
            .query_row(
                "SELECT mode, state, reason_code
                   FROM collector_account_state WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::Query)
    }

    /// Read all collector state for the scheduler's load (C3 R7).
    pub(crate) fn load_collector_state_rows(&self) -> Result<CollectorStateRows, StoreError> {
        let connection = self.open_read_only_connection()?;
        let account = connection
            .query_row(
                "SELECT mode, config_fingerprint, state, reason_code, since_ms,
                        rate_limited_until_ms, next_probe_ms
                   FROM collector_account_state WHERE singleton_id = 1",
                [],
                |row| {
                    Ok(CollectorAccountRow {
                        mode: row.get(0)?,
                        config_fingerprint: row.get(1)?,
                        state: row.get(2)?,
                        reason_code: row.get(3)?,
                        since_ms: row.get(4)?,
                        rate_limited_until_ms: row.get(5)?,
                        next_probe_ms: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Query)?;
        let vehicles = connection
            .prepare(
                "SELECT vehicle_id, phase, hold, reason_code, phase_since_ms,
                        suspended_until_ms, next_probe_ms, last_wake_read_at_ms
                   FROM collector_vehicle_state ORDER BY vehicle_id",
            )
            .map_err(StoreError::Query)?
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?
            .into_iter()
            .map(|(id, phase, hold, reason, since, suspended, probe, wake)| {
                Ok(CollectorVehicleRow {
                    vehicle_id: collector_vehicle_uuid(&id)?,
                    phase,
                    hold,
                    reason_code: reason,
                    phase_since_ms: since,
                    suspended_until_ms: suspended,
                    next_probe_ms: probe,
                    last_wake_read_at_ms: wake,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let counters = connection
            .prepare(
                "SELECT month, mode, category, sent, billable, reserved
                   FROM collector_request_counters ORDER BY month, mode, category",
            )
            .map_err(StoreError::Query)?
            .query_map([], |row| {
                Ok(CollectorCounterRow {
                    month: row.get(0)?,
                    mode: row.get(1)?,
                    category: row.get(2)?,
                    sent: row.get(3)?,
                    billable: row.get(4)?,
                    reserved: row.get(5)?,
                })
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?;
        let open_gaps = Self::read_gaps(
            &connection,
            "SELECT gap_id, vehicle_id, kind, started_at_ms, ended_at_ms, alert,
                    notified_at_ms, created_at_ms
               FROM collection_gaps WHERE ended_at_ms IS NULL ORDER BY gap_id",
            [],
        )?;
        let actions = Self::read_pending_actions(&connection)?;
        Ok(CollectorStateRows {
            account,
            vehicles,
            counters,
            open_gaps,
            actions,
        })
    }

    /// The unapplied owner actions, oldest first and bounded (C3 R65).
    fn read_pending_actions(
        connection: &Connection,
    ) -> Result<Vec<CollectorActionRecord>, StoreError> {
        connection
            .prepare(
                "SELECT action_id, action, vehicle_id, requested_at_ms
                   FROM collector_owner_actions WHERE applied_at_ms IS NULL
                  ORDER BY action_id LIMIT ?1",
            )
            .map_err(StoreError::Query)?
            .query_map(params![COLLECTOR_ACTION_READ_LIMIT], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?
            .into_iter()
            .map(|(action_id, action, vehicle, requested_at_ms)| {
                Ok(CollectorActionRecord {
                    action_id,
                    action,
                    vehicle_id: vehicle.as_deref().map(collector_vehicle_uuid).transpose()?,
                    requested_at_ms,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()
    }

    /// The owner actions not yet applied, read at each settings refresh of the
    /// loop (C3 R65): the rows are tiny and the read is on a read-only
    /// connection, so an idle collector pays almost nothing for it.
    pub(crate) fn pending_collector_owner_actions(
        &self,
    ) -> Result<Vec<CollectorActionRecord>, StoreError> {
        let connection = self.open_read_only_connection()?;
        Self::read_pending_actions(&connection)
    }

    fn read_gaps<P: rusqlite::Params>(
        connection: &Connection,
        sql: &str,
        parameters: P,
    ) -> Result<Vec<CollectionGapRecord>, StoreError> {
        connection
            .prepare(sql)
            .map_err(StoreError::Query)?
            .query_map(parameters, |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?
            .into_iter()
            .map(
                |(gap_id, vehicle, kind, started, ended, alert, notified, created)| {
                    Ok(CollectionGapRecord {
                        gap_id,
                        vehicle_id: vehicle.as_deref().map(collector_vehicle_uuid).transpose()?,
                        kind,
                        started_at_ms: started,
                        ended_at_ms: ended,
                        alert: alert != 0,
                        notified_at_ms: notified,
                        created_at_ms: created,
                    })
                },
            )
            .collect()
    }

    /// Every gap, oldest first, for status and tests (C3 R13, R66). Bounded by
    /// `limit`.
    #[allow(dead_code)] // Consumer: the gaps resource of the next `hub-http-v1` minor (C3 R66, section 8 item 15), written by the protocol owner; tested here.
    pub(crate) fn collection_gaps(
        &self,
        limit: i64,
    ) -> Result<Vec<CollectionGapRecord>, StoreError> {
        let connection = self.open_read_only_connection()?;
        Self::read_gaps(
            &connection,
            "SELECT gap_id, vehicle_id, kind, started_at_ms, ended_at_ms, alert,
                    notified_at_ms, created_at_ms
               FROM collection_gaps ORDER BY gap_id LIMIT ?1",
            params![limit],
        )
    }

    /// Run the collector's tick transaction (C3 R5). `work` runs inside one
    /// immediate write transaction and returns its result and whether it wrote
    /// anything. A transaction that wrote nothing is rolled back, not committed,
    /// so a tick whose requests produced nothing to store costs no durable write.
    /// `before_commit` runs just before the commit of a transaction that wrote
    /// (the tick-commit hook of the tests); an error from either rolls back.
    pub(crate) fn collector_write_transaction<T, E>(
        &self,
        work: impl FnOnce(&Transaction<'_>) -> Result<(T, bool), E>,
        before_commit: impl FnOnce() -> Result<(), E>,
    ) -> Result<T, E>
    where
        E: From<StoreError>,
    {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let (value, wrote) = work(&transaction)?;
        if wrote {
            before_commit()?;
            transaction.commit().map_err(StoreError::LifecycleWrite)?;
        }
        Ok(value)
    }

    /// Write the scheduler rows of one tick inside the caller's transaction
    /// (C3 R5). Returns whether anything was written.
    pub(crate) fn write_collector_rows_in_transaction(
        transaction: &Transaction<'_>,
        write: &CollectorRowsWrite,
    ) -> Result<bool, StoreError> {
        validate_timestamp("collector rows now_ms", write.now_ms)?;
        if write.is_empty() {
            return Ok(false);
        }
        if let Some(account) = &write.account {
            transaction
                .execute(
                    "INSERT INTO collector_account_state(
                        singleton_id, mode, config_fingerprint, state, reason_code,
                        since_ms, rate_limited_until_ms, next_probe_ms, updated_at_ms)
                     VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(singleton_id) DO UPDATE SET
                        mode = excluded.mode,
                        config_fingerprint = excluded.config_fingerprint,
                        state = excluded.state,
                        reason_code = excluded.reason_code,
                        since_ms = excluded.since_ms,
                        rate_limited_until_ms = excluded.rate_limited_until_ms,
                        next_probe_ms = excluded.next_probe_ms,
                        updated_at_ms = excluded.updated_at_ms",
                    params![
                        account.mode,
                        account.config_fingerprint,
                        account.state,
                        account.reason_code,
                        account.since_ms,
                        account.rate_limited_until_ms,
                        account.next_probe_ms,
                        write.now_ms,
                    ],
                )
                .map_err(StoreError::Query)?;
        }
        for vehicle in &write.vehicles {
            transaction
                .execute(
                    "INSERT INTO collector_vehicle_state(
                        vehicle_id, phase, hold, reason_code, phase_since_ms,
                        suspended_until_ms, next_probe_ms, last_wake_read_at_ms,
                        updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(vehicle_id) DO UPDATE SET
                        phase = excluded.phase,
                        hold = excluded.hold,
                        reason_code = excluded.reason_code,
                        phase_since_ms = excluded.phase_since_ms,
                        suspended_until_ms = excluded.suspended_until_ms,
                        next_probe_ms = excluded.next_probe_ms,
                        last_wake_read_at_ms = excluded.last_wake_read_at_ms,
                        updated_at_ms = excluded.updated_at_ms",
                    params![
                        vehicle.vehicle_id.to_string(),
                        vehicle.phase,
                        vehicle.hold,
                        vehicle.reason_code,
                        vehicle.phase_since_ms,
                        vehicle.suspended_until_ms,
                        vehicle.next_probe_ms,
                        vehicle.last_wake_read_at_ms,
                        write.now_ms,
                    ],
                )
                .map_err(StoreError::Query)?;
        }
        for counter in &write.counters {
            transaction
                .execute(
                    "INSERT INTO collector_request_counters(
                        month, mode, category, sent, billable, reserved, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(month, mode, category) DO UPDATE SET
                        sent = excluded.sent,
                        billable = excluded.billable,
                        reserved = excluded.reserved,
                        updated_at_ms = excluded.updated_at_ms",
                    params![
                        counter.month,
                        counter.mode,
                        counter.category,
                        counter.sent,
                        counter.billable,
                        counter.reserved,
                        write.now_ms,
                    ],
                )
                .map_err(StoreError::Query)?;
        }
        for close in &write.gap_closes {
            Self::close_gap_in_transaction(transaction, close)?;
        }
        for insert in &write.gap_inserts {
            Self::insert_gap_in_transaction(transaction, insert, write.now_ms)?;
        }
        for action_id in &write.applied_actions {
            transaction
                .execute(
                    "UPDATE collector_owner_actions SET applied_at_ms = ?2
                      WHERE action_id = ?1 AND applied_at_ms IS NULL",
                    params![action_id, write.now_ms],
                )
                .map_err(StoreError::Query)?;
        }
        Ok(true)
    }

    fn close_gap_in_transaction(
        transaction: &Transaction<'_>,
        close: &CollectorGapClose,
    ) -> Result<(), StoreError> {
        validate_timestamp("collection gap end", close.ended_at_ms)?;
        transaction
            .execute(
                "UPDATE collection_gaps
                    SET ended_at_ms = MAX(started_at_ms, ?3),
                        alert = CASE
                            WHEN alert = 1 OR MAX(started_at_ms, ?3) - started_at_ms >= ?4
                            THEN 1 ELSE 0 END
                  WHERE kind = ?1 AND vehicle_id IS ?2 AND ended_at_ms IS NULL",
                params![
                    close.kind,
                    close.vehicle_id.map(|id| id.to_string()),
                    close.ended_at_ms,
                    close.alert_at_or_over_ms,
                ],
            )
            .map_err(StoreError::Query)?;
        Ok(())
    }

    fn insert_gap_in_transaction(
        transaction: &Transaction<'_>,
        insert: &CollectorGapInsert,
        created_at_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("collection gap start", insert.started_at_ms)?;
        transaction
            .execute(
                "INSERT INTO collection_gaps(
                    vehicle_id, kind, started_at_ms, ended_at_ms, alert,
                    notified_at_ms, created_at_ms)
                 SELECT ?1, ?2, ?3, ?4, ?5, NULL, ?6
                  WHERE ?4 IS NOT NULL OR NOT EXISTS(
                    SELECT 1 FROM collection_gaps
                     WHERE kind = ?2 AND vehicle_id IS ?1 AND ended_at_ms IS NULL)",
                params![
                    insert.vehicle_id.map(|id| id.to_string()),
                    insert.kind,
                    insert.started_at_ms,
                    insert.ended_at_ms,
                    i64::from(insert.alert),
                    created_at_ms,
                ],
            )
            .map_err(StoreError::Query)?;
        Ok(())
    }

    /// Whether any car that is collected (not Disabled or Removed) was awake at
    /// the last persisted transition (C3 R8: "the phase at the gap's start is the
    /// persisted `collector_vehicle_state.phase`").
    fn persisted_awake(transaction: &Transaction<'_>) -> Result<bool, StoreError> {
        transaction
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM collector_vehicle_state
                      WHERE hold NOT IN ('disabled', 'removed')
                        AND phase IN {COLLECTOR_AWAKE_PHASES_SQL})"
                ),
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)
    }

    /// Sealing a catalogue copy (C3 R8, B2 clarification 3), inside the caller's
    /// transaction: turn the retained collector lease into an open account-level
    /// `hub_stopped` gap, so the copy keeps the evidence of the silence that the
    /// removed host authority would have given. The gap starts at the lease's
    /// last heartbeat (not at `sealed_at_ms`), is created at `sealed_at_ms`, has
    /// no end and no notification, and alerts when a car was awake at the
    /// persisted phases; the first start that uses the copy closes it. Nothing
    /// is written when no lease is retained or an account-level stop is already
    /// open (a vehicle-scoped stop does not count): an existing stop is
    /// preserved exactly. Returns whether a gap was inserted. The caller removes
    /// the lease in the same transaction after this read.
    pub(crate) fn seal_collector_lease_gap_in_transaction(
        transaction: &Transaction<'_>,
        sealed_at_ms: i64,
    ) -> Result<bool, StoreError> {
        validate_timestamp("collector lease seal", sealed_at_ms)?;
        let heartbeat_ms: Option<i64> = transaction
            .query_row(
                "SELECT heartbeat_at_ms FROM supervised_collector_lease
                  WHERE singleton_id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some(heartbeat_ms) = heartbeat_ms else {
            return Ok(false);
        };
        let open_stop: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM collection_gaps
                  WHERE kind IN ('hub_stopped', 'hub_down') AND vehicle_id IS NULL
                    AND ended_at_ms IS NULL)",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        if open_stop {
            return Ok(false);
        }
        let awake = Self::persisted_awake(transaction)?;
        Self::insert_gap_in_transaction(
            transaction,
            &CollectorGapInsert {
                vehicle_id: None,
                kind: "hub_stopped".to_owned(),
                started_at_ms: heartbeat_ms,
                ended_at_ms: None,
                alert: awake,
            },
            sealed_at_ms,
        )?;
        Ok(true)
    }

    /// The lease acquisition's part of C3 R8 (B2 round 3, MIG-R3-1; B2 apply, K1),
    /// inside the acquisition's transaction. Replacing an expired predecessor's lease
    /// overwrites its heartbeat, which is the only record of when it fell silent, and
    /// the start's own downtime record is a separate commit that can fail after the
    /// acquisition. So the evidence is made durable in the acquisition itself: when
    /// the lease being replaced has expired and no account-level `hub_stopped` or
    /// `hub_down` gap is open, an open account-level `hub_down` gap starts at the
    /// predecessor's heartbeat, whatever the silence so far (created now, no end, no
    /// notification, alerting when a car was awake at the persisted phases, as the
    /// sealing of a catalogue copy does with its `hub_stopped`). No minimum is applied
    /// here: a failed start would otherwise overwrite the earliest heartbeat with each
    /// attempt, and a chain of attempts each shorter than the minimum would lose an
    /// outage that is longer than it altogether. The minimum is applied where the
    /// downtime is finalised, by the start that succeeds (`close_or_record_downtime`):
    /// it closes the gap at its start time, or removes it when the whole silence from
    /// the earliest heartbeat is no longer than the minimum. So a successful start
    /// records the same `hub_down` gap as before (CT52), and a start that fails leaves
    /// the evidence for the next one. Returns whether a gap was inserted.
    pub(crate) fn record_predecessor_downtime_in_transaction(
        transaction: &Transaction<'_>,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        let predecessor: Option<(i64, i64)> = transaction
            .query_row(
                "SELECT heartbeat_at_ms, lease_until_ms FROM supervised_collector_lease
                  WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let Some((heartbeat_ms, lease_until_ms)) = predecessor else {
            return Ok(false);
        };
        if lease_until_ms > now_ms {
            return Ok(false);
        }
        let open_account_gap: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM collection_gaps
                  WHERE kind IN ('hub_stopped', 'hub_down') AND vehicle_id IS NULL
                    AND ended_at_ms IS NULL)",
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        if open_account_gap {
            return Ok(false);
        }
        let awake = Self::persisted_awake(transaction)?;
        Self::insert_gap_in_transaction(
            transaction,
            &CollectorGapInsert {
                vehicle_id: None,
                kind: "hub_down".to_owned(),
                started_at_ms: heartbeat_ms,
                ended_at_ms: None,
                alert: awake,
            },
            now_ms,
        )?;
        Ok(true)
    }

    /// The start of a collector run (C3 R7 step 1 and R8): in one transaction,
    /// reset a stored non-collecting account state when the mode or the
    /// configuration fingerprint changed (and clear the holds whose only remedy
    /// is such a change), lift a refusal that the three conditions of
    /// [`CollectorRunStart::refusal_reset`] name (C5's own row included), close
    /// the previous run's `hub_stopped` gap or record a `hub_down` gap from the
    /// previous heartbeat. The credential unit may be built before this runs;
    /// what it does with the outcome (clearing or restoring a refusal, writing
    /// its status) follows this commit and precedes the first request.
    pub(crate) fn begin_collector_run(
        &self,
        request: &CollectorRunStartRequest<'_>,
    ) -> Result<CollectorRunStart, StoreError> {
        validate_timestamp("collector start", request.start_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let mut outcome = CollectorRunStart::default();
        // Alerts judge the phases the previous run left, so read them first.
        let awake = Self::persisted_awake(&transaction)?;

        let stored: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT mode, config_fingerprint, state
                   FROM collector_account_state WHERE singleton_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        // C3 R7, B2 clarification 3: one refusal decision from the rows as they
        // stand before this start changes anything.
        outcome.refusal_reset = match &stored {
            Some((mode, fingerprint, state)) => {
                mode != request.mode
                    || (matches!(state.as_str(), "owner_api_refused" | "provider_refused")
                        && fingerprint != request.config_fingerprint)
            }
            None => Self::c5_refusal_stopped_in_another_mode(&transaction, request.mode)?,
        };
        if let Some((mode, fingerprint, state)) = stored
            && (mode != request.mode || fingerprint != request.config_fingerprint)
        {
            if state != "collecting" {
                transaction
                    .execute(
                        "UPDATE collector_account_state
                            SET state = 'collecting', reason_code = 'config_changed',
                                since_ms = ?1, rate_limited_until_ms = NULL,
                                next_probe_ms = NULL, updated_at_ms = ?1
                          WHERE singleton_id = 1",
                        params![request.start_ms],
                    )
                    .map_err(StoreError::Query)?;
                // Every account-scope gap of the old state ends with it.
                for kind in [
                    "owner_api_refused",
                    "provider_refused",
                    "fleet_access_refused",
                    "billing_suspended",
                    "cap_reached",
                    "config_error",
                    "account_locked",
                ] {
                    Self::close_gap_in_transaction(
                        &transaction,
                        &CollectorGapClose {
                            vehicle_id: None,
                            kind: kind.to_owned(),
                            ended_at_ms: request.start_ms,
                            alert_at_or_over_ms: request.gap_alert_ms,
                        },
                    )?;
                }
                outcome.account_reset = true;
            }
            transaction
                .execute(
                    "UPDATE collector_account_state
                        SET mode = ?1, config_fingerprint = ?2, updated_at_ms = ?3
                      WHERE singleton_id = 1",
                    params![request.mode, request.config_fingerprint, request.start_ms],
                )
                .map_err(StoreError::Query)?;
            // A hold that only a mode or fingerprint change can lift (C3 R51).
            let cleared: Vec<String> = transaction
                .prepare(
                    "SELECT vehicle_id FROM collector_vehicle_state
                      WHERE hold = 'paused' AND reason_code = 'paused_access_refused'",
                )
                .map_err(StoreError::Query)?
                .query_map([], |row| row.get(0))
                .map_err(StoreError::Query)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::Query)?;
            for vehicle in &cleared {
                transaction
                    .execute(
                        "UPDATE collector_vehicle_state
                            SET phase = 'unknown', hold = 'none',
                                reason_code = 'config_changed', phase_since_ms = ?2,
                                suspended_until_ms = NULL, next_probe_ms = NULL,
                                updated_at_ms = ?2
                          WHERE vehicle_id = ?1",
                        params![vehicle, request.start_ms],
                    )
                    .map_err(StoreError::Query)?;
                Self::close_gap_in_transaction(
                    &transaction,
                    &CollectorGapClose {
                        vehicle_id: Some(collector_vehicle_uuid(vehicle)?),
                        kind: "vehicle_paused".to_owned(),
                        ended_at_ms: request.start_ms,
                        alert_at_or_over_ms: request.gap_alert_ms,
                    },
                )?;
            }
            outcome.holds_cleared = cleared.len();
        }

        if outcome.refusal_reset {
            // C5 owns its row (R52a transaction exception): the lift commits or
            // rolls back with the C3 reset above.
            Self::lift_collector_auth_refusal_in_transaction(&transaction, request.start_ms)?;
        }

        let (stopped_gap_closed, hub_down_inserted) = Self::close_or_record_downtime(
            &transaction,
            request.start_ms,
            request.previous_heartbeat_ms,
            request.gap_min_ms,
            request.gap_alert_ms,
            awake,
        )?;
        outcome.stopped_gap_closed = stopped_gap_closed;
        outcome.hub_down_inserted = hub_down_inserted;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(outcome)
    }

    /// C3 R8, inside the caller's transaction: close the previous run's open
    /// `hub_stopped` gap at `start_ms`, or, when none is open, record a `hub_down`
    /// gap from the previous heartbeat if the silence is longer than
    /// `gap_min_ms`. A closed gap alerts when it ran at least `gap_alert_ms`;
    /// a `hub_down` gap alerts when a car was awake at the persisted phases or
    /// it is that long. Returns `(stopped_gap_closed, hub_down_inserted)`.
    fn close_or_record_downtime(
        transaction: &Transaction<'_>,
        start_ms: i64,
        previous_heartbeat_ms: Option<i64>,
        gap_min_ms: i64,
        gap_alert_ms: i64,
        awake: bool,
    ) -> Result<(bool, bool), StoreError> {
        let open_stop: Option<i64> = transaction
            .query_row(
                "SELECT gap_id FROM collection_gaps
                  WHERE kind = 'hub_stopped' AND ended_at_ms IS NULL
                  ORDER BY gap_id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        // The downtime a lease acquisition recorded open (MIG-R3-1, K1): it is the
        // `hub_down` gap this start owes. The acquisition records it whatever the
        // silence, so that the earliest heartbeat survives a chain of failed starts; the
        // minimum is applied here, to the whole silence from that heartbeat. A longer
        // silence is closed at the start time (no second gap is recorded for it), and
        // one no longer than the minimum is not downtime and the open row is removed.
        let open_down: Option<(i64, i64)> = transaction
            .query_row(
                "SELECT gap_id, started_at_ms FROM collection_gaps
                  WHERE kind = 'hub_down' AND vehicle_id IS NULL AND ended_at_ms IS NULL
                  ORDER BY gap_id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let mut down_recorded = false;
        if let Some((gap_id, started_at_ms)) = open_down {
            if start_ms.saturating_sub(started_at_ms) > gap_min_ms {
                Self::close_gap_in_transaction(
                    transaction,
                    &CollectorGapClose {
                        vehicle_id: None,
                        kind: "hub_down".to_owned(),
                        ended_at_ms: start_ms,
                        alert_at_or_over_ms: gap_alert_ms,
                    },
                )?;
                down_recorded = true;
            } else {
                transaction
                    .execute(
                        "DELETE FROM collection_gaps WHERE gap_id = ?1",
                        params![gap_id],
                    )
                    .map_err(StoreError::Query)?;
            }
        }
        if open_stop.is_some() {
            Self::close_gap_in_transaction(
                transaction,
                &CollectorGapClose {
                    vehicle_id: None,
                    kind: "hub_stopped".to_owned(),
                    ended_at_ms: start_ms,
                    alert_at_or_over_ms: gap_alert_ms,
                },
            )?;
            return Ok((true, down_recorded));
        }
        if open_down.is_some() {
            return Ok((false, down_recorded));
        }
        if let Some(heartbeat) = previous_heartbeat_ms
            && start_ms.saturating_sub(heartbeat) > gap_min_ms
        {
            let length = start_ms - heartbeat;
            Self::insert_gap_in_transaction(
                transaction,
                &CollectorGapInsert {
                    vehicle_id: None,
                    kind: "hub_down".to_owned(),
                    started_at_ms: heartbeat,
                    ended_at_ms: Some(start_ms),
                    alert: awake || length >= gap_alert_ms,
                },
                start_ms,
            )?;
            return Ok((false, true));
        }
        Ok((false, false))
    }

    /// The start of a managed configuration wait (C3 R8, B2 clarification 3):
    /// not an ordinary R7 start, so it resets no account state, clears no hold,
    /// lifts no refusal and changes no count or action. In one transaction it
    /// closes the preceding `hub_stopped` gap or records the preceding downtime
    /// under the ordinary rule, then opens the `hub_stopped` gap of the wait,
    /// alerting when a car was awake at the persisted phases. A later ordinary
    /// start closes that gap; an orderly end of the wait writes nothing.
    pub(crate) fn begin_cadence_wait(
        &self,
        request: &CadenceWaitStartRequest,
    ) -> Result<CadenceWaitStart, StoreError> {
        validate_timestamp("cadence wait start", request.start_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let awake = Self::persisted_awake(&transaction)?;
        let (stopped_gap_closed, hub_down_inserted) = Self::close_or_record_downtime(
            &transaction,
            request.start_ms,
            request.previous_heartbeat_ms,
            request.gap_min_ms,
            request.gap_alert_ms,
            awake,
        )?;
        Self::insert_gap_in_transaction(
            &transaction,
            &CollectorGapInsert {
                vehicle_id: None,
                kind: "hub_stopped".to_owned(),
                started_at_ms: request.start_ms,
                ended_at_ms: None,
                alert: awake,
            },
            request.start_ms,
        )?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(CadenceWaitStart {
            stopped_gap_closed,
            hub_down_inserted,
        })
    }

    /// The stop of a collector run (C3 R8): in one final transaction, write the
    /// scheduler's last rows and open the `hub_stopped` gap. The gap alerts when a
    /// car was awake at the persisted phases; its length is judged at the next
    /// start. A second stop with a gap already open adds nothing.
    pub(crate) fn end_collector_run(
        &self,
        rows: Option<&CollectorRowsWrite>,
        stop_ms: i64,
    ) -> Result<(), StoreError> {
        validate_timestamp("collector stop", stop_ms)?;
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        if let Some(rows) = rows {
            Self::write_collector_rows_in_transaction(&transaction, rows)?;
        }
        let awake = Self::persisted_awake(&transaction)?;
        Self::insert_gap_in_transaction(
            &transaction,
            &CollectorGapInsert {
                vehicle_id: None,
                kind: "hub_stopped".to_owned(),
                started_at_ms: stop_ms,
                ended_at_ms: None,
                alert: awake,
            },
            stop_ms,
        )?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    /// Insert an owner action (the control CLI's `collector retry` and
    /// `collector resume`, C3 R65). `resume_vehicle` needs a vehicle;
    /// `retry_collection` takes none. The table's CHECK refuses any other pairing.
    /// Public because the control CLI is a separate binary of this crate.
    pub fn insert_collector_owner_action(
        &self,
        action: &str,
        vehicle_id: Option<Uuid>,
        requested_at_ms: i64,
    ) -> Result<i64, StoreError> {
        validate_timestamp("collector action", requested_at_ms)?;
        let connection = self.open()?;
        connection
            .execute(
                "INSERT INTO collector_owner_actions(action, vehicle_id, requested_at_ms)
                 VALUES (?1, ?2, ?3)",
                params![action, vehicle_id.map(|id| id.to_string()), requested_at_ms],
            )
            .map_err(StoreError::Query)?;
        Ok(connection.last_insert_rowid())
    }

    /// Mark every unnotified alert gap as notified (C3 R66: the control CLI's
    /// `collector gaps acknowledge` and, on the Mac, the GUI's after it has shown
    /// them). Returns how many gaps were marked. Public because the control CLI is
    /// a separate binary of this crate.
    pub fn acknowledge_collection_gaps(&self, now_ms: i64) -> Result<usize, StoreError> {
        validate_timestamp("collection gap acknowledgement", now_ms)?;
        let connection = self.open()?;
        connection
            .execute(
                "UPDATE collection_gaps SET notified_at_ms = ?1
                  WHERE alert = 1 AND notified_at_ms IS NULL",
                params![now_ms],
            )
            .map_err(StoreError::Query)
    }

    /// The count of open gaps and of alert gaps not yet notified (C3 R13).
    pub(crate) fn collection_gap_counts(&self) -> Result<(usize, usize), StoreError> {
        let connection = self.open_read_only_connection()?;
        let (open, unnotified): (i64, i64) = connection
            .query_row(
                "SELECT COALESCE(SUM(ended_at_ms IS NULL), 0),
                        COALESCE(SUM(alert = 1 AND notified_at_ms IS NULL), 0)
                   FROM collection_gaps",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(StoreError::Query)?;
        Ok((
            usize::try_from(open).map_err(|_| StoreError::InvalidStoredCount)?,
            usize::try_from(unnotified).map_err(|_| StoreError::InvalidStoredCount)?,
        ))
    }

    /// Record how many cars the running collector now shows as unable to write
    /// their stream receipts (C3 R28): the in-process source of readiness's
    /// `collector_stream_audit_failed`. Zero clears it.
    pub(crate) fn set_stream_audit_failed_cars(&self, cars: usize) {
        self.stream_audit_failed_cars
            .store(cars, std::sync::atomic::Ordering::Release);
    }

    fn stream_audit_failed_cars(&self) -> usize {
        self.stream_audit_failed_cars
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Read a car's lifecycle snapshot inside the caller's transaction (C3 R14,
    /// R40): the lifecycle's last reachability and whether a drive is open, the
    /// state of the current discovery row, and the latest stored observation
    /// time. The last is a bounded read of at most one row per record type.
    pub(crate) fn collector_lifecycle_snapshot_in_transaction(
        transaction: &Transaction<'_>,
        vehicle_id: Uuid,
    ) -> Result<CollectorLifecycleSnapshot, StoreError> {
        let mut snapshot = CollectorLifecycleSnapshot::default();
        if let Some(record) = load_lifecycle_state_in_transaction(transaction, vehicle_id)? {
            let state = crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
                .map_err(|_| StoreError::InvalidLifecycleSession)?;
            snapshot.last_reach = state.last_reach;
            snapshot.drive_open = state.open_drive.is_some();
        }
        let discovery: Option<String> = transaction
            .query_row(
                "SELECT payload_json FROM current_observations
                  WHERE vehicle_id = ?1
                    AND record_type IN ('owner_api_discovery_v1', 'fleet_api_discovery_v1')
                  ORDER BY observed_at_ms DESC LIMIT 1",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        snapshot.current_discovery_state = discovery
            .as_deref()
            .and_then(|json| serde_json::from_str::<Value>(json).ok())
            .map(|payload| observation_vehicle_state(&payload));
        // The newest receipt among the rows the lifecycle has been delivered
        // (R40 evidence). A current discovery row that no delivery wrote (the row a
        // discovery equal to the lifecycle's reachability makes when none existed,
        // `DiscoveryAction::WriteCurrentOnly`) is no evidence, however early the tick
        // that wrote it: it takes the next id of the shared sequence, above the
        // lifecycle's cursor, which only a delivered observation raises later (and
        // that observation's own receipt is then newer). The other record types are
        // always delivered, so they count as they are.
        snapshot.last_stored_obs_ms = transaction
            .query_row(
                "SELECT MAX(received_at_ms) FROM current_observations
                  WHERE vehicle_id = ?1
                    AND (record_type NOT IN ('owner_api_discovery_v1', 'fleet_api_discovery_v1')
                         OR observation_id <= COALESCE(
                              (SELECT last_observation_id FROM vehicle_lifecycle_state
                                WHERE vehicle_id = ?1), 0))",
                params![vehicle_id.to_string()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        Ok(snapshot)
    }

    /// [`Self::collector_lifecycle_snapshot_in_transaction`] on a read-only
    /// connection, for the start load of the lifecycle views and the R40 check
    /// (C3 R7 step 6, R40).
    pub(crate) fn collector_lifecycle_snapshot(
        &self,
        vehicle_id: Uuid,
    ) -> Result<CollectorLifecycleSnapshot, StoreError> {
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(StoreError::Begin)?;
        Self::collector_lifecycle_snapshot_in_transaction(&transaction, vehicle_id)
    }

    /// The lifecycle state of a car inside the caller's transaction (C3 R5,
    /// materialisation inside the tick).
    pub(crate) fn lifecycle_state_in_transaction(
        transaction: &Transaction<'_>,
        vehicle_id: Uuid,
    ) -> Result<Option<LifecycleStateRecord>, StoreError> {
        load_lifecycle_state_in_transaction(transaction, vehicle_id)
    }

    /// One page of a car's observations after a cursor, inside the caller's
    /// transaction (C3 R5, materialisation inside the tick).
    pub(crate) fn observations_after_in_transaction(
        transaction: &Transaction<'_>,
        vehicle_id: Uuid,
        after_observation_id: i64,
        limit: u32,
    ) -> Result<Vec<ObservationRecord>, StoreError> {
        observations_after_id_in_transaction(transaction, vehicle_id, after_observation_id, limit)
    }
}
