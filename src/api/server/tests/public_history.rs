// SPDX-License-Identifier: AGPL-3.0-only

//! The hub-http-v1@1.2.0 history read routes: drives, the drive item, drive
//! positions, charges, the charge item and charge samples. Every test uses a
//! real SQLite store seeded with synthetic rows (no real coordinates, VINs or
//! tokens) and the in-process router.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::*;
use crate::{
    api::public_query::{MAX_PUBLIC_PAGE_BYTES, PageKind, PageQuery},
    hub_pack::{GeofenceBillingType, ProjectionCharge, ProjectionChargeSample, ProjectionPosition},
};

#[path = "vehicle_grants.rs"]
mod vehicle_grants;

const KEY_BYTE: u8 = 71;

fn vehicle_a() -> Uuid {
    Uuid::from_u128(0x1111_1111_1111_4111_8111_1111_1111_aaaa)
}

fn vehicle_b() -> Uuid {
    Uuid::from_u128(0x2222_2222_2222_4222_8222_2222_2222_bbbb)
}

fn vehicle_c() -> Uuid {
    Uuid::from_u128(0x3333_3333_3333_4333_8333_3333_3333_cccc)
}

fn cursor_key() -> CursorKey {
    CursorKey::from_bytes([KEY_BYTE; 32])
}

fn json_text<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).expect("row JSON")
}

// ---------------------------------------------------------------- fixtures

fn charge_fixture(id: i64, start_date_ms: i64, end_date_ms: Option<i64>) -> ProjectionCharge {
    ProjectionCharge {
        id,
        car_id: 1,
        start_date_ms,
        end_date_ms,
        charge_energy_added: Some(21.5),
        charge_energy_used_kwh: Some(22.0),
        start_ideal_range_km: Some(100.0),
        end_ideal_range_km: Some(200.0),
        cost: Some(4.5),
        fast_charger_type: Some("synthetic-type".to_owned()),
        billing_type: Some(GeofenceBillingType::PerKwh),
        cost_per_unit: Some(0.21),
        session_fee: Some(0.0),
        start_latitude: Some(0.0),
        start_longitude: Some(0.0),
        start_battery_level: Some(30),
        end_battery_level: Some(60),
        duration_min: Some(43),
        address: Some("Synthetic charging address".to_owned()),
        location_name: Some("Synthetic location".to_owned()),
        geofence: Some("Synthetic geofence".to_owned()),
        is_dc: Some(false),
        charge_rate_km_per_hour: Some(31.0),
        max_charger_power_kw: Some(11.0),
        outside_temp_avg: Some(10.5),
        start_rated_range_km: Some(90.0),
        end_rated_range_km: Some(180.0),
    }
}

fn position_fixture(id: i64, drive_id: i64, date_ms: i64) -> ProjectionPosition {
    ProjectionPosition {
        id,
        drive_id: Some(drive_id),
        car_id: 1,
        date_ms,
        latitude: 0.0,
        longitude: 0.0,
        speed: Some(30),
        power: Some(12.0),
        battery_level: Some(80),
        usable_battery_level: Some(79),
        elevation: Some(100),
        odometer: Some(1000.5),
        ideal_battery_range_km: Some(300.0),
        est_battery_range_km: Some(290.0),
        rated_battery_range_km: Some(280.0),
        fan_status: Some(2),
        driver_temp_setting: Some(21.0),
        passenger_temp_setting: Some(21.5),
        is_climate_on: Some(true),
        is_rear_defroster_on: Some(false),
        is_front_defroster_on: Some(false),
        inside_temp: Some(20.0),
        outside_temp: Some(10.0),
        battery_heater: Some(false),
        battery_heater_on: Some(true),
        battery_heater_no_power: Some(false),
        tpms_pressure_fl: Some(2.9),
        tpms_pressure_fr: Some(2.9),
        tpms_pressure_rl: Some(2.8),
        tpms_pressure_rr: Some(2.8),
    }
}

fn sample_fixture(id: i64, charge_id: i64, timestamp_ms: i64) -> ProjectionChargeSample {
    ProjectionChargeSample {
        id,
        charge_process_id: charge_id,
        timestamp_ms,
        battery_level: Some(31),
        usable_battery_level: Some(30),
        charge_energy_added_kwh: Some(0.1),
        charger_power_kw: Some(7.0),
        charger_voltage: Some(230.0),
        charger_actual_current: Some(30.0),
        charger_pilot_current: Some(32.0),
        charger_phases: Some(1),
        ideal_range_km: Some(150.0),
        rated_range_km: Some(140.0),
        outside_temp_c: Some(9.5),
        battery_heater_on: Some(false),
        battery_heater: Some(false),
        battery_heater_no_power: Some(false),
        not_enough_power_to_heat: Some(false),
        fast_charger_present: Some(false),
        fast_charger_brand: Some("synthetic-brand".to_owned()),
        fast_charger_type: Some("synthetic-type".to_owned()),
        charge_cable: Some("IEC".to_owned()),
    }
}

fn insert_drive_json(store: &HubStore, vehicle: Uuid, id: i64, json: &serde_json::Value) {
    store
        .open()
        .expect("open store")
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![vehicle.to_string(), id, json.to_string()],
        )
        .expect("seed materialised drive JSON");
}

fn insert_open_drive(store: &HubStore, vehicle: Uuid, id: i64, start_date_ms: i64) {
    let mut json = serde_json::to_value(public_drive_fixture(id, start_date_ms)).expect("drive");
    json["end_date_ms"] = serde_json::Value::Null;
    insert_drive_json(store, vehicle, id, &json);
}

fn insert_charge(store: &HubStore, vehicle: Uuid, charge: &ProjectionCharge) {
    store
        .open()
        .expect("open store")
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, ?2, 1, ?3)",
            rusqlite::params![
                vehicle.to_string(),
                charge.id,
                serde_json::to_string(charge).expect("charge JSON")
            ],
        )
        .expect("seed materialised charge");
}

fn insert_position(store: &HubStore, vehicle: Uuid, position: &ProjectionPosition) {
    store
        .open()
        .expect("open store")
        .execute(
            "INSERT INTO materialised_positions(
                 vehicle_id, position_id, drive_id, car_id, position_json
             ) VALUES (?1, ?2, ?3, 1, ?4)",
            rusqlite::params![
                vehicle.to_string(),
                position.id,
                position.drive_id,
                serde_json::to_string(position).expect("position JSON")
            ],
        )
        .expect("seed materialised position");
}

fn insert_sample(store: &HubStore, vehicle: Uuid, sample: &ProjectionChargeSample) {
    store
        .open()
        .expect("open store")
        .execute(
            "INSERT INTO materialised_charge_samples(
                 vehicle_id, sample_id, charge_id, sample_json
             ) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                vehicle.to_string(),
                sample.id,
                sample.charge_process_id,
                serde_json::to_string(sample).expect("sample JSON")
            ],
        )
        .expect("seed materialised charge sample");
}

fn insert_terrain_row(
    store: &HubStore,
    vehicle: Uuid,
    position_id: i64,
    drive_id: i64,
    status: &str,
) {
    store
        .open()
        .expect("open store")
        .execute(
            "INSERT INTO terrain_elevation_provenance(
                 vehicle_id, position_id, drive_id, latitude, longitude, elevation_m,
                 tile_name, tile_hash, dataset_source, dataset_version, status,
                 error_code, attempts, attempted_at_ms, retry_after_ms
             ) VALUES (?1, ?2, ?3, 0.0, 0.0, NULL, 'synthetic-tile', NULL,
                       'synthetic-terrain', '1', ?4, NULL, 1, 1000, 0)",
            rusqlite::params![vehicle.to_string(), position_id, drive_id, status],
        )
        .expect("seed terrain provenance");
}

/// Seed one vehicle with the standard synthetic history and publish it.
///
/// Drives (start): 10 (1000), 11 (2000), 12 (3000), 13 (3000), 14 (5000), plus
/// the unfinished drive 15 (6000). Positions of drive 10: 101..=105 at
/// 1000, 1100, 1100, 1300, 1400 (a date tie across a page boundary); drive 11:
/// 111, 112; drive 12: none; the unfinished drive 15: 151.
/// Charges (start): 20 (1000), 21 (2000), 22 (3000), 23 (3000), 24 (5000), plus
/// the unfinished charge 25 (6000). Samples of charge 20: 201..=205 at
/// 1000, 1100, 1100, 1300, 1400; charge 21: 211, 212; charge 22: none; the
/// unfinished charge 25: 251.
fn seed_standard_history(store: &HubStore, vehicle: Uuid, source: Uuid) {
    seed_active_vehicle_identity(store, source, vehicle);
    for (id, start) in [
        (10, 1_000),
        (11, 2_000),
        (12, 3_000),
        (13, 3_000),
        (14, 5_000),
    ] {
        seed_public_drive(store, vehicle, &public_drive_fixture(id, start));
    }
    insert_open_drive(store, vehicle, 15, 6_000);
    for (id, date) in [
        (101, 1_000),
        (102, 1_100),
        (103, 1_100),
        (104, 1_300),
        (105, 1_400),
    ] {
        insert_position(store, vehicle, &position_fixture(id, 10, date));
    }
    for (id, date) in [(111, 2_000), (112, 2_100)] {
        insert_position(store, vehicle, &position_fixture(id, 11, date));
    }
    insert_position(store, vehicle, &position_fixture(151, 15, 6_000));
    for (id, start) in [
        (20, 1_000),
        (21, 2_000),
        (22, 3_000),
        (23, 3_000),
        (24, 5_000),
    ] {
        insert_charge(
            store,
            vehicle,
            &charge_fixture(id, start, Some(start + 500)),
        );
    }
    insert_charge(store, vehicle, &charge_fixture(25, 6_000, None));
    for (id, timestamp) in [
        (201, 1_000),
        (202, 1_100),
        (203, 1_100),
        (204, 1_300),
        (205, 1_400),
    ] {
        insert_sample(store, vehicle, &sample_fixture(id, 20, timestamp));
    }
    for (id, timestamp) in [(211, 2_000), (212, 2_100)] {
        insert_sample(store, vehicle, &sample_fixture(id, 21, timestamp));
    }
    insert_sample(store, vehicle, &sample_fixture(251, 25, 6_000));
    mark_vehicle_published(store, vehicle);
}

struct History {
    _temp: tempfile::TempDir,
    store: HubStore,
}

