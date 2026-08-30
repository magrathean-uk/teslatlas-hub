// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::{get, post},
};
use tokio::{net::TcpListener, task::JoinHandle};

use super::*;
use crate::credentials::{CredentialError, LegacyAuthManager};

const TEST_TOKEN: &str = "test-owner-token";
const TEST_VIN: &str = "5YJ3E1EA7KF000001";

#[derive(Clone, Default)]
struct FakeState {
    requests: Arc<Mutex<Vec<FakeRequest>>>,
    vehicles_body: Arc<Mutex<String>>,
    data_bodies: Arc<Mutex<BTreeMap<String, (StatusCode, String)>>>,
    action_bodies: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FakeRequest {
    method: String,
    path: String,
    query: String,
    authorization_is_expected: bool,
}

impl FakeState {
    fn with_vehicles(body: &str) -> Self {
        Self {
            vehicles_body: Arc::new(Mutex::new(body.to_owned())),
            ..Self::default()
        }
    }
}

#[test]
fn vehicle_products_require_a_valid_distinct_stream_id() {
    let product = |vehicle_id| ProductWire {
        vehicle_id,
        id: Some(serde_json::json!(9)),
        vin: Some(TEST_VIN.to_owned()),
        state: Some("online".to_owned()),
        display_name: None,
    };

    let vehicle = product(Some(serde_json::json!(42)))
        .into_vehicle()
        .expect("vehicle-shaped product")
        .expect("valid product");
    assert_eq!(vehicle.id.get(), 9);
    assert_eq!(vehicle.stream_id.get(), 42);

    let missing = product(None)
        .into_vehicle()
        .expect("vehicle-like product cannot be ignored")
        .expect_err("missing stream id must fail closed");
    assert_eq!(missing, OwnerApiError::InvalidVehicleRecord);

    for invalid in [
        serde_json::Value::Null,
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(""),
        serde_json::json!("not-a-number"),
        serde_json::json!(9_223_372_036_854_775_808_u64),
    ] {
        let error = product(Some(invalid))
            .into_vehicle()
            .expect("vehicle-shaped product")
            .expect_err("invalid stream id must fail closed");
        assert_eq!(error, OwnerApiError::InvalidVehicleRecord);
    }

    let energy_product = ProductWire {
        vehicle_id: None,
        id: None,
        vin: None,
        state: None,
        display_name: None,
    };
    assert!(energy_product.into_vehicle().is_none());
}

#[test]
fn vehicle_data_endpoint_preserves_provider_path_and_encodes_only_the_endpoint_query() {
    let base = Url::parse("http://127.0.0.1/owner-proxy/").expect("base URL");
    let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).expect("fake client");
    let endpoint = client
        .vehicle_data_endpoint(VehicleId(7))
        .expect("vehicle endpoint");

    assert_eq!(
        endpoint.path(),
        "/owner-proxy/api/1/vehicles/7/vehicle_data"
    );
    assert_eq!(
        endpoint.query(),
        Some(
            "endpoints=charge_state%3Bclimate_state%3Bclosures_state%3Bdrive_state%3Bgui_settings%3Blocation_data%3Bvehicle_config%3Bvehicle_state%3Bvehicle_data_combo"
        )
    );
    assert!(endpoint.username().is_empty());
    assert!(endpoint.password().is_none());
    assert!(!endpoint.query().unwrap().contains(TEST_TOKEN));
}

#[test]
fn production_base_requires_explicit_https_and_rejects_secret_bearing_forms() {
    for base in [
        "http://owner.example",
        "http://127.0.0.1:9/",
        "http://[::1]:9/",
    ] {
        assert!(matches!(
            OwnerApiBase::parse(base),
            Err(OwnerApiConfigError::HttpsRequired)
        ));
    }
    assert!(matches!(
        OwnerApiBase::parse("https://token@owner.example"),
        Err(OwnerApiConfigError::EmbeddedBaseCredential)
    ));
    assert!(matches!(
        OwnerApiBase::parse("https://owner.example/?token=bad"),
        Err(OwnerApiConfigError::BaseParametersNotPermitted)
    ));
    let base = OwnerApiBase::parse("https://owner.example/api").expect("https base");
    assert_eq!(base.url.as_str(), "https://owner.example/api/");
    assert!(matches!(
        OwnerApi::new(OwnerApiOptions::new(base, Duration::ZERO)),
        Err(OwnerApiConfigError::ZeroTimeout)
    ));
}

