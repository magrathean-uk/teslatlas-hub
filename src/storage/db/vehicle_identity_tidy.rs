// SPDX-License-Identifier: AGPL-3.0-only

// The one-time tidy of stored vehicle identity values (C6 Part I, C6 R54 to
// R57; owner answer OW1).
//
// The tidy clears and rederives the identity names the replaced live projection
// stored for live vehicles: `model`, `trim_badging` and `marketing_name` in
// `materialised_cars.car_json`, and the same three patch fields of
// `car_metadata` in the lifecycle blob. It runs in `HubStore::initialize`, the
// read-write opening sequence that migrates, after the last migration and
// before any collector, import, publication, live-sync or HTTP work starts, and
// at most once per store: the marker row `vehicle_identity_tidy_v1` in
// `store_data_steps` (C6 R63) is inserted in the transaction that completes
// it. A read-only open, a diagnostic open and `open_existing` never run it
// (C6 R54).
//
// It decides by vehicle source kind and by column, never by value (C6 R55):
// every value in an owned column of a live vehicle is cleared and rederived
// through the descriptor functions (C6 R56) whoever wrote it, and an imported
// vehicle is not touched at all. It keeps no list of the replaced code's
// spellings and calls none of its functions. A VIN is personal data, so the
// summary, the error text and every comment here hold no VIN, name, vehicle id
// or stored value (C6 R19, R57).
//
// This file is pulled into `db.rs` by `include!`, so it carries `//` and `///`
// comments only and no inner attributes.

/// The marker key of the identity tidy in `store_data_steps` (C6 section 5,
/// R57). One key per one-time step; the version suffix lets a later owner
/// instruction add its own step without reusing this one.
pub(crate) const VEHICLE_IDENTITY_TIDY_STEP_KEY: &str = "vehicle_identity_tidy_v1";

/// The longest lifecycle blob `vehicle_lifecycle_state` accepts (its CHECK).
/// C1's codec refuses a blob over its own, larger cap; the table's bound is
/// the one a write to the table meets, so a re-encoded blob over it counts as
/// refused by the codec (C6 R56 skip) rather than aborting the whole tidy.
const TIDY_LIFECYCLE_BLOB_MAX_BYTES: usize = 65_536;

/// The two stored vehicle-data record types whose rows state the identity keys
/// (C6 R56 1, R22).
const TIDY_VEHICLE_DATA_TYPES: [&str; 2] =
    ["owner_api_vehicle_data_v1", "fleet_api_vehicle_data_v1"];

/// The six Hub model codes (C6 R4), for the `models_unrecognised` count. The
/// count is checked against these codes and not through `vehicle_model_name`
/// (C6 R57, C6 ST-I17).
const TIDY_MODEL_CODES: [&str; 6] = ["S", "X", "3", "Y", "R", "C"];

/// The counts the tidy records in its marker (C6 R57), exactly these six keys
/// and nothing else: no VIN, name, vehicle id or stored value.
///
/// `live_vehicles` counts every vehicle without import evidence, including a
/// live vehicle the tidy skipped as unreadable; `models_unrecognised` counts
/// the live vehicles it tidied whose model is neither `""` nor a Hub code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub(crate) struct VehicleIdentityTidySummary {
    pub live_vehicles: u64,
    pub imported_vehicles_skipped: u64,
    pub vehicles_skipped_unreadable: u64,
    pub cars_changed: u64,
    pub lifecycle_records_changed: u64,
    pub models_unrecognised: u64,
}

/// A vehicle-data row of one vehicle, as the tidy reads it (C6 R22).
struct TidyObservationRow {
    observation_id: i64,
    observed_at_ms: i64,
    payload: Value,
}

/// The newest statement of one key (C6 R23, R24): its order key and, when the
/// value is a JSON string, the text. A value of another JSON type, including
/// null, is the newest statement and carries no text (C6 R56 3, 4: the stored
/// value is a fallback for the model only, never for the trim).
struct NewestStatement {
    time_ms: i64,
    observation_id: i64,
    text: Option<String>,
}

/// The newest statements of the three keys the tidy reads (C6 R56 1): the root
/// `vin`, `vehicle_config.car_type` and `vehicle_config.trim_badging`.
#[derive(Default)]
struct IdentityStatements {
    vin: Option<NewestStatement>,
    car_type: Option<NewestStatement>,
    trim_badging: Option<NewestStatement>,
}