impl History {
    /// Vehicles A and B hold identical data, C is published and empty.
    fn new() -> Self {
        let temp = crate::private_tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        seed_standard_history(&store, vehicle_a(), Uuid::from_u128(0xa1));
        seed_standard_history(&store, vehicle_b(), Uuid::from_u128(0xb1));
        seed_active_vehicle_identity(&store, Uuid::from_u128(0xc1), vehicle_c());
        mark_vehicle_published(&store, vehicle_c());
        // A closed drive and charge without any child rows.
        for vehicle in [vehicle_a(), vehicle_b()] {
            seed_public_drive(&store, vehicle, &public_drive_fixture(16, 100));
            insert_charge(&store, vehicle, &charge_fixture(26, 100, Some(200)));
        }
        Self { _temp: temp, store }
    }

    fn app(&self) -> Router {
        public_query_router(self.store.clone(), KEY_BYTE)
    }
}

/// One page route over vehicle A's standard data.
struct PageRoute {
    label: &'static str,
    kind: PageKind,
    base: String,
    /// The route of the same kind under a sibling parent (nested routes only).
    sibling_base: Option<String>,
    scope_id: i64,
    expected_ids: Vec<i64>,
    keys: Vec<i64>,
    key_name: &'static str,
    window: (i64, i64),
    window_ids: Vec<i64>,
    /// An ordering key shared by two items and the ids that carry it.
    tie_key: i64,
    tie_ids: Vec<i64>,
    empty_base: String,
}

fn page_routes(vehicle: Uuid) -> Vec<PageRoute> {
    vec![
        PageRoute {
            label: "drives",
            kind: PageKind::Drives,
            base: format!("/v1/vehicles/{vehicle}/drives"),
            sibling_base: None,
            scope_id: 0,
            // 15 is unfinished and 16 starts earliest.
            expected_ids: vec![14, 13, 12, 11, 10, 16],
            keys: vec![5_000, 3_000, 3_000, 2_000, 1_000, 100],
            key_name: "start_date_ms",
            window: (2_000, 5_000),
            window_ids: vec![13, 12, 11],
            tie_key: 3_000,
            tie_ids: vec![13, 12],
            empty_base: format!("/v1/vehicles/{}/drives", vehicle_c()),
        },
        PageRoute {
            label: "positions",
            kind: PageKind::Positions,
            base: format!("/v1/vehicles/{vehicle}/drives/10/positions"),
            sibling_base: Some(format!("/v1/vehicles/{vehicle}/drives/11/positions")),
            scope_id: 10,
            expected_ids: vec![101, 102, 103, 104, 105],
            keys: vec![1_000, 1_100, 1_100, 1_300, 1_400],
            key_name: "date_ms",
            window: (1_100, 1_400),
            window_ids: vec![102, 103, 104],
            tie_key: 1_100,
            tie_ids: vec![102, 103],
            empty_base: format!("/v1/vehicles/{vehicle}/drives/16/positions"),
        },
        PageRoute {
            label: "charges",
            kind: PageKind::Charges,
            base: format!("/v1/vehicles/{vehicle}/charges"),
            sibling_base: None,
            scope_id: 0,
            expected_ids: vec![24, 23, 22, 21, 20, 26],
            keys: vec![5_000, 3_000, 3_000, 2_000, 1_000, 100],
            key_name: "start_date_ms",
            window: (2_000, 5_000),
            window_ids: vec![23, 22, 21],
            tie_key: 3_000,
            tie_ids: vec![23, 22],
            empty_base: format!("/v1/vehicles/{}/charges", vehicle_c()),
        },
        PageRoute {
            label: "charge_samples",
            kind: PageKind::ChargeSamples,
            base: format!("/v1/vehicles/{vehicle}/charges/20/samples"),
            sibling_base: Some(format!("/v1/vehicles/{vehicle}/charges/21/samples")),
            scope_id: 20,
            expected_ids: vec![201, 202, 203, 204, 205],
            keys: vec![1_000, 1_100, 1_100, 1_300, 1_400],
            key_name: "timestamp_ms",
            window: (1_100, 1_400),
            window_ids: vec![202, 203, 204],
            tie_key: 1_100,
            tie_ids: vec![202, 203],
            empty_base: format!("/v1/vehicles/{vehicle}/charges/26/samples"),
        },
    ]
}

// ----------------------------------------------------------------- requests

async fn call_with(app: &Router, uri: &str, headers: &[(&str, &str)]) -> Response {
    let mut request = Request::builder().uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    app.clone()
        .oneshot(request.body(Body::empty()).expect("request"))
        .await
        .expect("in-memory response")
}

async fn call(app: &Router, uri: &str) -> Response {
    call_with(app, uri, &[]).await
}

async fn body_bytes(response: Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes()
        .to_vec()
}

fn ids_of(page: &serde_json::Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|item| item["id"].as_i64().expect("item id"))
        .collect()
}

fn with_query(base: &str, query: &str) -> String {
    if query.is_empty() {
        base.to_owned()
    } else {
        format!("{base}?{query}")
    }
}

async fn expect_error(response: Response, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status, "status for error {code}");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], code);
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    assert_eq!(body.as_object().expect("error envelope").len(), 1);
}

/// Walk one route to its end, returning every page.
async fn walk(app: &Router, base: &str, query: &str) -> Vec<serde_json::Value> {
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut parts = Vec::new();
        if !query.is_empty() {
            parts.push(query.to_owned());
        }
        if let Some(cursor) = cursor.as_ref() {
            parts.push(format!("cursor={cursor}"));
        }
        let response = call(app, &with_query(base, &parts.join("&"))).await;
        assert_eq!(response.status(), StatusCode::OK, "walk {base}");
        let page = response_json(response).await;
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        pages.push(page);
        assert!(pages.len() <= 20, "bounded walk");
        if cursor.is_none() {
            return pages;
        }
    }
}

/// A cursor signed with the test key, for binding checks.
fn craft_cursor(
    kind: PageKind,
    vehicle: Uuid,
    scope_id: i64,
    from_ms: i64,
    to_ms: i64,
    last: (i64, i64),
) -> String {
    PageQuery {
        kind,
        vehicle_id: vehicle,
        scope_id,
        from_ms,
        to_ms,
        limit: 1,
        after: None,
    }
    .next_cursor(&cursor_key(), last)
}

fn bearer_for(store: &HubStore) -> String {
    let now_ms = current_epoch_ms().expect("pairing clock");
    let invitation = store
        .create_pairing("history test", now_ms - 1, i64::MAX)
        .expect("pairing invitation");
    let access = store
        .claim_pairing(
            invitation.pairing_id,
            invitation.secret(),
            "history client",
            now_ms,
        )
        .expect("paired access");
    access.access_token.as_bearer().to_owned()
}

// --------------------------------------------------------- contract helpers

fn contract_defs() -> serde_json::Value {
    let path = format!(
        "{}/protocol/profiles/hub-http-v1/1.2.0/resources.schema.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = fs::read_to_string(path).expect("committed hub-http-v1@1.2.0 resources schema");
    let schema: serde_json::Value = serde_json::from_str(&text).expect("schema JSON");
    schema["$defs"].clone()
}

fn schema_allows(schema: &serde_json::Value, value: &serde_json::Value) -> bool {
    if let Some(options) = schema.get("anyOf").and_then(|options| options.as_array()) {
        return options.iter().any(|option| schema_allows(option, value));
    }
    if schema.get("$ref").is_some() {
        return value.is_object();
    }
    match schema["type"].as_str() {
        Some("integer") => value.is_i64() || value.is_u64(),
        Some("number") => value.is_number(),
        Some("string") => value.is_string(),
        Some("boolean") => value.is_boolean(),
        Some("null") => value.is_null(),
        Some("object") => value.is_object(),
        _ => false,
    }
}

/// An item carries exactly the contract's property names (less the optional
/// members this batch deliberately omits) and every value has a contract type.
fn assert_item_matches_contract(item: &serde_json::Value, definition: &str, omitted: &[&str]) {
    let defs = contract_defs();
    let properties = defs[definition]["properties"]
        .as_object()
        .unwrap_or_else(|| panic!("contract definition {definition}"));
    let expected: BTreeSet<String> = properties
        .keys()
        .filter(|name| !omitted.contains(&name.as_str()))
        .cloned()
        .collect();
    let object = item.as_object().expect("item object");
    let actual: BTreeSet<String> = object.keys().cloned().collect();
    assert_eq!(
        actual, expected,
        "{definition} fields differ from the contract"
    );
    for required in defs[definition]["required"]
        .as_array()
        .expect("required list")
    {
        assert!(
            object.contains_key(required.as_str().expect("required name")),
            "{definition} lacks required {required}"
        );
    }
    for (name, value) in object {
        assert!(
            schema_allows(&properties[name], value),
            "{definition}.{name} = {value} violates {}",
            properties[name]
        );
    }
}

// ------------------------------------------------------------------- pages

#[tokio::test]
async fn page_routes_walk_three_pages_with_keyset_continuity() {
    let history = History::new();
    let app = history.app();
    for route in page_routes(vehicle_a()) {
        let pages = walk(&app, &route.base, "limit=2").await;
        let counts: Vec<usize> = pages
            .iter()
            .map(|page| page["items"].as_array().expect("items").len())
            .collect();
        let expected_counts: Vec<usize> = route.expected_ids.chunks(2).map(<[i64]>::len).collect();
        assert_eq!(counts, expected_counts, "{} page sizes", route.label);
        assert_eq!(counts.len(), 3, "{} takes three pages", route.label);
        let walked: Vec<i64> = pages.iter().flat_map(ids_of).collect();
        assert_eq!(
            walked, route.expected_ids,
            "{} order and continuity",
            route.label
        );
        assert_eq!(
            walked.iter().collect::<BTreeSet<_>>().len(),
            walked.len(),
            "{} duplicates",
            route.label
        );
        let last = pages.last().expect("last page");
        assert!(last["next_cursor"].is_null(), "{}", route.label);
        for page in &pages[..pages.len() - 1] {
            let cursor = page["next_cursor"].as_str().expect("interior cursor");
            assert!(cursor.starts_with("tqp1."), "{} issues tqp1", route.label);
            assert_eq!(cursor.len(), 186, "{} cursor length", route.label);
        }
        // Items carry the ordering key that the cursor continues from.
        let walked_keys: Vec<i64> = pages
            .iter()
            .flat_map(|page| page["items"].as_array().expect("items").clone())
            .map(|item| item[route.key_name].as_i64().expect("ordering key"))
            .collect();
        assert_eq!(walked_keys, route.keys, "{} keys", route.label);

        // The default limit serves everything in one page without a cursor.
        let single = walk(&app, &route.base, "").await;
        assert_eq!(single.len(), 1, "{}", route.label);
        assert_eq!(ids_of(&single[0]), route.expected_ids);
        assert!(single[0]["next_cursor"].is_null());

        // A limit equal to the row count has no next page; one below has.
        let exact = walk(
            &app,
            &route.base,
            &format!("limit={}", route.expected_ids.len()),
        )
        .await;
        assert_eq!(exact.len(), 1, "{} exact fit", route.label);
        let below = walk(
            &app,
            &route.base,
            &format!("limit={}", route.expected_ids.len() - 1),
        )
        .await;
        assert_eq!(below.len(), 2, "{} one below", route.label);
        assert_eq!(ids_of(&below[1]), vec![*route.expected_ids.last().unwrap()]);

        // The limit may change between pages; the walk stays gapless.
        let first = call(&app, &with_query(&route.base, "limit=2")).await;
        let cursor = response_json(first).await["next_cursor"]
            .as_str()
            .expect("cursor")
            .to_owned();
        let resumed = call(
            &app,
            &with_query(&route.base, &format!("limit=1&cursor={cursor}")),
        )
        .await;
        assert_eq!(resumed.status(), StatusCode::OK, "{}", route.label);
        assert_eq!(
            ids_of(&response_json(resumed).await),
            vec![route.expected_ids[2]]
        );
    }
}

#[tokio::test]
async fn unfinished_sessions_are_never_listed() {
    let history = History::new();
    let app = history.app();
    let routes = page_routes(vehicle_a());
    for (route, unfinished) in [(&routes[0], 15), (&routes[2], 25)] {
        let walked: Vec<i64> = walk(&app, &route.base, "limit=2")
            .await
            .iter()
            .flat_map(ids_of)
            .collect();
        assert!(!walked.contains(&unfinished), "{}", route.label);
        assert_eq!(walked.len(), route.expected_ids.len());
    }
}

#[tokio::test]
async fn page_routes_filter_by_a_half_open_window() {
    let history = History::new();
    let app = history.app();
    for route in page_routes(vehicle_a()) {
        let (from_ms, to_ms) = route.window;
        let query = format!("from_ms={from_ms}&to_ms={to_ms}");
        let pages = walk(&app, &route.base, &query).await;
        assert_eq!(pages.len(), 1, "{}", route.label);
        assert_eq!(
            ids_of(&pages[0]),
            route.window_ids,
            "{} window",
            route.label
        );
        // The same window walked two items at a time stays inside it.
        let paged: Vec<i64> = walk(&app, &route.base, &format!("{query}&limit=2"))
            .await
            .iter()
            .flat_map(ids_of)
            .collect();
        assert_eq!(paged, route.window_ids, "{} paged window", route.label);
        // from is inclusive and to is exclusive: one key selects exactly its items.
        let tie = walk(
            &app,
            &route.base,
            &format!("from_ms={}&to_ms={}", route.tie_key, route.tie_key + 1),
        )
        .await;
        assert_eq!(ids_of(&tie[0]), route.tie_ids, "{} one key", route.label);
        let above = walk(
            &app,
            &route.base,
            &format!("from_ms={}&to_ms={}", route.tie_key + 1, route.tie_key + 2),
        )
        .await;
        assert!(
            ids_of(&above[0])
                .iter()
                .all(|id| !route.tie_ids.contains(id)),
            "{} exclusive to",
            route.label
        );
        let beyond = walk(&app, &route.base, "from_ms=9000&to_ms=9100").await;
        assert_eq!(
            beyond[0]["items"].as_array().unwrap().len(),
            0,
            "{}",
            route.label
        );
    }
}

#[tokio::test]
async fn empty_pages_are_an_empty_item_list_without_a_cursor() {
    let history = History::new();
    let app = history.app();
    let routes = page_routes(vehicle_a());
    for route in &routes {
        let response = call(&app, &route.empty_base).await;
        assert_eq!(response.status(), StatusCode::OK, "{}", route.label);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let etag = response.headers()[header::ETAG].clone();
        assert_eq!(
            String::from_utf8(body_bytes(response).await).unwrap(),
            r#"{"items":[],"next_cursor":null}"#,
            "{}",
            route.label
        );
        // An empty page is conditional like any other.
        let again = call_with(
            &app,
            &route.empty_base,
            &[("if-none-match", etag.to_str().unwrap())],
        )
        .await;
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED, "{}", route.label);
    }
}