#[test]
fn loopback_http_owner_api_base_is_accepted_for_local_fake_source() {
    let base =
        OwnerApiBase::parse_loopback_http_for_test("http://127.0.0.1:9/").expect("loopback http");
    assert!(base.is_loopback_http());
    assert_eq!(base.as_str(), "http://127.0.0.1:9/");
    assert!(matches!(
        OwnerApi::new(OwnerApiOptions::new(base, Duration::from_secs(1))),
        Err(OwnerApiConfigError::HttpsRequired)
    ));
    assert!(matches!(
        OwnerApiBase::parse_loopback_http_for_test("http://localhost:9/owner/"),
        Err(OwnerApiConfigError::HttpsRequired)
    ));
    assert!(matches!(
        OwnerApiBase::parse_loopback_http_for_test("http://192.168.1.2/"),
        Err(OwnerApiConfigError::HttpsRequired)
    ));
    assert!(OwnerApiBase::parse_loopback_http_for_test("http://[::1]:9/").is_ok());
    for base in [
        "http://token@127.0.0.1/",
        "http://127.0.0.1/?token=bad",
        "http://127.0.0.1/#token",
        "http://localhost:9/",
    ] {
        assert!(OwnerApiBase::parse_loopback_http_for_test(base).is_err());
    }
}

#[test]
fn owner_api_errors_and_debug_redact_bearer_values() {
    let error = OwnerApiBase::parse("https://access-secret@owner.example/")
        .expect_err("credential-bearing base rejected");
    assert!(!format!("{error}").contains("access-secret"));
    assert!(!format!("{error:?}").contains("access-secret"));
    let base = OwnerApiBase::parse("https://owner.example/access-secret/refresh-secret/").unwrap();
    let options = OwnerApiOptions::new(base.clone(), Duration::from_secs(1));
    let client = OwnerApi::new(options.clone()).expect("HTTPS client");
    for rendered in [
        format!("{base:?}"),
        format!("{options:?}"),
        format!("{client:?}"),
    ] {
        assert!(!rendered.contains("access-secret"));
        assert!(!rendered.contains("refresh-secret"));
    }
}

#[test]
fn provider_payload_scrubber_removes_nested_token_key_variants() {
    let mut value = serde_json::json!({
        "accessToken": "one",
        "nested": [{
            "refresh-token": "two",
            "Backseat_Token": "three",
            "xAuthorization": "four",
            "token_type": "Bearer",
            "battery_level": 80
        }],
        "state": "online"
    });
    scrub_sensitive_value(&mut value);

    let rendered = value.to_string();
    for secret in ["one", "two", "three", "four"] {
        assert!(!rendered.contains(secret));
    }
    assert_eq!(value["nested"][0]["token_type"], "Bearer");
    assert_eq!(value["nested"][0]["battery_level"], 80);
    assert_eq!(value["state"], "online");
}

#[test]
fn provider_payload_scrubber_removes_credential_key_variants_without_losing_telemetry() {
    let mut value = serde_json::json!({
        "password": "password-secret",
        "api-key": "api-key-secret",
        "cookie": "cookie-secret",
        "session_secret": "session-secret",
        "nested": [{
            "PassWord": "nested-password-secret",
            "x_api_key": "nested-api-key-secret",
            "set-cookie": "nested-cookie-secret",
            "session-secret": "nested-session-secret",
            "cookie_status": "present",
            "battery_level": 80
        }],
        "token_type": "Bearer",
        "state": "online"
    });
    scrub_sensitive_value(&mut value);

    let rendered = value.to_string();
    for secret in [
        "password-secret",
        "api-key-secret",
        "cookie-secret",
        "session-secret",
        "nested-password-secret",
        "nested-api-key-secret",
        "nested-cookie-secret",
        "nested-session-secret",
    ] {
        assert!(!rendered.contains(secret), "secret survived: {secret}");
    }
    assert_eq!(value["nested"][0]["cookie_status"], "present");
    assert_eq!(value["nested"][0]["battery_level"], 80);
    assert_eq!(value["token_type"], "Bearer");
    assert_eq!(value["state"], "online");
}