impl IdentityStatements {
    fn text(statement: &Option<NewestStatement>) -> Option<&str> {
        statement
            .as_ref()
            .and_then(|statement| statement.text.as_deref())
    }

    /// Keep the statement with the greatest statement time, then the greatest
    /// observation id (C6 R24).
    fn keep_newest(slot: &mut Option<NewestStatement>, candidate: NewestStatement) {
        let replace = match slot {
            Some(current) => {
                (candidate.time_ms, candidate.observation_id)
                    > (current.time_ms, current.observation_id)
            }
            None => true,
        };
        if replace {
            *slot = Some(candidate);
        }
    }
}

/// The newest identity statements of a vehicle's vehicle-data rows (C6 R22 to
/// R24). A row counts when its payload is an object whose root `record_type` is
/// one of the two vehicle-data types. Its data object is
/// `provider_raw_json.response` when that is an object, else `vehicle_data`
/// (C1 R3). A full source states every key it carries, including JSON null; a
/// sparse source (a non-empty root `provider_feed` string, C1 R76) states only
/// the keys whose value is not null. The root `vin` is stated at the row's
/// `observed_at_ms`; the keys of `vehicle_config` at that group's time, the
/// group's own `timestamp` when plausible (C1 R5, `group_time`), else
/// `observed_at_ms`. Stream and discovery rows carry none of these keys.
fn newest_identity_statements(rows: &[TidyObservationRow]) -> IdentityStatements {
    let mut statements = IdentityStatements::default();
    for row in rows {
        let Some(root) = row.payload.as_object() else {
            continue;
        };
        let accepted = root
            .get("record_type")
            .and_then(Value::as_str)
            .is_some_and(|record_type| TIDY_VEHICLE_DATA_TYPES.contains(&record_type));
        if !accepted {
            continue;
        }
        let Some(data) = root
            .get("provider_raw_json")
            .and_then(|raw| raw.get("response"))
            .and_then(Value::as_object)
            .or_else(|| root.get("vehicle_data").and_then(Value::as_object))
        else {
            continue;
        };
        let sparse = root
            .get("provider_feed")
            .and_then(Value::as_str)
            .is_some_and(|feed| !feed.trim().is_empty());
        let states = |value: &&Value| !sparse || !value.is_null();
        let candidate = |value: &Value, time_ms: i64| NewestStatement {
            time_ms,
            observation_id: row.observation_id,
            text: value.as_str().map(str::to_owned),
        };
        if let Some(value) = data.get("vin").filter(states) {
            IdentityStatements::keep_newest(
                &mut statements.vin,
                candidate(value, row.observed_at_ms),
            );
        }
        if let Some(config) = data.get("vehicle_config").and_then(Value::as_object) {
            let time_ms = crate::lifecycle::group_time(Some(config), row.observed_at_ms);
            if let Some(value) = config.get("car_type").filter(states) {
                IdentityStatements::keep_newest(
                    &mut statements.car_type,
                    candidate(value, time_ms),
                );
            }
            if let Some(value) = config.get("trim_badging").filter(states) {
                IdentityStatements::keep_newest(
                    &mut statements.trim_badging,
                    candidate(value, time_ms),
                );
            }
        }
    }
    statements
}

/// A vehicle's stored lifecycle record, as far as the tidy reads it.
struct StoredLifecycle<'a> {
    open_session_json: &'a [u8],
    quarantined: bool,
}

/// What the tidy writes for one vehicle (C6 R56): each part only when its
/// bytes change.
struct VehicleIdentityTidyWrites {
    /// The new `car_json` and the car's id, when the stored car changes.
    car: Option<(String, i64)>,
    /// The new lifecycle blob, when it changes.
    lifecycle_blob: Option<Vec<u8>>,
    /// The model is neither `""` nor one of the six Hub codes.
    model_unrecognised: bool,
}

enum VehicleIdentityTidyPlan {
    /// A skip: neither part is written and the vehicle is counted (C6 R56).
    Unreadable,
    Tidy(VehicleIdentityTidyWrites),
}