async fn expect_invalid_cursor(app: &Router, uri: &str, context: &str) {
    let response = call(app, uri).await;
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "{context}: {uri}"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let body = response_json(response).await;
    assert_eq!(body["error"]["code"], "invalid_cursor", "{context}: {uri}");
}

#[tokio::test]
async fn page_cursors_are_bound_to_kind_vehicle_scope_window_and_tag() {
    let history = History::new();
    let app = history.app();
    let vehicle = vehicle_a();
    for route in page_routes(vehicle) {
        let key_a = route.keys[1];
        let last = (key_a, route.expected_ids[1]);
        let valid = craft_cursor(route.kind, vehicle, route.scope_id, 0, i64::MAX, last);
        let accepted = call(&app, &with_query(&route.base, &format!("cursor={valid}"))).await;
        assert_eq!(
            accepted.status(),
            StatusCode::OK,
            "{} base cursor is valid",
            route.label
        );

        // Each entry differs from a valid cursor in exactly one binding.
        let mut rejected: Vec<(&str, String)> = Vec::new();
        for other in [
            PageKind::Drives,
            PageKind::Charges,
            PageKind::Positions,
            PageKind::ChargeSamples,
        ] {
            if other != route.kind {
                let cursor = craft_cursor(other, vehicle, route.scope_id, 0, i64::MAX, last);
                rejected.push(("kind", format!("cursor={cursor}")));
            }
        }
        let cursor = craft_cursor(route.kind, vehicle_b(), route.scope_id, 0, i64::MAX, last);
        rejected.push(("vehicle", format!("cursor={cursor}")));
        let cursor = craft_cursor(route.kind, vehicle, route.scope_id + 1, 0, i64::MAX, last);
        rejected.push(("scope", format!("cursor={cursor}")));
        let cursor = craft_cursor(route.kind, vehicle, route.scope_id, 1, i64::MAX, last);
        rejected.push((
            "cursor minted for another from_ms",
            format!("cursor={cursor}"),
        ));
        let cursor = craft_cursor(route.kind, vehicle, route.scope_id, 0, i64::MAX - 1, last);
        rejected.push((
            "cursor minted for another to_ms",
            format!("cursor={cursor}"),
        ));
        // The request's window differs from the cursor's.
        rejected.push((
            "request from_ms differs",
            format!("from_ms=1&cursor={valid}"),
        ));
        rejected.push((
            "request to_ms differs",
            format!("to_ms={}&cursor={valid}", i64::MAX - 1),
        ));
        // Last keys outside the window, or a non-positive id.
        let cursor = craft_cursor(
            route.kind,
            vehicle,
            route.scope_id,
            key_a + 1,
            i64::MAX,
            last,
        );
        rejected.push((
            "key below from_ms",
            format!("from_ms={}&cursor={cursor}", key_a + 1),
        ));
        let cursor = craft_cursor(route.kind, vehicle, route.scope_id, 0, key_a, last);
        rejected.push(("key at to_ms", format!("to_ms={key_a}&cursor={cursor}")));
        let cursor = craft_cursor(route.kind, vehicle, route.scope_id, 0, i64::MAX, (key_a, 0));
        rejected.push(("non-positive id", format!("cursor={cursor}")));
        let cursor = craft_cursor(
            route.kind,
            vehicle,
            route.scope_id,
            0,
            i64::MAX,
            (key_a, -5),
        );
        rejected.push(("negative id", format!("cursor={cursor}")));
        // Tampered tag, upper-case hex, wrong prefix, truncation, garbage.
        let mut tampered = valid.clone().into_bytes();
        let last_byte = tampered.last_mut().expect("cursor byte");
        *last_byte = if *last_byte == b'a' { b'b' } else { b'a' };
        rejected.push((
            "tampered tag",
            format!("cursor={}", String::from_utf8(tampered).unwrap()),
        ));
        let mut parts: Vec<String> = valid.split('.').map(str::to_owned).collect();
        parts[2] = parts[2].to_uppercase();
        assert_ne!(
            parts[2],
            valid.split('.').nth(2).unwrap(),
            "the tag has a letter"
        );
        rejected.push(("upper-case tag", format!("cursor={}", parts.join("."))));
        let mut parts: Vec<String> = valid.split('.').map(str::to_owned).collect();
        parts[1] = parts[1].to_uppercase();
        assert_ne!(
            parts[1],
            valid.split('.').nth(1).unwrap(),
            "the payload has a letter"
        );
        rejected.push(("upper-case payload", format!("cursor={}", parts.join("."))));
        rejected.push((
            "prefix",
            format!("cursor={}", valid.replacen("tqp1", "tqp2", 1)),
        ));
        rejected.push(("truncated", format!("cursor={}", &valid[..valid.len() - 2])));
        rejected.push(("extra segment", format!("cursor={valid}.00")));
        rejected.push(("garbage", "cursor=garbage".to_owned()));
        rejected.push(("empty", "cursor=".to_owned()));
        // A legacy tqd1 cursor is accepted on drives only.
        if route.kind != PageKind::Drives {
            let legacy = legacy_drive_cursor(vehicle, 0, i64::MAX, last);
            rejected.push(("tqd1 outside drives", format!("cursor={legacy}")));
        }
        for (reason, query) in rejected {
            expect_invalid_cursor(&app, &with_query(&route.base, &query), reason).await;
        }
    }

    // A real cursor of one drive's positions (or one charge's samples) on a
    // sibling parent: only the scope differs.
    for route in page_routes(vehicle) {
        let Some(sibling) = route.sibling_base.as_ref() else {
            continue;
        };
        let first = call(&app, &with_query(&route.base, "limit=2")).await;
        let cursor = response_json(first).await["next_cursor"]
            .as_str()
            .expect("cursor")
            .to_owned();
        expect_invalid_cursor(
            &app,
            &with_query(sibling, &format!("limit=2&cursor={cursor}")),
            "sibling scope",
        )
        .await;
    }

    // A real cursor of vehicle A on vehicle B, and under another cursor key.
    for route in page_routes(vehicle_a()) {
        let first = call(&app, &with_query(&route.base, "limit=2")).await;
        let cursor = response_json(first).await["next_cursor"]
            .as_str()
            .expect("cursor")
            .to_owned();
        let on_b = route
            .base
            .replace(&vehicle_a().to_string(), &vehicle_b().to_string());
        let query = format!("limit=2&cursor={cursor}");
        expect_invalid_cursor(&app, &with_query(&on_b, &query), "other vehicle").await;
        let other_key = public_query_router(history.store.clone(), KEY_BYTE + 1);
        expect_invalid_cursor(&other_key, &with_query(&route.base, &query), "other key").await;
    }
}