#[test]
fn vehicle_data_retains_only_allowlisted_provider_json_v1_telemetry() {
    let data = parse_vehicle_data(
        VehicleId(7),
        serde_json::json!({
            "response": {
                "vehicle_id": 7,
                "vin": "5YJ3E1EA7KF000001",
                "state": "online",
                "drive_state": {"shift_state": "P", "timestamp": 1_800_000_000_000_i64},
                "charge_state": {"battery_level": 80},
                "climate_state": {"inside_temp": 21.5},
                "vehicle_state": {
                    "odometer": 1234.5,
                    "future_session_credential": "future-secret",
                    "software_update": {
                        "status": "downloading",
                        "version": "2026.20",
                        "download_perc": 42,
                        "install_perc": 0,
                        "expected_duration_sec": 900
                    }
                },
                "vehicle_config": {
                    "car_type": "model3",
                    "future_blob": {"credential": "nested-secret"}
                },
                "gui_settings": {"gui_distance_units": "mi/hr"},
                "unknown_group": {"battery_level": 1},
                "display_name": ["not", "scalar"],
                "car_type": "x".repeat(MAX_PROVIDER_TELEMETRY_TEXT_BYTES + 1)
            },
            "provider_trace": "trace-1",
            "provider_future_secret": "root-secret"
        }),
    )
    .expect("provider response parses");

    assert_eq!(data.fields()["charge_state"]["battery_level"], 80);
    assert_eq!(
        data.provider_raw_json(),
        &serde_json::json!({
            "response": {
                "vehicle_id": 7,
                "vin": "5YJ3E1EA7KF000001",
                "state": "online",
                "drive_state": {"shift_state": "P", "timestamp": 1_800_000_000_000_i64},
                "charge_state": {"battery_level": 80},
                "climate_state": {"inside_temp": 21.5},
                "vehicle_state": {
                    "odometer": 1234.5,
                    "software_update": {
                        "status": "downloading",
                        "version": "2026.20",
                        "download_perc": 42,
                        "install_perc": 0
                    }
                },
                "vehicle_config": {"car_type": "model3"},
                "gui_settings": {"gui_distance_units": "mi/hr"}
            }
        })
    );
    let rendered = data.provider_raw_json().to_string();
    for rejected in [
        "provider_trace",
        "unknown_group",
        "future_session_credential",
        "expected_duration_sec",
        "future-secret",
        "root-secret",
        "nested-secret",
    ] {
        assert!(!rendered.contains(rejected), "field survived: {rejected}");
    }

    assert!(matches!(
        VehicleData::from_provider_raw_json(
            VehicleId(7),
            serde_json::json!({"response": {"unknown_group": {"future_secret": "x"}}}),
        ),
        Err(OwnerApiError::SensitiveDataInResponse)
    ));
    assert!(matches!(
        VehicleData::from_provider_raw_json(
            VehicleId(7),
            serde_json::json!({"count": 1, "response": {"state": "online"}}),
        ),
        Err(OwnerApiError::InvalidVehicleDataEnvelope)
    ));
}

struct FakeServer {
    base_url: Url,
    _task: JoinHandle<()>,
}

impl FakeServer {
    async fn spawn(state: FakeState) -> Self {
        Self::start(
            Router::new()
                .route("/api/1/products", get(list_handler))
                .route(
                    "/api/1/vehicles/{vehicle_id}/vehicle_data",
                    get(data_handler),
                )
                .with_state(state),
        )
        .await
    }

    async fn start(router: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake listener");
        let address = listener.local_addr().expect("fake address");
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("fake server runs");
        });
        Self {
            base_url: Url::parse(&format!("http://{address}/")).expect("fake URL"),
            _task: task,
        }
    }

    fn client(&self, timeout: Duration) -> OwnerApi {
        OwnerApi::for_fake_http(self.base_url.clone(), timeout).expect("fake client")
    }
}

fn guarded_legacy_manager(issuer: Url, admitted: Arc<AtomicBool>) -> LegacyAuthManager {
    let auth = crate::legacy_auth::LegacyAuth::for_test(issuer, TEST_TOKEN, "test-refresh-token")
        .with_test_schedule(2_000_000_000, 1_900_000_000);
    LegacyAuthManager::for_test_with_sensitive_access(
        auth,
        Arc::new(|_, _| Ok(())),
        Arc::new(move || {
            admitted
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or(CredentialError::MacKeychainHelperInvalid)
        }),
    )
}

