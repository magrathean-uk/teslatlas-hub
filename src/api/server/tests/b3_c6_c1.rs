// SPDX-License-Identifier: AGPL-3.0-only

//! C6 Part C over HTTP (C6 ST-S33 and C6 ST-S48, C6 R50, C6 R58 (g)): the current
//! response, its header and the capabilities actually served; and full pages
//! of the four history routes at their largest limit, which now carry the
//! aliases. Synthetic data only.

use super::*;
use crate::current_state::build_current_vehicle_summary;

/// The default `transient_max_age_ms` and the evaluation time of the current
/// fixture (C6 section 8).
const GAP_MS: i64 = 600_000;
const T0: i64 = 1_760_000_000_000;

/// C6 ST-S1's `P0`: a parked response.
fn p0() -> serde_json::Value {
    serde_json::json!({
        "display_name": "Test Car",
        "vin": "5YJ3E1EA0KFTEST01",
        "drive_state": {
            "shift_state": null, "speed": null, "power": 0,
            "latitude": 10.1234567, "longitude": 20.7654321,
            "heading": 90, "elevation": 50.4
        },
        "charge_state": {
            "battery_level": 80, "usable_battery_level": 79,
            "battery_range": 200.0, "ideal_battery_range": 210.0, "est_battery_range": 150.5,
            "charge_limit_soc": 90, "charging_state": "Disconnected",
            "conn_charge_cable": "<invalid>", "charger_power": 0,
            "charge_energy_added": 12.346, "charge_port_door_open": false,
            "time_to_full_charge": 0.0
        },
        "climate_state": {"inside_temp": 21.25, "outside_temp": 10.04, "is_climate_on": false},
        "vehicle_state": {
            "odometer": 1000.0, "car_version": "2024.14.3 aa1bb2", "locked": true,
            "sentry_mode": false, "df": 0, "dr": 0, "pf": 0, "pr": 0, "ft": 0, "rt": 0,
            "fd_window": 0, "fp_window": 0, "rd_window": 0, "rp_window": 0,
            "service_mode": false, "tpms_pressure_fl": 2.875,
            "software_update": {
                "status": "", "version": "", "download_perc": 0, "install_perc": 1
            }
        },
        "vehicle_config": {
            "car_type": "model3", "trim_badging": "74d", "exterior_color": "MidnightSilver",
            "wheel_type": "Pinwheel18", "spoiler_type": "None", "sun_roof_installed": 0
        }
    })
}

/// The groups of `response` stamped with `time_ms`, as C6 section 8's `VD`.
fn stamped_vehicle_data(time_ms: i64) -> serde_json::Value {
    let mut response = p0();
    for group in [
        "drive_state",
        "charge_state",
        "climate_state",
        "vehicle_state",
        "vehicle_config",
    ] {
        response[group]["timestamp"] = serde_json::json!(time_ms);
    }
    serde_json::json!({
        "record_type": "owner_api_vehicle_data_v1",
        "source_vehicle_id": "1",
        "source_vehicle_state": "online",
        "provider_raw_json": {"response": response}
    })
}

async fn get_bytes(app: &Router, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("response body")
        .to_bytes()
        .to_vec();
    (status, headers, body)
}

/// The keys of a JSON object in the order the bytes carry them.
struct Keys(Vec<String>);

impl<'de> serde::Deserialize<'de> for Keys {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{IgnoredAny, MapAccess, Visitor};
        struct Collect;
        impl<'de> Visitor<'de> for Collect {
            type Value = Keys;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Keys, A::Error> {
                let mut keys = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    keys.push(key);
                }
                Ok(Keys(keys))
            }
        }
        deserializer.deserialize_map(Collect)
    }
}

fn key_order(bytes: &[u8]) -> Vec<String> {
    serde_json::from_slice::<Keys>(bytes)
        .expect("a JSON object")
        .0
}

/// The keys, in the order the bytes carry them, of the first object that
/// follows `"key":` inside `bytes`.
fn nested_key_order(bytes: &[u8], key: &str) -> Vec<String> {
    let needle = format!("\"{key}\":");
    let start = bytes
        .windows(needle.len())
        .position(|window| window == needle.as_bytes())
        .expect("the nested key is written")
        + needle.len();
    serde_json::Deserializer::from_slice(&bytes[start..])
        .into_iter::<Keys>()
        .next()
        .expect("a value")
        .expect("an object")
        .0
}

