// SPDX-License-Identifier: AGPL-3.0-only

//! C6 Part C2 spec tests over the committed `hub-http-v1@1.3.0` profile files
//! (C6 ST-S46, C6 ST-S47, C6 ST-S50, with the profile half of C6 ST-S54 in the
//! server tests). Test ids carry their unit prefix (C6 R65). The expected
//! tables are written out here from C6 R47 to R49 and R58, so the profile
//! builder cannot agree with itself. The published 1.2.0 files are only read.

use std::path::PathBuf;

use serde_json::Value;

use super::b3_c6_c1::{PAIRS, cases, schema_allows};

pub(crate) fn profile_file(minor: &str, name: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/protocol/profiles/hub-http-v1/{minor}/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
}

pub(crate) fn profile_document(minor: &str, name: &str) -> Value {
    let text = std::fs::read_to_string(profile_file(minor, name))
        .unwrap_or_else(|error| panic!("committed hub-http-v1@{minor} {name}: {error}"));
    serde_json::from_str(&text).expect("profile document JSON")
}

/// The problems of one served item against one minor's item definition: empty
/// when it is valid, and a label per missing required key otherwise.
pub(crate) fn item_problems(minor: &str, item: &str, body: &Value) -> Vec<String> {
    let schema = profile_document(minor, "resources.schema.json");
    let defs = &schema["$defs"];
    if schema_allows(defs, &defs[item], body) {
        return Vec::new();
    }
    vec![format!("{item} is not valid against hub-http-v1@{minor}")]
}

/// The 33 history renames of C6 R58 (a): item, 1.2.0 key (the alias), new key.
pub(crate) fn history_pairs() -> [(&'static str, &'static str, &'static str); 33] {
    PAIRS
}

/// C6 R47: the 28 top-level renames, 1.2.0 key then 1.3.0 key.
const CURRENT_PAIRS: [(&str, &str); 28] = [
    ("state", "activity_state"),
    ("since", "state_since_ms"),
    ("healthy", "processing_healthy"),
    ("geofence", "geofence_name"),
    ("model", "model_code"),
    ("version", "software_version"),
    ("update_available", "software_update_available"),
    ("update_status", "software_update_status"),
    ("update_version", "software_update_version"),
    ("download_perc", "software_update_download_perc"),
    ("install_perc", "software_update_install_perc"),
    ("speed", "speed_kmh"),
    ("odometer", "odometer_km"),
    ("rated_battery_range_km", "battery_range_km"),
    (
        "active_route_miles_to_arrival",
        "active_route_km_to_arrival",
    ),
    ("plugged_in", "charge_cable_connected"),
    ("doors_open", "any_door_open"),
    ("windows_open", "any_window_open"),
    ("driver_front_door_open", "door_driver_front_open"),
    ("driver_rear_door_open", "door_driver_rear_open"),
    ("passenger_front_door_open", "door_passenger_front_open"),
    ("passenger_rear_door_open", "door_passenger_rear_open"),
    ("driver_front_window_open", "window_driver_front_open"),
    ("driver_rear_window_open", "window_driver_rear_open"),
    ("passenger_front_window_open", "window_passenger_front_open"),
    ("passenger_rear_window_open", "window_passenger_rear_open"),
    ("frunk_open", "front_trunk_open"),
    ("trunk_open", "rear_trunk_open"),
];

/// C6 R48: the four `car` renames.
const CAR_PAIRS: [(&str, &str); 4] = [
    ("model", "model_code"),
    ("marketing_name", "variant_name"),
    ("source_eid", "api_vehicle_id"),
    ("source_vid", "stream_vehicle_id"),
];

/// C6 R49: the eight `car.settings` renames.
const SETTINGS_PAIRS: [(&str, &str); 8] = [
    ("enabled", "collection_enabled"),
    ("use_streaming_api", "streaming_enabled"),
    ("suspend_min", "sleep_window_min"),
    ("suspend_after_idle_min", "sleep_after_idle_min"),
    ("suspend_min_resolved", "sleep_window_min_resolved"),
    ("req_not_unlocked", "sleep_requires_locked"),
    ("free_supercharging", "supercharging_free"),
    ("lfp_battery", "battery_lfp"),
];

/// C6 R27: the class T output fields (transient, R28) and class P output fields
/// (persistent) the summary derives, written out from the specification table.
const CLASS_T: [&str; 25] = [
    "speed_kmh",
    "power",
    "shift_state",
    "active_route_destination",
    "active_route_latitude",
    "active_route_longitude",
    "active_route_km_to_arrival",
    "active_route_minutes_to_arrival",
    "active_route_traffic_minutes_delay",
    "active_route_energy_at_arrival",
    "charger_power",
    "charger_voltage",
    "charger_actual_current",
    "charger_phases",
    "time_to_full_charge",
    "inside_temp",
    "outside_temp",
    "is_climate_on",
    "is_preconditioning",
    "climate_keeper_mode",
    "sentry_mode",
    "is_user_present",
    "center_display_state",
    "software_update_download_perc",
    "software_update_install_perc",
];
const CLASS_P: [&str; 38] = [
    "latitude",
    "longitude",
    "heading",
    "elevation",
    "battery_level",
    "usable_battery_level",
    "battery_range_km",
    "ideal_battery_range_km",
    "est_battery_range_km",
    "charge_limit_soc",
    "charge_energy_added",
    "charge_current_request",
    "charge_current_request_max",
    "scheduled_charging_start_time",
    "charge_port_door_open",
    "odometer_km",
    "locked",
    "tpms_soft_warning_fl",
    "tpms_soft_warning_fr",
    "tpms_soft_warning_rl",
    "tpms_soft_warning_rr",
    "sun_roof_percent_open",
    "sun_roof_state",
    "tpms_pressure_fl",
    "tpms_pressure_fr",
    "tpms_pressure_rl",
    "tpms_pressure_rr",
    "sun_roof_installed",
    "door_driver_front_open",
    "door_driver_rear_open",
    "door_passenger_front_open",
    "door_passenger_rear_open",
    "front_trunk_open",
    "rear_trunk_open",
    "window_driver_front_open",
    "window_driver_rear_open",
    "window_passenger_front_open",
    "window_passenger_rear_open",
];

/// The class P entries that are not newest-statement fields: the four software
/// fields (first-usable source chains, R36 and R37) and the two three-valued
/// aggregates (R33). They carry the persistence sentence and not the
/// newest-statement sentence.
const CLASS_P_PLAIN: [&str; 6] = [
    "software_version",
    "software_update_available",
    "software_update_status",
    "software_update_version",
    "any_door_open",
    "any_window_open",
];

/// C6 R58 (e): the property and required counts of each history item.
const ITEM_COUNTS: [(&str, usize, usize); 4] = [
    ("drive", 45, 43),
    ("charge", 40, 39),
    ("position", 31, 30),
    ("charge_sample", 21, 21),
];

fn alias_text(new: &str) -> String {
    format!("Deprecated alias of {new}; carries the same value.")
}

fn keys(object: &Value) -> Vec<String> {
    let mut all: Vec<String> = object
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect();
    all.sort();
    all
}

fn definition<'a>(schema: &'a Value, name: &str) -> &'a Value {
    match name {
        "car" => &schema["$defs"]["current"]["properties"]["car"]["anyOf"][0],
        "settings" => {
            &schema["$defs"]["current"]["properties"]["car"]["anyOf"][0]["properties"]["settings"]
        }
        _ => &schema["$defs"][name],
    }
}