/// C6 R56: the writes for one live vehicle, from the stored car, the stored
/// lifecycle record and the newest statements, all or nothing.
///
/// The VIN is the stored car's when it has one, else the newest root `vin`
/// statement when that is a string. The model is `vehicle_model_code(VIN,
/// text)` with the text the newest `car_type` statement when that is a string,
/// else the stored model (no text without a stored car), written as the code or
/// `""`. The trim is `live_config_text` of the newest `trim_badging` statement
/// when that is a string, else `None`; the stored trim is never a fallback. The
/// marketing name is `None`. Every other field of the car, of the blob and of
/// the record stays as stored.
///
/// The vehicle is a skip, with nothing written, when its `car_json` does not
/// deserialise as `ProjectionCar`, its lifecycle record is quarantined, its
/// blob does not decode, or C1's codec refuses the re-encoded blob (here also
/// when the table's own bound would). A car whose id is not positive cannot
/// carry the sync mutation of a change and is a skip when a change is planned.
fn plan_vehicle_identity_tidy(
    car_json: Option<&str>,
    lifecycle: Option<StoredLifecycle<'_>>,
    statements: &IdentityStatements,
) -> VehicleIdentityTidyPlan {
    let car: Option<ProjectionCar> = match car_json {
        Some(json) => match serde_json::from_str(json) {
            Ok(car) => Some(car),
            Err(_) => return VehicleIdentityTidyPlan::Unreadable,
        },
        None => None,
    };
    let state = match &lifecycle {
        Some(stored) => {
            if stored.quarantined {
                return VehicleIdentityTidyPlan::Unreadable;
            }
            match crate::lifecycle::OpenSessionState::decode(stored.open_session_json) {
                Ok(state) => Some(state),
                Err(_) => return VehicleIdentityTidyPlan::Unreadable,
            }
        }
        None => None,
    };

    let vin = car
        .as_ref()
        .and_then(|car| car.vin.as_deref())
        .or_else(|| IdentityStatements::text(&statements.vin));
    let text = IdentityStatements::text(&statements.car_type)
        .or_else(|| car.as_ref().map(|car| car.model.as_str()));
    let model = crate::hub_pack::vehicle_model_code(vin, text);
    let trim =
        crate::hub_pack::live_config_text(IdentityStatements::text(&statements.trim_badging));
    let model_unrecognised = model
        .as_deref()
        .is_some_and(|code| !TIDY_MODEL_CODES.contains(&code));

    let mut car_write = None;
    if let (Some(mut car), Some(stored_json)) = (car, car_json) {
        car.model = model.clone().unwrap_or_default();
        car.trim_badging = trim.clone();
        car.marketing_name = None;
        let Ok(tidied) = serde_json::to_string(&car) else {
            return VehicleIdentityTidyPlan::Unreadable;
        };
        if tidied != stored_json {
            if car.id <= 0 {
                return VehicleIdentityTidyPlan::Unreadable;
            }
            car_write = Some((tidied, car.id));
        }
    }

    let mut blob_write = None;
    if let (Some(mut state), Some(stored)) = (state, &lifecycle)
        && let Some(patch) = state.car_metadata.as_mut()
    {
        patch.model = model.clone();
        patch.trim_badging = trim;
        patch.marketing_name = None;
        let Ok(encoded) = state.encode() else {
            return VehicleIdentityTidyPlan::Unreadable;
        };
        if encoded.len() > TIDY_LIFECYCLE_BLOB_MAX_BYTES {
            return VehicleIdentityTidyPlan::Unreadable;
        }
        if encoded != stored.open_session_json {
            blob_write = Some(encoded);
        }
    }

    VehicleIdentityTidyPlan::Tidy(VehicleIdentityTidyWrites {
        car: car_write,
        lifecycle_blob: blob_write,
        model_unrecognised,
    })
}

