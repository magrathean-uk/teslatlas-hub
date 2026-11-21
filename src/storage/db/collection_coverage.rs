// SPDX-License-Identifier: AGPL-3.0-only

/// Schema v80: the collection windows this store covers, written beside the
/// window ledger so the import cutoff travels with `hub.sqlite` through backup,
/// restore and a new host. `gaps_json` holds the window's collection gaps at
/// handback; `vehicle_ends_json` holds each vehicle's end of the window (the
/// close for a vehicle with collection evidence at handback, the vehicle's last
/// Hub row after a restore seal) and `sealed` records how the window closed.
const COLLECTION_COVERAGE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS collection_coverage (
    seq INTEGER PRIMARY KEY,
    grant_digest TEXT NOT NULL,
    opened_at_ms INTEGER NOT NULL,
    closed_at_ms INTEGER CHECK(closed_at_ms IS NULL OR closed_at_ms >= opened_at_ms),
    gaps_json TEXT NOT NULL CHECK(json_valid(gaps_json)),
    vehicle_ends_json TEXT CHECK(vehicle_ends_json IS NULL OR json_valid(vehicle_ends_json)),
    sealed TEXT NOT NULL
) STRICT;";

/// `sealed` of a window that is open in the store.
pub const COVERAGE_SEALED_OPEN: &str = "open";
/// `sealed` of a window handed back on the host that ran it.
pub const COVERAGE_SEALED_HANDBACK: &str = "handback";
/// `sealed` of a window found open in a restored store with no open window on
/// this host: closed per vehicle at that vehicle's last Hub-range row.
pub const COVERAGE_SEALED_RESTORE: &str = "restore";

/// A coverage record could not be read or written.
#[derive(Debug, Error)]
pub enum CoverageError {
    #[error("the collection coverage could not be read or written: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("a collection coverage record is malformed: {0}")]
    Malformed(String),
    /// A window of the same sequence but another grant is already recorded.
    #[error("collection coverage already records another window as sequence {seq}")]
    Conflict { seq: i64 },
    /// A window is still open in the store: the cutoff is not recorded.
    #[error("collection window {seq} is open in the store")]
    WindowOpen { seq: i64 },
    /// The sessions still open at a handback could not be closed.
    #[error("the open sessions could not be closed at the handback: {0}")]
    Lifecycle(String),
}

/// An interval of one window in which the collector was not running.
/// `vehicle_id` is absent for a gap of the whole window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct CoverageGap {
    pub vehicle_id: Option<String>,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// A window handed back: what the store records beside the ledger line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowCoverage {
    pub seq: i64,
    pub grant_digest: String,
    pub opened_at_ms: i64,
    pub closed_at_ms: i64,
    /// The gaps of the handback (whole-window, `vehicle_id` absent).
    pub gaps: Vec<CoverageGap>,
}

/// Opens `<data_dir>/hub.sqlite` for a coverage write. `None` when the data
/// directory holds no store (a window can run before the store exists, and
/// then there is no import to protect).
pub fn open_coverage_store(data_dir: &Path) -> Result<Option<Connection>, CoverageError> {
    let database = data_dir.join("hub.sqlite");
    if !database.is_file() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(
        &database,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(Some(connection))
}

fn coverage_table_present(connection: &Connection) -> Result<bool, CoverageError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'collection_coverage')",
        [],
        |row| row.get(0),
    )?)
}

fn named_table_present(connection: &Connection, table: &str) -> Result<bool, CoverageError> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )?)
}

fn seconds_to_ms(seconds: i64) -> Result<i64, CoverageError> {
    seconds
        .checked_mul(1000)
        .ok_or_else(|| CoverageError::Malformed("a time is out of range".to_owned()))
}

/// Gaps of the whole window, in seconds as the ledger holds them, as store
/// gaps in milliseconds.
pub fn coverage_gaps_from_seconds(
    gaps: impl IntoIterator<Item = (i64, i64)>,
) -> Result<Vec<CoverageGap>, CoverageError> {
    gaps.into_iter()
        .map(|(start, end)| {
            Ok(CoverageGap {
                vehicle_id: None,
                start_ms: seconds_to_ms(start)?,
                end_ms: seconds_to_ms(end)?,
            })
        })
        .collect()
}