fn required(definition: &Value) -> Vec<&str> {
    definition["required"]
        .as_array()
        .expect("required")
        .iter()
        .map(|name| name.as_str().expect("name"))
        .collect()
}

// ------------------------------------------------------------------ ST-S46

#[test]
fn c6_st_s46_schemas() {
    let new = profile_document("1.3.0", "resources.schema.json");
    let base = profile_document("1.2.0", "resources.schema.json");

    // Counts: R58 (e) for the four items, R47 to R49 for the summary.
    for (item, properties, required_keys) in ITEM_COUNTS {
        let one = definition(&new, item);
        assert_eq!(
            (
                one["properties"].as_object().expect("properties").len(),
                required(one).len()
            ),
            (properties, required_keys),
            "{item}"
        );
        assert_ne!(
            one.get("additionalProperties"),
            Some(&Value::Bool(false)),
            "{item}: stays open, so a 1.2.0 client reads it"
        );
    }
    for (name, properties) in [("current", 115), ("car", 20), ("settings", 16)] {
        let one = definition(&new, name);
        assert_eq!(
            one["properties"].as_object().expect("properties").len(),
            properties,
            "{name}"
        );
        assert_eq!(
            required(one).len(),
            properties,
            "{name}: every key required"
        );
    }
    // The base counts, so that the additions are the stated ones.
    assert_eq!(
        [
            definition(&base, "current")["properties"]
                .as_object()
                .unwrap()
                .len(),
            definition(&base, "car")["properties"]
                .as_object()
                .unwrap()
                .len(),
            definition(&base, "settings")["properties"]
                .as_object()
                .unwrap()
                .len(),
        ],
        [85, 14, 8]
    );

    // Each of the 33 history pairs: the new property has the old property's
    // 1.2.0 schema; the old one keeps it, deprecated, naming its replacement;
    // requiredness moves with the key.
    assert_eq!(PAIRS.len(), 33);
    for (item, old, new_key) in PAIRS {
        let base_item = definition(&base, item);
        let one = definition(&new, item);
        let base_schema = &base_item["properties"][old];
        assert_eq!(&one["properties"][new_key], base_schema, "{item}.{new_key}");
        let alias = &one["properties"][old];
        assert_eq!(alias["deprecated"], true, "{item}.{old}");
        assert_eq!(
            alias["description"],
            alias_text(new_key).as_str(),
            "{item}.{old}"
        );
        let mut plain = alias.clone();
        plain.as_object_mut().unwrap().remove("deprecated");
        plain.as_object_mut().unwrap().remove("description");
        assert_eq!(
            &plain, base_schema,
            "{item}.{old}: otherwise the 1.2.0 schema"
        );
        assert_eq!(
            required(base_item).contains(&old),
            required(one).contains(&new_key),
            "{item}.{new_key}: requiredness"
        );
        assert!(required(base_item).contains(&old) == required(one).contains(&old));
    }
    // R47 to R49: the same for the summary, its car and its settings. The car's
    // `model` alias is the stored text, so its replacement is a nullable code.
    for (name, pairs) in [
        ("current", &CURRENT_PAIRS[..]),
        ("car", &CAR_PAIRS[..]),
        ("settings", &SETTINGS_PAIRS[..]),
    ] {
        for (old, new_key) in pairs {
            let one = definition(&new, name);
            let base_schema = &definition(&base, name)["properties"][*old];
            assert_eq!(one["properties"][*old]["deprecated"], true, "{name}.{old}");
            assert!(
                one["properties"][*old]["description"]
                    .as_str()
                    .is_some_and(|text| text.contains(new_key)),
                "{name}.{old} names {new_key}"
            );
            if (name, *old) == ("car", "model") {
                assert_eq!(one["properties"]["model"]["type"], "string");
                assert_eq!(
                    one["properties"][*new_key],
                    serde_json::json!({"anyOf": [base_schema, {"type": "null"}]})
                );
            } else {
                assert_eq!(
                    &one["properties"][*new_key], base_schema,
                    "{name}.{new_key}"
                );
            }
            assert!(required(one).contains(old) && required(one).contains(new_key));
        }
    }
    let current = definition(&new, "current");
    assert_eq!(
        current["properties"]["active_route_miles_to_arrival"]["description"],
        "Deprecated alias of active_route_km_to_arrival; carries the provider miles value as given, because its name states miles."
    );
    assert_eq!(
        definition(&new, "car")["properties"]["model"]["description"],
        "Deprecated alias of model_code; carries the stored model text as 1.2.0 served it."
    );
    for (location, name) in [(current, "model_name"), (current, "model_year")] {
        assert!(required(location).contains(&name), "{name}");
    }
    assert!(
        !definition(&new, "car")["properties"]
            .as_object()
            .unwrap()
            .contains_key("source_model_text")
    );

    // Nothing else of the resources moves: the other definitions are the base's.
    for name in [
        "vehicles",
        "address_provenance",
        "elevation_provenance",
        "drives",
        "positions",
        "charges",
        "charge_samples",
        "health",
        "ready",
    ] {
        assert_eq!(new["$defs"][name], base["$defs"][name], "{name}");
    }
    assert_eq!(keys(&new["$defs"]), keys(&base["$defs"]));

    // Dual-minor item validation: every item the C6-C1 serialisers write
    // (distinct and null fixtures) is valid against this minor's item schema
    // and against 1.2.0's, and a body without a required key is refused by
    // this minor's, so the check cannot pass vacuously.
    let all = cases();
    assert_eq!(all.len(), 8);
    for case in &all {
        let body: Value = serde_json::from_slice(&case.bytes).expect("item JSON");
        let label = format!("{} ({})", case.item, case.fixture);
        assert!(
            item_problems("1.3.0", case.item, &body).is_empty(),
            "{label}: valid against hub-http-v1@1.3.0"
        );
        assert!(
            item_problems("1.2.0", case.item, &body).is_empty(),
            "{label}: valid against hub-http-v1@1.2.0"
        );
        let item_schema = definition(&new, case.item);
        for name in required(item_schema) {
            let mut missing = body.clone();
            missing.as_object_mut().expect("item").remove(name);
            assert!(
                !item_problems("1.3.0", case.item, &missing).is_empty(),
                "{label}: refused without {name}"
            );
        }
    }
}