/// The cursor of hub-http-v1@1.0.0/1.1.0 drives: 49-byte payload, never issued.
fn legacy_drive_cursor(vehicle: Uuid, from_ms: i64, to_ms: i64, last: (i64, i64)) -> String {
    let mut payload = [0_u8; 49];
    payload[0] = 1;
    payload[1..17].copy_from_slice(vehicle.as_bytes());
    payload[17..25].copy_from_slice(&from_ms.to_be_bytes());
    payload[25..33].copy_from_slice(&to_ms.to_be_bytes());
    payload[33..41].copy_from_slice(&last.0.to_be_bytes());
    payload[41..49].copy_from_slice(&last.1.to_be_bytes());
    let tag = cursor_key().public_query_cursor_tag(&payload);
    format!("tqd1.{}.{}", hex::encode(payload), hex::encode(tag))
}

#[tokio::test]
async fn drives_accept_the_legacy_tqd1_cursor_and_issue_tqp1() {
    let history = History::new();
    let app = history.app();
    let base = format!("/v1/vehicles/{}/drives", vehicle_a());
    // Continue after drive 13 (start 3000, id 13): drives 12, 11, 10, 16 follow.
    let legacy = legacy_drive_cursor(vehicle_a(), 0, i64::MAX, (3_000, 13));
    let response = call(
        &app,
        &with_query(&base, &format!("limit=2&cursor={legacy}")),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = response_json(response).await;
    assert_eq!(ids_of(&page), vec![12, 11]);
    assert!(
        page["next_cursor"].as_str().unwrap().starts_with("tqp1."),
        "a legacy cursor is answered with a tqp1 cursor"
    );
    // Its bindings still hold: another vehicle's tqd1 cursor is refused.
    let foreign = legacy_drive_cursor(vehicle_b(), 0, i64::MAX, (3_000, 13));
    expect_error(
        call(&app, &with_query(&base, &format!("cursor={foreign}"))).await,
        StatusCode::BAD_REQUEST,
        "invalid_cursor",
    )
    .await;
    let other_window = legacy_drive_cursor(vehicle_a(), 1, i64::MAX, (3_000, 13));
    expect_error(
        call(&app, &with_query(&base, &format!("cursor={other_window}"))).await,
        StatusCode::BAD_REQUEST,
        "invalid_cursor",
    )
    .await;
    // The tag is still checked: another Hub's cursor key, and a flipped tag
    // digit, are refused even though every binding in the payload matches.
    let other_key = public_query_router(history.store.clone(), KEY_BYTE + 1);
    expect_invalid_cursor(
        &other_key,
        &with_query(&base, &format!("limit=2&cursor={legacy}")),
        "tqd1 signed by another Hub's key",
    )
    .await;
    let mut tampered = legacy.clone();
    let last = tampered.pop().expect("tag digit");
    tampered.push(if last == '0' { '1' } else { '0' });
    expect_invalid_cursor(
        &app,
        &with_query(&base, &format!("limit=2&cursor={tampered}")),
        "tqd1 with a flipped tag digit",
    )
    .await;
}

#[tokio::test]
async fn page_etag_is_strong_stable_and_conditional() {
    let history = History::new();
    let app = history.app();
    for route in page_routes(vehicle_a()) {
        let uri = with_query(&route.base, "limit=2");
        let first = call(&app, &uri).await;
        assert_eq!(first.status(), StatusCode::OK, "{}", route.label);
        assert_eq!(first.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(first.headers()[header::CONTENT_TYPE], "application/json");
        let etag = first.headers()[header::ETAG].to_str().unwrap().to_owned();
        let body = body_bytes(first).await;
        assert!(
            etag.starts_with('"') && etag.ends_with('"') && !etag.starts_with("W/"),
            "{} ETag is strong and quoted",
            route.label
        );
        assert_eq!(
            etag,
            format!("\"{}\"", hex::encode(Sha256::digest(&body))),
            "{} ETag is the SHA-256 of the body",
            route.label
        );
        // Stable across requests.
        let second = call(&app, &uri).await;
        assert_eq!(second.headers()[header::ETAG].to_str().unwrap(), etag);
        assert_eq!(body_bytes(second).await, body);
        // Conditional: a match is 304 with an empty body, the ETag and no-store.
        for condition in [etag.clone(), format!("\"x\", {etag}"), "*".to_owned()] {
            let conditional = call_with(&app, &uri, &[("if-none-match", condition.as_str())]).await;
            assert_eq!(
                conditional.status(),
                StatusCode::NOT_MODIFIED,
                "{}",
                route.label
            );
            assert_eq!(conditional.headers()[header::ETAG].to_str().unwrap(), etag);
            assert_eq!(conditional.headers()[header::CACHE_CONTROL], "no-store");
            assert!(body_bytes(conditional).await.is_empty());
        }
        // Another ETag is a full answer, and another page has another ETag.
        let mismatch = call_with(&app, &uri, &[("if-none-match", "\"other\"")]).await;
        assert_eq!(mismatch.status(), StatusCode::OK);
        let next_page = {
            let cursor = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["next_cursor"]
                .as_str()
                .unwrap()
                .to_owned();
            call(
                &app,
                &with_query(&route.base, &format!("limit=2&cursor={cursor}")),
            )
            .await
        };
        assert_ne!(next_page.headers()[header::ETAG].to_str().unwrap(), etag);
    }
}

// ------------------------------------------------------------- byte budget

#[tokio::test]
async fn drive_and_charge_pages_stop_before_one_mebibyte_with_a_cursor() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    seed_active_vehicle_identity(&store, Uuid::from_u128(0xd1), vehicle_a());
    mark_vehicle_published(&store, vehicle_a());
    // Four rows of about 300 KiB: three fit under 1 MiB, the fourth does not.
    for id in 1..=4_i64 {
        let mut drive = public_drive_fixture(id, id * 1_000);
        drive.start_address = Some("a".repeat(300 * 1024));
        seed_public_drive(&store, vehicle_a(), &drive);
        let mut charge = charge_fixture(id, id * 1_000, Some(id * 1_000 + 10));
        charge.address = Some("c".repeat(300 * 1024));
        insert_charge(&store, vehicle_a(), &charge);
    }
    let app = public_query_router(store, KEY_BYTE);
    for base in [
        format!("/v1/vehicles/{}/drives", vehicle_a()),
        format!("/v1/vehicles/{}/charges", vehicle_a()),
    ] {
        let response = call(&app, &with_query(&base, "limit=500")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_bytes(response).await;
        assert!(
            body.len() <= MAX_PUBLIC_PAGE_BYTES,
            "{base}: {} bytes",
            body.len()
        );
        assert!(
            body.len() > MAX_PUBLIC_PAGE_BYTES / 2,
            "{base} is a large page"
        );
        let first: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            ids_of(&first),
            vec![4, 3, 2],
            "{base} stops before the fourth"
        );
        let cursor = first["next_cursor"]
            .as_str()
            .expect("cursor after the last item");
        // The cursor continues exactly after the last included item.
        let rest = call(
            &app,
            &with_query(&base, &format!("limit=500&cursor={cursor}")),
        )
        .await;
        assert_eq!(ids_of(&response_json(rest).await), vec![1]);
    }
}

#[tokio::test]
async fn position_pages_stop_before_one_mebibyte_and_traverse_once() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    seed_active_vehicle_identity(&store, Uuid::from_u128(0xd2), vehicle_a());
    mark_vehicle_published(&store, vehicle_a());
    seed_public_drive(&store, vehicle_a(), &public_drive_fixture(7, 1_000));
    {
        let mut connection = store.open().expect("open store");
        let transaction = connection.transaction().expect("transaction");
        for id in 1..=2_100_i64 {
            transaction
                .execute(
                    "INSERT INTO materialised_positions(
                         vehicle_id, position_id, drive_id, car_id, position_json
                     ) VALUES (?1, ?2, 7, 1, ?3)",
                    rusqlite::params![
                        vehicle_a().to_string(),
                        id,
                        serde_json::to_string(&position_fixture(id, 7, 10_000 + id))
                            .expect("position JSON")
                    ],
                )
                .expect("seed position");
        }
        transaction.commit().expect("commit positions");
    }
    let app = public_query_router(store, KEY_BYTE);
    let base = format!("/v1/vehicles/{}/drives/7/positions", vehicle_a());
    let first = call(&app, &with_query(&base, "limit=2048")).await;
    assert_eq!(first.status(), StatusCode::OK);
    let body = body_bytes(first).await;
    assert!(body.len() <= MAX_PUBLIC_PAGE_BYTES);
    let first: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let first_ids = ids_of(&first);
    assert!(
        first_ids.len() < 2048,
        "{} positions fit under the cap",
        first_ids.len()
    );
    assert!(first["next_cursor"].is_string());
    let mut all = first_ids;
    let mut cursor = first["next_cursor"].as_str().map(str::to_owned);
    let mut pages = 1;
    while let Some(current) = cursor {
        let response = call(
            &app,
            &with_query(&base, &format!("limit=2048&cursor={current}")),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_bytes(response).await;
        assert!(body.len() <= MAX_PUBLIC_PAGE_BYTES);
        let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
        all.extend(ids_of(&page));
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        pages += 1;
        assert!(pages <= 4, "bounded traversal");
    }
    assert_eq!(
        all,
        (1..=2_100).collect::<Vec<_>>(),
        "every position exactly once"
    );
}

#[tokio::test]
async fn charge_sample_pages_stop_before_one_mebibyte_with_a_cursor() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    seed_active_vehicle_identity(&store, Uuid::from_u128(0xd3), vehicle_a());
    mark_vehicle_published(&store, vehicle_a());
    insert_charge(&store, vehicle_a(), &charge_fixture(3, 1_000, Some(2_000)));
    for id in 1..=60_i64 {
        let mut sample = sample_fixture(id, 3, 1_000 + id);
        sample.charge_cable = Some("k".repeat(20_000));
        insert_sample(&store, vehicle_a(), &sample);
    }
    let app = public_query_router(store, KEY_BYTE);
    let base = format!("/v1/vehicles/{}/charges/3/samples", vehicle_a());
    let pages = walk(&app, &base, "limit=2048").await;
    assert!(pages.len() >= 2, "a 1.2 MB list needs more than one page");
    let walked: Vec<i64> = pages.iter().flat_map(ids_of).collect();
    assert_eq!(walked, (1..=60).collect::<Vec<_>>());
    for page in &pages {
        assert!(serde_json::to_vec(page).unwrap().len() <= MAX_PUBLIC_PAGE_BYTES);
    }
}

