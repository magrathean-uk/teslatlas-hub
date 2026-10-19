// SPDX-License-Identifier: AGPL-3.0-only

//! Pure helpers of the current-vehicle summary (C6 Part S1): which stored rows
//! count (C6 R22), the statements they make and their times (C6 R23), the merge
//! (C6 R24, R25, R26), the field readers (C6 R27), transient visibility (C6
//! R28), the activity state, its start and processing health (C6 R29, R30,
//! R31) and the newest-observation time (C6 R32).
//!
//! Everything here is a pure function of its arguments: no clock, storage,
//! configuration or randomness, and nothing is logged (C6 R21, R45).

use serde_json::{Map, Value};
use uuid::Uuid;

use super::ActivityState;
use crate::{
    collection::teslamate_current::IMPORT_POSITION_RECORD_TYPE,
    db::{LifecycleStateRecord, ObservationRecord},
    lifecycle::{
        DEFAULT_OFFLINE_DRIVE_TIMEOUT, K_SKEW_MS, OpenSessionState, VehiclePhase, group_time,
        normalise_enum_text, round_to_precision,
    },
};

/// Longest text the summary serves, in Unicode scalar values (C6 section 5):
/// the `maxLength` of every string in the published `current` contract.
pub(super) const K_TEXT_MAX: usize = 65_536;

/// The stored vocabulary of the rows the summary reads (C6 R22). The
/// import-source position type is read through its one definition (C6 R64,
/// R70) and is not written here.
const OWNER_VEHICLE_DATA: &str = "owner_api_vehicle_data_v1";
const FLEET_VEHICLE_DATA: &str = "fleet_api_vehicle_data_v1";
const OWNER_DISCOVERY: &str = "owner_api_discovery_v1";
const FLEET_DISCOVERY: &str = "fleet_api_discovery_v1";
const STREAM_UPDATE: &str = "tesla_stream_update_v1";

/// The group objects whose keys are statements (C6 R23). `software_update` is
/// the object nested in `vehicle_state` and is read through [`NESTED_GROUP`].
const GROUPS: [&str; 6] = [
    "drive_state",
    "charge_state",
    "climate_state",
    "vehicle_state",
    "vehicle_config",
    "gui_settings",
];
const NESTED_PARENT: &str = "vehicle_state";
const NESTED_KEY: &str = "software_update";
const NESTED_GROUP: &str = "vehicle_state.software_update";
/// Root keys of a vehicle-data object that are statements (C6 R23).
const ROOT_GROUP: &str = "root";
const ROOT_KEYS: [&str; 2] = ["display_name", "vin"];
/// Keys of an import-source position row that are statements (C6 R23).
const IMPORT_GROUP: &str = "import_position";
const IMPORT_KEYS: [&str; 5] = [
    "battery_level",
    "ideal_battery_range_km",
    "est_battery_range_km",
    "rated_battery_range_km",
    "odometer_km",
];

/// What kind of observation an accepted row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowKind {
    VehicleData,
    Discovery,
    Stream,
    ImportPosition,
}

/// One row the summary accepts (C6 R22).
pub(super) struct AcceptedRow<'a> {
    pub(super) kind: RowKind,
    pub(super) record: &'a ObservationRecord,
    root: &'a Map<String, Value>,
}

impl<'a> AcceptedRow<'a> {
    /// The object that carries the Tesla groups (C6 R23): a vehicle-data row's
    /// `provider_raw_json.response` when that is an object, else its
    /// `vehicle_data` (C1 R3); a stream row's `fields`.
    fn data_object(&self) -> Option<&'a Map<String, Value>> {
        match self.kind {
            RowKind::VehicleData => self
                .root
                .get("provider_raw_json")
                .and_then(|raw| raw.get("response"))
                .and_then(Value::as_object)
                .or_else(|| self.root.get("vehicle_data").and_then(Value::as_object)),
            RowKind::Stream => self.root.get("fields").and_then(Value::as_object),
            RowKind::Discovery | RowKind::ImportPosition => None,
        }
    }

    /// The root `source_vehicle_state` text, when the row carries one.
    fn source_vehicle_state(&self) -> Option<&'a str> {
        self.root
            .get("source_vehicle_state")
            .and_then(Value::as_str)
    }

    /// A sparse source (C6 R23, C1 R76): a non-empty root `provider_feed`
    /// string. Its nulls are no statements.
    fn is_sparse(&self) -> bool {
        self.root
            .get("provider_feed")
            .and_then(Value::as_str)
            .is_some_and(|feed| !feed.trim().is_empty())
    }
}