/// Records that a window opened: one row, open, written before the ledger's
/// opened line so a window never runs without it. Returns `false` when the
/// store has no coverage table yet (a store older than schema v80).
pub fn record_coverage_open(
    connection: &Connection,
    seq: i64,
    grant_digest: &str,
    opened_at_ms: i64,
) -> Result<bool, CoverageError> {
    if !coverage_table_present(connection)? {
        return Ok(false);
    }
    let existing: Option<String> = connection
        .query_row(
            "SELECT grant_digest FROM collection_coverage WHERE seq = ?1",
            [seq],
            |row| row.get(0),
        )
        .optional()?;
    match existing {
        Some(digest) if digest == grant_digest => Ok(true),
        Some(_) => Err(CoverageError::Conflict { seq }),
        None => {
            connection.execute(
                "INSERT INTO collection_coverage
                     (seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, vehicle_ends_json, sealed)
                 VALUES (?1, ?2, ?3, NULL, '[]', NULL, ?4)",
                params![seq, grant_digest, opened_at_ms, COVERAGE_SEALED_OPEN],
            )?;
            Ok(true)
        }
    }
}

/// The collection gaps the store recorded inside `[opened_ms, closed_ms]`,
/// clamped to it. An open gap runs to the close.
/// When restoring, exclude the backup's account stop, open or since closed by a
/// resumed window's first start: sealing the lease
/// gives it an earlier start than its creation and it can overlap stored rows.
fn store_gaps_within(
    connection: &Connection,
    opened_ms: i64,
    closed_ms: i64,
    include_backup_stop: bool,
) -> Result<Vec<CoverageGap>, CoverageError> {
    if !named_table_present(connection, "collection_gaps")? {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT vehicle_id, started_at_ms, ended_at_ms FROM collection_gaps
         WHERE started_at_ms < ?2 AND (ended_at_ms IS NULL OR ended_at_ms > ?1)
           AND (?3 OR kind != 'hub_stopped' OR vehicle_id IS NOT NULL
                OR started_at_ms = created_at_ms)
         ORDER BY gap_id",
    )?;
    let rows = statement.query_map(params![opened_ms, closed_ms, include_backup_stop], |row| {
        Ok((
            row.get::<_, Option<String>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<i64>>(2)?,
        ))
    })?;
    let mut gaps = Vec::new();
    for row in rows {
        let (vehicle_id, start, end) = row?;
        let start_ms = start.max(opened_ms);
        let end_ms = end.unwrap_or(closed_ms).min(closed_ms);
        if end_ms > start_ms {
            gaps.push(CoverageGap {
                vehicle_id,
                start_ms,
                end_ms,
            });
        }
    }
    Ok(gaps)
}