#[tokio::test]
async fn native_setup_discovery_is_one_bounded_products_request() {
    let state = FakeState::with_vehicles(
        r#"{"response":[{"vehicle_id":71,"id":70,"vin":"5YJ3E1EA7KF000001","state":"asleep","display_name":"Athena"}],"count":1}"#,
    );
    let fake = FakeServer::spawn(state.clone()).await;
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        fake.base_url.clone(),
        TEST_TOKEN,
        "test-refresh-token",
    );

    let vehicles = fake
        .client(Duration::from_secs(2))
        .list_vehicles_with_legacy_auth_once(&auth)
        .await
        .expect("native discovery");

    assert_eq!(vehicles.len(), 1);
    assert_eq!(vehicles[0].id.get(), 70);
    let requests = state.requests.lock().expect("requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/api/1/products");
    assert!(requests[0].authorization_is_expected);
}

#[tokio::test]
async fn explicit_vehicle_actions_use_exact_paths_bodies_and_one_request_each() {
    let state = FakeState::default();
    let fake = FakeServer::start(
        Router::new()
            .route("/api/1/vehicles/{vehicle_id}/wake_up", post(wake_handler))
            .route(
                "/api/1/vehicles/{vehicle_id}/command/{command}",
                post(command_handler),
            )
            .with_state(state.clone()),
    )
    .await;
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        fake.base_url.clone(),
        TEST_TOKEN,
        "test-refresh-token",
    );
    let client = fake.client(Duration::from_secs(2));

    let wake = client
        .execute_vehicle_action_once(&auth, VehicleId(70), LegacyVehicleAction::Wake)
        .await
        .expect("wake response");
    assert_eq!(wake.state.as_deref(), Some("online"));
    client
        .execute_vehicle_action_once(
            &auth,
            VehicleId(70),
            LegacyVehicleAction::SetChargeLimit(80),
        )
        .await
        .expect("set charge limit response");
    let mut manager = crate::credentials::LegacyAuthManager::for_test(
        crate::legacy_auth::LegacyAuth::for_test(
            fake.base_url.clone(),
            TEST_TOKEN,
            "test-refresh-token",
        ),
        Arc::new(|_, _| Ok(())),
    );
    let mut fuse = LegacyAuthFuse::default();
    client
        .execute_vehicle_action_with_legacy_auth_fused(
            &mut manager,
            &mut fuse,
            VehicleId(70),
            LegacyVehicleAction::ClimateStart,
        )
        .await
        .expect("resident command response");
    assert!(matches!(
        client
            .execute_vehicle_action_once(
                &auth,
                VehicleId(70),
                LegacyVehicleAction::SetChargeLimit(49),
            )
            .await,
        Err(OwnerApiError::InvalidCommand)
    ));

    let requests = state.requests.lock().expect("requests");
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/1/vehicles/70/wake_up");
    assert_eq!(
        requests[1].path,
        "/api/1/vehicles/70/command/set_charge_limit"
    );
    assert_eq!(
        requests[2].path,
        "/api/1/vehicles/70/command/auto_conditioning_start"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.authorization_is_expected)
    );
    drop(requests);
    assert_eq!(
        *state.action_bodies.lock().expect("action bodies"),
        vec![json!({}), json!({"percent": 80}), json!({})]
    );
}