/// The rows of `vehicle_id` the summary reads (C6 R22). A row's type is the
/// payload's root `record_type` when that is a string; every other row is
/// ignored entirely, also for `observed_at_ms`.
pub(super) fn accepted_rows(
    vehicle_id: Uuid,
    observations: &[ObservationRecord],
) -> Vec<AcceptedRow<'_>> {
    observations
        .iter()
        .filter(|record| record.vehicle_id == vehicle_id)
        .filter_map(|record| {
            let root = record.payload.as_object()?;
            let kind = match root.get("record_type")?.as_str()? {
                OWNER_VEHICLE_DATA | FLEET_VEHICLE_DATA => RowKind::VehicleData,
                OWNER_DISCOVERY | FLEET_DISCOVERY => RowKind::Discovery,
                STREAM_UPDATE => RowKind::Stream,
                IMPORT_POSITION_RECORD_TYPE => RowKind::ImportPosition,
                _ => return None,
            };
            Some(AcceptedRow { kind, record, root })
        })
        .collect()
}

/// The greatest `observed_at_ms` over accepted rows (C6 R32).
pub(super) fn newest_observed_at_ms(rows: &[AcceptedRow<'_>]) -> Option<i64> {
    rows.iter().map(|row| row.record.observed_at_ms).max()
}

/// One statement (C6 R23): a field, its JSON value, its statement time, the
/// row's `observed_at_ms` and the observation id.
pub(super) struct Statement<'a> {
    group: &'static str,
    key: &'a str,
    pub(super) value: &'a Value,
    pub(super) time_ms: i64,
    pub(super) row_observed_at_ms: i64,
    pub(super) observation_id: i64,
}

impl Statement<'_> {
    /// Whether this statement is the dotted `candidate` (`group.key`).
    fn is(&self, candidate: &str) -> bool {
        candidate
            .strip_prefix(self.group)
            .and_then(|rest| rest.strip_prefix('.'))
            == Some(self.key)
    }

    /// Whether the value is an import-source value, which is already metric.
    pub(super) fn is_import(&self) -> bool {
        self.group == IMPORT_GROUP
    }

    /// The merge order of C6 R24: the greatest statement time, then the greatest
    /// observation id.
    fn order(&self) -> (i64, i64) {
        (self.time_ms, self.observation_id)
    }
}

/// Every statement of the accepted rows (C6 R23).
pub(super) struct Statements<'a>(Vec<Statement<'a>>);

impl<'a> Statements<'a> {
    pub(super) fn collect(rows: &[AcceptedRow<'a>]) -> Self {
        let mut statements = Vec::new();
        for row in rows {
            let observed = row.record.observed_at_ms;
            let id = row.record.observation_id;
            let mut push = |group: &'static str, key: &'a str, value: &'a Value, time_ms: i64| {
                statements.push(Statement {
                    group,
                    key,
                    value,
                    time_ms,
                    row_observed_at_ms: observed,
                    observation_id: id,
                });
            };
            match row.kind {
                RowKind::Discovery => {}
                RowKind::ImportPosition => {
                    for key in IMPORT_KEYS {
                        if let Some(value) = row.root.get(key).filter(|value| !value.is_null()) {
                            push(IMPORT_GROUP, key, value, observed);
                        }
                    }
                }
                RowKind::VehicleData | RowKind::Stream => {
                    let Some(data) = row.data_object() else {
                        continue;
                    };
                    // A full vehicle-data source states null; a stream frame or a
                    // sparse source cannot tell "not sent" from null (C6 R23).
                    let keeps_null = row.kind == RowKind::VehicleData && !row.is_sparse();
                    for key in ROOT_KEYS {
                        if let Some(value) =
                            data.get(key).filter(|value| keeps_null || !value.is_null())
                        {
                            push(ROOT_GROUP, key, value, observed);
                        }
                    }
                    for group in GROUPS {
                        let Some(members) = data.get(group).and_then(Value::as_object) else {
                            continue;
                        };
                        let time_ms = group_time(Some(members), observed);
                        for (key, value) in members {
                            if !keeps_null && value.is_null() {
                                continue;
                            }
                            match value.as_object() {
                                Some(nested) if group == NESTED_PARENT && key == NESTED_KEY => {
                                    // The nested object takes `vehicle_state`'s time.
                                    for (inner, inner_value) in nested {
                                        if keeps_null || !inner_value.is_null() {
                                            push(NESTED_GROUP, inner, inner_value, time_ms);
                                        }
                                    }
                                }
                                _ => push(group, key, value, time_ms),
                            }
                        }
                    }
                }
            }
        }
        Self(statements)
    }

