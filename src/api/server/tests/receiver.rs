// SPDX-License-Identifier: AGPL-3.0-only

//! The self-hosted telemetry receiver's route and startup (G2-6 6A S5, S9; T2,
//! T6, T8). Every store here is a temporary synthetic one, every VIN and token is
//! made up, and the admission clock is injected: each POST reads it exactly once.

use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    os::unix::fs::PermissionsExt,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use super::super::{
    FleetTelemetryIngress, router_with_access_and_telemetry, serve_for_admitted_user,
    serve_with_cursor_key,
};
use super::{admitted_server_fixture, local_plain_server_config, wait_for_tcp_listener};
use crate::{
    config::FleetTelemetryConfig,
    db::{HubStore, MAX_RAW_OBSERVATION_BYTES, RECEIVER_EVENT_RECORD_TYPE, StoreCounts},
    fleet_telemetry::{FleetTelemetryAccumulator, MAX_FLEET_TELEMETRY_INPUT_BYTES},
    protocol::{CursorKey, Sha256Digest},
};

const VIN: &str = "5YJ3E1EA7KF000001";
const OTHER_VIN: &str = "5YJ3E1EA7KF000002";
const EID: u64 = 70;
const T0: i64 = 1_800_000_000_000;
/// The first admission time of a fixture: after every envelope it receives.
const ADMIT0: i64 = T0 + 100_000;

const VECTORS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/protocol/profiles/receiver-v1/1.0.0/vectors.json"
));
const PROJECTED_EXAMPLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/protocol/profiles/receiver-v1/1.0.0/examples/projected-envelope.json"
));
const ALERT_EXAMPLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/protocol/profiles/receiver-v1/1.0.0/examples/alert-envelope.json"
));
const ERROR_EXAMPLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/protocol/profiles/receiver-v1/1.0.0/examples/error-envelope.json"
));

// ---- envelope builders ----

fn envelope(txid: &str, tx_type: &str, ts: i64, received: i64, payload: Value) -> Value {
    json!({
        "version": 1,
        "vin": VIN,
        "txid": txid,
        "tx_type": tx_type,
        "received_at_ms": received,
        "timestamp_ms": ts,
        "payload": payload,
    })
}

fn vehicle(txid: &str, ts: i64, data: Value) -> Value {
    envelope(
        txid,
        "V",
        ts,
        ts + 100,
        json!({"vin": VIN, "createdAt": "2027-01-15T08:00:00Z", "data": data}),
    )
}

fn connectivity(txid: &str, ts: i64, status: &str, connection_id: Option<&str>) -> Value {
    let mut payload = json!({"status": status, "createdAt": "2027-01-15T08:00:00Z"});
    if let Some(id) = connection_id {
        payload["connectionId"] = json!(id);
    }
    envelope(txid, "connectivity", ts, ts + 100, payload)
}

fn soc(percent: i64) -> Value {
    json!({"Soc": {"intValue": percent.to_string()}})
}

// ---- the fixture ----

struct Fixture {
    _temporary: tempfile::TempDir,
    token_path: std::path::PathBuf,
    token: String,
    store: HubStore,
    cursor_key: CursorKey,
    vehicle_id: Uuid,
    ingress: FleetTelemetryIngress,
    app: Router,
    admission: Arc<AtomicI64>,
    step: Arc<AtomicI64>,
}