// ------------------------------------------------------------------ ST-S47

/// The hand-over texts of C1 R95, C2 R82 and C8 R48 under the new keys they name, with the nullability and unit of each entry: item, key, nullable,
/// unit and `source`, copied from the coordinator's B3 addenda (v2, sections 5.2 to 5.4) and not from the profile builder, so an edit of any word of a
/// handed-over text fails here.
const HANDOVER: [(&str, &str, bool, &str, &str); 17] = [
    (
        "drive",
        "ascent_m",
        true,
        "metres",
        r##"Total climb of the drive in whole metres. Saturated at 32767: a larger total is served as 32767 and is then a lower bound. The drive's positions keep their own elevations."##,
    ),
    (
        "drive",
        "descent_m",
        true,
        "metres",
        r##"Total descent of the drive in whole metres. Saturated at 32767: a larger total is served as 32767 and is then a lower bound. The drive's positions keep their own elevations."##,
    ),
    (
        "drive",
        "duration_min",
        true,
        "minutes, integer",
        r##"Whole minutes of the observed drive, from `started_at_ms` to `ended_at_ms`. Saturated at 32767. When the drive starts from a recent parked observation, `distance_km` and the start values include the short stretch before the first movement observation, which this duration does not cover."##,
    ),
    (
        "drive",
        "distance_km",
        true,
        "km",
        r##"Distance of the drive in km, from the odometer. When the Hub saw the car parked within its gap interval before the drive's first movement observation, the distance and the start values (start coordinates, battery level and ranges) are taken from that parked observation. They then include the stretch driven before the first movement observation, while `started_at_ms` and `duration_min` cover only the observed part. Distance divided by duration can therefore overstate the average speed by up to one parked polling interval of driving."##,
    ),
    (
        "drive",
        "ended_at_ms",
        false,
        "epoch milliseconds",
        r##"End of the drive. When the car did not state the end (for example a source that reports no gear, or a drive closed because evidence stopped arriving), the end is the Hub's last evidence that the drive was in progress: the first observation without movement evidence that follows the drive's last observation with movement evidence, or, if there is none, the last observation folded into the drive. An observation without movement evidence does not show that the car had stopped. Observations after that point are not counted in the drive's duration, distance or averages. The end is never earlier than `started_at_ms`."##,
    ),
    (
        "charge",
        "started_at_ms",
        false,
        "epoch milliseconds",
        r##"Start of the session. A session the Hub first saw already in progress starts at its first sample, which is then later than the true start (an upper bound); its added energy is still complete. A session the Hub records after a period without data, because the car's own energy counter shows energy was added while the Hub saw nothing, starts at the last time up to which the car's energy had been accounted for, which is earlier than the true start (a lower bound)."##,
    ),
    (
        "charge",
        "ended_at_ms",
        false,
        "epoch milliseconds",
        r##"End of the session. A session that continued across a period without data and then ended has as its end the first observation after that period, an upper bound: the true end lies between the last sample before the period and this time. A session recorded after a period without data (see `started_at_ms`) also ends at the first observation after it, an upper bound. For both, `duration_min` is an upper bound, and the stored samples show the period without data. A per-minute cost is computed only when the stored samples show no unobserved stretch at the start or the end of the session (see `cost`)."##,
    ),
    (
        "charge",
        "charge_energy_used_kwh",
        true,
        "kWh",
        r##"Energy that entered the car through its charge inlet during the session, as the car reports it, over the same span as `charge_energy_added_kwh`. AC session (`is_dc` false): the on-board charger's input energy, computed from the AC input power. It is what a meter before the car registers, apart from cable losses. Because it is computed from apparent power, it can overstate the real energy where the power factor is below 1 (about 1 % at 0.99, about 5 % at 0.95). DC session (`is_dc` true): the integral of the DC charging power the source reports. From sources with the Owner API's charge-state shape, on the one car measured so far, it was about 7 % above the battery-side counter (median of 81 sessions, never below), consistent with a reading taken before the battery. From Fleet Telemetry it is not yet measured. Unknown kind (`is_dc` null): the integral of the per-sample power the Hub selects. That is inlet energy for an AC session, and the reported DC power's integral for a DC session that showed no kind evidence. Null when not measured; no energy is invented for unobserved time. It prices no DC session (see `cost`)."##,
    ),
    (
        "charge",
        "cost",
        true,
        "currency units; no currency code is carried; decimal serialised as a JSON number",
        r##"Cost of the session in currency units. Null means the cost is unknown: no complete tariff applied, its energy or time basis is missing, or nobody entered one. Zero is a known zero. Per-kWh tariffs: AC and unknown-kind sessions are priced on `charge_energy_used_kwh`, DC sessions on `charge_energy_added_kwh` only; a DC session without that figure has no per-kWh cost. Per-minute tariffs (`tariff_type` `per_minute`): the cost covers the whole connected time from `started_at_ms` to `ended_at_ms`, pauses included, in unrounded minutes. It is null when the stored samples show an unobserved stretch at either end of the session: the first sample is more than ten minutes after `started_at_ms`, the last two samples are more than ten minutes apart, or `ended_at_ms` is more than ten minutes after the last sample (the cases described under those keys). A session first seen in progress starts at its first sample and is priced over its observed time; a gap inside a session that later samples close is connected time and is priced; a row with no stored samples is priced over its own recorded start and end. Free Supercharging: when the owner's free-Supercharging setting is on, a session typed `Supercharger`, or a DC session of another DC type whose samples carry the charger brand Tesla and no other brand, costs zero. The session fee is added once per row (see `tariff_session_fee`). The Hub's automatic close and fill never overwrite a stored cost: a cost the import carried and a cost the owner entered stay. The owner can replace any stored cost, an imported one included. A total the owner enters replaces the cost and leaves the row's tariff unchanged. A unit price the owner enters, per kWh or per minute, replaces the row's tariff with that type and rate and no session fee, and the cost becomes the price on that tariff, with no free-charging rule and no fence fee; a per-minute price follows the per-minute rule above."##,
    ),
    (
        "charge",
        "tariff_type",
        true,
        "text; known values per_kwh and per_minute; preserve unknown values",
        r##"The tariff recorded on the charge. For a charge the Hub collected: the tariff copied from the owner's geofence that contained its start point when it closed, or, when the owner asked the Hub to fill its missing cost after saving that geofence's tariff, the tariff of that geofence as the owner had just saved it. Later geofence edits never change it. It is null when no complete tariff was copied. For an imported charge: the tariff of the geofence the import source linked to the charge, as the import source held it when the history was imported; it is not a copy made when the session closed, and one of the three tariff keys can be null while another is not. For a charge whose price the owner entered as a unit price: the type the owner gave, which replaces any earlier tariff."##,
    ),
    (
        "charge",
        "tariff_rate",
        true,
        "currency units per kWh when tariff_type is per_kwh, per minute when per_minute; decimal serialised as a JSON number",
        r##"Rate of the tariff recorded on the charge; `tariff_type` states which tariff it is and how and when it was recorded, and later geofence edits never change it. Currency units per kWh when `tariff_type` is `per_kwh`, per minute when `per_minute`. A unit price the owner enters replaces it."##,
    ),
    (
        "charge",
        "tariff_session_fee",
        true,
        "currency units; decimal serialised as a JSON number",
        r##"Session fee of the tariff recorded on the charge (see `tariff_type`). On a row the Hub collected that continues the previous row's plug-in (the car's own charging session continued with no unplug, drive or movement in between), the fee is waived so that a fee is charged once per connection: it is 0 when the tariff had a fee, and null, as in the tariff, when it had none. A unit price the owner enters replaces the tariff and clears the fee (null)."##,
    ),
    (
        "charge",
        "is_dc",
        true,
        "boolean",
        r##"Session kind, by one rule for collected sessions and for closed imported sessions whose import applied that rule. True once any sample shows DC evidence (a DC charger type, the fast-charger flag with a type that is not Tesla AC equipment, or a power no AC inlet can carry), and then stays true. False when samples show AC evidence and none shows DC. Null when no sample says. A closed imported session whose import stored its kind by an earlier rule keeps that value until it is imported again."##,
    ),
    (
        "charge",
        "fast_charger_type",
        true,
        "text",
        r##"Charger type of the session, by one rule for collected sessions and for closed imported sessions whose import applied that rule: the trimmed type of the first sample whose type carries information. Later values do not change it. Values that say nothing (empty, `<invalid>`, `Unknown`, `SNA`, `Other`, `Tesla`) are skipped. A closed imported session whose import stored its type by an earlier rule keeps that value until it is imported again."##,
    ),
    (
        "drive",
        "start_geofence_name",
        true,
        "text",
        r##"For a row the Hub collected: the name of the owner's geofence that contained the row's stored point when the row closed (the drive's start or end point; the charge's start point). Where fences overlap, the one with the smallest radius, then the nearest centre. Null when none did or the point is missing. For an imported row: the label the import source recorded. When the owner later creates, edits or deletes a geofence, stored rows are relabelled by one rule. A row whose point the change moves into or out of a fence takes the Hub's new answer there. A label that named a renamed fence takes the new name. Any other label, an imported one included, is kept, unless every fence of that name was deleted; then a row with a point takes the Hub's current answer there. Tariff fields and cost are never relabelled."##,
    ),
    (
        "drive",
        "end_geofence_name",
        true,
        "text",
        r##"For a row the Hub collected: the name of the owner's geofence that contained the row's stored point when the row closed (the drive's start or end point; the charge's start point). Where fences overlap, the one with the smallest radius, then the nearest centre. Null when none did or the point is missing. For an imported row: the label the import source recorded. When the owner later creates, edits or deletes a geofence, stored rows are relabelled by one rule. A row whose point the change moves into or out of a fence takes the Hub's new answer there. A label that named a renamed fence takes the new name. Any other label, an imported one included, is kept, unless every fence of that name was deleted; then a row with a point takes the Hub's current answer there. Tariff fields and cost are never relabelled."##,
    ),
    (
        "charge",
        "geofence_name",
        true,
        "text",
        r##"For a row the Hub collected: the name of the owner's geofence that contained the row's stored point when the row closed (the drive's start or end point; the charge's start point). Where fences overlap, the one with the smallest radius, then the nearest centre. Null when none did or the point is missing. For an imported row: the label the import source recorded. When the owner later creates, edits or deletes a geofence, stored rows are relabelled by one rule. A row whose point the change moves into or out of a fence takes the Hub's new answer there. A label that named a renamed fence takes the new name. Any other label, an imported one included, is kept, unless every fence of that name was deleted; then a row with a point takes the Hub's current answer there. Tariff fields and cost are never relabelled."##,
    ),
];
/// C2 R82's one current-summary text (addenda 5.3, last row), placed as the `source` of the `car` entry after its key (C6-C2 decision D6).
const CAR_EFFICIENCY: &str = r##"Battery energy that adds one km of rated range, in Wh per km: the median of the figures of recent Hub-measured AC sessions. Before three such sessions exist, it is the value imported from the source database, whose basis the Hub does not guarantee."##;
const IMPORT_ADDRESS: &str = "The address was copied unchanged from the history of an import source, the source database the Hub's owner migrated from. The Hub did not look it up and does not replace it with a lookup. The Hub states nothing about how the import source obtained it.";
const IMPORT_ELEVATION: &str = "The elevation was copied unchanged from the history of an import source, the source database the Hub's owner migrated from. The Hub did not sample it from terrain data and does not replace it. The Hub states nothing about how the import source obtained it.";