    /// The newest statement among the dotted `candidates` (C6 R24).
    pub(super) fn newest(&self, candidates: &[&str]) -> Option<&Statement<'a>> {
        self.0
            .iter()
            .filter(|statement| candidates.iter().any(|candidate| statement.is(candidate)))
            .max_by_key(|statement| statement.order())
    }

    /// The newest group that states both keys of a pair (C6 R26): both
    /// statements come from the same observation and the same group.
    fn newest_pair(&self, first: &str, second: &str) -> Option<(&Statement<'a>, &Statement<'a>)> {
        self.0
            .iter()
            .filter(|statement| statement.is(first))
            .filter_map(|a| {
                let b = self.0.iter().find(|b| {
                    b.is(second) && b.observation_id == a.observation_id && b.group == a.group
                })?;
                Some((a, b))
            })
            .max_by_key(|(a, _)| a.order())
    }

    /// The `elevation` statement (C6 R27, C1 R38a): per observation, the
    /// `elevation` key when its reading is valid, else `native_location_elevation`
    /// when that is valid, else the key that is present and invalid; the newest
    /// observation wins even when it holds no valid reading.
    fn newest_elevation(&self) -> Option<(&Statement<'a>, Option<i64>)> {
        let read = |statement: Option<&Statement<'a>>| {
            statement.and_then(|s| integer(s.value, -32_768, 32_767))
        };
        self.0
            .iter()
            .filter(|s| {
                s.is("drive_state.elevation") || s.is("drive_state.native_location_elevation")
            })
            .map(|s| {
                let of_key = |key: &str| {
                    self.0
                        .iter()
                        .find(|o| o.observation_id == s.observation_id && o.is(key))
                };
                let primary = of_key("drive_state.elevation");
                let native = of_key("drive_state.native_location_elevation");
                match (read(primary), read(native)) {
                    (Some(value), _) => (primary, Some(value)),
                    (None, Some(value)) => (native, Some(value)),
                    (None, None) => (primary.or(native), None),
                }
            })
            .filter_map(|(statement, value)| Some((statement?, value)))
            .max_by_key(|(statement, _)| statement.order())
    }
}

/// C1 `valid_coordinates` (C6 R26): both halves present, finite and inside
/// latitude -90..=90 and longitude -180..=180. HOLD: C1's own function is
/// private to the lifecycle module in the B2 tree, and its visibility edit
/// waits on C1/C2 (the same file is rewritten by those parts). This private
/// copy keeps the C6-S1 reader independent of that edit; once C1/C2 has
/// integrated, the serial writer deletes it and imports
/// `crate::lifecycle::valid_coordinates` made `pub(crate)` (C6 section 7
/// item 3), and reports that visibility change in the Part I or S1 receipt.
fn valid_coordinates(latitude: Option<f64>, longitude: Option<f64>) -> Option<(f64, f64)> {
    match (latitude, longitude) {
        (Some(latitude), Some(longitude))
            if latitude.is_finite()
                && longitude.is_finite()
                && (-90.0..=90.0).contains(&latitude)
                && (-180.0..=180.0).contains(&longitude) =>
        {
            Some((latitude, longitude))
        }
        _ => None,
    }
}

/// A numeric field (C1 R4a): a finite JSON number, or a JSON string whose
/// trimmed text parses as a finite decimal number.
pub(super) fn number(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

/// An integer field (C1 R4a order): read as a number, rounded half away from
/// zero, converted (a value outside the `i64` range is the wrong type), then
/// range-checked.
pub(super) fn integer(value: &Value, min: i64, max: i64) -> Option<i64> {
    let rounded = number(value)?.round();
    // `i64::MAX as f64` is 2^63, the first f64 above `i64::MAX`.
    if rounded.abs() >= i64::MAX as f64 {
        return None;
    }
    let converted = rounded as i64;
    (min..=max).contains(&converted).then_some(converted)
}

/// An exact integer (C6 R27): a numeric value with no fractional part.
pub(super) fn exact_integer(value: &Value) -> Option<f64> {
    let number = number(value)?;
    (number.fract() == 0.0).then_some(number)
}

/// A boolean field (C1 R4a): JSON `true` or `false` only.
pub(super) fn boolean(value: &Value) -> Option<bool> {
    value.as_bool()
}

/// The text rule (C6 R27): a trimmed string, `None` when empty or longer than
/// [`K_TEXT_MAX`] scalar values.
pub(super) fn text(value: &Value) -> Option<String> {
    let trimmed = value.as_str()?.trim();
    (!trimmed.is_empty() && trimmed.chars().count() <= K_TEXT_MAX).then(|| trimmed.to_owned())
}

/// Persistent last-known value or transient value (C6 R27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Persistent,
    Transient,
}