/// Records a window's handback: closes its row, or inserts the closed row when
/// the store lacks it (the mirror of a ledger handback). The gaps are the
/// handback's own plus the collection gaps the store holds inside the window.
/// A vehicle covers the whole window only when an observation of it arrived
/// inside the window; any other vehicle's end is the open, so it is not
/// covered (#102). The same transaction closes the drives and charges still open at their last
/// evidence (M03-T20, `close_open_sessions_at_handback`). Idempotent: a
/// window already sealed is left as it is and closes nothing again. Returns
/// `false` when the store has no coverage table yet.
pub fn record_coverage_handback(
    connection: &Connection,
    window: &WindowCoverage,
) -> Result<bool, CoverageError> {
    if !coverage_table_present(connection)? {
        return Ok(false);
    }
    if window.closed_at_ms < window.opened_at_ms {
        return Err(CoverageError::Malformed(
            "a window closed before it opened".to_owned(),
        ));
    }
    let transaction = connection.unchecked_transaction()?;
    let existing: Option<(String, String)> = transaction
        .query_row(
            "SELECT grant_digest, sealed FROM collection_coverage WHERE seq = ?1",
            [window.seq],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((digest, sealed)) = &existing {
        if *digest != window.grant_digest {
            return Err(CoverageError::Conflict { seq: window.seq });
        }
        if sealed != COVERAGE_SEALED_OPEN {
            return Ok(true);
        }
    }
    let mut gaps = window.gaps.clone();
    for gap in store_gaps_within(&transaction, window.opened_at_ms, window.closed_at_ms, true)? {
        if !gaps.contains(&gap) {
            gaps.push(gap);
        }
    }
    let gaps_json = serde_json::to_string(&gaps)
        .map_err(|error| CoverageError::Malformed(error.to_string()))?;
    let ends_json =
        Value::Object(handback_vehicle_ends(&transaction, window.opened_at_ms, window.closed_at_ms)?)
            .to_string();
    if existing.is_some() {
        transaction.execute(
            "UPDATE collection_coverage
             SET opened_at_ms = ?2, closed_at_ms = ?3, gaps_json = ?4, sealed = ?5,
                 vehicle_ends_json = ?6
             WHERE seq = ?1",
            params![
                window.seq,
                window.opened_at_ms,
                window.closed_at_ms,
                gaps_json,
                COVERAGE_SEALED_HANDBACK,
                ends_json
            ],
        )?;
    } else {
        transaction.execute(
            "INSERT INTO collection_coverage
                 (seq, grant_digest, opened_at_ms, closed_at_ms, gaps_json, vehicle_ends_json, sealed)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                window.seq,
                window.grant_digest,
                window.opened_at_ms,
                window.closed_at_ms,
                gaps_json,
                ends_json,
                COVERAGE_SEALED_HANDBACK
            ],
        )?;
    }
    close_open_sessions_at_handback(&transaction, &|_| window.closed_at_ms)?;
    transaction.commit()?;
    Ok(true)
}

/// The end of a handed-back window per registered vehicle (#102): the close
/// for a vehicle with an observation received inside `[opened_ms, closed_ms]`,
/// the open (no coverage) for one without. Raw rows are pruned once the
/// lifecycle has applied them, so the newest row per record type in
/// `current_observations` counts too, as does a Hub-range row dated inside
/// the window (it exists only because an observation of the vehicle was
/// collected). The Hub's own lifecycle events and the import source's current
/// position are not evidence of collection.
fn handback_vehicle_ends(
    connection: &Connection,
    opened_ms: i64,
    closed_ms: i64,
) -> Result<serde_json::Map<String, Value>, CoverageError> {
    let raw = named_table_present(connection, "raw_observations")?;
    let current = named_table_present(connection, "current_observations")?;
    let vehicles: Vec<String> = {
        let mut statement =
            connection.prepare("SELECT vehicle_id FROM vehicles ORDER BY vehicle_id")?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    let mut ends = serde_json::Map::new();
    for vehicle in vehicles {
        let observed_in_raw = raw
            && connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM raw_observations
                  WHERE vehicle_id = ?1 AND received_at_ms BETWEEN ?2 AND ?3
                    AND json_extract(payload_json, '$.record_type')
                        IS NOT 'hub_lifecycle_event_v1')",
                params![vehicle, opened_ms, closed_ms],
                |row| row.get::<_, bool>(0),
            )?;
        let observed = observed_in_raw
            || (current
                && connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM current_observations
                      WHERE vehicle_id = ?1 AND received_at_ms BETWEEN ?2 AND ?3
                        AND record_type IS NOT ?4)",
                    params![
                        vehicle,
                        opened_ms,
                        closed_ms,
                        crate::collection::source_db_current::SOURCE_DB_POSITION_RECORD_TYPE
                    ],
                    |row| row.get::<_, bool>(0),
                )?);
        let observed = observed
            || latest_hub_row_moment(connection, &vehicle, opened_ms, Some(closed_ms))?
                .is_some();
        let end = if observed { closed_ms } else { opened_ms };
        ends.insert(vehicle, Value::from(end));
    }
    Ok(ends)
}