#[test]
fn c6_st_s47_semantics() {
    let new = profile_document("1.3.0", "field-semantics.json");
    let base = profile_document("1.2.0", "field-semantics.json");
    assert_eq!(new["profile_id"], "hub-http-v1@1.3.0");

    // (a) Every new key has an entry under its item; each alias entry holds its
    // replacement's nullable and unit and the one deprecation sentence, nothing else.
    for (item, old, new_key) in PAIRS {
        let alias = &new[item][old];
        let entry = &new[item][new_key];
        assert!(entry.is_object(), "{item}.{new_key} has an entry");
        assert_eq!(
            alias,
            &serde_json::json!({
                "nullable": entry["nullable"],
                "source": alias_text(new_key),
                "unit": entry["unit"],
            }),
            "{item}.{old}"
        );
        // The moved entry keeps the base's nullability and unit, but for the
        // tariff rate whose unit named the old key (B3 addenda v2 5.3 note 1).
        let moved = &base[item][old];
        assert_eq!(entry["nullable"], moved["nullable"], "{item}.{new_key}");
        if new_key != "tariff_rate" {
            assert_eq!(entry["unit"], moved["unit"], "{item}.{new_key}");
        }
    }
    assert_eq!(
        new["charge"]["tariff_rate"]["unit"],
        "currency units per kWh when tariff_type is per_kwh, per minute when per_minute; decimal serialised as a JSON number"
    );
    assert_eq!(
        new["charge"]["cost_per_unit"]["unit"],
        new["charge"]["tariff_rate"]["unit"]
    );

    // (b) The hand-over texts sit under the new keys the units name, byte for byte,
    // with their nullability and unit, and under no alias key; every other moved
    // entry is the base's, unchanged.
    let handover_keys: Vec<(&str, &str)> = HANDOVER.iter().map(|row| (row.0, row.1)).collect();
    for (item, key, nullable, unit, text) in HANDOVER {
        let entry = &new[item][key];
        assert_eq!(
            entry["source"], text,
            "{item}.{key} carries its text exactly"
        );
        assert_eq!(entry["nullable"], nullable, "{item}.{key}: nullable");
        assert_eq!(entry["unit"], unit, "{item}.{key}: unit");
        for (old_item, old, new_key) in PAIRS {
            if old_item == item && new_key == key {
                assert_ne!(
                    new[item][old]["source"], new[item][key]["source"],
                    "{item}.{old} (an alias) does not carry the text"
                );
            }
        }
    }
    for (item, old, new_key) in PAIRS {
        if !handover_keys.contains(&(item, new_key)) {
            assert_eq!(
                new[item][new_key], base[item][old],
                "{item}.{new_key} moves unchanged"
            );
        }
    }
    for item in ["drive", "charge", "position", "charge_sample"] {
        for key in keys(&base[item]) {
            let moved_away = PAIRS.iter().any(|row| row.0 == item && row.1 == key);
            if !moved_away && !handover_keys.contains(&(item, key.as_str())) {
                assert_eq!(
                    new[item][&key], base[item][&key],
                    "{item}.{key} is the base's"
                );
            }
        }
    }
    assert_eq!(
        keys(&new["drive"]).len()
            + keys(&new["charge"]).len()
            + keys(&new["position"]).len()
            + keys(&new["charge_sample"]).len(),
        45 + 40 + 31 + 21
    );

    // (c) The `current` entries: 115, every alias entry names its replacement
    // (the miles alias keeps its own unit), and the derived ones carry the
    // staleness and bound statements of C6 R28 to R30.
    let current = &new["current"];
    assert_eq!(current.as_object().unwrap().len(), 115);
    for (old, new_key) in CURRENT_PAIRS {
        assert!(current[new_key].is_object(), "{new_key}");
        assert_eq!(
            current[old]["nullable"], current[new_key]["nullable"],
            "{old}"
        );
        if old == "active_route_miles_to_arrival" {
            assert_eq!(current[old]["unit"], "miles, provider value unchanged");
            assert!(
                current[old]["source"]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("Deprecated alias of {new_key}; "))
            );
        } else {
            assert_eq!(
                current[old]["source"],
                alias_text(new_key).as_str(),
                "{old}"
            );
            assert_eq!(current[old]["unit"], current[new_key]["unit"], "{old}");
        }
    }
    let source = |key: &str| current[key]["source"].as_str().expect(key).to_owned();
    assert!(source("state_since_ms").contains("upper bound"));
    assert!(source("state_since_ms").contains("lower bound"));
    assert!(source("activity_state").contains("There is no age rule"));
    assert!(source("observed_at_ms").contains("compare it with your own clock"));
    // Every derived output field keeps its base unit and carries a rule written from
    // C6 R27 and R28: class T the transient rule (null while asleep or offline, null
    // when the statement predates the open online interval by more than the
    // 60-second skew, null when older than the staleness bound at the later of
    // `observed_at_ms` and the Hub's clock), class P the persistence sentence.
    let transient = "Transient: null while activity_state is asleep or offline; null while the online interval of the car is open and the statement was received more than 60 seconds before that interval began; and null when the statement is older than the Hub's staleness bound (the configured offline drive timeout, 600 seconds by default), measured at the later of observed_at_ms and the Hub's clock.";
    let persistent = "Persistent: the last known value is kept through sleep however old it is; compare observed_at_ms with your own clock.";
    let newest = "The newest statement of the field decides; when it is not valid the field is null and no older value is used.";
    for key in CLASS_T {
        assert!(
            source(key).ends_with(&format!(" {newest} {transient}")),
            "{key} carries the R28 rule"
        );
    }
    for key in CLASS_P {
        assert!(
            source(key).ends_with(&format!(" {newest} {persistent}")),
            "{key} is persistent"
        );
    }
    assert_eq!(CLASS_T.len() + CLASS_P.len(), 25 + 38);
    for key in CLASS_P_PLAIN {
        let text = source(key);
        assert!(
            text.ends_with(&format!(" {persistent}")),
            "{key} is persistent"
        );
        assert!(!text.contains(newest), "{key} has no newest-statement rule");
        assert!(!text.contains("Transient:"), "{key} is not transient");
    }
    // charging_state is class P with the R34 exception: the asleep exception is
    // stated, and so is the persistence of every other state.
    assert_eq!(
        source("charging_state"),
        "The charging state, normalised: Charging, Starting, Stopped, Complete, Disconnected and NoPower in that spelling; Unknown and null give null; any other text is passed through trimmed. While activity_state is asleep, Charging and Starting are null. Otherwise persistent: the last known value is kept through sleep however old it is; compare observed_at_ms with your own clock."
    );
    // display_name is a fallback chain (R39): no newest-statement sentence, which
    // would say null where the Hub serves a fallback name.
    assert_eq!(
        source("display_name"),
        format!(
            "The name the owner gave the car: the newest display name the vehicle list states, else the newest vehicle name of the vehicle state, else the stored car name when it is not empty; trimmed; null when none is usable. A source whose newest statement is not usable text (not a string, empty after trimming or longer than 65536 characters) passes to the next source. {persistent}"
        )
    );
    for (key, entry) in current.as_object().unwrap() {
        let text = entry["source"].as_str().unwrap_or_default();
        assert!(
            !text.contains("materialised identity"),
            "{key}: no generic 1.2.0 sentence is left"
        );
        assert!(
            !text.contains("C6 R") && !text.contains("(C6"),
            "{key}: no internal label is published"
        );
    }
    assert!(source("battery_level").starts_with("Battery level in whole percent, from 0 to 100"));
    assert!(source("latitude").contains("rounded to 6 decimals"));
    assert!(source("scheduled_charging_start_time").contains("epoch seconds"));
    assert_eq!(
        current["car"]["source"],
        format!("car.efficiency_wh_per_km: {CAR_EFFICIENCY}").as_str(),
        "the car entry carries the C2 R82 text"
    );
    for key in ["model_name", "model_year"] {
        assert_eq!(current[key]["nullable"], true, "{key}");
    }

    // (d) R61's two texts, exact, and the phrase occurs exactly twice in the
    // file; the provenance enums and the other values are the base's.
    assert_eq!(
        new["address_provenance"]["source"]["values"]["import"],
        IMPORT_ADDRESS
    );
    assert_eq!(
        new["elevation_provenance"]["source"]["values"]["import"],
        IMPORT_ELEVATION
    );
    let file = std::fs::read_to_string(profile_file("1.3.0", "field-semantics.json")).unwrap();
    assert_eq!(
        file.matches("copied unchanged from the history of an import source")
            .count(),
        2
    );
    for name in ["address_provenance", "elevation_provenance"] {
        let mut left = new[name].clone();
        let mut right = base[name].clone();
        left["source"]["values"]["import"] = Value::Null;
        right["source"]["values"]["import"] = Value::Null;
        assert_eq!(left, right, "{name}: nothing but the import text differs");
    }
    let resources_new = profile_document("1.3.0", "resources.schema.json");
    let resources_base = profile_document("1.2.0", "resources.schema.json");
    for (name, values) in [
        (
            "address_provenance",
            ["import", "geocoder", "manual"].as_slice(),
        ),
        (
            "elevation_provenance",
            ["vehicle", "import", "terrain"].as_slice(),
        ),
    ] {
        assert_eq!(
            resources_new["$defs"][name]["properties"]["source"]["enum"],
            resources_base["$defs"][name]["properties"]["source"]["enum"]
        );
        assert_eq!(
            resources_new["$defs"][name]["properties"]["source"]["enum"],
            serde_json::json!(values)
        );
    }
    assert_eq!(new["null_rule"], base["null_rule"]);
}