/// The facts that decide whether a transient value is shown (C6 R28).
pub(super) struct Visibility {
    /// (a) The activity state is `asleep` or `offline`.
    quiet: bool,
    /// (b) The earliest row time still inside the open `online` interval.
    row_floor_ms: Option<i64>,
    /// (c) `max(observed_at_ms, now_ms)` and the longest age.
    t_now_ms: i64,
    max_age_ms: i64,
}

impl Visibility {
    /// `transient_max_age_ms <= 0` reads as the default (C6 R21).
    pub(super) fn new(
        activity: Option<ActivityState>,
        lifecycle: Option<&OpenSessionState>,
        observed_at_ms: Option<i64>,
        now_ms: i64,
        transient_max_age_ms: i64,
    ) -> Self {
        let max_age_ms = if transient_max_age_ms > 0 {
            transient_max_age_ms
        } else {
            i64::try_from(DEFAULT_OFFLINE_DRIVE_TIMEOUT.as_millis()).unwrap_or(i64::MAX)
        };
        Self {
            quiet: matches!(
                activity,
                Some(ActivityState::Asleep | ActivityState::Offline)
            ),
            row_floor_ms: lifecycle
                .and_then(|state| state.open_state.as_ref())
                .filter(|open| open.state == "online")
                .map(|open| open.start_date_ms.saturating_sub(K_SKEW_MS)),
            t_now_ms: observed_at_ms.map_or(now_ms, |observed| observed.max(now_ms)),
            max_age_ms,
        }
    }

    fn shows(&self, class: Class, statement: &Statement<'_>) -> bool {
        match class {
            Class::Persistent => true,
            Class::Transient => {
                !self.quiet
                    && self
                        .row_floor_ms
                        .is_none_or(|floor| statement.row_observed_at_ms >= floor)
                    && self.t_now_ms.saturating_sub(statement.time_ms) <= self.max_age_ms
            }
        }
    }
}

/// The field reader: the newest statement wins even when invalid (C6 R24,
/// R25), then a transient value must be visible (C6 R28).
pub(super) struct Fields<'s, 'a> {
    statements: &'s Statements<'a>,
    visibility: Visibility,
}

impl<'s, 'a> Fields<'s, 'a> {
    pub(super) fn new(statements: &'s Statements<'a>, visibility: Visibility) -> Self {
        Self {
            statements,
            visibility,
        }
    }