/// Closes, at their last evidence, the drives and charges the lifecycle still
/// holds open when coverage is handed back (M03-T20). The Hub collects no more
/// after a handback, so a session left open would never reach the
/// materialised tables, sync or the packs. For each vehicle with an open drive
/// or charge, one `handback` Hub event dated `at_ms(vehicle)` is stored and
/// applied through the ordinary intake, as the collector stores its `no_data`
/// event, so the close is replayable. The drive ends at its last fold and the
/// charge at its newest sample, never at the handback; their open children are
/// materialised under them with their sync upserts. A vehicle with nothing
/// open gets no event. Returns how many vehicles were closed.
fn close_open_sessions_at_handback(
    transaction: &Transaction<'_>,
    at_ms: &dyn Fn(&str) -> i64,
) -> Result<usize, CoverageError> {
    let lifecycle = |error: &dyn std::fmt::Display| CoverageError::Lifecycle(error.to_string());
    let vehicles: Vec<(String, String)> = {
        let mut statement = transaction.prepare(
            "SELECT state.vehicle_id, vehicles.source_id
               FROM vehicle_lifecycle_state AS state
               JOIN vehicles ON vehicles.vehicle_id = state.vehicle_id
              ORDER BY state.vehicle_id",
        )?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?
    };
    let mut closed = 0;
    for (vehicle, source) in vehicles {
        let (Ok(vehicle_id), Ok(source_id)) = (vehicle.parse::<Uuid>(), source.parse::<Uuid>())
        else {
            continue;
        };
        let Some(record) = load_lifecycle_state_in_transaction(transaction, vehicle_id)
            .map_err(|error| lifecycle(&error))?
        else {
            continue;
        };
        let Ok(mut state) = crate::lifecycle::OpenSessionState::decode(&record.open_session_json)
        else {
            continue;
        };
        if state.open_drive.is_none() && state.open_charge.is_none() {
            continue;
        }
        let at = at_ms(&vehicle);
        append_observation_in_transaction(
            transaction,
            &ObservationInput {
                source_id,
                vehicle_id,
                observed_at_ms: at,
                payload: serde_json::json!({
                    "record_type": "hub_lifecycle_event_v1",
                    "event": crate::lifecycle::HUB_EVENT_HANDBACK,
                }),
            },
            at,
        )
        .map_err(|error| lifecycle(&error))?;
        // The ordinary intake: every stored observation after the cursor.
        let observations = observations_after_id_in_transaction(
            transaction,
            vehicle_id,
            state.last_observation_id,
            MAX_OBSERVATION_QUERY_LIMIT,
        )
        .map_err(|error| lifecycle(&error))?;
        let mut delta = crate::lifecycle::LifecycleDelta::default();
        let mut quarantined = record.quarantined;
        for observation in observations {
            let sample = crate::lifecycle::LifecycleSample {
                observation_id: observation.observation_id,
                observed_at_ms: observation.observed_at_ms,
                vehicle_state: observation_vehicle_state(&observation.payload),
                payload: observation.payload,
            };
            let step = crate::lifecycle::apply_sample_with_offline_drive_timeout(
                state,
                record.car_id,
                &sample,
                crate::lifecycle::DEFAULT_OFFLINE_DRIVE_TIMEOUT,
            )
            .map_err(|error| lifecycle(&error))?;
            state = step.state;
            quarantined |= step.quarantined;
            crate::lifecycle::merge_step_delta(&mut delta, step.delta);
        }
        if let Some(open) = state.open_drive.as_mut() {
            open.positions.clear();
        }
        if let Some(open) = state.open_charge.as_mut() {
            open.samples.clear();
        }
        let encoded = state.encode().map_err(|error| lifecycle(&error))?;
        HubStore::commit_lifecycle_delta_in_transaction(
            transaction,
            &LifecycleCommit {
                vehicle_id,
                car_id: record.car_id,
                open_session_json: &encoded,
                last_observation_id: state.last_observation_id,
                quarantined,
                updated_at_ms: at.max(record.updated_at_ms),
                delta: &delta,
            },
        )
        .map_err(|error| lifecycle(&error))?;
        closed += 1;
    }
    Ok(closed)
}