#[tokio::test]
async fn a_single_item_over_the_byte_cap_is_a_503_not_a_split_page() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    seed_active_vehicle_identity(&store, Uuid::from_u128(0xd4), vehicle_a());
    mark_vehicle_published(&store, vehicle_a());
    let mut drive = public_drive_fixture(1, 1_000);
    drive.start_address = Some("a".repeat(MAX_PUBLIC_PAGE_BYTES));
    seed_public_drive(&store, vehicle_a(), &drive);
    // Charge 1 (newest, listed first) is over the cap on its own; charge 2
    // is small and owns the oversize sample.
    let mut charge = charge_fixture(1, 2_000, Some(2_500));
    charge.address = Some("c".repeat(MAX_PUBLIC_PAGE_BYTES));
    insert_charge(&store, vehicle_a(), &charge);
    insert_charge(&store, vehicle_a(), &charge_fixture(2, 1_000, Some(1_500)));
    let mut sample = sample_fixture(1, 2, 1_000);
    sample.charge_cable = Some("k".repeat(MAX_PUBLIC_PAGE_BYTES));
    insert_sample(&store, vehicle_a(), &sample);
    let app = public_query_router(store, KEY_BYTE);
    for (uri, message) in [
        (
            format!("/v1/vehicles/{}/drives", vehicle_a()),
            "a drive exceeds the response byte limit",
        ),
        (
            format!("/v1/vehicles/{}/charges", vehicle_a()),
            "a charge exceeds the response byte limit",
        ),
        (
            format!("/v1/vehicles/{}/charges/2/samples", vehicle_a()),
            "a charge sample exceeds the response byte limit",
        ),
    ] {
        let response = call(&app, &uri).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(response.headers().get(header::ETAG).is_none());
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "service_unavailable");
        assert_eq!(body["error"]["message"], message);
    }
    // The 503 is the oversize charge alone: a window without it is a page.
    let page = call(
        &app,
        &format!("/v1/vehicles/{}/charges?from_ms=0&to_ms=1500", vehicle_a()),
    )
    .await;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(ids_of(&response_json(page).await), vec![2]);
}

#[tokio::test]
async fn item_routes_obey_the_response_byte_cap_like_the_pages() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    seed_active_vehicle_identity(&store, Uuid::from_u128(0xd5), vehicle_a());
    mark_vehicle_published(&store, vehicle_a());
    let seed_drive = |id: i64, address: usize| {
        let mut drive = public_drive_fixture(id, 1_000 * id);
        drive.start_address = Some("a".repeat(address));
        seed_public_drive(&store, vehicle_a(), &drive);
    };
    seed_drive(1, 1);
    let app = public_query_router(store.clone(), KEY_BYTE);
    let one = body_bytes(call(&app, &format!("/v1/vehicles/{}/drives/1", vehicle_a())).await).await;
    // The body is `one.len()` bytes with a one byte address: pad it to the cap
    // exactly, then one byte past it.
    let exact = MAX_PUBLIC_PAGE_BYTES - (one.len() - 1);
    seed_drive(2, exact);
    seed_drive(3, exact + 1);
    let uri = |id: i64| format!("/v1/vehicles/{}/drives/{id}", vehicle_a());
    let at_the_cap = call(&app, &uri(2)).await;
    assert_eq!(at_the_cap.status(), StatusCode::OK);
    assert_eq!(body_bytes(at_the_cap).await.len(), MAX_PUBLIC_PAGE_BYTES);
    let over = call(&app, &uri(3)).await;
    assert_eq!(over.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(over.headers()[header::CACHE_CONTROL], "no-store");
    assert!(over.headers().get(header::ETAG).is_none());
    let body = response_json(over).await;
    assert_eq!(body["error"]["code"], "service_unavailable");
    assert_eq!(
        body["error"]["message"],
        "a drive exceeds the response byte limit"
    );
    // An oversize charge, likewise; its small neighbour is still served.
    let mut charge = charge_fixture(1, 2_000, Some(2_500));
    charge.address = Some("c".repeat(MAX_PUBLIC_PAGE_BYTES));
    insert_charge(&store, vehicle_a(), &charge);
    insert_charge(&store, vehicle_a(), &charge_fixture(2, 1_000, Some(1_500)));
    let response = call(&app, &format!("/v1/vehicles/{}/charges/1", vehicle_a())).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = response_json(response).await;
    assert_eq!(
        body["error"]["message"],
        "a charge exceeds the response byte limit"
    );
    let response = call(&app, &format!("/v1/vehicles/{}/charges/2", vehicle_a())).await;
    assert_eq!(response.status(), StatusCode::OK);
}

// --------------------------------------------------------------- item routes

#[tokio::test]
async fn drive_and_charge_items_have_exactly_the_contract_fields() {
    let history = History::new();
    let app = history.app();
    let vehicle = vehicle_a();

    let response = call(&app, &format!("/v1/vehicles/{vehicle}/drives/13")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let drive = response_json(response).await;
    assert_item_matches_contract(
        &drive,
        "drive",
        &["start_address_provenance", "end_address_provenance"],
    );
    assert_eq!(drive["id"], 13);
    assert_eq!(drive["vehicle_id"], vehicle.to_string());
    assert_eq!(drive["start_date_ms"], 3_000);
    assert_eq!(drive["end_date_ms"], 3_500);
    assert!(!drive.as_object().unwrap().contains_key("car_id"));
    // The item is the same record the page lists.
    let page = response_json(call(&app, &format!("/v1/vehicles/{vehicle}/drives")).await).await;
    let listed = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == 13)
        .expect("drive 13 is listed");
    assert_eq!(listed, &drive);

    let response = call(&app, &format!("/v1/vehicles/{vehicle}/charges/23")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let charge = response_json(response).await;
    assert_item_matches_contract(&charge, "charge", &["address_provenance"]);
    assert_eq!(charge["id"], 23);
    assert_eq!(charge["vehicle_id"], vehicle.to_string());
    assert_eq!(charge["start_date_ms"], 3_000);
    assert_eq!(charge["end_date_ms"], 3_500);
    assert_eq!(charge["billing_type"], "per_kwh");
    assert_eq!(charge["cost"], 4.5);
    assert_eq!(charge["cost_per_unit"], 0.21);
    assert_eq!(charge["session_fee"], 0.0);
    assert_eq!(charge["charge_rate_km_per_hour"], 31.0);
    assert!(!charge.as_object().unwrap().contains_key("car_id"));
    let page = response_json(call(&app, &format!("/v1/vehicles/{vehicle}/charges")).await).await;
    let listed = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == 23)
        .expect("charge 23 is listed");
    assert_eq!(listed, &charge);
}

#[tokio::test]
async fn position_and_sample_items_have_exactly_the_contract_fields() {
    let history = History::new();
    let app = history.app();
    let vehicle = vehicle_a();

    let positions =
        response_json(call(&app, &format!("/v1/vehicles/{vehicle}/drives/10/positions")).await)
            .await;
    let position = &positions["items"][1];
    assert_item_matches_contract(position, "position", &[]);
    assert_eq!(position["id"], 102);
    assert_eq!(position["drive_id"], 10);
    assert_eq!(position["date_ms"], 1_100);
    // power is a number (an f64 projection), speed an integer.
    assert!(position["power"].is_f64() && position["power"] == 12.0);
    assert!(position["speed"].is_i64());
    assert!(position["elevation_provenance"].is_null());
    for key in [
        "car_id",
        "is_rear_defroster_on",
        "is_front_defroster_on",
        "battery_heater_no_power",
    ] {
        assert!(!position.as_object().unwrap().contains_key(key), "{key}");
    }

    let samples =
        response_json(call(&app, &format!("/v1/vehicles/{vehicle}/charges/20/samples")).await)
            .await;
    let sample = &samples["items"][1];
    assert_item_matches_contract(sample, "charge_sample", &[]);
    assert_eq!(sample["id"], 202);
    assert_eq!(sample["charge_id"], 20);
    assert_eq!(sample["timestamp_ms"], 1_100);
    // The four charger measurements are numbers, the phase count an integer.
    for key in [
        "charger_power_kw",
        "charger_voltage",
        "charger_actual_current",
        "charger_pilot_current",
    ] {
        assert!(sample[key].is_f64(), "{key} is a JSON number");
    }
    assert!(sample["charger_phases"].is_i64());
    for key in [
        "charge_process_id",
        "battery_heater",
        "battery_heater_no_power",
        "not_enough_power_to_heat",
    ] {
        assert!(!sample.as_object().unwrap().contains_key(key), "{key}");
    }
}