impl Fixture {
    async fn new() -> Self {
        let temporary = crate::private_tempdir().expect("temporary Hub");
        let token = "a".repeat(64);
        let token_path = temporary.path().join("telemetry-token");
        fs::write(&token_path, format!("{token}\n")).expect("token");
        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).expect("private token");
        let store = HubStore::initialize(temporary.path().join("data")).expect("store");
        let cursor_key = CursorKey::from_bytes([61; 32]);
        let vehicle_id =
            crate::collector::configure_fleet_vehicle_for_test(&store, &cursor_key, EID, VIN).await;
        let admission = Arc::new(AtomicI64::new(ADMIT0));
        let step = Arc::new(AtomicI64::new(1_000));
        let (ingress, app) = Self::serve(&token_path, &store, &cursor_key, &admission, &step);
        store.instrumentation().reset();
        Self {
            _temporary: temporary,
            token_path,
            token,
            store,
            cursor_key,
            vehicle_id,
            ingress,
            app,
            admission,
            step,
        }
    }

    fn serve(
        token_path: &std::path::Path,
        store: &HubStore,
        cursor_key: &CursorKey,
        admission: &Arc<AtomicI64>,
        step: &Arc<AtomicI64>,
    ) -> (FleetTelemetryIngress, Router) {
        let ingress = FleetTelemetryIngress::from_token_file(token_path).expect("ingress");
        let (clock, step) = (Arc::clone(admission), Arc::clone(step));
        ingress
            .receiver
            .hooks()
            .set_clock(move || clock.fetch_add(step.load(Ordering::SeqCst), Ordering::SeqCst));
        let app = router_with_access_and_telemetry(
            store.clone(),
            false,
            false,
            false,
            None,
            Some(cursor_key.clone()),
            None,
            Some(ingress.clone()),
        );
        (ingress, app)
    }

    /// A process restart: a new receiver (empty caches, no accumulators) over the
    /// same store, with the same admission clock.
    fn restart(&mut self) {
        let (ingress, app) = Self::serve(
            &self.token_path,
            &self.store,
            &self.cursor_key,
            &self.admission,
            &self.step,
        );
        self.ingress = ingress;
        self.app = app;
    }

    /// Every later POST is admitted at exactly `ms` (the clock stands still).
    fn admit_at(&self, ms: i64) {
        self.admission.store(ms, Ordering::SeqCst);
        self.step.store(0, Ordering::SeqCst);
    }

    async fn post_bytes(&self, body: Vec<u8>) -> StatusCode {
        self.app
            .clone()
            .oneshot(
                Request::post("/v1/internal/fleet-telemetry")
                    .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .expect("receiver response")
            .status()
    }

    async fn post(&self, value: &Value) -> StatusCode {
        self.post_bytes(serde_json::to_vec(value).unwrap()).await
    }

    fn counts(&self) -> StoreCounts {
        self.store.instrumentation().snapshot()
    }

    fn reset_counts(&self) {
        self.store.instrumentation().reset();
    }

    fn sql<T>(&self, sql: &str, row: impl FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>) -> T {
        self.store
            .open()
            .expect("database")
            .query_row(sql, [], row)
            .expect("query")
    }

    fn state_bytes(&self) -> Option<Vec<u8>> {
        self.store
            .load_receiver_state(self.vehicle_id)
            .expect("state")
    }

    /// The accumulated Owner-shaped state the Hub last stored for the vehicle.
    fn response(&self) -> Option<Value> {
        crate::collector::fleet_telemetry_seed_for_vin(&self.store, VIN).expect("seed")
    }

    /// Stored alert and error events: (observed, received, payload).
    fn events(&self) -> Vec<(i64, i64, Value)> {
        self.store
            .open()
            .unwrap()
            .prepare(
                "SELECT observed_at_ms, received_at_ms, payload_json FROM raw_observations
                  WHERE json_extract(payload_json, '$.record_type') = ?1
                  ORDER BY observation_id",
            )
            .unwrap()
            .query_map([RECEIVER_EVENT_RECORD_TYPE], |row| {
                let payload: String = row.get(2)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    serde_json::from_str(&payload).unwrap(),
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn current_rows(&self, record_type: &str) -> Vec<(i64, i64, i64, Value)> {
        self.store
            .open()
            .unwrap()
            .prepare(
                "SELECT observation_id, observed_at_ms, received_at_ms, payload_json
                   FROM current_observations WHERE record_type = ?1",
            )
            .unwrap()
            .query_map([record_type], |row| {
                let payload: String = row.get(3)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    serde_json::from_str(&payload).unwrap(),
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn count(&self, table: &str) -> i64 {
        self.sql(&format!("SELECT COUNT(*) FROM {table}"), |row| row.get(0))
    }

    /// Persistent triggers that log what writes the identity tables, by class.
    fn install_identity_triggers(&self) {
        self.store
            .open()
            .unwrap()
            .execute_batch(
                "CREATE TABLE identity_write_log(
                    id INTEGER PRIMARY KEY AUTOINCREMENT, what TEXT NOT NULL);
                 CREATE TRIGGER log_sources_insert AFTER INSERT ON sources
                    BEGIN INSERT INTO identity_write_log(what) VALUES('source:insert'); END;
                 CREATE TRIGGER log_sources_delete AFTER DELETE ON sources
                    BEGIN INSERT INTO identity_write_log(what) VALUES('source:delete'); END;
                 CREATE TRIGGER log_sources_update AFTER UPDATE ON sources
                    BEGIN INSERT INTO identity_write_log(what) VALUES('source:update'); END;
                 CREATE TRIGGER log_vehicles_insert AFTER INSERT ON vehicles
                    BEGIN INSERT INTO identity_write_log(what) VALUES('vehicle:insert'); END;
                 CREATE TRIGGER log_vehicles_delete AFTER DELETE ON vehicles
                    BEGIN INSERT INTO identity_write_log(what) VALUES('vehicle:delete'); END;
                 CREATE TRIGGER log_aliases_insert AFTER INSERT ON vehicle_identity_aliases
                    BEGIN INSERT INTO identity_write_log(what) VALUES('alias:insert'); END;
                 CREATE TRIGGER log_aliases_delete AFTER DELETE ON vehicle_identity_aliases
                    BEGIN INSERT INTO identity_write_log(what) VALUES('alias:delete'); END;
                 CREATE TRIGGER log_vehicles_identity AFTER UPDATE ON vehicles
                  WHEN NEW.source_id IS NOT OLD.source_id
                    OR NEW.source_vehicle_key IS NOT OLD.source_vehicle_key
                    OR NEW.vin IS NOT OLD.vin
                    BEGIN INSERT INTO identity_write_log(what) VALUES('vehicle:identity-change'); END;
                 CREATE TRIGGER log_vehicles_equal_rewrite AFTER UPDATE ON vehicles
                  WHEN NEW.source_id IS OLD.source_id
                   AND NEW.source_vehicle_key IS OLD.source_vehicle_key
                   AND NEW.vin IS OLD.vin AND NEW.display_name IS OLD.display_name
                   AND NEW.created_at_ms IS OLD.created_at_ms
                   AND NEW.last_seen_at_ms IS OLD.last_seen_at_ms
                    BEGIN INSERT INTO identity_write_log(what) VALUES('vehicle:equal-rewrite'); END;
                 CREATE TRIGGER log_vehicles_metadata AFTER UPDATE ON vehicles
                  WHEN NEW.source_id IS OLD.source_id
                   AND NEW.source_vehicle_key IS OLD.source_vehicle_key
                   AND NEW.vin IS OLD.vin
                   AND (NEW.display_name IS NOT OLD.display_name
                        OR NEW.last_seen_at_ms IS NOT OLD.last_seen_at_ms)
                    BEGIN INSERT INTO identity_write_log(what) VALUES('vehicle:metadata-update'); END;",
            )
            .expect("identity triggers");
    }

    fn identity_log(&self) -> Vec<String> {
        self.store
            .open()
            .unwrap()
            .prepare("SELECT what FROM identity_write_log ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

fn log_count(log: &[String], what: &str) -> usize {
    log.iter().filter(|entry| entry.as_str() == what).count()
}

// ---- T2: the profile's vectors ----

#[tokio::test]
async fn receiver_answers_the_profile_vectors() {
    let fixture = Fixture::new().await;
    let vectors: Value = serde_json::from_str(VECTORS).unwrap();

    // Without the bearer nothing is read or written.
    let unauthorized = fixture
        .app
        .clone()
        .oneshot(
            Request::post("/v1/internal/fleet-telemetry")
                .body(Body::from(PROJECTED_EXAMPLE))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(fixture.counts(), StoreCounts::default());

    // Every receiver-negative entry is a 400 with no storage, registration,
    // transaction or clock delta, schema-valid or not.
    let negatives = vectors["receiver_negative_envelopes"].as_array().unwrap();
    assert!(negatives.len() >= 14);
    for negative in negatives {
        fixture.reset_counts();
        let reads_before = fixture.ingress.receiver.hooks().clock_reads();
        let status = fixture.post(&negative["envelope"]).await;
        assert_eq!(
            status,
            StatusCode::from_u16(negative["expected_status"].as_u64().unwrap() as u16).unwrap(),
            "{}",
            negative["id"]
        );
        assert_eq!(
            fixture.counts(),
            StoreCounts::default(),
            "{}",
            negative["id"]
        );
        assert_eq!(
            fixture.ingress.receiver.hooks().clock_reads(),
            reads_before,
            "{}",
            negative["id"]
        );
    }

    // The three examples and the three non-projection envelopes are accepted.
    for example in [PROJECTED_EXAMPLE, ALERT_EXAMPLE, ERROR_EXAMPLE] {
        let body: Value = serde_json::from_str(example).unwrap();
        assert_eq!(fixture.post(&body).await, StatusCode::NO_CONTENT, "{body}");
    }
    for entry in vectors["non_projection_envelopes"].as_array().unwrap() {
        assert_eq!(
            fixture.post(&entry["envelope"]).await,
            StatusCode::NO_CONTENT,
            "{}",
            entry["kind"]
        );
    }
    // Alerts and errors are kept once each (the alert example is also a vector).
    let kept: Vec<String> = fixture
        .events()
        .iter()
        .map(|(_, _, record)| record["txid"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kept, vec!["edge-alert-0003", "edge-error-0004"]);
    assert_eq!(
        fixture.response().unwrap()["charge_state"]["usable_battery_level"],
        80
    );
}

#[tokio::test]
async fn receiver_validates_before_any_store_read() {
    let fixture = Fixture::new().await;
    let mut lowercase = vehicle("tx-1", T0, soc(80));
    lowercase["vin"] = json!(VIN.to_lowercase());
    let mut positional = vehicle("tx-2", T0, soc(80));
    positional["payload"] = json!([1, 2, 3]);
    let mut aliased = vehicle("tx-3", T0, soc(80));
    aliased["deviceClientVersion"] = json!("1.0");
    let mut null_version = vehicle("tx-4", T0, soc(80));
    null_version["firmware_version"] = Value::Null;
    let mut unsupported = vehicle("tx-5", T0, soc(80));
    unsupported["tx_type"] = json!("telemetry");
    let mut late = vehicle("tx-6", T0, soc(80));
    late["timestamp_ms"] = json!(T0 + 100 + 300_001);
    for body in [
        lowercase,
        positional,
        aliased,
        null_version,
        unsupported,
        late,
    ] {
        assert_eq!(fixture.post(&body).await, StatusCode::BAD_REQUEST, "{body}");
    }
    assert_eq!(
        fixture.post_bytes(b"{not json".to_vec()).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fixture.counts(),
        StoreCounts::default(),
        "no open, no write"
    );
    assert_eq!(fixture.ingress.receiver.hooks().clock_reads(), 0);
    // A valid record then reads the store.
    assert_eq!(
        fixture.post(&vehicle("tx-7", T0, soc(80))).await,
        StatusCode::NO_CONTENT
    );
    assert!(fixture.counts().opens() > 0);
    assert_eq!(fixture.ingress.receiver.hooks().clock_reads(), 1);
}

// ---- alerts and errors ----

#[tokio::test]
async fn receiver_stores_alerts_as_events_with_admission_time() {
    let fixture = Fixture::new().await;
    fixture.admit_at(T0 + 5_000);
    let alert = envelope(
        "alert-1",
        "alerts",
        T0 + 200,
        T0 + 300,
        json!({"name": "userPresent", "createdAt": "2027-01-15T08:00:00.200Z"}),
    );
    assert_eq!(fixture.post(&alert).await, StatusCode::NO_CONTENT);
    let events = fixture.events();
    assert_eq!(events.len(), 1);
    let (observed, received, record) = &events[0];
    assert_eq!(
        *observed,
        T0 + 200,
        "the event time is the observation time"
    );
    assert_eq!(
        *received,
        T0 + 5_000,
        "the Hub admission time is the receipt time"
    );
    assert_eq!(record["record_type"], RECEIVER_EVENT_RECORD_TYPE);
    assert_eq!(record["tx_type"], "alerts");
    assert_eq!(record["txid"], "alert-1");
    assert_eq!(record["envelope_received_at_ms"], T0 + 300);
    assert_eq!(record["timestamp_ms"], T0 + 200);
    assert_eq!(record["payload"]["name"], "userPresent");
    // No current row, no accumulator, no projection.
    assert!(fixture.current_rows(RECEIVER_EVENT_RECORD_TYPE).is_empty());
    assert!(fixture.current_rows("fleet_api_vehicle_data_v1").is_empty());
    assert_eq!(
        fixture.state_bytes(),
        None,
        "an event never touches the accumulator"
    );

    // A replay after a lost acknowledgement stores nothing new, whatever the clock.
    fixture.admit_at(T0 + 9_000);
    assert_eq!(fixture.post(&alert).await, StatusCode::NO_CONTENT);
    assert_eq!(fixture.events(), events);

    // An error is kept the same way.
    let error = envelope(
        "error-1",
        "errors",
        T0 + 400,
        T0 + 450,
        json!({"name": "invalidData", "message": "synthetic"}),
    );
    assert_eq!(fixture.post(&error).await, StatusCode::NO_CONTENT);
    assert_eq!(fixture.events().len(), 2);
    assert_eq!(fixture.events()[1].2["tx_type"], "errors");
    // The retained events survive the prune that follows a later vehicle record.
    assert_eq!(
        fixture.post(&vehicle("tx-v", T0 + 1_000, soc(80))).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(fixture.events().len(), 2, "events outlive later intake");
}

#[tokio::test]
async fn receiver_refuses_an_oversized_event_before_any_store_access() {
    let fixture = Fixture::new().await;
    // A body at the route's bound whose stored event record is larger than the
    // stored-observation bound: the stored form adds the record type and renames
    // the dispatcher arrival time.
    let build = |pad: usize| {
        serde_json::to_vec(&envelope(
            "t",
            "alerts",
            T0 + 200,
            T0 + 300,
            json!({"name": "n", "pad": "x".repeat(pad)}),
        ))
        .unwrap()
    };
    let mut pad = MAX_FLEET_TELEMETRY_INPUT_BYTES - 200;
    while build(pad + 1).len() <= MAX_FLEET_TELEMETRY_INPUT_BYTES {
        pad += 1;
    }
    let body = build(pad);
    assert!(
        body.len() <= MAX_FLEET_TELEMETRY_INPUT_BYTES,
        "the body fits the route bound"
    );
    let stored = serde_json::to_vec(&json!({
        "record_type": RECEIVER_EVENT_RECORD_TYPE,
        "tx_type": "alerts",
        "txid": "t",
        "envelope_received_at_ms": T0 + 300,
        "timestamp_ms": T0 + 200,
        "payload": {"name": "n", "pad": "x".repeat(pad)},
    }))
    .unwrap();
    assert!(
        stored.len() > MAX_RAW_OBSERVATION_BYTES,
        "the stored record exceeds the observation bound ({} bytes)",
        stored.len()
    );

    // The first POST after start: refused with no clock read, open, registration
    // or transaction.
    assert_eq!(fixture.post_bytes(body).await, StatusCode::BAD_REQUEST);
    assert_eq!(fixture.ingress.receiver.hooks().clock_reads(), 0);
    assert_eq!(fixture.counts(), StoreCounts::default());
    assert!(fixture.events().is_empty());
}

#[tokio::test]
async fn receiver_uses_one_admission_time_per_post() {
    let fixture = Fixture::new().await;
    // The clock advances by a second on every read, so a second read would show.
    let hooks = fixture.ingress.receiver.hooks();
    let first = fixture.admission.load(Ordering::SeqCst);
    assert_eq!(
        fixture.post(&vehicle("tx-1", T0, soc(80))).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(hooks.clock_reads(), 1);
    let current = fixture.current_rows("fleet_api_vehicle_data_v1");
    assert_eq!(
        current[0].2, first,
        "the observation is received at the admission time"
    );
    let state_time: i64 = fixture.sql(
        "SELECT updated_at_ms FROM fleet_telemetry_accumulator_states",
        |row| row.get(0),
    );
    assert_eq!(state_time, first, "the state is written at the same time");
    let second = fixture.admission.load(Ordering::SeqCst);
    assert_eq!(second, first + 1_000);
    let alert = envelope("a-1", "alerts", T0 + 200, T0 + 300, json!({"name": "n"}));
    assert_eq!(fixture.post(&alert).await, StatusCode::NO_CONTENT);
    assert_eq!(hooks.clock_reads(), 2, "one read per accepted POST");
    assert_eq!(fixture.events()[0].1, second);
}

#[tokio::test]
async fn receiver_unknown_vehicle_is_unprocessable() {
    let fixture = Fixture::new().await;
    let mut unknown = vehicle("tx-1", T0, soc(80));
    unknown["vin"] = json!(OTHER_VIN);
    unknown["payload"]["vin"] = json!(OTHER_VIN);
    assert_eq!(
        fixture.post(&unknown).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut alert = envelope("a-1", "alerts", T0 + 200, T0 + 300, json!({"name": "n"}));
    alert["vin"] = json!(OTHER_VIN);
    assert_eq!(fixture.post(&alert).await, StatusCode::UNPROCESSABLE_ENTITY);
    // A disabled vehicle is as unknown as an unconfigured one.
    fixture
        .store
        .open()
        .unwrap()
        .execute(
            "UPDATE car_settings SET enabled = 0 WHERE vehicle_id = ?1",
            [fixture.vehicle_id.to_string()],
        )
        .unwrap();
    assert_eq!(
        fixture.post(&vehicle("tx-2", T0, soc(80))).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let counts = fixture.counts();
    assert_eq!(counts.write_transaction_attempts, 0);
    assert_eq!(counts.registration_calls(), 0);
    assert!(fixture.events().is_empty());
}

// ---- restart equivalence ----

/// Forty envelopes: out-of-order fields, an unavailable value, split pack
/// samples, a connection end with delayed frames of the old connection, an
/// unchanged connection id, a new connection and unknown fields.
fn script() -> Vec<Value> {
    let v = |txid: &str, offset: i64, data: Value| vehicle(txid, T0 + offset, data);
    let c = |txid: &str, offset: i64, status: &str, id: Option<&str>| {
        connectivity(txid, T0 + offset, status, id)
    };
    let script = vec![
        c("c01", 0, "CONNECTED", Some("conn-1")),
        v("v02", 1_000, soc(80)),
        v("v03", 2_000, json!({"BatteryLevel": {"doubleValue": 79.0}})),
        v(
            "v04",
            3_000,
            json!({
                "Location": {"locationValue": {"latitude": 51.5, "longitude": -0.12}},
                "VehicleSpeed": {"doubleValue": 48.0},
                "Gear": {"shiftStateValue": "ShiftStateD"}
            }),
        ),
        v("v05", 4_000, json!({"PackVoltage": {"doubleValue": 400.0}})),
        v("v06", 4_100, json!({"PackCurrent": {"doubleValue": 10.0}})),
        v("v07", 2_500, soc(78)),
        v("v08", 500, soc(60)),
        v(
            "v09",
            5_000,
            json!({"InsideTemp": {"invalidValue": "not_available"}}),
        ),
        v("v10", 5_100, json!({"InsideTemp": {"doubleValue": 21.5}})),
        v(
            "v11",
            5_200,
            json!({"FutureTeslaField": {"stringValue": "x"}}),
        ),
        v(
            "v12",
            6_000,
            json!({"DCChargingEnergyIn": {"doubleValue": 1.0}, "ChargerPower": {"doubleValue": 3.0}}),
        ),
        v(
            "v13",
            6_100,
            json!({"ACChargingPower": {"doubleValue": 7.0}}),
        ),
        v(
            "v14",
            6_200,
            json!({"DCChargingPower": {"doubleValue": 0.0}}),
        ),
        v(
            "v15",
            7_000,
            json!({"DCChargingEnergyIn": {"doubleValue": 2.5}}),
        ),
        v("v16", 7_500, json!({"PackVoltage": {"doubleValue": 401.0}})),
        v("v17", 7_600, json!({"PackCurrent": {"doubleValue": 9.0}})),
        v("v18", 8_000, json!({"Odometer": {"doubleValue": 1000.0}})),
        v("v19", 9_000, soc(77)),
        v(
            "v20",
            9_500,
            json!({
                "FastChargerPresent": {"booleanValue": true},
                "FastChargerType": {"fastChargerValue": "FastChargerSupercharger"}
            }),
        ),
        c("c21", 20_000, "DISCONNECTED", None),
        v("v22", 10_000, soc(70)),
        v(
            "v23",
            19_999,
            json!({"BatteryLevel": {"doubleValue": 50.0}}),
        ),
        v("v24", 21_000, soc(75)),
        c("c25", 22_000, "CONNECTED", Some("conn-2")),
        v(
            "v26",
            23_000,
            json!({"DCChargingEnergyIn": {"doubleValue": 2.6}}),
        ),
        v("v27", 23_100, json!({"ChargerPower": {"doubleValue": 4.0}})),
        v(
            "v28",
            23_200,
            json!({"ACChargingPower": {"doubleValue": 6.5}}),
        ),
        v(
            "v29",
            24_000,
            json!({"PackVoltage": {"doubleValue": 399.0}}),
        ),
        v("v30", 24_050, json!({"PackCurrent": {"doubleValue": 8.0}})),
        v("v31", 25_000, soc(74)),
        c("c32", 26_000, "CONNECTED", Some("conn-2")),
        v(
            "v33",
            27_000,
            json!({"BatteryLevel": {"doubleValue": 73.0}}),
        ),
        v("v34", 21_500, soc(72)),
        c("c35", 30_000, "DISCONNECTED", None),
        c("c36", 31_000, "CONNECTED", Some("conn-3")),
        v(
            "v37",
            32_000,
            json!({"Gear": {"shiftStateValue": "ShiftStatePark"}}),
        ),
        v("v38", 33_000, soc(71)),
        v(
            "v39",
            34_000,
            json!({"DCChargingEnergyIn": {"doubleValue": 0.1}}),
        ),
        v("v40", 35_000, json!({"ChargerPower": {"doubleValue": 1.0}})),
    ];
    assert_eq!(script.len(), 40);
    script
}

/// Run the script through a fresh fixture, restarting the receiver before
/// envelope `restart_before` (never when `None`), at the same admission time per
/// envelope, and record what each POST left behind.
async fn run_script(restart_before: Option<usize>) -> Vec<(u16, Option<Vec<u8>>, Option<Value>)> {
    let mut fixture = Fixture::new().await;
    let mut seen = Vec::new();
    for (index, record) in script().iter().enumerate() {
        if restart_before == Some(index) {
            fixture.restart();
        }
        fixture.admit_at(ADMIT0 + index as i64 * 1_000);
        let status = fixture.post(record).await;
        seen.push((status.as_u16(), fixture.state_bytes(), fixture.response()));
    }
    if restart_before == Some(script().len()) {
        fixture.restart();
    }
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn receiver_restart_is_equivalent_for_every_split() {
    let reference = run_script(None).await;
    assert!(reference.iter().all(|(status, _, _)| *status == 204));
    // The script exercises what the comparison is meant to protect.
    let last = reference.last().unwrap().2.clone().unwrap();
    assert_eq!(last["charge_state"]["usable_battery_level"], 71);
    assert!(
        last["charge_state"].get("fast_charger_present").is_none(),
        "groups reset"
    );
    // The state bytes are the accumulator after the same envelopes, restart or not.
    let mut accumulator = FleetTelemetryAccumulator::empty(VIN).unwrap();
    for record in script() {
        accumulator
            .apply_json(&serde_json::to_vec(&record).unwrap())
            .unwrap();
    }
    assert_eq!(
        reference.last().unwrap().1.as_deref(),
        Some(accumulator.encode_complete().unwrap().as_slice())
    );
    for split in 0..=script().len() {
        let restarted = run_script(Some(split)).await;
        assert_eq!(restarted, reference, "restart before envelope {split}");
    }
}

// ---- unusable stored state ----

#[tokio::test]
async fn receiver_discards_unusable_state_once() {
    for wrong_vin in [false, true] {
        let fixture = Fixture::new().await;
        crate::test_log_capture::arm();
        let unusable = if wrong_vin {
            FleetTelemetryAccumulator::empty(OTHER_VIN)
                .unwrap()
                .encode_complete()
                .unwrap()
        } else {
            b"{not a state".to_vec()
        };
        fixture
            .store
            .open()
            .unwrap()
            .execute(
                "INSERT INTO fleet_telemetry_accumulator_states(
                    vehicle_id, state_version, state_json, updated_at_ms)
                 VALUES (?1, 2, ?2, 1)",
                rusqlite::params![fixture.vehicle_id.to_string(), unusable],
            )
            .unwrap();
        let first = vehicle("tx-1", T0, soc(80));
        assert_eq!(fixture.post(&first).await, StatusCode::NO_CONTENT);
        let second = vehicle("tx-2", T0 + 1_000, soc(79));
        assert_eq!(fixture.post(&second).await, StatusCode::NO_CONTENT);

        // The result equals a fresh accumulator applying the same records, and
        // the stored state is repaired.
        let mut expected = FleetTelemetryAccumulator::empty(VIN).unwrap();
        for record in [&first, &second] {
            expected
                .apply_json(&serde_json::to_vec(record).unwrap())
                .unwrap();
        }
        let repaired = fixture.state_bytes().unwrap();
        assert_eq!(
            repaired,
            expected.encode_complete().unwrap(),
            "wrong_vin={wrong_vin}"
        );
        FleetTelemetryAccumulator::restore_complete_for_vin(&repaired, VIN)
            .expect("the repaired state is usable");

        let log = crate::test_log_capture::disarm();
        let warnings: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("accumulator_state_discarded"))
            .collect();
        assert_eq!(warnings.len(), 1, "wrong_vin={wrong_vin}: {log}");
        assert!(warnings[0].contains("WARN"));
        assert!(warnings[0].contains("collector.receiver"));
        assert!(warnings[0].contains(&fixture.vehicle_id.to_string()));
        assert!(
            !log.contains(VIN) && !log.contains(OTHER_VIN),
            "no VIN in the log"
        );
    }
}

// ---- sparse marker, connections, energy ----

#[tokio::test]
async fn receiver_marks_sparse_and_resets_groups_per_connection() {
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture
            .post(&connectivity("c1", T0, "CONNECTED", Some("conn-1")))
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture
            .post(&vehicle(
                "v1",
                T0 + 1_000,
                json!({"Soc": {"intValue": "80"}, "ChargerPower": {"doubleValue": 4.0}})
            ))
            .await,
        StatusCode::NO_CONTENT
    );
    // Every stored snapshot is a sparse Fleet record.
    let stored = fixture.current_rows("fleet_api_vehicle_data_v1");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].3["provider_feed"], "fleet_telemetry");
    let response = fixture.response().unwrap();
    assert_eq!(response["charge_state"]["usable_battery_level"], 80);
    assert_eq!(response["charge_state"]["charger_power"], 4.0);

    // The connection ends: the groups reset, the watermarks stay.
    assert_eq!(
        fixture
            .post(&connectivity("c2", T0 + 2_000, "DISCONNECTED", None))
            .await,
        StatusCode::NO_CONTENT
    );
    let after = fixture.response().unwrap();
    assert!(
        after.get("charge_state").is_none(),
        "the group is reset: {after}"
    );
    assert_eq!(after["state"], "offline");
    // A new connection id starts from empty groups again.
    assert_eq!(
        fixture
            .post(&connectivity("c3", T0 + 3_000, "CONNECTED", Some("conn-2")))
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture.post(&vehicle("v2", T0 + 4_000, soc(70))).await,
        StatusCode::NO_CONTENT
    );
    let again = fixture.response().unwrap();
    assert_eq!(again["charge_state"]["usable_battery_level"], 70);
    assert!(again["charge_state"].get("charger_power").is_none());
}

#[tokio::test]
async fn receiver_drops_delayed_frames_of_an_ended_connection_across_restart() {
    let mut fixture = Fixture::new().await;
    assert_eq!(
        fixture.post(&vehicle("v1", T0, soc(80))).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture
            .post(&connectivity("c1", T0 + 20_000, "DISCONNECTED", None))
            .await,
        StatusCode::NO_CONTENT
    );
    for restart in [false, true] {
        if restart {
            fixture.restart();
        }
        // A frame stamped before, and one stamped at, the boundary are dropped.
        for (txid, offset) in [("late-a", 10_000), ("late-b", 20_000)] {
            assert_eq!(
                fixture.post(&vehicle(txid, T0 + offset, soc(55))).await,
                StatusCode::NO_CONTENT,
                "restart={restart}"
            );
            let response = fixture.response().unwrap();
            assert!(
                response.get("charge_state").is_none(),
                "restart={restart} {txid}: {response}"
            );
        }
    }
    assert_eq!(
        fixture.post(&vehicle("fresh", T0 + 30_000, soc(73))).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture.response().unwrap()["charge_state"]["usable_battery_level"],
        73
    );
}

#[tokio::test]
async fn receiver_keeps_one_energy_counter_across_a_reconnect() {
    let fixture = Fixture::new().await;
    let energy = |fixture: &Fixture| {
        fixture.response().unwrap()["charge_state"]["charge_energy_added"].as_f64()
    };
    assert_eq!(
        fixture
            .post(&connectivity("c1", T0, "CONNECTED", Some("conn-1")))
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        fixture
            .post(&vehicle(
                "e1",
                T0 + 1_000,
                json!({"DCChargingEnergyIn": {"doubleValue": 5.0}})
            ))
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(energy(&fixture), Some(5.0));
    // The unit of the other signal is never mixed in.
    assert_eq!(
        fixture
            .post(&vehicle(
                "e2",
                T0 + 2_000,
                json!({"ChargeEnergyAdded": {"doubleValue": 99.0}})
            ))
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(energy(&fixture), Some(5.0));
    // The reconnect starts a new counter from the vehicle's own reading.
    assert_eq!(
        fixture
            .post(&connectivity("c2", T0 + 3_000, "CONNECTED", Some("conn-2")))
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(energy(&fixture), None);
    assert_eq!(
        fixture
            .post(&vehicle(
                "e3",
                T0 + 4_000,
                json!({"DCChargingEnergyIn": {"doubleValue": 0.5}, "ChargeEnergyAdded": {"doubleValue": 98.0}})
            ))
            .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(energy(&fixture), Some(0.5));
}

// ---- transactions, registration, failure ----

#[tokio::test]
async fn receiver_commits_once_per_post() {
    let fixture = Fixture::new().await;
    for (index, record) in [
        vehicle("t1", T0, soc(80)),
        vehicle("t2", T0 + 1_000, soc(79)),
        // A record that changes no state still commits once.
        vehicle("t3", T0 + 500, soc(10)),
        connectivity("t4", T0 + 2_000, "CONNECTED", Some("conn-1")),
        envelope("t5", "alerts", T0 + 3_000, T0 + 3_100, json!({"name": "n"})),
        // A replay of an event still takes the one transaction.
        envelope("t5", "alerts", T0 + 3_000, T0 + 3_100, json!({"name": "n"})),
    ]
    .iter()
    .enumerate()
    {
        let before = fixture.counts();
        assert_eq!(
            fixture.post(record).await,
            StatusCode::NO_CONTENT,
            "{index}"
        );
        let after = fixture.counts();
        assert_eq!(
            after.write_transaction_attempts - before.write_transaction_attempts,
            1,
            "{index}: one transaction attempt"
        );
        assert_eq!(
            after.write_transaction_commits - before.write_transaction_commits,
            1,
            "{index}: one commit"
        );
    }
}

#[tokio::test]
async fn receiver_never_writes_an_unchanged_registration() {
    let mut fixture = Fixture::new().await;
    fixture.install_identity_triggers();
    fixture.reset_counts();
    let config = json!({"Version": {"stringValue": "2026.20.1 abc"}, "CarType": {"carTypeValue": "CarTypeModel3"}});

    // The first POST after start, then ten more.
    let mut offset = 0;
    let mut next = |data: Value| {
        offset += 1_000;
        vehicle(&format!("u{offset}"), T0 + offset, data)
    };
    let first = next(soc(80));
    assert_eq!(fixture.post(&first).await, StatusCode::NO_CONTENT);
    for percent in 70..80 {
        let record = next(soc(percent));
        assert_eq!(fixture.post(&record).await, StatusCode::NO_CONTENT);
    }
    let counts = fixture.counts();
    assert_eq!(counts.source_registration_calls, 0);
    assert_eq!(counts.vehicle_registration_calls, 0);
    let log = fixture.identity_log();
    for forbidden in [
        "source:insert",
        "source:delete",
        "source:update",
        "vehicle:insert",
        "vehicle:delete",
        "alias:insert",
        "alias:delete",
        "vehicle:identity-change",
        "vehicle:equal-rewrite",
        "vehicle:metadata-update",
    ] {
        assert_eq!(log_count(&log, forbidden), 0, "{forbidden}: {log:?}");
    }

    // The first POST after a restart resolves the same identity read-only.
    fixture.restart();
    let record = next(soc(60));
    assert_eq!(fixture.post(&record).await, StatusCode::NO_CONTENT);
    assert!(
        fixture.identity_log().is_empty(),
        "a restart writes nothing either"
    );

    // A record carrying vehicle configuration updates the vehicle's metadata
    // inside the intake (the lifecycle's own update, once per lifecycle pass):
    // counted apart, and not a registration.
    let record = next(config);
    assert_eq!(fixture.post(&record).await, StatusCode::NO_CONTENT);
    let log = fixture.identity_log();
    let lifecycle_updates = log_count(&log, "vehicle:metadata-update");
    let lifecycle_rewrites = log_count(&log, "vehicle:equal-rewrite");
    assert!(
        lifecycle_updates > 0,
        "the lifecycle update is measured separately: {log:?}"
    );

    // A commit failure and its retry still register nothing and leave the
    // lifecycle-only updates where they were.
    fixture.ingress.receiver.hooks().fail_next_commit();
    let record = next(soc(59));
    let logged_before = fixture.identity_log();
    assert_eq!(fixture.post(&record).await, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        fixture.identity_log(),
        logged_before,
        "a failed transaction leaves no committed trigger rows"
    );
    assert_eq!(fixture.post(&record).await, StatusCode::NO_CONTENT);

    let counts = fixture.counts();
    assert_eq!(
        counts.registration_calls(),
        0,
        "no registration call at all"
    );
    let log = fixture.identity_log();
    for forbidden in [
        "source:insert",
        "source:delete",
        "source:update",
        "vehicle:insert",
        "vehicle:delete",
        "alias:insert",
        "alias:delete",
        "vehicle:identity-change",
    ] {
        assert_eq!(log_count(&log, forbidden), 0, "{forbidden}: {log:?}");
    }
    // The one successful intake after the configuration record repeats the
    // lifecycle's own vehicle update (the failed one left nothing); no other
    // vehicle row write appears.
    assert_eq!(
        log_count(&log, "vehicle:metadata-update"),
        2 * lifecycle_updates,
        "one lifecycle update per successful intake: {log:?}"
    );
    assert_eq!(
        log_count(&log, "vehicle:equal-rewrite"),
        2 * lifecycle_rewrites,
        "{log:?}"
    );
}

#[tokio::test]
async fn receiver_registers_a_changed_identity_once() {
    let mut fixture = Fixture::new().await;
    // Remove a required alias: the receiver's source key for this vehicle.
    let removed = fixture
        .store
        .open()
        .unwrap()
        .execute(
            "DELETE FROM vehicle_identity_aliases
              WHERE vehicle_id = ?1 AND alias_kind = 'source_key'",
            [fixture.vehicle_id.to_string()],
        )
        .unwrap();
    assert_eq!(removed, 1);
    fixture.reset_counts();
    assert_eq!(
        fixture.post(&vehicle("r1", T0, soc(80))).await,
        StatusCode::NO_CONTENT
    );
    let counts = fixture.counts();
    assert_eq!(
        counts.source_registration_calls, 1,
        "one source registration"
    );
    assert_eq!(
        counts.vehicle_registration_calls, 1,
        "one vehicle registration"
    );
    // Registered again, the identity is unchanged from now on, even after restart.
    assert_eq!(
        fixture.post(&vehicle("r2", T0 + 1_000, soc(79))).await,
        StatusCode::NO_CONTENT
    );
    fixture.restart();
    assert_eq!(
        fixture.post(&vehicle("r3", T0 + 2_000, soc(78))).await,
        StatusCode::NO_CONTENT
    );
    let counts = fixture.counts();
    assert_eq!(counts.source_registration_calls, 1);
    assert_eq!(counts.vehicle_registration_calls, 1);
}

#[tokio::test]
async fn receiver_commit_failure_is_503_and_keeps_the_accumulator() {
    let mut fixture = Fixture::new().await;
    let a = vehicle("a", T0, soc(80));
    let b = vehicle(
        "b",
        T0 + 1_000,
        json!({"BatteryLevel": {"doubleValue": 70.0}}),
    );
    assert_eq!(fixture.post(&a).await, StatusCode::NO_CONTENT);
    let after_a = fixture.state_bytes().unwrap();
    let response_a = fixture.response().unwrap();

    fixture.ingress.receiver.hooks().fail_next_commit();
    assert_eq!(fixture.post(&b).await, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        fixture.state_bytes().unwrap(),
        after_a,
        "the stored state is unchanged"
    );
    assert_eq!(
        fixture.response().unwrap(),
        response_a,
        "nothing of the record committed"
    );

    // The retry applies the record to the prior accumulator and writes the state.
    assert_eq!(fixture.post(&b).await, StatusCode::NO_CONTENT);
    let mut expected = FleetTelemetryAccumulator::empty(VIN).unwrap();
    for record in [&a, &b] {
        expected
            .apply_json(&serde_json::to_vec(record).unwrap())
            .unwrap();
    }
    let expected = expected.encode_complete().unwrap();
    assert_eq!(fixture.state_bytes().unwrap(), expected);
    // The next process restores exactly that state.
    fixture.restart();
    assert_eq!(
        fixture.post(&vehicle("c", T0 + 2_000, soc(60))).await,
        StatusCode::NO_CONTENT
    );
    let response = fixture.response().unwrap();
    assert_eq!(response["charge_state"]["battery_level"], 70);
    assert_eq!(response["charge_state"]["usable_battery_level"], 60);

    // A failure of a first POST leaves no state row and the retry stores it.
    let fresh = Fixture::new().await;
    fresh.ingress.receiver.hooks().fail_next_commit();
    let first = vehicle("first", T0, soc(80));
    assert_eq!(fresh.post(&first).await, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(fresh.state_bytes(), None);
    assert!(fresh.current_rows("fleet_api_vehicle_data_v1").is_empty());
    assert_eq!(fresh.post(&first).await, StatusCode::NO_CONTENT);
    assert!(fresh.state_bytes().is_some());
}

#[tokio::test]
async fn receiver_stores_a_delayed_drive_record_that_arrives_after_a_newer_charge_record() {
    // The stored observation is ordered by the accumulator's newest event time. A
    // drive group whose own timestamp is older than an earlier snapshot's receipt
    // time must still reach the current row (an observation older than the current
    // row is never stored).
    let fixture = Fixture::new().await;
    assert_eq!(
        fixture.post(&vehicle("a", T0 + 5_000, soc(80))).await,
        StatusCode::NO_CONTENT
    );
    let delayed = vehicle(
        "b",
        T0 + 4_000,
        json!({
            "Location": {"locationValue": {"latitude": 51.5, "longitude": -0.12}},
            "VehicleSpeed": {"doubleValue": 48.0}
        }),
    );
    assert_eq!(fixture.post(&delayed).await, StatusCode::NO_CONTENT);
    let response = fixture.response().unwrap();
    assert_eq!(response["charge_state"]["usable_battery_level"], 80);
    assert_eq!(
        response["drive_state"]["speed"].as_f64(),
        Some(48.0),
        "the delayed drive fields reached the stored snapshot: {response}"
    );
    // Neither record's observation is older than the one before it.
    let current = fixture.current_rows("fleet_api_vehicle_data_v1");
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].1, T0 + 5_000, "ordered by the newest event time");
}

#[tokio::test]
async fn receiver_replay_after_a_lost_acknowledgement_stores_nothing_new() {
    let fixture = Fixture::new().await;
    let record = vehicle("lost-ack", T0, soc(80));
    assert_eq!(fixture.post(&record).await, StatusCode::NO_CONTENT);
    let current = fixture.current_rows("fleet_api_vehicle_data_v1");
    let state = fixture.state_bytes();
    let mutations = fixture.count("sync_mutations");
    let raw = fixture.count("raw_observations");
    assert_eq!(fixture.post(&record).await, StatusCode::NO_CONTENT);
    assert_eq!(fixture.current_rows("fleet_api_vehicle_data_v1"), current);
    assert_eq!(fixture.state_bytes(), state);
    assert_eq!(fixture.count("sync_mutations"), mutations);
    assert_eq!(fixture.count("raw_observations"), raw);
}

#[tokio::test]
async fn receiver_logs_carry_no_vin_payload_or_token() {
    let fixture = Fixture::new().await;
    crate::test_log_capture::arm();
    let secret_payload = "payload-marker-7c1e";
    let mut records = vec![
        vehicle("l1", T0, soc(80)),
        envelope(
            "l2",
            "alerts",
            T0 + 200,
            T0 + 300,
            json!({"name": secret_payload}),
        ),
    ];
    for record in &records {
        assert_eq!(fixture.post(record).await, StatusCode::NO_CONTENT);
    }
    // A failing commit and a refused record also log nothing identifying.
    fixture.ingress.receiver.hooks().fail_next_commit();
    records.push(vehicle("l3", T0 + 2_000, soc(79)));
    assert_eq!(
        fixture.post(&records[2]).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let mut unknown = vehicle("l4", T0, soc(80));
    unknown["vin"] = json!(OTHER_VIN);
    unknown["payload"]["vin"] = json!(OTHER_VIN);
    assert_eq!(
        fixture.post(&unknown).await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let log = crate::test_log_capture::disarm();
    assert!(
        log.contains("collector.receiver"),
        "the failure was logged: {log}"
    );
    for secret in [VIN, OTHER_VIN, secret_payload, fixture.token.as_str()] {
        assert!(!log.contains(secret), "{secret} must not be logged: {log}");
    }
    assert!(!log.contains(fixture._temporary.path().to_string_lossy().as_ref()));
}

// ---- startup: publication independent of collection ----

fn free_bind() -> SocketAddr {
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("reserve port");
    let bind = reservation.local_addr().expect("reserved address");
    drop(reservation);
    bind
}

#[tokio::test]
async fn collectorless_serve_drains_the_outbox_at_start() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let (admission, data) = admitted_server_fixture(&temporary);
    let store = HubStore::initialize(&data).expect("store");
    let key = crate::teslamate_credentials::load_or_create_cursor_key(&data).expect("cursor key");
    let vehicle_id = crate::collector::v2_base_with_pending_work(&store, &key, 9, VIN, None, T0);
    crate::collector::add_pending_observation(&store, 9, VIN, T0 + 60_000, "P", 0);
    let pending = |store: &HubStore| -> i64 {
        store
            .open()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1 AND published = 0",
                [vehicle_id.to_string()],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert!(pending(&store) > 0, "derived work is waiting");
    store.instrumentation().reset();

    let bind = free_bind();
    let config = local_plain_server_config(data.clone(), bind);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let serving = store.clone();
    let server = tokio::spawn(async move {
        serve_for_admitted_user(
            serving,
            &config,
            Sha256Digest::of_bytes(b"collector-less publication test"),
            admission,
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });
    wait_for_tcp_listener(bind).await;
    tokio::time::timeout(Duration::from_secs(30), async {
        while pending(&store) > 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the pending work is published after the Hub starts listening");
    assert_eq!(
        store.instrumentation().snapshot().publication_task_starts,
        1
    );

    shutdown_tx.send(()).expect("signal shutdown");
    server.await.expect("server task").expect("server shutdown");
    let counts = store.instrumentation().snapshot();
    assert_eq!(counts.publication_task_starts, 1);
    assert_eq!(
        counts.publication_task_exits, 1,
        "the task was joined with the server"
    );
}

#[tokio::test]
async fn serve_with_a_collector_supplied_key_starts_no_publication_task() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let (admission, data) = admitted_server_fixture(&temporary);
    let store = HubStore::initialize(&data).expect("store");
    let key = CursorKey::from_bytes([77; 32]);
    store.instrumentation().reset();
    let bind = free_bind();
    let config = local_plain_server_config(data.clone(), bind);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let serving = store.clone();
    let server = tokio::spawn(async move {
        serve_for_admitted_user(
            serving,
            &config,
            Sha256Digest::of_bytes(b"collector-owned replay test"),
            admission,
            Some(key),
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    });
    wait_for_tcp_listener(bind).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        store.instrumentation().snapshot().publication_task_starts,
        0
    );
    shutdown_tx.send(()).expect("signal shutdown");
    server.await.expect("server task").expect("server shutdown");
    assert_eq!(
        store.instrumentation().snapshot().publication_task_starts,
        0
    );
}

// ---- startup: the configured offline-drive timeout (R4) ----

/// A receiver route with a zero `offline_drive_timeout_seconds` would answer every
/// vehicle and connectivity POST with 503 for ever (the lifecycle refuses a zero
/// gap), so startup refuses it with a clear error before any listener opens.
#[tokio::test]
async fn receiver_refuses_a_zero_offline_drive_timeout_at_startup() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let (admission, data) = admitted_server_fixture(&temporary);
    let store = HubStore::initialize(&data).expect("store");
    let token_path = temporary.path().join("telemetry-token");
    fs::write(&token_path, format!("{}\n", "a".repeat(64))).expect("token");
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600)).expect("private token");
    let configured = |timeout_seconds: u64, bind: SocketAddr| {
        let mut config = local_plain_server_config(data.clone(), bind);
        config.collector.offline_drive_timeout_seconds = timeout_seconds;
        config.collector.fleet_telemetry = Some(FleetTelemetryConfig {
            hostname: "telemetry.example.test".to_owned(),
            port: 443,
            ca_certificate_path: temporary.path().join("telemetry-ca.pem"),
            ingest_token_path: token_path.clone(),
        });
        config
    };
    let serve = |config: crate::config::HubConfig| {
        let store = store.clone();
        let admission = Arc::clone(&admission);
        async move {
            serve_with_cursor_key(
                store,
                &config,
                Sha256Digest::of_bytes(b"zero offline-drive timeout test"),
                Some(CursorKey::from_bytes([61; 32])),
                Some(admission),
                // Shutdown is already requested: a server that did start stops at once.
                std::future::ready(()),
            )
            .await
        }
    };

    let refused = tokio::time::timeout(Duration::from_secs(30), serve(configured(0, free_bind())))
        .await
        .expect("a refused startup returns promptly")
        .expect_err("a zero offline_drive_timeout_seconds is refused");
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(
        refused.to_string(),
        "collector.offline_drive_timeout_seconds must be greater than zero when \
         [collector.fleet_telemetry] is configured"
    );

    // The same configuration with a positive timeout starts and stops cleanly.
    tokio::time::timeout(Duration::from_secs(30), serve(configured(600, free_bind())))
        .await
        .expect("a started server stops promptly")
        .expect("a positive timeout starts");

    // Without the receiver route a zero timeout is not this startup's concern.
    let mut without_route = configured(0, free_bind());
    without_route.collector.fleet_telemetry = None;
    tokio::time::timeout(Duration::from_secs(30), serve(without_route))
        .await
        .expect("a started server stops promptly")
        .expect("no receiver route, no refusal");
}