/// The end of a restored window for `vehicle`: the instant after the latest
/// moment, from `opened_ms` on, of its Hub-range drives, charges and
/// positions, and of the Hub-range positions and charge samples of sessions
/// still open in the lifecycle (M03-T18), so the half-open coverage holds that
/// last row too; `opened_ms` when it has none.
fn last_hub_row_ms(
    connection: &Connection,
    vehicle: &str,
    opened_ms: i64,
) -> Result<i64, CoverageError> {
    let latest = latest_hub_row_moment(connection, vehicle, opened_ms, None)?;
    Ok(latest
        .map_or(opened_ms, |latest| latest.saturating_add(1))
        .max(opened_ms))
}

/// The latest moment of `vehicle`'s Hub-range rows (as `last_hub_row_ms`
/// reads them) from `from_ms` on, and up to `to_ms` when given.
fn latest_hub_row_moment(
    connection: &Connection,
    vehicle: &str,
    from_ms: i64,
    to_ms: Option<i64>,
) -> Result<Option<i64>, CoverageError> {
    Ok(connection.query_row(
        "SELECT MAX(moment) FROM (
             SELECT CAST(COALESCE(json_extract(drive_json, '$.end_date_ms'),
                                  json_extract(drive_json, '$.start_date_ms')) AS INTEGER) AS moment
               FROM materialised_drives
              WHERE vehicle_id = ?1 AND drive_id BETWEEN ?2 AND ?3
             UNION ALL
             SELECT CAST(COALESCE(json_extract(charge_json, '$.end_date_ms'),
                                  json_extract(charge_json, '$.start_date_ms')) AS INTEGER)
               FROM materialised_charges
              WHERE vehicle_id = ?1 AND charge_id BETWEEN ?2 AND ?3
             UNION ALL
             SELECT CAST(json_extract(position_json, '$.date_ms') AS INTEGER)
               FROM materialised_positions
              WHERE vehicle_id = ?1 AND position_id BETWEEN ?2 AND ?3
             UNION ALL
             SELECT CAST(COALESCE(json_extract(row_json, '$.date_ms'),
                                  json_extract(row_json, '$.timestamp_ms')) AS INTEGER)
               FROM lifecycle_open_rows
              WHERE vehicle_id = ?1 AND domain IN ('position', 'charge_sample')
                AND source_row_id BETWEEN ?2 AND ?3
         ) WHERE moment >= ?4 AND (?5 IS NULL OR moment <= ?5)",
        params![
            vehicle,
            hub_ids::HUB_ID_BASE,
            hub_ids::HUB_ID_MAX,
            from_ms,
            to_ms
        ],
        |row| row.get(0),
    )?)
}