fn ascending(keys: &[String]) -> bool {
    keys.windows(2)
        .all(|pair| pair[0].as_bytes() < pair[1].as_bytes())
}

// ------------------------------------------------------------------ C6 ST-S33

/// The capability tokens the Hub served before C6 (C6 R51: C6 adds none).
const BASE_TOKENS: [&str; 2] = ["query.vehicles", "query.current"];
const CURSOR_KEY_TOKENS: [&str; 7] = [
    "query.vehicles",
    "query.current",
    "query.drives",
    "query.charges",
    "query.charge.samples",
    "query.positions",
    "sync.packs",
];
const PAIRED_TOKENS: [&str; 8] = [
    "query.vehicles",
    "query.current",
    "query.drives",
    "query.charges",
    "query.charge.samples",
    "query.positions",
    "sync.packs",
    "auth.revoke",
];

#[tokio::test]
async fn c6_st_s33_http() {
    // A store holding C6 ST-S1's rows and the stored car the observation made.
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let source = store
        .register_source(&SourceDescriptor::new("owner_api", "c6-c1-http"), 1_000)
        .expect("source");
    let vehicle = store
        .register_vehicle(
            &VehicleDescriptor::new(source.source_id, "1").with_tesla_identity(Some(1), None),
            1_000,
        )
        .expect("vehicle");
    store
        .accept_owner_observation_and_lifecycle(
            &ObservationInput {
                source_id: source.source_id,
                vehicle_id: vehicle.vehicle_id,
                observed_at_ms: T0,
                payload: stamped_vehicle_data(T0),
            },
            T0 + 1,
            1,
        )
        .expect("owner observation");
    mark_vehicle_published(&store, vehicle.vehicle_id);
    let state = |store: &HubStore| {
        AppState::new(store.clone(), false, false, false, None, None, None, None)
            .with_summary_now_ms(Some(T0))
    };
    let app = router_for_state(state(&store), CorsPolicy::default());
    let uri = format!("/v1/vehicles/{}/current", vehicle.vehicle_id);
    let (status, headers, body) = get_bytes(&app, &uri).await;

    // 200, no-store, JSON.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(headers[header::CONTENT_TYPE], "application/json");

    // The body is exactly the serialised summary, the stored car filled per C6 R40.
    let rows = store
        .current_observations_for_vehicle(vehicle.vehicle_id)
        .expect("rows");
    let lifecycle = store
        .load_lifecycle_state(vehicle.vehicle_id)
        .expect("lifecycle");
    let car = store
        .materialised_car_for_vehicle(vehicle.vehicle_id)
        .expect("stored car");
    assert!(car.is_some(), "the observation stored a car");
    let expected = build_current_vehicle_summary(
        vehicle.vehicle_id,
        &rows,
        car,
        lifecycle.as_ref(),
        None,
        T0,
        GAP_MS,
    );
    assert!(expected.car.is_some());
    assert_eq!(body, serde_json::to_vec(&expected).expect("expected bytes"));

    // The served keys: 115, in ascending byte order, the car with its 20 and
    // its settings with 16; both the new and the 1.2.0 spellings.
    let order = key_order(&body);
    assert_eq!(order.len(), 115);
    assert!(ascending(&order), "ascending byte order");
    let current: serde_json::Value = serde_json::from_slice(&body).expect("current body");
    let car_order = nested_key_order(&body, "car");
    assert_eq!(car_order.len(), 20);
    assert!(ascending(&car_order));
    let settings_order = nested_key_order(&body, "settings");
    assert_eq!(settings_order.len(), 16);
    assert!(ascending(&settings_order));
    assert_eq!(current["vehicle_id"], vehicle.vehicle_id.to_string());
    assert_eq!(current["observed_at_ms"], T0);
    assert_eq!(current["activity_state"], "online");
    assert_eq!(current["state"], "online");
    assert_eq!(current["battery_level"], 80);
    assert_eq!(current["latitude"], 10.123457);
    assert_eq!(current["longitude"], 20.765432);
    assert_eq!(current["odometer_km"], current["odometer"]);
    assert_eq!(current["software_version"], "2024.14.3");
    assert_eq!(current["version"], "2024.14.3");
    assert_eq!(current["model_code"], "3");
    assert_eq!(current["model"], "3");
    assert_eq!(current["model_name"], "Model 3");
    assert_eq!(current["model_year"], 2019);
    assert_eq!(current["car"]["model_code"], "3");
    assert_eq!(current["car"]["model"], "3");
    assert_eq!(current["car"]["settings"]["collection_enabled"], true);
    assert_eq!(current["car"]["settings"]["enabled"], true);
    assert_eq!(current["geofence_name"], serde_json::Value::Null);
    assert_eq!(current["geofence"], serde_json::Value::Null);

    // The same route served twice, and after a store restart, is byte-identical.
    let (_, _, again) = get_bytes(&app, &uri).await;
    assert_eq!(again, body);
    drop(store);
    let reopened = HubStore::initialize(temp.path()).expect("reopened store");
    let app = router_for_state(state(&reopened), CorsPolicy::default());
    let (_, _, after_restart) = get_bytes(&app, &uri).await;
    assert_eq!(after_restart, body);

    // Discovery advertises exactly what is served: C6 adds no capability, and
    // the profile is the one the Hub advertises (C6-C2 moves it to the minor of
    // the body, C6 R51 and C6 R51a).
    let key = CursorKey::from_bytes([61; 32]);
    let routers = [
        ("router()", router(reopened.clone()), &BASE_TOKENS[..]),
        (
            "public_query_router()",
            public_query_router(reopened.clone(), 61),
            &CURSOR_KEY_TOKENS[..],
        ),
        (
            "paired_router()",
            paired_router(reopened.clone(), &key),
            &PAIRED_TOKENS[..],
        ),
    ];
    for (name, router, tokens) in routers {
        let (status, _, body) = get_bytes(&router, "/.well-known/teslatlas-hub").await;
        assert_eq!(status, StatusCode::OK, "{name}");
        let discovery: serde_json::Value = serde_json::from_slice(&body).expect("discovery");
        assert_eq!(discovery["profile"], HUB_HTTP_PROFILE_ID, "{name}");
        // The literal is the minor advertised before C6 (C6 R51a's fallback):
        // the 1.3.0 profile files are not in this change. The change that moves
        // discovery to C6's minor (C6 R62) edits this line and the constant
        // together, or keeps the literal under a recorded R51a decision.
        assert_eq!(
            discovery["profile"], "hub-http-v1@1.2.0",
            "{name}: the minor advertised before C6"
        );
        let advertised: Vec<&str> = discovery["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .map(|token| token.as_str().expect("token"))
            .collect();
        assert_eq!(advertised, tokens, "{name}: the capabilities served");
    }
}

// ------------------------------------------------------------------ C6 ST-S48

const PAGE_CAP: usize = 1024 * 1024;

/// One history route of the full-page test.
struct FullPage {
    label: &'static str,
    base: String,
    limit: u32,
    /// The ids in the order the route serves them.
    expected_ids: Vec<i64>,
    /// The number of keys of one served item (C6 R58 (a), as amended for the served keys).
    item_keys: usize,
}

fn insert_all(store: &HubStore, sql: &str, rows: Vec<Vec<rusqlite::types::Value>>) {
    let mut connection = store.open().expect("open store");
    let transaction = connection.transaction().expect("transaction");
    for row in rows {
        transaction
            .execute(sql, rusqlite::params_from_iter(row))
            .expect("seed row");
    }
    transaction.commit().expect("commit rows");
}

fn text(value: impl ToString) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(value.to_string())
}