#[tokio::test]
async fn items_with_every_optional_field_unknown_still_carry_every_member_as_null() {
    let history = History::new();
    // Rewrite the closed drive 16, charge 26 and one position and sample so
    // every optional field is absent from the stored JSON (older rows).
    let vehicle = vehicle_a();
    let connection = history.store.open().expect("open store");
    connection
        .execute(
            "UPDATE materialised_drives SET drive_json = ?1
             WHERE vehicle_id = ?2 AND drive_id = 16",
            rusqlite::params![
                r#"{"id":16,"car_id":1,"start_date_ms":100,"end_date_ms":200,
                    "distance_km":null,"duration_min":null,"efficiency":null,
                    "outside_temp_avg":null,"speed_max":null,"start_address":null,
                    "end_address":null,"start_geofence":null,"end_geofence":null,
                    "start_latitude":null,"start_longitude":null,"end_latitude":null,
                    "end_longitude":null,"start_soc":null,"end_soc":null,
                    "start_rated_range_km":null,"end_rated_range_km":null}"#,
                vehicle.to_string()
            ],
        )
        .expect("sparse drive");
    connection
        .execute(
            "UPDATE materialised_charges SET charge_json = ?1
             WHERE vehicle_id = ?2 AND charge_id = 26",
            rusqlite::params![
                r#"{"id":26,"car_id":1,"start_date_ms":100,"end_date_ms":200,
                    "charge_energy_added":null,"start_battery_level":null,
                    "end_battery_level":null,"duration_min":null,"address":null,
                    "location_name":null,"geofence":null,"is_dc":null,
                    "charge_rate_km_per_hour":null,"max_charger_power_kw":null,
                    "outside_temp_avg":null,"start_rated_range_km":null,
                    "end_rated_range_km":null}"#,
                vehicle.to_string()
            ],
        )
        .expect("sparse charge");
    connection
        .execute(
            "INSERT INTO materialised_positions(
                 vehicle_id, position_id, drive_id, car_id, position_json
             ) VALUES (?1, 900, 16, 1, ?2)",
            rusqlite::params![
                vehicle.to_string(),
                r#"{"id":900,"drive_id":16,"car_id":1,"date_ms":150,
                    "latitude":0.0,"longitude":0.0,"speed":null,"power":null,
                    "battery_level":null,"usable_battery_level":null,
                    "elevation":null,"odometer":null,"ideal_battery_range_km":null}"#
            ],
        )
        .expect("sparse position");
    connection
        .execute(
            "INSERT INTO materialised_charge_samples(
                 vehicle_id, sample_id, charge_id, sample_json
             ) VALUES (?1, 900, 26, ?2)",
            rusqlite::params![
                vehicle.to_string(),
                r#"{"id":900,"charge_process_id":26,"timestamp_ms":150,
                    "battery_level":null,"usable_battery_level":null,
                    "charge_energy_added_kwh":null,"charger_power_kw":null,
                    "charger_voltage":null,"charger_actual_current":null,
                    "charger_pilot_current":null,"charger_phases":null,
                    "ideal_range_km":null,"rated_range_km":null,"outside_temp_c":null,
                    "battery_heater_on":null,"battery_heater":null,
                    "battery_heater_no_power":null,"not_enough_power_to_heat":null,
                    "fast_charger_present":null,"fast_charger_brand":null,
                    "fast_charger_type":null,"charge_cable":null}"#
            ],
        )
        .expect("sparse sample");
    drop(connection);
    let app = history.app();
    let drive = response_json(call(&app, &format!("/v1/vehicles/{vehicle}/drives/16")).await).await;
    assert_item_matches_contract(
        &drive,
        "drive",
        &["start_address_provenance", "end_address_provenance"],
    );
    assert!(drive["distance_km"].is_null() && drive["start_address"].is_null());
    let charge =
        response_json(call(&app, &format!("/v1/vehicles/{vehicle}/charges/26")).await).await;
    assert_item_matches_contract(&charge, "charge", &["address_provenance"]);
    assert!(charge["billing_type"].is_null() && charge["cost"].is_null());
    let positions =
        response_json(call(&app, &format!("/v1/vehicles/{vehicle}/drives/16/positions")).await)
            .await;
    assert_item_matches_contract(&positions["items"][0], "position", &[]);
    assert!(positions["items"][0]["tpms_pressure_fl"].is_null());
    let samples =
        response_json(call(&app, &format!("/v1/vehicles/{vehicle}/charges/26/samples")).await)
            .await;
    assert_item_matches_contract(&samples["items"][0], "charge_sample", &[]);
}

#[tokio::test]
async fn item_routes_ignore_the_query_string() {
    let history = History::new();
    let app = history.app();
    let vehicle = vehicle_a();
    for uri in [
        format!("/v1/vehicles/{vehicle}/drives/10"),
        format!("/v1/vehicles/{vehicle}/charges/20"),
    ] {
        let plain = body_bytes(call(&app, &uri).await).await;
        for query in [
            "limit=0",
            "from_ms=abc&to_ms=-1",
            "unknown=1",
            "cursor=garbage",
            "limit=1&limit=2",
        ] {
            let response = call(&app, &with_query(&uri, query)).await;
            assert_eq!(response.status(), StatusCode::OK, "{uri}?{query}");
            assert_eq!(body_bytes(response).await, plain);
        }
    }
}

#[tokio::test]
async fn unknown_and_unfinished_sessions_are_not_found() {
    let history = History::new();
    let app = history.app();
    let vehicle = vehicle_a();
    // 15 is an unfinished drive (its position row exists but is not served),
    // 25 an unfinished charge (likewise its sample).
    for template in [
        format!("/v1/vehicles/{vehicle}/drives/ID"),
        format!("/v1/vehicles/{vehicle}/drives/ID/positions"),
    ] {
        for id in ["999", "15"] {
            expect_error(
                call(&app, &template.replace("ID", id)).await,
                StatusCode::NOT_FOUND,
                "drive_not_found",
            )
            .await;
        }
    }
    for template in [
        format!("/v1/vehicles/{vehicle}/charges/ID"),
        format!("/v1/vehicles/{vehicle}/charges/ID/samples"),
    ] {
        for id in ["999", "25"] {
            expect_error(
                call(&app, &template.replace("ID", id)).await,
                StatusCode::NOT_FOUND,
                "charge_not_found",
            )
            .await;
        }
    }
    // A drive that exists only for another vehicle is unknown here.
    seed_public_drive(
        &history.store,
        vehicle_b(),
        &public_drive_fixture(77, 9_000),
    );
    assert_eq!(
        call(&app, &format!("/v1/vehicles/{}/drives/77", vehicle_b()))
            .await
            .status(),
        StatusCode::OK
    );
    expect_error(
        call(&app, &format!("/v1/vehicles/{vehicle}/drives/77")).await,
        StatusCode::NOT_FOUND,
        "drive_not_found",
    )
    .await;
    // A drive id on the charge route (and the reverse) is another thing.
    expect_error(
        call(&app, &format!("/v1/vehicles/{vehicle}/charges/10")).await,
        StatusCode::NOT_FOUND,
        "charge_not_found",
    )
    .await;
    expect_error(
        call(&app, &format!("/v1/vehicles/{vehicle}/drives/20")).await,
        StatusCode::NOT_FOUND,
        "drive_not_found",
    )
    .await;
}

#[tokio::test]
async fn the_parent_is_looked_up_before_the_page_even_with_a_cursor() {
    let history = History::new();
    let app = history.app();
    let cursor = craft_cursor(
        PageKind::Positions,
        vehicle_a(),
        15,
        0,
        i64::MAX,
        (6_000, 151),
    );
    expect_error(
        call(
            &app,
            &format!(
                "/v1/vehicles/{}/drives/15/positions?cursor={cursor}",
                vehicle_a()
            ),
        )
        .await,
        StatusCode::NOT_FOUND,
        "drive_not_found",
    )
    .await;
    // The unfinished drive's own positions are not an empty 200 either.
    expect_error(
        call(
            &app,
            &format!("/v1/vehicles/{}/drives/15/positions", vehicle_a()),
        )
        .await,
        StatusCode::NOT_FOUND,
        "drive_not_found",
    )
    .await;
}

// ------------------------------------------------------------- access order

fn all_routes(vehicle: &str, drive: &str, charge: &str) -> Vec<String> {
    vec![
        format!("/v1/vehicles/{vehicle}/drives"),
        format!("/v1/vehicles/{vehicle}/drives/{drive}"),
        format!("/v1/vehicles/{vehicle}/drives/{drive}/positions"),
        format!("/v1/vehicles/{vehicle}/charges"),
        format!("/v1/vehicles/{vehicle}/charges/{charge}"),
        format!("/v1/vehicles/{vehicle}/charges/{charge}/samples"),
    ]
}