/// Seals every window that is open in the store, except `running` (the grant
/// sequence and digest of a window open on this host). Such a window is the
/// mark of a restore of a backup taken inside a window: no window is open on
/// this host, so the window is closed per vehicle just after that vehicle's
/// last Hub-range row (`sealed = 'restore'`, `vehicle_ends_json`, the end
/// excluded), and the Hub's rows inside it, the last one included, stand, as
/// in a closed window. When it seals one and no window runs here, the drives
/// and charges still open close at their last evidence, the event dated at
/// that vehicle's end (M03-T20); a window running here owns them and closes
/// them by its own evidence. Returns how many were sealed.
pub fn seal_restored_open_windows(
    connection: &Connection,
    running: Option<(i64, &str)>,
) -> Result<usize, CoverageError> {
    if !coverage_table_present(connection)? {
        return Ok(0);
    }
    let transaction = connection.unchecked_transaction()?;
    let open: Vec<(i64, String, i64)> = {
        let mut statement = transaction.prepare(
            "SELECT seq, grant_digest, opened_at_ms FROM collection_coverage
             WHERE closed_at_ms IS NULL ORDER BY seq",
        )?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<_, _>>()?
    };
    let vehicles: Vec<String> = {
        let mut statement = transaction.prepare("SELECT vehicle_id FROM vehicles ORDER BY vehicle_id")?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    let mut sealed = 0;
    let mut last_ends: Option<(serde_json::Map<String, Value>, i64)> = None;
    for (seq, digest, opened_ms) in open {
        if running.is_some_and(|(running_seq, running_digest)| {
            running_seq == seq && running_digest == digest
        }) {
            continue;
        }
        let mut ends = serde_json::Map::new();
        let mut closed_ms = opened_ms;
        for vehicle in &vehicles {
            let end = last_hub_row_ms(&transaction, vehicle, opened_ms)?;
            closed_ms = closed_ms.max(end);
            ends.insert(vehicle.clone(), Value::from(end));
        }
        let gaps = store_gaps_within(&transaction, opened_ms, closed_ms, false)?;
        let gaps_json = serde_json::to_string(&gaps)
            .map_err(|error| CoverageError::Malformed(error.to_string()))?;
        transaction.execute(
            "UPDATE collection_coverage
             SET closed_at_ms = ?2, vehicle_ends_json = ?3, sealed = ?4, gaps_json = ?5
             WHERE seq = ?1 AND closed_at_ms IS NULL",
            params![
                seq,
                closed_ms,
                Value::Object(ends.clone()).to_string(),
                COVERAGE_SEALED_RESTORE,
                gaps_json,
            ],
        )?;
        sealed += 1;
        last_ends = Some((ends, closed_ms));
    }
    if running.is_none()
        && let Some((ends, closed_ms)) = last_ends
    {
        close_open_sessions_at_handback(&transaction, &|vehicle| {
            ends.get(vehicle).and_then(Value::as_i64).unwrap_or(closed_ms)
        })?;
    }
    transaction.commit()?;
    Ok(sealed)
}

/// Merges `pieces` into sorted, disjoint, half-open intervals.
fn merge_coverage_pieces(mut pieces: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    pieces.retain(|(start, end)| end > start);
    pieces.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::with_capacity(pieces.len());
    for (start, end) in pieces {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// The intervals, in milliseconds, in which the Hub collected `vehicle`
/// itself: the union of the recorded windows minus the gaps whose vehicle is
/// absent or is this one. A window sealed by a restore ends at that
/// vehicle's own end. A store with no coverage table covers nothing; a window
/// still open in the store refuses, as its cutoff is not recorded.
pub fn coverage_intervals_for_vehicle(
    connection: &Connection,
    vehicle: &str,
) -> Result<Vec<(i64, i64)>, CoverageError> {
    coverage_intervals_in(connection, vehicle, false)
}

/// The digest of a vehicle's coverage intervals (M03-T22): what a planned
/// physical head is bound to, so it is served only while the coverage it was
/// planned with is the vehicle's coverage.
pub fn coverage_digest(intervals: &[(i64, i64)]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"teslatlas-hub collection coverage v1\n"); // rename:keep
    for (start, end) in intervals {
        hash.update(format!("{start} {end}\n"));
    }
    crate::protocol::Sha256Digest::from_bytes(hash.finalize().into()).to_string()
}

/// The digest of the vehicle's coverage from its closed windows (M03-T22): a
/// window still open adds nothing until it is handed back, so a head planned
/// before it is served while the window runs.
pub fn closed_coverage_digest_for_vehicle(
    connection: &Connection,
    vehicle: &str,
) -> Result<String, CoverageError> {
    Ok(coverage_digest(&coverage_intervals_in(connection, vehicle, true)?))
}

/// The windows the window ledger has handed back whose coverage write failed
/// in this process, per store (M03-T22). While a store has one its coverage
/// lacks a handback, so none of its planned physical heads is served (a head
/// planned before the window would be stale) and none is planned from it,
/// until a write of that handback (the supervisor's retry, the next start or
/// import admission) closes the window's row. In memory only: a start
/// reconciles the ledger with the store before anything is served.
static UNRECONCILED_COVERAGE: std::sync::Mutex<UnreconciledCoverage> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

type UnreconciledCoverage = std::collections::BTreeMap<PathBuf, std::collections::BTreeSet<i64>>;

fn unreconciled_coverage() -> std::sync::MutexGuard<'static, UnreconciledCoverage> {
    UNRECONCILED_COVERAGE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn unreconciled_coverage_key(database: &Path) -> PathBuf {
    std::fs::canonicalize(database).unwrap_or_else(|_| database.to_path_buf())
}

/// Records that the handback of window `seq` did not reach the coverage of
/// the store in `data_dir` (M03-T22).
pub fn mark_coverage_unreconciled(data_dir: &Path, seq: i64) {
    let key = unreconciled_coverage_key(&data_dir.join("hub.sqlite"));
    unreconciled_coverage().entry(key).or_default().insert(seq);
}

/// Whether this process holds a handback of the store in `data_dir` that its
/// coverage may still lack (M03-T22); it reads no store.
pub fn coverage_unreconciled_pending(data_dir: &Path) -> bool {
    let registry = unreconciled_coverage();
    !registry.is_empty()
        && registry
            .get(&unreconciled_coverage_key(&data_dir.join("hub.sqlite")))
            .is_some_and(|seqs| !seqs.is_empty())
}

/// Whether the coverage of the store at `database` still lacks a handback
/// this process failed to write (M03-T22). A window whose row the store now
/// holds closed is forgotten.
pub fn coverage_unreconciled_in(
    connection: &Connection,
    database: &Path,
) -> Result<bool, CoverageError> {
    let key = {
        let registry = unreconciled_coverage();
        if registry.is_empty() {
            return Ok(false);
        }
        let key = unreconciled_coverage_key(database);
        if registry.get(&key).is_none_or(|seqs| seqs.is_empty()) {
            return Ok(false);
        }
        key
    };
    let seqs = unreconciled_coverage().get(&key).cloned().unwrap_or_default();
    let mut closed = Vec::new();
    if coverage_table_present(connection)? {
        for seq in &seqs {
            let closed_at: Option<Option<i64>> = connection
                .query_row(
                    "SELECT closed_at_ms FROM collection_coverage WHERE seq = ?1",
                    [seq],
                    |row| row.get(0),
                )
                .optional()?;
            if matches!(closed_at, Some(Some(_))) {
                closed.push(*seq);
            }
        }
    }
    let mut registry = unreconciled_coverage();
    let Some(pending) = registry.get_mut(&key) else {
        return Ok(false);
    };
    for seq in closed {
        pending.remove(&seq);
    }
    let unreconciled = !pending.is_empty();
    if !unreconciled {
        registry.remove(&key);
    }
    Ok(unreconciled)
}

fn coverage_intervals_in(
    connection: &Connection,
    vehicle: &str,
    skip_open: bool,
) -> Result<Vec<(i64, i64)>, CoverageError> {
    if !coverage_table_present(connection)? {
        return Ok(Vec::new());
    }
    let mut statement = connection.prepare(
        "SELECT seq, opened_at_ms, closed_at_ms, gaps_json, vehicle_ends_json
         FROM collection_coverage ORDER BY seq",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut pieces = Vec::new();
    for row in rows {
        let (seq, opened_ms, closed_ms, gaps_json, ends_json) = row?;
        let Some(closed_ms) = closed_ms else {
            if skip_open {
                continue;
            }
            return Err(CoverageError::WindowOpen { seq });
        };
        let end_ms = match ends_json {
            Some(text) => {
                let ends: serde_json::Map<String, Value> = serde_json::from_str(&text)
                    .map_err(|error| CoverageError::Malformed(error.to_string()))?;
                ends.get(vehicle)
                    .and_then(Value::as_i64)
                    .map_or(opened_ms, |end| end.clamp(opened_ms, closed_ms))
            }
            None => closed_ms,
        };
        let gaps: Vec<CoverageGap> = serde_json::from_str(&gaps_json)
            .map_err(|error| CoverageError::Malformed(error.to_string()))?;
        let mut gaps: Vec<(i64, i64)> = gaps
            .into_iter()
            .filter(|gap| gap.vehicle_id.as_deref().is_none_or(|id| id == vehicle))
            .map(|gap| (gap.start_ms.max(opened_ms), gap.end_ms.min(end_ms)))
            .filter(|(start, end)| end > start)
            .collect();
        gaps.sort_unstable();
        let mut cursor = opened_ms;
        for (start, end) in gaps {
            if start > cursor {
                pieces.push((cursor, start));
            }
            cursor = cursor.max(end);
        }
        if end_ms > cursor {
            pieces.push((cursor, end_ms));
        }
    }
    Ok(merge_coverage_pieces(pieces))
}