fn integer(value: i64) -> rusqlite::types::Value {
    rusqlite::types::Value::Integer(value)
}

/// The item bytes of a one-item page: between `"items":[` and `],"next_cursor"`.
fn only_item(page: &[u8]) -> Vec<u8> {
    let text = std::str::from_utf8(page).expect("page text");
    let start = text.find("\"items\":[").expect("items") + "\"items\":[".len();
    let end = text.rfind("],\"next_cursor\"").expect("cursor member");
    text.as_bytes()[start..end].to_vec()
}

async fn walk_full_pages(app: &Router, route: &FullPage) {
    let limit = route.limit;
    let first_uri = format!("{}?limit={limit}", route.base);
    let (status, _, first) = get_bytes(app, &first_uri).await;
    assert_eq!(status, StatusCode::OK, "{}", route.label);
    assert!(
        first.len() <= PAGE_CAP,
        "{}: {} bytes",
        route.label,
        first.len()
    );
    let page: serde_json::Value = serde_json::from_slice(&first).expect("first page");
    let items = page["items"].as_array().expect("items");

    // The first page ends at the byte cap, not at the limit, and has a cursor.
    assert!(
        items.len() < limit as usize,
        "{}: {} items fit under the cap at limit {limit}",
        route.label,
        items.len()
    );
    assert!(!items.is_empty());
    let cursor = page["next_cursor"].as_str().expect("next cursor");

    // Its last item is complete: a whole JSON object with every key, in order.
    let raw = std::str::from_utf8(&first).expect("page text");
    let items_start = raw.find("\"items\":[").expect("items") + "\"items\":[".len();
    let items_end = raw.rfind("],\"next_cursor\"").expect("cursor member");
    let last_item_start = {
        // The last item starts after the last `},{` separator of the item list.
        let list = &raw[items_start..items_end];
        list.rfind("},{").map_or(0, |index| index + 2) + items_start
    };
    let last_item = &raw.as_bytes()[last_item_start..items_end];
    let last_keys = key_order(last_item);
    assert_eq!(
        last_keys.len(),
        route.item_keys,
        "{}: last item keys",
        route.label
    );
    assert!(
        ascending(&last_keys),
        "{}: last item key order",
        route.label
    );
    assert_eq!(
        items.last().expect("last item")["id"],
        serde_json::json!(route.expected_ids[items.len() - 1]),
        "{}: last item id",
        route.label
    );

    // The next item would not have fitted: the cap, not an arbitrary cut.
    let (_, _, one) = get_bytes(app, &format!("{}?limit=1&cursor={cursor}", route.base)).await;
    let next_item = only_item(&one);
    assert!(
        first.len() + 1 + next_item.len() > PAGE_CAP,
        "{}: the page was cut by the cap ({} + 1 + {} bytes)",
        route.label,
        first.len(),
        next_item.len()
    );

    // The continuation starts immediately after the last item; the walk serves
    // every id once, in order.
    let mut walked: Vec<i64> = items
        .iter()
        .map(|item| item["id"].as_i64().unwrap())
        .collect();
    let second_uri = format!("{}?limit={limit}&cursor={cursor}", route.base);
    let (_, _, second) = get_bytes(app, &second_uri).await;
    let second: serde_json::Value = serde_json::from_slice(&second).expect("second page");
    assert_eq!(
        second["items"][0]["id"],
        serde_json::json!(route.expected_ids[items.len()]),
        "{}: the second page starts right after the first",
        route.label
    );
    let mut next = page["next_cursor"].as_str().map(str::to_owned);
    let mut pages = 1;
    while let Some(cursor) = next {
        let uri = format!("{}?limit={limit}&cursor={cursor}", route.base);
        let (status, _, body) = get_bytes(app, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.len() <= PAGE_CAP, "{}: page {pages}", route.label);
        let page: serde_json::Value = serde_json::from_slice(&body).expect("page");
        walked.extend(
            page["items"]
                .as_array()
                .expect("items")
                .iter()
                .map(|item| item["id"].as_i64().unwrap()),
        );
        next = page["next_cursor"].as_str().map(str::to_owned);
        pages += 1;
        assert!(pages <= 12, "{}: bounded walk", route.label);
    }
    assert!(pages >= 2, "{}: more than one page", route.label);
    assert_eq!(
        walked, route.expected_ids,
        "{}: no skip, no duplicate",
        route.label
    );
}