/// C6 R55: whether the store links a vehicle to an import. Any one piece of
/// evidence excludes the entire vehicle; every doubt resolves to "imported",
/// because a wrong "live" would touch imported source values.
///
/// (a) The stored car has a `source_model_text` that is not null (only the
/// import sets it, C6 R13a); `car_json` is read as JSON, so an unreadable car
/// still shows it. (b) The vehicle has a `current_observations` row of the
/// import-source position type, read through its one definition (C6 R64).
/// (c) The store links the vehicle to an import source, by the stored source
/// kind equal to the import source kind (its one definition, C6 R64, R70), in
/// `vehicles.source_id`, `vehicle_identity_aliases.source_id` (a live vehicle
/// that an import later reused is linked only there) or
/// `lifecycle_source_watermarks.source_id`; or through the import's own record
/// of the cars it projected: the vehicle's import projection head row, its
/// projection-state head row and any `import_generations` row (`vehicle_id` in
/// each of the three tables).
fn vehicle_has_import_evidence(
    transaction: &Transaction<'_>,
    vehicle_id: &str,
    car_json: Option<&str>,
) -> Result<bool, StoreError> {
    let car_states_source_text = car_json
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
        .and_then(|car| car.get("source_model_text").map(|text| !text.is_null()))
        .unwrap_or(false);
    if car_states_source_text {
        return Ok(true);
    }
    transaction
        .query_row(
            "SELECT
                EXISTS(SELECT 1 FROM current_observations
                        WHERE vehicle_id = ?1 AND record_type = ?3)
                OR EXISTS(SELECT 1 FROM vehicles AS v
                            JOIN sources AS s ON s.source_id = v.source_id
                           WHERE v.vehicle_id = ?1 AND s.source_kind = ?2)
                OR EXISTS(SELECT 1 FROM vehicle_identity_aliases AS a
                            JOIN sources AS s ON s.source_id = a.source_id
                           WHERE a.vehicle_id = ?1 AND s.source_kind = ?2)
                OR EXISTS(SELECT 1 FROM lifecycle_source_watermarks AS w
                            JOIN sources AS s ON s.source_id = w.source_id
                           WHERE w.vehicle_id = ?1 AND s.source_kind = ?2)
                OR EXISTS(SELECT 1 FROM teslamate_import_projection_heads
                           WHERE vehicle_id = ?1)
                OR EXISTS(SELECT 1 FROM teslamate_import_projection_state_heads
                           WHERE vehicle_id = ?1)
                OR EXISTS(SELECT 1 FROM import_generations WHERE vehicle_id = ?1)",
            params![
                vehicle_id,
                IMPORT_SOURCE_KIND,
                crate::collection::teslamate_current::IMPORT_POSITION_RECORD_TYPE
            ],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)
}