// ------------------------------------------------------------------ ST-S50

/// The text between two anchors, when each occurs exactly once and in order.
fn between<'a>(text: &'a str, left: &str, right: &str) -> Option<&'a str> {
    if text.matches(left).count() != 1 || text.matches(right).count() != 1 {
        return None;
    }
    let start = text.find(left)? + left.len();
    let end = text.find(right)?;
    text.get(start..end)
}

/// The problems of the two carried-forward entries of `new` against the
/// base's (C6 ST-S50 (a) to (d)). The span the base text holds is found
/// between its neighbours, so no name is written here; the name is read from
/// the base file for the matcher of (c).
fn neutral_problems(base: &Value, new: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let (left, right) = ("are whole numbers in ", " but are typed number");
    let base_rule = base["number_rule"].as_str().unwrap();
    let new_rule = new["number_rule"].as_str().unwrap_or_default();
    let span = between(base_rule, left, right);
    if !new_rule.contains("whole numbers in the source database") {
        problems.push("number_rule lacks the neutral phrase".to_owned());
    }
    match span {
        Some(span)
            if !span.is_empty()
                && !span.contains(' ')
                && between(new_rule, left, right) == Some("the source database")
                && base_rule.replace(
                    &format!("{left}{span}{right}"),
                    &format!("{left}the source database{right}"),
                ) == new_rule => {}
        _ => {
            problems.push("number_rule differs from the base by more than the one span".to_owned())
        }
    }
    let base_trace = base["source_trace"].as_array().unwrap();
    let new_trace = new["source_trace"].as_array().cloned().unwrap_or_default();
    if new_trace.len() != 5 {
        problems.push("source_trace does not have five entries".to_owned());
        return problems;
    }
    if new_trace[..4] != base_trace[..4] {
        problems.push("source_trace entries 0 to 3 differ".to_owned());
    }
    let (left, right) = ("Hub import", " projection: ");
    let base_entry = base_trace[4].as_str().unwrap();
    let new_entry = new_trace[4].as_str().unwrap_or_default();
    if !new_entry.contains("Hub import projection: ") {
        problems.push("source_trace entry 4 lacks the neutral label".to_owned());
    }
    match between(base_entry, left, right) {
        Some(span)
            if span.starts_with('/')
                && !span.contains(' ')
                && between(new_entry, left, right) == Some("")
                && base_entry
                    .replace(&format!("{left}{span}{right}"), &format!("{left}{right}"))
                    == new_entry => {}
        _ => problems.push(
            "source_trace entry 4 differs from the base by more than the span and its slash"
                .to_owned(),
        ),
    }
    problems
}