#[tokio::test]
async fn c6_st_s48_full_pages() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let vehicle = Uuid::from_u128(0x6c6c_0048_0000_4000_8000_0000_0000_0001);
    seed_active_vehicle_identity(&store, Uuid::from_u128(0x6c48), vehicle);
    mark_vehicle_published(&store, vehicle);
    let vehicle_text = vehicle.to_string();

    // Drives and charges: 520 rows each, a bounded 3,000-character address, so
    // that a page at the largest limit of 500 is well over 1 MiB. Drive 7 and
    // charge 7 are also the parents of the positions and samples below.
    let address = "a".repeat(3_000);
    let mut drives = Vec::new();
    let mut charges = Vec::new();
    for id in 1..=520_i64 {
        let drive = serde_json::json!({
            "id": id, "car_id": 1, "start_date_ms": id * 1_000, "end_date_ms": id * 1_000 + 10,
            "start_address": address
        });
        drives.push(vec![text(&vehicle_text), integer(id), text(drive)]);
        let charge = serde_json::json!({
            "id": id, "car_id": 1, "start_date_ms": id * 1_000, "end_date_ms": id * 1_000 + 10,
            "address": address
        });
        charges.push(vec![text(&vehicle_text), integer(id), text(charge)]);
    }
    insert_all(
        &store,
        "INSERT INTO materialised_drives(vehicle_id, drive_id, car_id, drive_json)
         VALUES (?1, ?2, 1, ?3)",
        drives,
    );
    insert_all(
        &store,
        "INSERT INTO materialised_charges(vehicle_id, charge_id, car_id, charge_json)
         VALUES (?1, ?2, 1, ?3)",
        charges,
    );
    // Positions of drive 7 and samples of charge 7: 2,100 rows each, at the
    // largest limit of 2,048.
    let mut positions = Vec::new();
    let mut samples = Vec::new();
    for id in 1..=2_100_i64 {
        let position = serde_json::json!({
            "id": id, "drive_id": 7, "car_id": 1, "date_ms": 10_000 + id,
            "latitude": 47.123456, "longitude": 19.123456, "speed": 50, "power": 12.5,
            "battery_level": 70, "usable_battery_level": 69, "elevation": 120,
            "odometer": 12345.678, "ideal_battery_range_km": 300.5,
            "est_battery_range_km": 290.5, "rated_battery_range_km": 280.5,
            "inside_temp": 20.5, "outside_temp": 10.5, "tpms_pressure_fl": 2.9
        });
        positions.push(vec![text(&vehicle_text), integer(id), text(position)]);
        let sample = serde_json::json!({
            "id": id, "charge_process_id": 7, "timestamp_ms": 10_000 + id,
            "battery_level": 50, "charger_power_kw": 11.5, "charger_voltage": 230.5,
            "ideal_range_km": 150.5, "rated_range_km": 140.5,
            "charge_cable": "c".repeat(300)
        });
        samples.push(vec![text(&vehicle_text), integer(id), text(sample)]);
    }
    insert_all(
        &store,
        "INSERT INTO materialised_positions(
             vehicle_id, position_id, drive_id, car_id, position_json
         ) VALUES (?1, ?2, 7, 1, ?3)",
        positions,
    );
    insert_all(
        &store,
        "INSERT INTO materialised_charge_samples(
             vehicle_id, sample_id, charge_id, sample_json
         ) VALUES (?1, ?2, 7, ?3)",
        samples,
    );

    let app = public_query_router(store, 61);
    let routes = [
        FullPage {
            label: "drives",
            base: format!("/v1/vehicles/{vehicle}/drives"),
            limit: 500,
            // Newest first.
            expected_ids: (1..=520).rev().collect(),
            item_keys: 43,
        },
        FullPage {
            label: "charges",
            base: format!("/v1/vehicles/{vehicle}/charges"),
            limit: 500,
            expected_ids: (1..=520).rev().collect(),
            item_keys: 39,
        },
        FullPage {
            label: "positions",
            base: format!("/v1/vehicles/{vehicle}/drives/7/positions"),
            limit: 2_048,
            expected_ids: (1..=2_100).collect(),
            item_keys: 31,
        },
        FullPage {
            label: "charge samples",
            base: format!("/v1/vehicles/{vehicle}/charges/7/samples"),
            limit: 2_048,
            expected_ids: (1..=2_100).collect(),
            item_keys: 21,
        },
    ];
    for route in &routes {
        walk_full_pages(&app, route).await;
    }
}