#[tokio::test]
async fn the_vehicle_is_checked_before_the_query_is_read() {
    let history = History::new();
    let app = history.app();
    // A retired vehicle (still in the catalogue, still published) is invisible.
    let retired = Uuid::from_u128(0xe1e1);
    seed_active_vehicle_identity(&history.store, Uuid::from_u128(0xe2), retired);
    seed_public_drive(&history.store, retired, &public_drive_fixture(10, 1_000));
    mark_vehicle_published(&history.store, retired);
    assert!(
        history
            .store
            .retire_vehicle(retired, 2_000)
            .expect("retire")
    );
    // An active vehicle that was never published is invisible too.
    let unpublished = Uuid::from_u128(0xe3e3);
    seed_active_vehicle_identity(&history.store, Uuid::from_u128(0xe4), unpublished);
    seed_public_drive(
        &history.store,
        unpublished,
        &public_drive_fixture(10, 1_000),
    );

    let bad_queries = [
        "",
        "limit=0",
        "limit=abc",
        "limit=99999",
        "from_ms=5&to_ms=5",
        "from_ms=abc",
        "cursor=garbage",
        "unknown=1",
    ];
    for vehicle in [
        Uuid::new_v4().to_string(),
        retired.to_string(),
        unpublished.to_string(),
    ] {
        for base in all_routes(&vehicle, "10", "20") {
            for query in bad_queries {
                expect_error(
                    call(&app, &with_query(&base, query)).await,
                    StatusCode::NOT_FOUND,
                    "vehicle_not_found",
                )
                .await;
            }
        }
    }
    // The published and active vehicle reads the query as usual.
    expect_error(
        call(
            &app,
            &format!("/v1/vehicles/{}/drives?limit=0", vehicle_a()),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_limit",
    )
    .await;
    // A drive or charge id that is not valid is 404 before the query is read.
    for (base, code) in [
        (
            format!("/v1/vehicles/{}/drives/abc/positions", vehicle_a()),
            "drive_not_found",
        ),
        (
            format!("/v1/vehicles/{}/charges/0/samples", vehicle_a()),
            "charge_not_found",
        ),
    ] {
        expect_error(
            call(&app, &with_query(&base, "limit=0&from_ms=abc")).await,
            StatusCode::NOT_FOUND,
            code,
        )
        .await;
    }
    // A syntactically valid but unknown or unfinished parent wins over the
    // query (carry-over 12): the parent is looked up before the query is read,
    // so a bad query cannot reveal which drives or charges exist.
    for (base, code) in [
        (
            format!("/v1/vehicles/{}/drives/999/positions", vehicle_a()),
            "drive_not_found",
        ),
        (
            // Drive 15 is unfinished.
            format!("/v1/vehicles/{}/drives/15/positions", vehicle_a()),
            "drive_not_found",
        ),
        (
            format!("/v1/vehicles/{}/charges/999/samples", vehicle_a()),
            "charge_not_found",
        ),
        (
            // Charge 25 is unfinished.
            format!("/v1/vehicles/{}/charges/25/samples", vehicle_a()),
            "charge_not_found",
        ),
    ] {
        expect_error(
            call(&app, &with_query(&base, "limit=0&from_ms=abc")).await,
            StatusCode::NOT_FOUND,
            code,
        )
        .await;
    }
    // An existing parent reads the query as usual.
    for base in [
        format!("/v1/vehicles/{}/drives/16/positions", vehicle_a()),
        format!("/v1/vehicles/{}/charges/26/samples", vehicle_a()),
    ] {
        expect_error(
            call(&app, &with_query(&base, "limit=0")).await,
            StatusCode::BAD_REQUEST,
            "invalid_limit",
        )
        .await;
    }
}

#[tokio::test]
async fn the_published_predicate_is_part_of_the_shared_vehicle_check() {
    let history = History::new();
    let app = history.app();
    let unpublished = Uuid::from_u128(0xf1f1);
    seed_active_vehicle_identity(&history.store, Uuid::from_u128(0xf2), unpublished);
    seed_public_drive(
        &history.store,
        unpublished,
        &public_drive_fixture(10, 1_000),
    );
    insert_position(&history.store, unpublished, &position_fixture(1, 10, 1_000));
    insert_charge(
        &history.store,
        unpublished,
        &charge_fixture(20, 1_000, Some(2_000)),
    );
    insert_sample(&history.store, unpublished, &sample_fixture(1, 20, 1_000));
    for uri in all_routes(&unpublished.to_string(), "10", "20") {
        expect_error(
            call(&app, &uri).await,
            StatusCode::NOT_FOUND,
            "vehicle_not_found",
        )
        .await;
    }
    // Publishing it makes every route serve its rows.
    mark_vehicle_published(&history.store, unpublished);
    for uri in all_routes(&unpublished.to_string(), "10", "20") {
        assert_eq!(call(&app, &uri).await.status(), StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
async fn every_route_needs_the_cursor_key_before_anything_else() {
    let history = History::new();
    // The loopback router built without a cursor key.
    let unsigned = router(history.store.clone());
    // The message follows the route family: the drive routes keep the text the
    // drives route always had, the charge routes say the same about charges.
    let messages = [
        "drive history cursor integrity is unavailable",
        "drive history cursor integrity is unavailable",
        "drive history cursor integrity is unavailable",
        "charge history cursor integrity is unavailable",
        "charge history cursor integrity is unavailable",
        "charge history cursor integrity is unavailable",
    ];
    for vehicle in [vehicle_a().to_string(), "not-a-uuid".to_owned()] {
        let routes = all_routes(&vehicle, "10", "20");
        assert_eq!(routes.len(), messages.len());
        for (uri, message) in routes.iter().zip(messages) {
            let response = call(&unsigned, uri).await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let body = response_json(response).await;
            assert_eq!(body["error"]["code"], "service_unavailable", "{uri}");
            assert_eq!(body["error"]["message"], message, "{uri}");
        }
    }
}

#[tokio::test]
async fn every_route_checks_the_bearer_before_the_cursor_key() {
    let history = History::new();
    // Device authentication on, no cursor key: without a bearer the answer is
    // the 401 challenge on every history route, never the cursor-key 503.
    let unsigned = router_with_access(history.store.clone(), false, true, false, None, None, None);
    let vehicle = vehicle_a().to_string();
    for uri in all_routes(&vehicle, "10", "20") {
        for headers in [vec![], vec![("authorization", "Bearer not-a-valid-token")]] {
            let response = call_with(&unsigned, &uri, &headers).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{uri} {headers:?}"
            );
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
            assert!(body_bytes(response).await.is_empty(), "{uri}");
        }
    }
    // With a valid bearer the cursor-key 503 is reached.
    let bearer = format!("Bearer {}", bearer_for(&history.store));
    for uri in all_routes(&vehicle, "10", "20") {
        let response = call_with(&unsigned, &uri, &[("authorization", bearer.as_str())]).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
    }
}

#[tokio::test]
async fn malformed_and_non_utf8_path_segments_are_answered_after_authentication() {
    let history = History::new();
    let key = cursor_key();
    let app = paired_router(history.store.clone(), &key);
    let bearer = bearer_for(&history.store);
    let auth = format!("Bearer {bearer}");
    let vehicle = vehicle_a().to_string();

    // Without a bearer every path, however malformed, is 401 with a challenge.
    let mut paths = all_routes("%FF", "%FF", "%FF");
    paths.extend(all_routes("not-a-uuid", "abc", "abc"));
    paths.extend(all_routes(&vehicle, "%FF", "%FF"));
    paths.extend(all_routes(&vehicle, "10", "20"));
    paths.extend(all_routes(&Uuid::nil().to_string(), "10", "20"));
    paths.push("/v1/vehicles/%FF/current".to_owned());
    paths.push(format!("/v1/vehicles/{vehicle}/current"));
    for path in &paths {
        for headers in [
            vec![],
            vec![("authorization", "Bearer not-a-valid-token")],
            vec![("authorization", "Basic abc")],
        ] {
            let response = call_with(&app, path, &headers).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{path} {headers:?}"
            );
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
            assert!(body_bytes(response).await.is_empty(), "{path}");
        }
    }

    // With a bearer: the route's not-found form, never axum's 400 text/plain.
    for base in all_routes("%FF", "10", "20") {
        expect_error(
            call_with(&app, &base, &[("authorization", auth.as_str())]).await,
            StatusCode::NOT_FOUND,
            "vehicle_not_found",
        )
        .await;
    }
    for base in all_routes("not-a-uuid", "10", "20") {
        expect_error(
            call_with(&app, &base, &[("authorization", auth.as_str())]).await,
            StatusCode::NOT_FOUND,
            "vehicle_not_found",
        )
        .await;
    }
    for bad in [
        "%FF",
        "abc",
        "0",
        "-1",
        "+",
        "99999999999999999999",
        "%C3%28",
        "1%FF",
        "",
    ] {
        let drive_item = format!("/v1/vehicles/{vehicle}/drives/{bad}");
        let positions = format!("/v1/vehicles/{vehicle}/drives/{bad}/positions");
        let charge_item = format!("/v1/vehicles/{vehicle}/charges/{bad}");
        let samples = format!("/v1/vehicles/{vehicle}/charges/{bad}/samples");
        for (uri, code) in [
            (drive_item, "drive_not_found"),
            (positions, "drive_not_found"),
            (charge_item, "charge_not_found"),
            (samples, "charge_not_found"),
        ] {
            let response = call_with(&app, &uri, &[("authorization", auth.as_str())]).await;
            if bad.is_empty() {
                // An empty segment does not match a route at all.
                assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
                continue;
            }
            expect_error(response, StatusCode::NOT_FOUND, code).await;
        }
    }
    // The one bad segment must not hide the vehicle check: an unknown vehicle
    // with a bad drive id is vehicle_not_found.
    expect_error(
        call_with(
            &app,
            &format!("/v1/vehicles/{}/drives/%FF/positions", Uuid::new_v4()),
            &[("authorization", auth.as_str())],
        )
        .await,
        StatusCode::NOT_FOUND,
        "vehicle_not_found",
    )
    .await;
    // /current: an empty 404 for a malformed or non-UTF-8 vehicle id.
    for bad in ["%FF", "not-a-uuid", "%FF%FF"] {
        let response = call_with(
            &app,
            &format!("/v1/vehicles/{bad}/current"),
            &[("authorization", auth.as_str())],
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{bad}");
        assert!(body_bytes(response).await.is_empty());
    }
    // A well-formed request still works with the bearer.
    let ok = call_with(
        &app,
        &format!("/v1/vehicles/{vehicle}/drives?limit=1"),
        &[("authorization", auth.as_str())],
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
}

/// Every byte of `text` written as `%XX`, in the given hexadecimal case.
fn percent_encode_all(text: &str, upper: bool) -> String {
    text.bytes()
        .map(|byte| {
            if upper {
                format!("%{byte:02X}")
            } else {
                format!("%{byte:02x}")
            }
        })
        .collect()
}

#[tokio::test]
async fn percent_encoded_ids_are_decoded_like_a_path_extractor() {
    let history = History::new();
    let app = history.app();
    let vehicle = vehicle_a().to_string();
    // Every spelling decodes to the same segment and so to the same response,
    // as Path<String> did before the raw per-segment parsing replaced it.
    for upper in [true, false] {
        let encoded_vehicle = percent_encode_all(&vehicle, upper);
        let encoded_drive = percent_encode_all("10", upper);
        let encoded_charge = percent_encode_all("20", upper);
        // A mixed spelling: only the dashes and the last digit are encoded.
        let mixed_drive = format!("1{}", percent_encode_all("0", upper));
        let mixed_charge = format!("2{}", percent_encode_all("0", upper));
        let mixed_vehicle = vehicle.replace('-', &percent_encode_all("-", upper));
        for (plain, encoded) in [
            (
                format!("/v1/vehicles/{vehicle}/current"),
                format!("/v1/vehicles/{encoded_vehicle}/current"),
            ),
            (
                format!("/v1/vehicles/{vehicle}/current"),
                format!("/v1/vehicles/{mixed_vehicle}/current"),
            ),
            (
                format!("/v1/vehicles/{vehicle}/drives?limit=2"),
                format!("/v1/vehicles/{encoded_vehicle}/drives?limit=2"),
            ),
            (
                format!("/v1/vehicles/{vehicle}/drives/10"),
                format!("/v1/vehicles/{mixed_vehicle}/drives/{mixed_drive}"),
            ),
            (
                format!("/v1/vehicles/{vehicle}/drives/10"),
                format!("/v1/vehicles/{encoded_vehicle}/drives/{encoded_drive}"),
            ),
            (
                format!("/v1/vehicles/{vehicle}/drives/10/positions"),
                format!("/v1/vehicles/{mixed_vehicle}/drives/{mixed_drive}/positions"),
            ),
            (
                format!("/v1/vehicles/{vehicle}/charges?limit=2"),
                format!("/v1/vehicles/{encoded_vehicle}/charges?limit=2"),
            ),
            (
                format!("/v1/vehicles/{vehicle}/charges/20"),
                format!("/v1/vehicles/{mixed_vehicle}/charges/{mixed_charge}"),
            ),
            (
                format!("/v1/vehicles/{vehicle}/charges/20"),
                format!("/v1/vehicles/{encoded_vehicle}/charges/{encoded_charge}"),
            ),
            (
                format!("/v1/vehicles/{vehicle}/charges/20/samples"),
                format!("/v1/vehicles/{mixed_vehicle}/charges/{mixed_charge}/samples"),
            ),
        ] {
            let served = call(&app, &plain).await;
            assert_eq!(served.status(), StatusCode::OK, "{plain}");
            let served = body_bytes(served).await;
            assert!(!served.is_empty(), "{plain}");
            let decoded = call(&app, &encoded).await;
            assert_eq!(decoded.status(), StatusCode::OK, "{encoded}");
            assert_eq!(body_bytes(decoded).await, served, "{encoded}");
        }
    }
    // The decoded value is what is validated: a percent-encoded id that decodes
    // to something malformed is still the route's not-found, and a '%' that
    // does not start two hexadecimal digits stays literal (so it is malformed).
    for (suffix, code) in [
        ("drives/%31%30%2D", "drive_not_found"),
        ("drives/1%3", "drive_not_found"),
        ("drives/1%zz", "drive_not_found"),
        ("drives/%30", "drive_not_found"),
        ("charges/2%3", "charge_not_found"),
        ("charges/%2D1/samples", "charge_not_found"),
    ] {
        expect_error(
            call(&app, &format!("/v1/vehicles/{vehicle}/{suffix}")).await,
            StatusCode::NOT_FOUND,
            code,
        )
        .await;
    }
}

#[tokio::test]
async fn the_nil_vehicle_is_an_unknown_vehicle_never_a_503() {
    let history = History::new();
    let app = history.app();
    let nil = Uuid::nil().to_string();
    for uri in all_routes(&nil, "10", "20") {
        expect_error(
            call(&app, &uri).await,
            StatusCode::NOT_FOUND,
            "vehicle_not_found",
        )
        .await;
        expect_error(
            call(&app, &with_query(&uri, "limit=0")).await,
            StatusCode::NOT_FOUND,
            "vehicle_not_found",
        )
        .await;
    }
    let response = call(&app, &format!("/v1/vehicles/{nil}/current")).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(body_bytes(response).await.is_empty());
    // And through the paired router with a real bearer.
    let paired = paired_router(history.store.clone(), &cursor_key());
    let bearer = bearer_for(&history.store);
    let auth = format!("Bearer {bearer}");
    for uri in all_routes(&nil, "10", "20") {
        expect_error(
            call_with(&paired, &uri, &[("authorization", auth.as_str())]).await,
            StatusCode::NOT_FOUND,
            "vehicle_not_found",
        )
        .await;
    }
    let response = call_with(
        &paired,
        &format!("/v1/vehicles/{nil}/current"),
        &[("authorization", auth.as_str())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(body_bytes(response).await.is_empty());
}

#[tokio::test]
async fn malformed_numeric_query_values_are_invalid_query_and_well_formed_ones_have_their_own_codes()
 {
    let history = History::new();
    let app = history.app();
    for route in page_routes(vehicle_a()) {
        let maximum = if matches!(route.kind, PageKind::Drives | PageKind::Charges) {
            500
        } else {
            2048
        };
        for query in [
            "limit=abc",
            "limit=-1",
            "limit=",
            "limit=1.5",
            "limit=4294967296",
            "from_ms=1.5",
            "from_ms=abc",
            "from_ms=9223372036854775808",
            "to_ms=9223372036854775808",
            "to_ms=-9223372036854775809",
            "to_ms=",
            "unknown=1",
            "limit=1&limit=2",
            "from_ms=1&from_ms=2",
            "LIMIT=1",
        ] {
            expect_error(
                call(&app, &with_query(&route.base, query)).await,
                StatusCode::BAD_REQUEST,
                "invalid_query",
            )
            .await;
        }
        for (query, code) in [
            ("limit=0".to_owned(), "invalid_limit"),
            (format!("limit={}", maximum + 1), "invalid_limit"),
            ("limit=4294967295".to_owned(), "invalid_limit"),
            ("from_ms=-1".to_owned(), "invalid_time_range"),
            ("to_ms=-1".to_owned(), "invalid_time_range"),
            ("from_ms=5&to_ms=5".to_owned(), "invalid_time_range"),
            ("from_ms=6&to_ms=5".to_owned(), "invalid_time_range"),
            ("to_ms=0".to_owned(), "invalid_time_range"),
            // Both malformed and out of range: the malformed value wins.
            ("limit=abc&from_ms=-1".to_owned(), "invalid_query"),
        ] {
            expect_error(
                call(&app, &with_query(&route.base, &query)).await,
                StatusCode::BAD_REQUEST,
                code,
            )
            .await;
        }
        // The maximum itself and the signed 64-bit maximum are accepted.
        let accepted = call(
            &app,
            &with_query(
                &route.base,
                &format!("limit={maximum}&from_ms=0&to_ms=9223372036854775807"),
            ),
        )
        .await;
        assert_eq!(accepted.status(), StatusCode::OK, "{}", route.label);
    }
}

// -------------------------------------------------------------- provenance

#[tokio::test]
async fn elevation_provenance_is_claimed_only_for_a_successful_terrain_lookup_of_a_served_elevation()
 {
    let history = History::new();
    let vehicle = vehicle_a();
    // 101: success + elevation (claimed). 102: void. 103: failed. 104: success
    // but the served elevation is null. 105: no terrain row at all.
    insert_terrain_row(&history.store, vehicle, 101, 10, "success");
    insert_terrain_row(&history.store, vehicle, 102, 10, "void");
    insert_terrain_row(&history.store, vehicle, 103, 10, "failed");
    insert_terrain_row(&history.store, vehicle, 104, 10, "success");
    let mut without_elevation = position_fixture(104, 10, 1_300);
    without_elevation.elevation = None;
    history
        .store
        .open()
        .expect("open store")
        .execute(
            "UPDATE materialised_positions SET position_json = ?1
             WHERE vehicle_id = ?2 AND position_id = 104",
            rusqlite::params![
                serde_json::to_string(&without_elevation).unwrap(),
                vehicle.to_string()
            ],
        )
        .expect("clear elevation");
    let app = history.app();
    let page =
        response_json(call(&app, &format!("/v1/vehicles/{vehicle}/drives/10/positions")).await)
            .await;
    let items = page["items"].as_array().unwrap();
    assert_eq!(
        items[0]["elevation_provenance"],
        serde_json::json!({
            "source": "terrain",
            "dataset": "synthetic-terrain",
            "dataset_version": "1",
            "tile": "synthetic-tile"
        })
    );
    assert_item_matches_contract(&items[0], "position", &[]);
    assert_eq!(items[0]["elevation"], 100);
    for item in &items[1..] {
        assert!(
            item["elevation_provenance"].is_null(),
            "position {} claims no provenance",
            item["id"]
        );
        assert!(
            item.as_object()
                .unwrap()
                .contains_key("elevation_provenance")
        );
    }
    assert!(items[3]["elevation"].is_null());
    // The join must not multiply or drop rows.
    assert_eq!(ids_of(&page), vec![101, 102, 103, 104, 105]);
}

#[tokio::test]
async fn drives_and_charges_emit_no_address_provenance_members() {
    let history = History::new();
    let app = history.app();
    let vehicle = vehicle_a();
    for uri in [
        format!("/v1/vehicles/{vehicle}/drives"),
        format!("/v1/vehicles/{vehicle}/charges"),
    ] {
        let text = String::from_utf8(body_bytes(call(&app, &uri).await).await).unwrap();
        assert!(!text.contains("provenance"), "{uri}");
    }
}

// ------------------------------------------------------ catalogue integrity

#[tokio::test]
async fn rows_whose_json_identity_differs_from_the_catalogue_key_fail_closed() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    seed_active_vehicle_identity(&store, Uuid::from_u128(0xa7), vehicle_a());
    mark_vehicle_published(&store, vehicle_a());
    let vehicle = vehicle_a();
    // Parents: drives and charges 5 and 6 are sound.
    for id in [5, 6] {
        seed_public_drive(&store, vehicle, &public_drive_fixture(id, 1_000));
        insert_charge(&store, vehicle, &charge_fixture(id, 1_000, Some(2_000)));
    }
    let connection = store.open().expect("open store");
    // Drive row 1 and charge row 1 hold JSON of id 99.
    connection
        .execute(
            "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
             VALUES (?1, 1, 1, ?2)",
            rusqlite::params![
                vehicle.to_string(),
                json_text(&public_drive_fixture(99, 1_000))
            ],
        )
        .expect("divergent drive");
    connection
        .execute(
            "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
             VALUES (?1, 1, 1, ?2)",
            rusqlite::params![
                vehicle.to_string(),
                json_text(&charge_fixture(99, 1_000, Some(2_000)))
            ],
        )
        .expect("divergent charge");
    // Drive 5: a position row whose JSON id differs; drive 6: one whose JSON
    // drive differs from its row. Likewise samples of charges 5 and 6.
    for (position_id, drive, value) in [
        (1, 5, position_fixture(99, 5, 1_000)),
        (2, 6, position_fixture(2, 8, 1_000)),
    ] {
        connection
            .execute(
                "INSERT INTO materialised_positions(
                     vehicle_id, position_id, drive_id, car_id, position_json
                 ) VALUES (?1, ?2, ?3, 1, ?4)",
                rusqlite::params![vehicle.to_string(), position_id, drive, json_text(&value)],
            )
            .expect("divergent position");
    }
    for (sample_id, charge, value) in [
        (1, 5, sample_fixture(99, 5, 1_000)),
        (2, 6, sample_fixture(2, 8, 1_000)),
    ] {
        connection
            .execute(
                "INSERT INTO materialised_charge_samples(
                     vehicle_id, sample_id, charge_id, sample_json
                 ) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![vehicle.to_string(), sample_id, charge, json_text(&value)],
            )
            .expect("divergent sample");
    }
    drop(connection);
    let app = public_query_router(store, KEY_BYTE);
    for (uri, message) in [
        (
            format!("/v1/vehicles/{vehicle}/drives/1"),
            "drive history is temporarily unavailable",
        ),
        (
            format!("/v1/vehicles/{vehicle}/charges/1"),
            "charge history is temporarily unavailable",
        ),
        (
            format!("/v1/vehicles/{vehicle}/charges"),
            "charge history is temporarily unavailable",
        ),
        (
            format!("/v1/vehicles/{vehicle}/drives/5/positions"),
            "drive positions are temporarily unavailable",
        ),
        (
            format!("/v1/vehicles/{vehicle}/drives/6/positions"),
            "drive positions are temporarily unavailable",
        ),
        (
            format!("/v1/vehicles/{vehicle}/charges/5/samples"),
            "charge samples are temporarily unavailable",
        ),
        (
            format!("/v1/vehicles/{vehicle}/charges/6/samples"),
            "charge samples are temporarily unavailable",
        ),
    ] {
        let response = call(&app, &uri).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        let body = response_json(response).await;
        assert_eq!(body["error"]["code"], "service_unavailable", "{uri}");
        assert_eq!(body["error"]["message"], message, "{uri}");
    }
}

// ---------------------------------------------------- discovery invariants

#[tokio::test]
async fn home_assistant_discovery_invariants_hold_with_and_without_a_cursor_key() {
    let history = History::new();
    for app in [
        public_query_router(history.store.clone(), KEY_BYTE),
        router(history.store.clone()),
        paired_router(history.store.clone(), &cursor_key()),
    ] {
        let response = call(&app, "/.well-known/teslatlas-hub").await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = response_json(response).await;
        // Home Assistant hard-checks exactly these (current_hub_client.py).
        assert_eq!(payload["api_versions"], serde_json::json!(["1.0"]));
        assert_eq!(payload["protocol"], "teslatlas-sync");
        assert_eq!(payload["protocol_major"], 1);
        let capabilities = payload["capabilities"].as_array().expect("capabilities");
        for required in ["query.vehicles", "query.current"] {
            assert!(
                capabilities.iter().any(|token| token == required),
                "{required} is advertised"
            );
        }
    }
}