    /// Read the newest statement among `candidates` with `read`.
    pub(super) fn with<T>(
        &self,
        class: Class,
        candidates: &[&str],
        read: impl FnOnce(&Statement<'a>) -> Option<T>,
    ) -> Option<T> {
        let newest = self.statements.newest(candidates)?;
        self.visibility
            .shows(class, newest)
            .then(|| read(newest))
            .flatten()
    }

    pub(super) fn flag(&self, class: Class, candidate: &str) -> Option<bool> {
        self.with(class, &[candidate], |s| boolean(s.value))
    }

    pub(super) fn text(&self, class: Class, candidate: &str) -> Option<String> {
        self.with(class, &[candidate], |s| text(s.value))
    }

    /// An integer within `min..=max`.
    pub(super) fn int(&self, class: Class, candidate: &str, min: i64, max: i64) -> Option<i64> {
        self.with(class, &[candidate], |s| integer(s.value, min, max))
    }

    /// An integer served as a number (C1 R35, R36).
    pub(super) fn int_number(
        &self,
        class: Class,
        candidate: &str,
        min: i64,
        max: i64,
    ) -> Option<f64> {
        self.int(class, candidate, min, max)
            .map(|value| value as f64)
    }

    /// A number rounded half away from zero to `precision` decimals and
    /// range-checked (C1 R35, R36).
    pub(super) fn scaled(
        &self,
        class: Class,
        candidate: &str,
        precision: u32,
        range: std::ops::RangeInclusive<f64>,
    ) -> Option<f64> {
        self.with(class, &[candidate], |s| {
            // `+ 0.0` turns a rounded negative zero into zero.
            let value = round_to_precision(number(s.value)?, precision) + 0.0;
            range.contains(&value).then_some(value)
        })
    }

    /// A number as given, within `range`.
    pub(super) fn plain(
        &self,
        class: Class,
        candidate: &str,
        range: std::ops::RangeInclusive<f64>,
    ) -> Option<f64> {
        self.with(class, &[candidate], |s| {
            number(s.value).filter(|value| range.contains(value))
        })
    }

    /// A coordinate pair (C6 R26): the newest group that states both keys,
    /// shown only when it passes `valid_coordinates`, rounded to 6 decimals;
    /// otherwise both are null.
    pub(super) fn pair(&self, class: Class, latitude: &str, longitude: &str) -> Option<(f64, f64)> {
        let (first, second) = self.statements.newest_pair(latitude, longitude)?;
        if !self.visibility.shows(class, first) {
            return None;
        }
        let (latitude, longitude) = valid_coordinates(number(first.value), number(second.value))?;
        Some((
            round_to_precision(latitude, 6),
            round_to_precision(longitude, 6),
        ))
    }

    /// Persistent `elevation` in whole metres (C6 R27, C1 R35, R38a).
    pub(super) fn elevation(&self) -> Option<f64> {
        let (_, value) = self.statements.newest_elevation()?;
        value.map(|metres| metres as f64)
    }
}

/// The shift state (C6 R27): C1 R40a normalisation; only `P`, `R`, `N` and `D`
/// are served.
pub(super) fn shift_state(value: &Value) -> Option<String> {
    normalise_enum_text(value.as_str()?)
        .filter(|name| matches!(*name, "P" | "R" | "N" | "D"))
        .map(str::to_owned)
}

/// What the stored lifecycle record says (C6 R29, R30, R31).
pub(super) struct LifecycleView {
    /// The decoded open-session state, when the record is present and decodes.
    pub(super) state: Option<OpenSessionState>,
    /// `true` when the record decodes and is not quarantined; `false` when it
    /// is quarantined or does not decode; `None` without a record.
    pub(super) processing_healthy: Option<bool>,
}

pub(super) fn lifecycle_view(lifecycle: Option<&LifecycleStateRecord>) -> LifecycleView {
    let Some(record) = lifecycle else {
        return LifecycleView {
            state: None,
            processing_healthy: None,
        };
    };
    let state = OpenSessionState::decode(&record.open_session_json).ok();
    LifecycleView {
        processing_healthy: Some(state.is_some() && !record.quarantined),
        state,
    }
}

/// `activity_state` (C6 R29): the lifecycle phase when the record decodes (a
/// quarantined record is still used); otherwise the newest reachability
/// evidence among the rows. There is no age rule.
pub(super) fn activity_state(
    lifecycle: Option<&OpenSessionState>,
    rows: &[AcceptedRow<'_>],
) -> Option<ActivityState> {
    if let Some(state) = lifecycle {
        return match state.phase {
            VehiclePhase::Unknown => None,
            VehiclePhase::Online | VehiclePhase::Suspended => Some(ActivityState::Online),
            VehiclePhase::Asleep => Some(ActivityState::Asleep),
            VehiclePhase::Offline => Some(ActivityState::Offline),
            VehiclePhase::Driving => Some(ActivityState::Driving),
            VehiclePhase::Charging => Some(ActivityState::Charging),
            VehiclePhase::Updating => Some(ActivityState::Updating),
        };
    }
    // Import-source positions are not reachability evidence (C6 R29).
    let newest = rows
        .iter()
        .filter(|row| row.kind != RowKind::ImportPosition)
        .max_by_key(|row| (row.record.observed_at_ms, row.record.observation_id))?;
    if matches!(newest.kind, RowKind::VehicleData | RowKind::Stream)
        && newest.data_object().is_some()
    {
        return Some(ActivityState::Online);
    }
    match newest.source_vehicle_state()? {
        "online" => Some(ActivityState::Online),
        "asleep" => Some(ActivityState::Asleep),
        "offline" => Some(ActivityState::Offline),
        _ => None,
    }
}

/// `state_since_ms` (C6 R30): the start of the condition `activity_state` names,
/// from the decoded lifecycle record only.
pub(super) fn state_since_ms(
    activity: Option<ActivityState>,
    lifecycle: Option<&OpenSessionState>,
) -> Option<i64> {
    let state = lifecycle?;
    match activity? {
        ActivityState::Driving => state.open_drive.as_ref().map(|open| open.start_date_ms),
        ActivityState::Charging => state.open_charge.as_ref().map(|open| open.start_date_ms),
        ActivityState::Updating => state.open_update.as_ref().map(|open| open.start_date_ms),
        word @ (ActivityState::Online | ActivityState::Asleep | ActivityState::Offline) => state
            .open_state
            .as_ref()
            .filter(|open| open.state == word.as_str())
            .map(|open| open.start_date_ms),
    }
}