#[tokio::test]
async fn missing_or_stale_sensitive_admission_blocks_before_owner_transport() {
    let state = FakeState::with_vehicles(r#"{"response":[],"count":0}"#);
    let fake = FakeServer::spawn(state.clone()).await;
    let admitted = Arc::new(AtomicBool::new(false));
    let mut manager = guarded_legacy_manager(fake.base_url.clone(), Arc::clone(&admitted));
    let mut fuse = LegacyAuthFuse::default();

    let error = fake
        .client(Duration::from_secs(2))
        .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
        .await
        .expect_err("missing admission must fence transport");
    assert!(matches!(
        error,
        OwnerApiAuthError::Auth(LegacyAuthManagerError::Credential(
            CredentialError::MacKeychainHelperInvalid
        ))
    ));
    assert!(state.requests.lock().expect("requests").is_empty());

    admitted.store(true, Ordering::Release);
    fake.client(Duration::from_secs(2))
        .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
        .await
        .expect("live admission permits request");
    assert_eq!(state.requests.lock().expect("requests").len(), 1);

    admitted.store(false, Ordering::Release);
    fake.client(Duration::from_secs(2))
        .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
        .await
        .expect_err("stale admission must fence transport");
    assert_eq!(state.requests.lock().expect("requests").len(), 1);
}
async fn list_handler(State(state): State<FakeState>, headers: HeaderMap) -> impl IntoResponse {
    record(&state, &headers, "/api/1/products");
    state.vehicles_body.lock().expect("fake list lock").clone()
}

async fn data_handler(
    State(state): State<FakeState>,
    AxumPath(vehicle_id): AxumPath<String>,
    headers: HeaderMap,
    uri: Uri,
) -> impl IntoResponse {
    let query = uri.query().unwrap_or_default();
    record_with_query(
        &state,
        &headers,
        &format!("/api/1/vehicles/{vehicle_id}/vehicle_data"),
        query,
    );
    if query
        != "endpoints=charge_state%3Bclimate_state%3Bclosures_state%3Bdrive_state%3Bgui_settings%3Blocation_data%3Bvehicle_config%3Bvehicle_state%3Bvehicle_data_combo"
    {
        return (
            StatusCode::BAD_REQUEST,
            r#"{"error":"vehicle_data endpoints query mismatch"}"#,
        )
            .into_response();
    }
    let response = state
        .data_bodies
        .lock()
        .expect("fake data lock")
        .get(&vehicle_id)
        .cloned()
        .unwrap_or((StatusCode::NOT_FOUND, r#"{"error":"not_found"}"#.to_owned()));
    response.into_response()
}

async fn wake_handler(
    State(state): State<FakeState>,
    AxumPath(vehicle_id): AxumPath<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    record_with_method(
        &state,
        &headers,
        &format!("/api/1/vehicles/{vehicle_id}/wake_up"),
        "",
        "POST",
    );
    state
        .action_bodies
        .lock()
        .expect("action body lock")
        .push(body);
    Json(json!({"response": {"state": "online", "id": 70}}))
}

async fn command_handler(
    State(state): State<FakeState>,
    AxumPath((vehicle_id, command)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    record_with_method(
        &state,
        &headers,
        &format!("/api/1/vehicles/{vehicle_id}/command/{command}"),
        "",
        "POST",
    );
    state
        .action_bodies
        .lock()
        .expect("action body lock")
        .push(body);
    Json(json!({"response": {"result": true, "reason": ""}}))
}

fn record(state: &FakeState, headers: &HeaderMap, path: &str) {
    record_with_query(state, headers, path, "");
}

fn record_with_query(state: &FakeState, headers: &HeaderMap, path: &str, query: &str) {
    record_with_method(state, headers, path, query, "GET");
}

fn record_with_method(
    state: &FakeState,
    headers: &HeaderMap,
    path: &str,
    query: &str,
    method: &str,
) {
    let authorization_is_expected = headers
        .get("authorization")
        .is_some_and(|value| value.as_bytes() == b"Bearer test-owner-token");
    state
        .requests
        .lock()
        .expect("fake request lock")
        .push(FakeRequest {
            method: method.to_owned(),
            path: path.to_owned(),
            query: query.to_owned(),
            authorization_is_expected,
        });
}

#[test]
fn retry_after_is_integer_exact_and_safe_on_bad_input() {
    let mut headers = HeaderMap::new();
    headers.insert("retry-after", HeaderValue::from_static("17"));
    assert_eq!(parse_retry_after(&headers), 17);

    headers.insert("retry-after", HeaderValue::from_static("bad"));
    assert_eq!(parse_retry_after(&headers), 300);
    headers.remove("retry-after");
    assert_eq!(parse_retry_after(&headers), 300);
}

#[test]
fn exact_exceeded_limit_and_not_found_bodies_are_typed() {
    assert!(is_owner_error_body(
        br#"{"error":"account disabled: EXCEEDED_LIMIT"}"#,
        "account disabled: EXCEEDED_LIMIT"
    ));
    assert!(is_owner_error_body(
        br#"{"error":"not_found"}"#,
        "not_found"
    ));
    assert!(!is_owner_error_body(br#"{"error":"other"}"#, "not_found"));
}