/// The vehicle-data rows of a vehicle (C6 R56 1), oldest id first. A row whose
/// payload text is not JSON is no row (the table's CHECK keeps it out).
fn load_identity_observation_rows(
    transaction: &Transaction<'_>,
    vehicle_id: &str,
) -> Result<Vec<TidyObservationRow>, StoreError> {
    let mut statement = transaction
        .prepare_cached(
            "SELECT observation_id, observed_at_ms, payload_json
               FROM current_observations
              WHERE vehicle_id = ?1 AND record_type IN (?2, ?3)
              ORDER BY observation_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(
            params![
                vehicle_id,
                TIDY_VEHICLE_DATA_TYPES[0],
                TIDY_VEHICLE_DATA_TYPES[1]
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(StoreError::Query)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::Query)?;
    Ok(rows
        .into_iter()
        .filter_map(|(observation_id, observed_at_ms, payload)| {
            Some(TidyObservationRow {
                observation_id,
                observed_at_ms,
                payload: serde_json::from_str(&payload).ok()?,
            })
        })
        .collect())
}

/// C6 R54 to R57: the identity tidy, in one write transaction that takes the
/// write lock before its first read (`BEGIN IMMEDIATE`).
///
/// Inside it, the marker row is checked first: when it exists the step ends
/// with no other read or write and the result is `None`. Otherwise every live
/// vehicle is tidied (C6 R56), the marker is inserted with the six counts and
/// the transaction commits once; the result is the counts. A storage error
/// drops the transaction (a rollback: no partial tidy, no marker, so the next
/// open runs the step again from the same inputs) and is returned as
/// [`StoreError::VehicleIdentityTidy`], whose text names the step and holds no
/// value. A per-vehicle problem is a skip, never an error.
///
/// Each car whose stored bytes change is published to live sync the way the
/// retained lifecycle writer publishes a changed car: one `car` upsert sync
/// mutation, and the export is marked dirty so that the next legacy pack
/// carries the tidied car. A car whose bytes do not change records nothing.
///
/// `clock` supplies `completed_at_ms` and is read only when the step runs.
pub(crate) fn run_vehicle_identity_tidy(
    connection: &mut Connection,
    clock: impl FnOnce() -> Result<i64, StoreError>,
) -> Result<Option<VehicleIdentityTidySummary>, StoreError> {
    tidy_vehicle_identities(connection, clock)
        .map_err(|error| StoreError::VehicleIdentityTidy(Box::new(error)))
}

fn tidy_vehicle_identities(
    connection: &mut Connection,
    clock: impl FnOnce() -> Result<i64, StoreError>,
) -> Result<Option<VehicleIdentityTidySummary>, StoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(StoreError::Begin)?;
    let done: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM store_data_steps WHERE step_key = ?1)",
            params![VEHICLE_IDENTITY_TIDY_STEP_KEY],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    if done {
        return Ok(None);
    }

    // Every vehicle with a stored car or a lifecycle record (C6 R55).
    let vehicle_ids = {
        let mut statement = transaction
            .prepare(
                "SELECT vehicle_id FROM materialised_cars
                 UNION
                 SELECT vehicle_id FROM vehicle_lifecycle_state
                 ORDER BY 1",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(StoreError::Query)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Query)?
    };

    let mut summary = VehicleIdentityTidySummary::default();
    for vehicle_id in &vehicle_ids {
        let car: Option<(i64, String)> = transaction
            .query_row(
                "SELECT car_id, car_json FROM materialised_cars WHERE vehicle_id = ?1",
                params![vehicle_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let car_json = car.as_ref().map(|(_, json)| json.as_str());
        if vehicle_has_import_evidence(&transaction, vehicle_id, car_json)? {
            summary.imported_vehicles_skipped += 1;
            continue;
        }
        summary.live_vehicles += 1;

        let lifecycle: Option<(Vec<u8>, bool)> = transaction
            .query_row(
                "SELECT open_session_json, quarantined FROM vehicle_lifecycle_state
                  WHERE vehicle_id = ?1",
                params![vehicle_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let statements =
            newest_identity_statements(&load_identity_observation_rows(&transaction, vehicle_id)?);
        let plan = plan_vehicle_identity_tidy(
            car_json,
            lifecycle
                .as_ref()
                .map(|(open_session_json, quarantined)| StoredLifecycle {
                    open_session_json,
                    quarantined: *quarantined,
                }),
            &statements,
        );
        let VehicleIdentityTidyPlan::Tidy(writes) = plan else {
            summary.vehicles_skipped_unreadable += 1;
            continue;
        };
        // The vehicle's sync identity is read before any write, so that a
        // vehicle the sync log cannot name is skipped whole.
        let sync_vehicle = match (&writes.car, Uuid::parse_str(vehicle_id)) {
            (Some(_), Ok(uuid)) => Some(uuid),
            (Some(_), Err(_)) => {
                summary.vehicles_skipped_unreadable += 1;
                continue;
            }
            (None, _) => None,
        };
        if writes.model_unrecognised {
            summary.models_unrecognised += 1;
        }
        if let (Some((car_json, car_entity_id)), Some(sync_vehicle), Some((car_row_id, _))) =
            (&writes.car, sync_vehicle, &car)
        {
            transaction
                .execute(
                    "UPDATE materialised_cars SET car_json = ?2 WHERE vehicle_id = ?1",
                    params![vehicle_id, car_json],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                &transaction,
                sync_vehicle,
                "car",
                *car_entity_id,
                *car_row_id,
                "upsert",
                car_json,
            )?;
            mark_export_dirty_in_transaction(&transaction, sync_vehicle)?;
            summary.cars_changed += 1;
        }
        if let Some(blob) = &writes.lifecycle_blob {
            transaction
                .execute(
                    "UPDATE vehicle_lifecycle_state SET open_session_json = ?2
                      WHERE vehicle_id = ?1",
                    params![vehicle_id, blob],
                )
                .map_err(StoreError::LifecycleWrite)?;
            summary.lifecycle_records_changed += 1;
        }
    }

    let completed_at_ms = clock()?;
    let summary_json =
        serde_json::to_string(&summary).map_err(StoreError::SerializeLifecycleRow)?;
    transaction
        .execute(
            "INSERT INTO store_data_steps(step_key, completed_at_ms, summary_json)
             VALUES (?1, ?2, ?3)",
            params![
                VEHICLE_IDENTITY_TIDY_STEP_KEY,
                completed_at_ms,
                summary_json
            ],
        )
        .map_err(StoreError::LifecycleWrite)?;
    transaction.commit().map_err(StoreError::LifecycleWrite)?;
    Ok(Some(summary))
}