/// C6 R68's matcher in the part a test can state without writing the name: the
/// span read from the base text, case-folded, with a run of up to six
/// characters that are neither letters nor digits allowed between any two of
/// its characters (the name in one word, and every joined form of its parts).
fn name_hits(text: &str, name: &str) -> usize {
    let folded: Vec<char> = name.to_lowercase().chars().collect();
    let haystack: Vec<char> = text.to_lowercase().chars().collect();
    let mut hits = 0;
    let mut start = 0;
    while start < haystack.len() {
        let mut at = start;
        let mut matched = true;
        for (index, wanted) in folded.iter().enumerate() {
            if index > 0 {
                let mut skipped = 0;
                while skipped < 6 && at < haystack.len() && !haystack[at].is_alphanumeric() {
                    at += 1;
                    skipped += 1;
                }
            }
            if haystack.get(at) == Some(wanted) {
                at += 1;
            } else {
                matched = false;
                break;
            }
        }
        if matched {
            hits += 1;
            start = at;
        } else {
            start += 1;
        }
    }
    hits
}

fn every_file(root: &std::path::Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).expect("profile directory") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            every_file(&path, files);
        } else {
            files.push(path);
        }
    }
}

#[test]
fn c6_st_s50_neutral_entries() {
    let base = profile_document("1.2.0", "field-semantics.json");
    let new = profile_document("1.3.0", "field-semantics.json");

    // (a), (b), (d): the exact transformation of the two entries, and the rest.
    assert_eq!(neutral_problems(&base, &new), Vec::<String>::new());
    assert_eq!(
        new["number_rule"],
        "position.power and the charge_sample fields charger_power_kw, charger_voltage, charger_actual_current and charger_pilot_current are whole numbers in the source database but are typed number, because the page routes serve the materialised projections of the Hub, which hold them as floating point: a whole value may carry a fraction digit (12 or 12.0). Clients accept both. Retyping these fields later is a new major profile."
    );
    assert_eq!(new["source_trace"].as_array().unwrap().len(), 5);
    assert!(
        new["source_trace"][4]
            .as_str()
            .unwrap()
            .starts_with("Hub import projection: ")
    );
    for index in 0..4 {
        assert_eq!(
            new["source_trace"][index], base["source_trace"][index],
            "entry {index}"
        );
    }

    // The mutants of the two entries are each reported: the base text left in
    // either entry; the slash left; an entry dropped, moved or duplicated; any
    // other word of `number_rule` changed.
    let mutants: Vec<(&str, Value)> = vec![
        ("number_rule left unchanged", {
            let mut copy = new.clone();
            copy["number_rule"] = base["number_rule"].clone();
            copy
        }),
        ("source_trace[4] left unchanged", {
            let mut copy = new.clone();
            copy["source_trace"][4] = base["source_trace"][4].clone();
            copy
        }),
        ("slash left before projection", {
            let mut copy = new.clone();
            let entry = copy["source_trace"][4]
                .as_str()
                .unwrap()
                .replace("Hub import projection", "Hub import/ projection");
            copy["source_trace"][4] = Value::String(entry);
            copy
        }),
        ("entry 4 dropped", {
            let mut copy = new.clone();
            copy["source_trace"].as_array_mut().unwrap().pop();
            copy
        }),
        ("entry 4 moved", {
            let mut copy = new.clone();
            let entry = copy["source_trace"].as_array_mut().unwrap().pop().unwrap();
            copy["source_trace"]
                .as_array_mut()
                .unwrap()
                .insert(0, entry);
            copy
        }),
        ("entry 4 duplicated", {
            let mut copy = new.clone();
            let entry = copy["source_trace"][4].clone();
            copy["source_trace"].as_array_mut().unwrap().push(entry);
            copy
        }),
        ("another word of number_rule changed", {
            let mut copy = new.clone();
            let rule = copy["number_rule"]
                .as_str()
                .unwrap()
                .replace("Clients accept both", "Clients accept either");
            copy["number_rule"] = Value::String(rule);
            copy
        }),
    ];
    for (label, mutant) in mutants {
        assert!(
            !neutral_problems(&base, &mutant).is_empty(),
            "the mutant is reported: {label}"
        );
    }

    // (c) The matcher over the whole new minor, with its control over the
    // base: the span read from the base text hits the base four times (in this
    // file: the two entries and the two import texts) and the new minor never.
    let name = between(
        base["number_rule"].as_str().unwrap(),
        "are whole numbers in ",
        " but are typed number",
    )
    .expect("the span of the base text");
    let base_file = std::fs::read_to_string(profile_file("1.2.0", "field-semantics.json")).unwrap();
    assert!(
        name_hits(&base_file, name) >= 4,
        "control: the base field semantics hold the name at least four times"
    );
    let mut files = Vec::new();
    every_file(&profile_file("1.3.0", ""), &mut files);
    assert!(
        files.len() >= 31,
        "the whole minor is scanned: {} files",
        files.len()
    );
    for path in files {
        let text = std::fs::read_to_string(&path).expect("UTF-8 profile file");
        assert_eq!(name_hits(&text, name), 0, "{}", path.display());
        assert_eq!(
            name_hits(&path.display().to_string(), name),
            0,
            "{}: the file name",
            path.display()
        );
    }
}
