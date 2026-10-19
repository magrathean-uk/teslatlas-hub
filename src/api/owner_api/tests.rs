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
/// The exact `vehicle_data` query of C3 R23 (CT7).
const CT7_EXPECTED_QUERY: &str = "endpoints=charge_state%3Bclimate_state%3Bdrive_state%3Blocation_data%3Bvehicle_config%3Bvehicle_state";

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
    // CT7: the endpoint groups of C3 R23, alphabetical, with the semicolons
    // encoded as %3B and nothing else in the query.
    assert_eq!(
        endpoint.query(),
        Some(
            "endpoints=charge_state%3Bclimate_state%3Bdrive_state%3Blocation_data%3Bvehicle_config%3Bvehicle_state"
        )
    );
    assert!(endpoint.username().is_empty());
    assert!(endpoint.password().is_none());
    assert!(!endpoint.query().unwrap().contains(TEST_TOKEN));
}

#[test]
fn ct59_the_allow_list_keeps_every_field_the_phase_and_sleep_rules_read() {
    // Every group C3 R23 requests, with the fields R15 and R29 read.
    let body = serde_json::json!({
        "response": {
            "vehicle_id": 7,
            "vin": TEST_VIN,
            "state": "online",
            "drive_state": {
                "shift_state": "D",
                "speed": 42,
                "power": 11,
                "latitude": 51.5,
                "longitude": -0.12,
                "timestamp": 1_800_000_000_123_i64
            },
            "charge_state": {
                "charging_state": "Charging",
                "battery_level": 63,
                "timestamp": 1_800_000_001_456_i64
            },
            "climate_state": {
                "is_climate_on": true,
                "climate_keeper_mode": "dog",
                "inside_temp": 21.5
            },
            "vehicle_state": {
                "odometer": 1234.5,
                "is_user_present": true,
                "remote_start": true,
                "locked": false,
                "sentry_mode": true,
                "service_mode": false,
                "software_update": {"status": "installing", "version": "2026.20"}
            },
            "vehicle_config": {"car_type": "model3", "trim_badging": "74d"}
        }
    });
    let filtered = allowlisted_provider_response_v1(&body).expect("allow-listed response");
    let value = |group: &str, field: &str| filtered[group][field].clone();

    assert_eq!(value("drive_state", "shift_state"), "D");
    assert_eq!(value("drive_state", "speed"), 42);
    assert_eq!(value("drive_state", "timestamp"), 1_800_000_000_123_i64);
    assert_eq!(value("charge_state", "charging_state"), "Charging");
    assert_eq!(value("charge_state", "timestamp"), 1_800_000_001_456_i64);
    assert_eq!(value("climate_state", "is_climate_on"), true);
    assert_eq!(value("climate_state", "climate_keeper_mode"), "dog");
    assert_eq!(value("vehicle_state", "is_user_present"), true);
    assert_eq!(value("vehicle_state", "remote_start"), true);
    assert_eq!(value("vehicle_state", "locked"), false);
    assert_eq!(value("vehicle_state", "sentry_mode"), true);
    assert_eq!(value("vehicle_state", "service_mode"), false);
    assert_eq!(
        filtered["vehicle_state"]["software_update"]["status"],
        "installing"
    );
    // The location and model metadata the data model needs survive as well.
    assert_eq!(value("drive_state", "latitude"), 51.5);
    assert_eq!(value("drive_state", "longitude"), -0.12);
    assert_eq!(value("vehicle_config", "car_type"), "model3");
    assert_eq!(value("vehicle_config", "trim_badging"), "74d");
}

/// 06 Nov 1994 08:49:37 GMT, the example instant of RFC 9110 section 5.6.7.
const RFC_EXAMPLE_EPOCH_SECONDS: u64 = 784_111_777;

fn headers_of(pairs: &[(&'static str, &str)]) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in pairs {
        headers.insert(
            reqwest::header::HeaderName::from_static(name),
            reqwest::header::HeaderValue::from_str(value).expect("header value"),
        );
    }
    headers
}

#[test]
fn retry_after_reads_delay_seconds_and_clamps_to_one_hour() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(RFC_EXAMPLE_EPOCH_SECONDS);
    let wait = |pairs: &[(&'static str, &str)]| retry_after_seconds_at(&headers_of(pairs), now);
    assert_eq!(wait(&[("retry-after", "17")]), 17);
    assert_eq!(wait(&[("retry-after", " 120 ")]), 120);
    assert_eq!(wait(&[("retry-after", "3600")]), 3_600);
    assert_eq!(
        wait(&[("retry-after", "3601")]),
        3_600,
        "clamped to one hour"
    );
    assert_eq!(wait(&[("retry-after", "18446744073709551615")]), 3_600);
    // Zero means "absent": an immediate retry lands in the same window.
    assert_eq!(wait(&[("retry-after", "0")]), 0);
    assert_eq!(wait(&[]), 0);
    // Not delay-seconds and not a date, or a value beyond u64: invalid.
    for invalid in ["soon", "-5", "1.5", "", "18446744073709551616", "12abc"] {
        assert_eq!(wait(&[("retry-after", invalid)]), 0, "{invalid:?}");
    }
}

#[test]
fn retry_after_falls_back_to_the_rate_limit_reset_header_when_absent_or_invalid() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(RFC_EXAMPLE_EPOCH_SECONDS);
    let wait = |pairs: &[(&'static str, &str)]| retry_after_seconds_at(&headers_of(pairs), now);
    assert_eq!(wait(&[("ratelimit-reset", "45")]), 45);
    assert_eq!(
        wait(&[("retry-after", "invalid"), ("ratelimit-reset", "45")]),
        45
    );
    assert_eq!(
        wait(&[("retry-after", "30"), ("ratelimit-reset", "45")]),
        30
    );
    // A valid Retry-After of 0 is a valid value, so it does not fall through.
    assert_eq!(wait(&[("retry-after", "0"), ("ratelimit-reset", "45")]), 0);
    // `RateLimit-Reset` is read only as delay-seconds, never as a date.
    assert_eq!(
        wait(&[("ratelimit-reset", "Sun, 06 Nov 1994 08:50:37 GMT")]),
        0
    );
    assert_eq!(wait(&[("ratelimit-reset", "999999")]), 3_600);
}

#[test]
fn retry_after_reads_the_three_http_date_forms_and_rounds_up() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(RFC_EXAMPLE_EPOCH_SECONDS);
    let wait_at = |value: &str, at: SystemTime| {
        retry_after_seconds_at(&headers_of(&[("retry-after", value)]), at)
    };
    // IMF-fixdate, rfc850-date and asctime-date for later instants.
    assert_eq!(wait_at("Sun, 06 Nov 1994 08:49:57 GMT", now), 20);
    assert_eq!(wait_at("Sunday, 06-Nov-94 08:50:37 GMT", now), 60);
    assert_eq!(wait_at("Sun Nov  6 08:51:37 1994", now), 120);
    assert_eq!(
        wait_at("Sun Nov 16 08:49:37 1994", now),
        3_600,
        "ten days, clamped"
    );
    // Whole seconds, rounded up.
    let half_second_later = now + Duration::from_millis(500);
    assert_eq!(
        wait_at("Sun, 06 Nov 1994 08:49:38 GMT", half_second_later),
        1
    );
    assert_eq!(wait_at("Sun, 06 Nov 1994 08:49:38 GMT", now), 1);
    // A date that is not in the future is "absent".
    assert_eq!(wait_at("Sun, 06 Nov 1994 08:49:37 GMT", now), 0);
    assert_eq!(wait_at("Sun, 06 Nov 1994 08:49:36 GMT", now), 0);
    // Impossible dates, wrong shapes and an unknown weekday are invalid.
    for invalid in [
        "Sun, 31 Nov 1994 08:49:57 GMT",
        "Sun, 06 Nov 1994 24:00:00 GMT",
        "Sun, 06 Nov 1994 08:49:57 UTC",
        "Sun, 6 Nov 1994 08:49:57 GMT",
        "Fun, 06 Nov 1994 08:49:57 GMT",
        "06 Nov 1994 08:49:57 GMT",
    ] {
        assert_eq!(wait_at(invalid, now), 0, "{invalid:?}");
    }
    // 29 February exists in a leap year only.
    let leap = SystemTime::UNIX_EPOCH + Duration::from_secs(1_709_164_800 - 10);
    assert_eq!(wait_at("Thu, 29 Feb 2024 00:00:00 GMT", leap), 10);
    assert_eq!(wait_at("Thu, 29 Feb 2023 00:00:00 GMT", leap), 0);
}

#[test]
fn retry_after_reads_a_two_digit_rfc850_year_by_the_fifty_year_rule() {
    // 2027-01-15 08:00:00 UTC.
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let wait = |value: &str| retry_after_seconds_at(&headers_of(&[("retry-after", value)]), now);
    // 2099 would be 72 years ahead, so it is 1999: in the past.
    assert_eq!(wait("Friday, 01-Jan-99 00:00:00 GMT"), 0);
    // 2060 is 33 years ahead, so it stays 2060: far in the future, clamped.
    assert_eq!(wait("Friday, 01-Jan-60 00:00:00 GMT"), 3_600);
    // The current year is read in the current century.
    assert_eq!(wait("Friday, 15-Jan-27 08:00:30 GMT"), 30);
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
        &ProviderSecrets::none(),
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
            &ProviderSecrets::none(),
        ),
        Err(OwnerApiError::SensitiveDataInResponse)
    ));
    assert!(matches!(
        VehicleData::from_provider_raw_json(
            VehicleId(7),
            serde_json::json!({"count": 1, "response": {"state": "online"}}),
            &ProviderSecrets::none(),
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
    // CT7: a client that asks for any other group list is refused, so every
    // test that reads vehicle data through this fake proves the query.
    if query != CT7_EXPECTED_QUERY {
        return (
            StatusCode::BAD_REQUEST,
            r#"{"error":"bad_request"}"#.to_owned(),
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

// Bearer reflection (csf_2a9d): a provider response that echoes a live
// credential into an allowlisted field must not reach VehicleData, storage,
// the current state or a log.

use provider_secrets::capture::LogCapture;

const LONG_REFLECTED_ACCESS: &str = TEST_TOKEN;
const LONG_REFLECTED_REFRESH: &str = "test-refresh-token";
const SHORT_ACCESS: &str = "short-access";
const SHORT_REFRESH: &str = "short-refresh";

fn reflecting_vehicle_data(access: &str, refresh: &str) -> String {
    json!({"response": {
        "id": 7,
        "vehicle_id": 7,
        "vin": TEST_VIN,
        "state": "online",
        "display_name": access,
        "drive_state": {"shift_state": "P", "timestamp": 1_800_000_000_000_i64},
        "charge_state": {"battery_level": 80, "charging_state": "Charging"},
        "vehicle_state": {
            "car_version": format!("build {refresh} end"),
            "vehicle_name": format!("Bearer {access}"),
            "odometer": 1234.5,
            "software_update": {
                "status": "available",
                "version": format!(" {refresh}\n")
            }
        },
        "vehicle_config": {"car_type": "model3"}
    }})
    .to_string()
}

fn short_secret_manager(issuer: Url) -> LegacyAuthManager {
    let auth = crate::legacy_auth::LegacyAuth::for_test(issuer, SHORT_ACCESS, SHORT_REFRESH)
        .with_test_schedule(2_000_000_000, 1_900_000_000);
    LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())))
}

/// Short tokens that carry a space of their own: validation accepts them
/// unchanged, so an echo keeps the space and trimming the echo drops it.
const PADDED_ACCESS: &str = "pad-access ";
const PADDED_REFRESH: &str = " pad-refresh";

fn padded_secret_manager(issuer: Url) -> LegacyAuthManager {
    let auth = crate::legacy_auth::LegacyAuth::for_test(issuer, PADDED_ACCESS, PADDED_REFRESH)
        .with_test_schedule(2_000_000_000, 1_900_000_000);
    LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())))
}

fn assert_no_credential_bytes(rendered: &str, secrets: &[&str]) {
    for secret in secrets {
        assert!(!rendered.contains(secret), "credential bytes survived");
    }
}

#[test]
fn provider_secret_matching_is_exact_for_short_secrets_and_contains_for_long_ones() {
    let long = "long-access-0123456789abcdef";
    let short = "short-token";
    let secrets = ProviderSecrets::new(&[long, short, ""]);

    for exact in [long, short] {
        assert!(secrets.reflects(exact), "exact {exact}");
    }
    for padded in [
        " short-token",
        "short-token\n",
        "\t short-token \r\n",
        "\u{c}short-token",
    ] {
        assert!(secrets.reflects(padded), "padded {padded:?}");
    }
    for bearer in [
        "Bearer short-token",
        "bearer short-token",
        "BEARER\tshort-token",
        "  Bearer   short-token  \n",
        "Bearer long-access-0123456789abcdef",
    ] {
        assert!(secrets.reflects(bearer), "bearer {bearer:?}");
    }
    for contained in [
        "car long-access-0123456789abcdef!",
        "Bearer: long-access-0123456789abcdef.",
        "xlong-access-0123456789abcdef",
    ] {
        assert!(secrets.reflects(contained), "contained {contained:?}");
    }

    // A short secret is not searched for inside longer text, and the scheme
    // needs a separator before the token.
    for unrelated in [
        "my short-token car",
        "short-tokens",
        "Bearershort-token",
        "Bearer short-token and more",
        "Model 3",
        "",
        "Bearer",
        "Bearer ",
        "long-access-0123456789abcde",
    ] {
        assert!(!secrets.reflects(unrelated), "unrelated {unrelated:?}");
    }

    // An empty secret is dropped, so it never matches an empty string, and an
    // empty set is a no-op.
    assert!(!ProviderSecrets::new(&[""]).reflects(""));
    assert!(!ProviderSecrets::none().reflects("anything at all"));
    assert!(!ProviderSecrets::none().reflects(long));
}

#[test]
fn provider_secret_that_starts_with_the_scheme_matches_after_trimming() {
    // The trimmed text is compared as it stands, not only after a leading
    // `Bearer` is removed, so a secret that itself begins with the scheme is
    // still recognised when whitespace pads it. It is short, so equality is
    // the only way it can match.
    let secrets = ProviderSecrets::new(&["Bearer odd"]);
    for reflected in ["Bearer odd", "  Bearer odd \n", "\tBearer odd"] {
        assert!(secrets.reflects(reflected), "reflected {reflected:?}");
    }
    for unrelated in ["odd", "Bearer odd and more", "Bearer  other"] {
        assert!(!secrets.reflects(unrelated), "unrelated {unrelated:?}");
    }
}

#[test]
fn provider_secret_with_padding_of_its_own_matches_with_or_without_the_padding() {
    // Legacy credential validation accepts a token that begins or ends with a
    // space and keeps it as given, so an echo of the secret can carry that
    // space: trimming only the echo would never equal the secret.
    let secrets = ProviderSecrets::new(&["trail-pad ", " lead-pad", "  both-pad  "]);
    for reflected in [
        "trail-pad ",
        " lead-pad",
        "  both-pad  ",
        "trail-pad",
        "lead-pad",
        "both-pad",
        " trail-pad \n",
        "\tlead-pad ",
        "\n both-pad\r\n",
        "Bearer trail-pad ",
        "Bearer  lead-pad",
        "bearer   both-pad  ",
        "Bearer trail-pad",
        "BEARER\tlead-pad\n",
    ] {
        assert!(secrets.reflects(reflected), "reflected {reflected:?}");
    }
    for unrelated in [
        "trail pad",
        "trail-pad2",
        "xlead-pad",
        "both-pa",
        "trail-pad and more",
        "Bearer other-pad ",
        "Bearer",
        "Bearer ",
        " ",
        "",
    ] {
        assert!(!secrets.reflects(unrelated), "unrelated {unrelated:?}");
    }

    // A padded secret that itself begins with the scheme word is compared as
    // it stands, not only after a leading `Bearer` is removed.
    let secrets = ProviderSecrets::new(&["Bearer odd "]);
    for reflected in ["Bearer odd ", "Bearer odd", "  Bearer odd \n"] {
        assert!(secrets.reflects(reflected), "reflected {reflected:?}");
    }
    for unrelated in ["odd", "Bearer odd and more"] {
        assert!(!secrets.reflects(unrelated), "unrelated {unrelated:?}");
    }

    // The 16-byte floor counts the credential as given, padding included: a
    // 16-byte padded credential embedded verbatim in longer text is a
    // reflection, although its trimmed form is under the floor. Equality is a
    // separate check and still ignores the padding.
    assert_eq!("abcdefghijklmno ".len(), 16);
    let padded = ProviderSecrets::new(&["abcdefghijklmno "]);
    for reflected in [
        "x abcdefghijklmno y",
        "note: abcdefghijklmno , more",
        "abcdefghijklmno ",
        "abcdefghijklmno",
        "  abcdefghijklmno\n",
        "Bearer abcdefghijklmno",
    ] {
        assert!(padded.reflects(reflected), "reflected {reflected:?}");
    }
    let leading = ProviderSecrets::new(&[" abcdefghijklmno"]);
    assert!(leading.reflects("x abcdefghijklmno y"));
    // The padding does not matter to the floor the other way round either: a
    // 15-byte padded credential is under it however it is echoed in text.
    assert_eq!("abcdefghijklmn ".len(), 15);
    let short = ProviderSecrets::new(&["abcdefghijklmn "]);
    assert!(short.reflects("abcdefghijklmn"));
    assert!(!short.reflects("x abcdefghijklmn y"));
    // A long credential is found inside longer text whether or not the echo
    // kept the padding.
    assert_eq!("abcdefghijklmnop ".len(), 17);
    let secrets = ProviderSecrets::new(&["abcdefghijklmnop "]);
    assert!(secrets.reflects("x abcdefghijklmnop y"));
    assert!(secrets.reflects("x abcdefghijklmnop, y"));
}

#[test]
fn trimmed_form_under_the_floor_is_never_searched_inside_text() {
    // 16 bytes as given, 15 once the padding is trimmed: the original bytes
    // reach the floor, the trimmed form does not. Only the original bytes may
    // be searched for inside longer text.
    for original in ["abcdefghijklmno ", " abcdefghijklmno", "   abcdefghijklmno"] {
        assert!(original.len() >= 16, "{original:?}");
        assert_eq!(original.trim_ascii(), "abcdefghijklmno");
        assert_eq!("abcdefghijklmno".len(), 15);
        let secrets = ProviderSecrets::new(&[original]);

        // Text that holds only the trimmed form, with other characters (never
        // the padding the credential carries) on either side of it: not a
        // reflection, so nothing is scrubbed.
        for unrelated in [
            "xabcdefghijklmno",
            "abcdefghijklmnoX",
            "xabcdefghijklmno-and-more",
            "prefix:abcdefghijklmno\tsuffix",
            "car-abcdefghijklmno-3",
        ] {
            assert!(
                !secrets.reflects(unrelated),
                "trimmed form under the floor matched inside {unrelated:?} for {original:?}"
            );
        }
        // The original bytes still are found inside longer text, and equality
        // still ignores the padding.
        assert!(secrets.reflects(&format!("x {original}y")));
        assert!(secrets.reflects("abcdefghijklmno"));
        assert!(secrets.reflects("  Bearer abcdefghijklmno\n"));
    }

    // Through the scrub itself: the unrelated text keeps its value, the text
    // that carries the original bytes does not.
    let secrets = ProviderSecrets::new(&["abcdefghijklmno "]);
    let mut fields = json!({
        "display_name": "xabcdefghijklmno-3",
        "note": "x abcdefghijklmno y",
    });
    let fields = fields.as_object_mut().expect("object");
    assert_eq!(secrets.scrub_fields(fields), vec!["note".to_owned()]);
    assert_eq!(fields["display_name"], json!("xabcdefghijklmno-3"));
    assert_eq!(fields["note"], Value::Null);
}

#[test]
fn a_secret_that_is_blank_after_trimming_is_never_stored() {
    // No credential path admits one, but the carrier is general: a blank
    // needle must not make every blank string a reflection, and it must not
    // disturb the secrets stored next to it.
    let blank = ProviderSecrets::new(&["", " ", "  ", "\t\n"]);
    for text in ["", " ", "   ", "\n", "Bearer", "Bearer ", "x"] {
        assert!(!blank.reflects(text), "blank secrets matched {text:?}");
    }
    let mixed = ProviderSecrets::new(&["  ", "real-secret "]);
    assert!(mixed.reflects("real-secret"));
    assert!(!mixed.reflects("  "));
    assert!(!mixed.reflects(""));
}

#[test]
fn provider_secret_contains_floor_is_sixteen_bytes() {
    let fifteen = "abcdefghijklmno";
    let sixteen = "abcdefghijklmnop";
    assert_eq!((fifteen.len(), sixteen.len()), (15, 16));

    let secrets = ProviderSecrets::new(&[fifteen]);
    assert!(secrets.reflects(fifteen));
    assert!(secrets.reflects(&format!("Bearer {fifteen}")));
    assert!(!secrets.reflects(&format!("x {fifteen} y")));

    let secrets = ProviderSecrets::new(&[sixteen]);
    assert!(secrets.reflects(sixteen));
    assert!(secrets.reflects(&format!("x {sixteen} y")));
    // Fragments of a long token are out of scope: only whole secrets match.
    assert!(!secrets.reflects(&sixteen[..15]));
}

#[test]
fn provider_secrets_never_format_their_contents() {
    let secrets = ProviderSecrets::new(&["debug-visible-secret-value"]);
    assert_eq!(format!("{secrets:?}"), "ProviderSecrets([redacted])");
    assert_eq!(format!("{secrets:#?}"), "ProviderSecrets([redacted])");
}

#[test]
fn vehicle_data_nulls_reflected_credentials_and_keeps_every_other_field() {
    let log = LogCapture::start();
    let secrets = ProviderSecrets::new(&[LONG_REFLECTED_ACCESS, LONG_REFLECTED_REFRESH]);
    let raw: Value = serde_json::from_str(&reflecting_vehicle_data(
        LONG_REFLECTED_ACCESS,
        LONG_REFLECTED_REFRESH,
    ))
    .expect("fixture JSON");

    let data = VehicleData::from_provider_raw_json(VehicleId(7), raw.clone(), &secrets)
        .expect("reflected response still parses");

    let expected = json!({"response": {
        "id": 7,
        "vehicle_id": 7,
        "vin": TEST_VIN,
        "state": "online",
        "display_name": null,
        "drive_state": {"shift_state": "P", "timestamp": 1_800_000_000_000_i64},
        "charge_state": {"battery_level": 80, "charging_state": "Charging"},
        "vehicle_state": {
            "car_version": null,
            "vehicle_name": null,
            "odometer": 1234.5,
            "software_update": {"status": "available", "version": null}
        },
        "vehicle_config": {"car_type": "model3"}
    }});
    assert_eq!(data.provider_raw_json(), &expected);
    assert_eq!(
        &Value::Object(data.fields().clone()),
        &expected["response"],
        "the projected fields are the scrubbed fields"
    );
    assert_no_credential_bytes(
        &data.provider_raw_json().to_string(),
        &[LONG_REFLECTED_ACCESS, LONG_REFLECTED_REFRESH],
    );

    // The same response without live secrets in scope is kept as the
    // provider sent it, which proves the scrub is what changed the output.
    let unscrubbed =
        VehicleData::from_provider_raw_json(VehicleId(7), raw, &ProviderSecrets::none())
            .expect("response parses");
    assert_eq!(unscrubbed.fields()["display_name"], LONG_REFLECTED_ACCESS);
    assert_eq!(
        unscrubbed.fields()["vehicle_state"]["vehicle_name"],
        format!("Bearer {LONG_REFLECTED_ACCESS}")
    );

    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 1, "one warning per response");
    for path in [
        "display_name",
        "vehicle_state.car_version",
        "vehicle_state.vehicle_name",
        "vehicle_state.software_update.version",
    ] {
        assert!(log.contains(path), "warning names {path}");
    }
    assert!(log.contains("fields=4"), "{log}");
    assert_no_credential_bytes(&log, &[LONG_REFLECTED_ACCESS, LONG_REFLECTED_REFRESH]);
}

#[test]
fn vehicle_data_without_a_reflection_logs_nothing_and_is_unchanged() {
    let log = LogCapture::start();
    let secrets = ProviderSecrets::new(&[LONG_REFLECTED_ACCESS, LONG_REFLECTED_REFRESH, ""]);
    let raw = json!({"response": {
        "display_name": "Athena",
        "state": "online",
        "vehicle_state": {"car_version": "2026.20 abcdef"}
    }});

    let data = VehicleData::from_provider_raw_json(VehicleId(7), raw.clone(), &secrets)
        .expect("response parses");

    assert_eq!(data.provider_raw_json(), &raw);
    assert!(log.contents().is_empty(), "{}", log.contents());
}

#[tokio::test]
async fn legacy_vehicle_data_scrubs_reflected_long_credentials_before_returning() {
    let log = LogCapture::start();
    let state = FakeState::default();
    state.data_bodies.lock().expect("data bodies").insert(
        "7".to_owned(),
        (
            StatusCode::OK,
            reflecting_vehicle_data(LONG_REFLECTED_ACCESS, LONG_REFLECTED_REFRESH),
        ),
    );
    let fake = FakeServer::spawn(state.clone()).await;
    let mut manager =
        guarded_legacy_manager(fake.base_url.clone(), Arc::new(AtomicBool::new(true)));
    let mut fuse = LegacyAuthFuse::default();

    let data = fake
        .client(Duration::from_secs(2))
        .vehicle_data_with_legacy_auth_fused(&mut manager, &mut fuse, VehicleId(7), None)
        .await
        .expect("vehicle data");

    let fields = data.fields();
    assert_eq!(fields["display_name"], Value::Null);
    assert_eq!(fields["vehicle_state"]["car_version"], Value::Null);
    assert_eq!(fields["vehicle_state"]["vehicle_name"], Value::Null);
    assert_eq!(
        fields["vehicle_state"]["software_update"],
        json!({"status": "available", "version": null})
    );
    assert_eq!(fields["vin"], TEST_VIN);
    assert_eq!(fields["charge_state"]["battery_level"], 80);
    assert_eq!(fields["vehicle_state"]["odometer"], 1234.5);
    assert_eq!(fields["vehicle_config"]["car_type"], "model3");
    assert_no_credential_bytes(
        &data.provider_raw_json().to_string(),
        &[LONG_REFLECTED_ACCESS, LONG_REFLECTED_REFRESH],
    );
    assert_eq!(state.requests.lock().expect("requests").len(), 1);

    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(log.contains("fields=4"), "{log}");
    for path in [
        "display_name",
        "vehicle_state.car_version",
        "vehicle_state.vehicle_name",
        "vehicle_state.software_update.version",
    ] {
        assert!(log.contains(path), "warning names {path}");
    }
    assert_no_credential_bytes(&log, &[LONG_REFLECTED_ACCESS, LONG_REFLECTED_REFRESH]);
}

#[tokio::test]
async fn legacy_vehicle_data_scrubs_short_credentials_only_by_equality() {
    let log = LogCapture::start();
    let state = FakeState::default();
    state.data_bodies.lock().expect("data bodies").insert(
        "7".to_owned(),
        (
            StatusCode::OK,
            reflecting_vehicle_data(SHORT_ACCESS, SHORT_REFRESH),
        ),
    );
    let fake = FakeServer::spawn(state).await;
    let mut manager = short_secret_manager(fake.base_url.clone());
    let mut fuse = LegacyAuthFuse::default();

    let data = fake
        .client(Duration::from_secs(2))
        .vehicle_data_with_legacy_auth_fused(&mut manager, &mut fuse, VehicleId(7), None)
        .await
        .expect("vehicle data");

    let fields = data.fields();
    // Equal to the access token, and `Bearer <access token>`, and the refresh
    // token padded by whitespace: all removed.
    assert_eq!(fields["display_name"], Value::Null);
    assert_eq!(fields["vehicle_state"]["vehicle_name"], Value::Null);
    assert_eq!(
        fields["vehicle_state"]["software_update"]["version"],
        Value::Null
    );
    // A short secret inside longer text is not searched for: the floor keeps
    // unrelated text intact.
    assert_eq!(
        fields["vehicle_state"]["car_version"],
        format!("build {SHORT_REFRESH} end")
    );
    assert_eq!(fields["charge_state"]["battery_level"], 80);

    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(log.contains("fields=3"), "{log}");
    assert!(!log.contains("vehicle_state.car_version"), "{log}");
    assert_no_credential_bytes(&log, &[SHORT_ACCESS, SHORT_REFRESH]);
}

#[tokio::test]
async fn legacy_vehicle_data_scrubs_credentials_that_carry_a_space_of_their_own() {
    let log = LogCapture::start();
    let state = FakeState::default();
    state.data_bodies.lock().expect("data bodies").insert(
        "7".to_owned(),
        (
            StatusCode::OK,
            reflecting_vehicle_data(PADDED_ACCESS, PADDED_REFRESH),
        ),
    );
    let fake = FakeServer::spawn(state).await;
    let mut manager = padded_secret_manager(fake.base_url.clone());
    let mut fuse = LegacyAuthFuse::default();

    let data = fake
        .client(Duration::from_secs(2))
        .vehicle_data_with_legacy_auth_fused(&mut manager, &mut fuse, VehicleId(7), None)
        .await
        .expect("vehicle data");

    let fields = data.fields();
    // The access token byte for byte, `Bearer <access token>` and the refresh
    // token inside other whitespace: all removed.
    assert_eq!(fields["display_name"], Value::Null);
    assert_eq!(fields["vehicle_state"]["vehicle_name"], Value::Null);
    assert_eq!(
        fields["vehicle_state"]["software_update"]["version"],
        Value::Null
    );
    // A short secret inside longer text is still not searched for.
    assert_eq!(
        fields["vehicle_state"]["car_version"],
        format!("build {PADDED_REFRESH} end")
    );
    assert_eq!(fields["charge_state"]["battery_level"], 80);
    assert_no_credential_bytes(&data.provider_raw_json().to_string(), &[PADDED_ACCESS]);

    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(log.contains("fields=3"), "{log}");
    assert_no_credential_bytes(&log, &[PADDED_ACCESS, PADDED_REFRESH]);
}

#[tokio::test]
async fn legacy_discovery_drops_vehicle_names_that_reflect_a_credential() {
    let log = LogCapture::start();
    let body = format!(
        r#"{{"response":[
            {{"vehicle_id":71,"id":70,"vin":"5YJ3E1EA7KF000001","state":"online","display_name":"{LONG_REFLECTED_ACCESS}"}},
            {{"vehicle_id":81,"id":80,"vin":"5YJ3E1EA7KF000002","state":"asleep","display_name":"Bearer {LONG_REFLECTED_REFRESH}"}},
            {{"vehicle_id":91,"id":90,"vin":"5YJ3E1EA7KF000003","state":"online","display_name":"Athena"}}
        ],"count":3}}"#
    );
    let state = FakeState::with_vehicles(&body);
    let fake = FakeServer::spawn(state).await;
    let mut manager =
        guarded_legacy_manager(fake.base_url.clone(), Arc::new(AtomicBool::new(true)));
    let mut fuse = LegacyAuthFuse::default();

    let vehicles = fake
        .client(Duration::from_secs(2))
        .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
        .await
        .expect("discovery");

    let names = vehicles
        .iter()
        .map(|vehicle| vehicle.display_name.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(names, vec![None, None, Some("Athena")]);
    assert_eq!(
        vehicles
            .iter()
            .map(|vehicle| vehicle.id.get())
            .collect::<Vec<_>>(),
        vec![70, 80, 90]
    );
    assert_eq!(vehicles[1].state, "asleep");
    assert_eq!(vehicles[0].vin, "5YJ3E1EA7KF000001");

    let once = fake
        .client(Duration::from_secs(2))
        .list_vehicles_with_legacy_auth_once(&crate::legacy_auth::LegacyAuth::for_test(
            fake.base_url.clone(),
            LONG_REFLECTED_ACCESS,
            LONG_REFLECTED_REFRESH,
        ))
        .await
        .expect("native discovery");
    assert_eq!(
        once.iter()
            .map(|vehicle| vehicle.display_name.as_deref())
            .collect::<Vec<_>>(),
        vec![None, None, Some("Athena")]
    );

    let log = log.contents();
    assert_eq!(
        log.matches("WARN").count(),
        2,
        "one warning per response: {log}"
    );
    assert!(log.contains("response[].display_name"), "{log}");
    assert!(log.contains("vehicles=2"), "{log}");
    assert_no_credential_bytes(&log, &[LONG_REFLECTED_ACCESS, LONG_REFLECTED_REFRESH]);
}

/// Three vehicles whose provider `state` is the access token, the refresh
/// token inside longer text (graphic characters only, as `valid_state` needs)
/// and an ordinary state.
fn state_reflecting_products(access: &str, refresh: &str) -> String {
    json!({"response": [
        {"vehicle_id": 71, "id": 70, "vin": "5YJ3E1EA7KF000001", "state": access, "display_name": "Athena"},
        {"vehicle_id": 81, "id": 80, "vin": "5YJ3E1EA7KF000002", "state": format!("x-{refresh}-y"), "display_name": "Hermes"},
        {"vehicle_id": 91, "id": 90, "vin": "5YJ3E1EA7KF000003", "state": "asleep", "display_name": "Apollo"}
    ], "count": 3})
    .to_string()
}

#[tokio::test]
async fn legacy_discovery_replaces_provider_states_that_reflect_a_credential() {
    let log = LogCapture::start();
    let state = FakeState::with_vehicles(&state_reflecting_products(
        LONG_REFLECTED_ACCESS,
        LONG_REFLECTED_REFRESH,
    ));
    let fake = FakeServer::spawn(state).await;
    let mut manager =
        guarded_legacy_manager(fake.base_url.clone(), Arc::new(AtomicBool::new(true)));
    let mut fuse = LegacyAuthFuse::default();

    let vehicles = fake
        .client(Duration::from_secs(2))
        .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
        .await
        .expect("discovery");
    let once = fake
        .client(Duration::from_secs(2))
        .list_vehicles_with_legacy_auth_once(&crate::legacy_auth::LegacyAuth::for_test(
            fake.base_url.clone(),
            LONG_REFLECTED_ACCESS,
            LONG_REFLECTED_REFRESH,
        ))
        .await
        .expect("native discovery");

    for listed in [&vehicles, &once] {
        assert_eq!(
            listed
                .iter()
                .map(|vehicle| vehicle.state.as_str())
                .collect::<Vec<_>>(),
            vec!["unknown", "unknown", "asleep"]
        );
        // Everything else the provider said is intact, the names included.
        assert_eq!(
            listed
                .iter()
                .map(|vehicle| vehicle.display_name.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("Athena"), Some("Hermes"), Some("Apollo")]
        );
        assert_eq!(
            listed
                .iter()
                .map(|vehicle| vehicle.id.get())
                .collect::<Vec<_>>(),
            vec![70, 80, 90]
        );
        assert_eq!(listed[0].vin, "5YJ3E1EA7KF000001");
    }

    let log = log.contents();
    assert_eq!(
        log.matches("WARN").count(),
        2,
        "one warning per response: {log}"
    );
    assert!(log.contains("response[].state"), "{log}");
    assert!(!log.contains("display_name"), "{log}");
    assert!(log.contains("vehicles=2"), "{log}");
    assert_no_credential_bytes(&log, &[LONG_REFLECTED_ACCESS, LONG_REFLECTED_REFRESH]);
}

#[tokio::test]
async fn legacy_discovery_replaces_a_short_credential_in_state_only_by_equality() {
    let log = LogCapture::start();
    let state = FakeState::with_vehicles(&state_reflecting_products(SHORT_ACCESS, SHORT_REFRESH));
    let fake = FakeServer::spawn(state).await;
    let mut manager = short_secret_manager(fake.base_url.clone());
    let mut fuse = LegacyAuthFuse::default();

    let vehicles = fake
        .client(Duration::from_secs(2))
        .list_vehicles_with_legacy_auth_fused(&mut manager, &mut fuse)
        .await
        .expect("discovery");

    assert_eq!(
        vehicles
            .iter()
            .map(|vehicle| vehicle.state.as_str())
            .collect::<Vec<_>>(),
        vec!["unknown", "x-short-refresh-y", "asleep"],
        "a short secret inside longer text is not searched for"
    );
    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 1, "{log}");
    assert!(log.contains("vehicles=1"), "{log}");
    assert_no_credential_bytes(&log, &[SHORT_ACCESS, SHORT_REFRESH]);
}

#[tokio::test]
async fn legacy_state_probe_replaces_a_provider_state_that_reflects_a_credential() {
    let log = LogCapture::start();
    let fake = FakeServer::start(Router::new().route(
        "/api/1/vehicles/{vehicle_id}",
        get(|AxumPath(vehicle_id): AxumPath<String>| async move {
            let state = match vehicle_id.as_str() {
                "70" => LONG_REFLECTED_ACCESS.to_owned(),
                "80" => format!("x-{LONG_REFLECTED_REFRESH}-y"),
                _ => "asleep".to_owned(),
            };
            json!({"response": {"id": 70, "state": state}}).to_string()
        }),
    ))
    .await;
    let mut manager =
        guarded_legacy_manager(fake.base_url.clone(), Arc::new(AtomicBool::new(true)));
    let mut fuse = LegacyAuthFuse::default();
    let client = fake.client(Duration::from_secs(2));

    let mut states = Vec::new();
    for vehicle_id in [70, 80, 90] {
        states.push(
            client
                .vehicle_state_with_legacy_auth_fused(
                    &mut manager,
                    &mut fuse,
                    VehicleId(vehicle_id),
                )
                .await
                .expect("state probe"),
        );
    }

    assert_eq!(states, vec!["unknown", "unknown", "asleep"]);
    let log = log.contents();
    assert_eq!(log.matches("WARN").count(), 2, "{log}");
    assert!(log.contains("response.state"), "{log}");
    assert_no_credential_bytes(&log, &[LONG_REFLECTED_ACCESS, LONG_REFLECTED_REFRESH]);
}

#[test]
fn scrubbing_a_vehicle_list_names_only_the_paths_it_changed() {
    let secrets = ProviderSecrets::new(&["list-secret-0123456789"]);
    let vehicle = |id: u64, state: &str, name: Option<&str>| {
        let mut vehicle = Vehicle::for_test(id, TEST_VIN, state);
        vehicle.display_name = name.map(str::to_owned);
        vehicle
    };

    let log = LogCapture::start();
    let mut clean = vec![vehicle(1, "online", Some("Athena"))];
    secrets.scrub_vehicle_list(&mut clean);
    assert_eq!(clean[0].state, "online");
    assert_eq!(clean[0].display_name.as_deref(), Some("Athena"));
    assert_eq!(log.contents(), "", "a clean list logs nothing");
    drop(log);

    let log = LogCapture::start();
    let mut states = vec![
        vehicle(1, "list-secret-0123456789", Some("Athena")),
        vehicle(2, "asleep", None),
    ];
    secrets.scrub_vehicle_list(&mut states);
    assert_eq!(states[0].state, "unknown");
    assert_eq!(states[0].display_name.as_deref(), Some("Athena"));
    assert_eq!(states[1].state, "asleep");
    let rendered = log.contents();
    assert!(rendered.contains("response[].state"), "{rendered}");
    assert!(!rendered.contains("display_name"), "{rendered}");
    assert!(rendered.contains("vehicles=1"), "{rendered}");
    drop(log);

    let log = LogCapture::start();
    let mut both = vec![
        vehicle(1, "list-secret-0123456789", None),
        vehicle(2, "online", Some("list-secret-0123456789")),
        vehicle(3, "online", Some("Apollo")),
    ];
    secrets.scrub_vehicle_list(&mut both);
    assert_eq!(
        both.iter()
            .map(|vehicle| (vehicle.state.as_str(), vehicle.display_name.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("unknown", None),
            ("online", None),
            ("online", Some("Apollo"))
        ]
    );
    let rendered = log.contents();
    assert_eq!(rendered.matches("WARN").count(), 1, "{rendered}");
    assert!(rendered.contains("response[].display_name"), "{rendered}");
    assert!(rendered.contains("response[].state"), "{rendered}");
    assert!(rendered.contains("vehicles=2"), "{rendered}");
    assert_no_credential_bytes(&rendered, &["list-secret-0123456789"]);
}

#[test]
fn scrubbing_one_state_replaces_only_a_reflection_and_never_logs_the_value() {
    let secrets = ProviderSecrets::new(&["probe-secret-0123456789", "short"]);
    let log = LogCapture::start();

    assert_eq!(secrets.scrub_vehicle_state("online".to_owned()), "online");
    assert_eq!(secrets.scrub_vehicle_state("shorter".to_owned()), "shorter");
    assert_eq!(log.contents(), "");
    for reflected in [
        "probe-secret-0123456789",
        "short",
        "x-probe-secret-0123456789-y",
    ] {
        assert_eq!(
            secrets.scrub_vehicle_state(reflected.to_owned()),
            "unknown",
            "{reflected}"
        );
    }
    assert_eq!(
        ProviderSecrets::none().scrub_vehicle_state("short".to_owned()),
        "short"
    );
    let rendered = log.contents();
    assert_eq!(rendered.matches("WARN").count(), 3, "{rendered}");
    assert_no_credential_bytes(&rendered, &["probe-secret-0123456789", "short"]);
}

#[test]
fn legacy_refresh_token_accessor_is_fenced_by_the_same_admission_as_the_access_token() {
    let admitted = Arc::new(AtomicBool::new(false));
    let manager = guarded_legacy_manager(
        Url::parse("http://127.0.0.1:9/").expect("issuer"),
        Arc::clone(&admitted),
    );

    assert!(matches!(
        manager.refresh_token_for_sensitive_use(),
        Err(LegacyAuthManagerError::Credential(
            CredentialError::MacKeychainHelperInvalid
        ))
    ));
    admitted.store(true, Ordering::Release);
    assert_eq!(
        manager
            .refresh_token_for_sensitive_use()
            .expect("admitted refresh token"),
        LONG_REFLECTED_REFRESH
    );
    assert_eq!(
        manager
            .access_token_for_sensitive_use()
            .expect("admitted access token"),
        LONG_REFLECTED_ACCESS
    );
}

// ---- C3 part K5: the error classes of the wire (R42, R45, R55) ----------------

/// A fake Owner wire that answers every request with one configured status,
/// headers and body, and counts the requests.
#[derive(Clone)]
struct StatusReply {
    status: StatusCode,
    headers: Vec<(&'static str, String)>,
    body: Arc<Vec<u8>>,
    hits: Arc<std::sync::atomic::AtomicUsize>,
    delay: Duration,
}

async fn status_handler(State(reply): State<StatusReply>) -> axum::response::Response {
    reply.hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    let mut response = (reply.status, reply.body.as_ref().clone()).into_response();
    for (name, value) in &reply.headers {
        response.headers_mut().insert(
            reqwest::header::HeaderName::from_static(name),
            reqwest::header::HeaderValue::from_str(value).expect("header value"),
        );
    }
    response
}

impl FakeServer {
    /// Answer every request with `status`, `headers` and `body`.
    async fn status(
        status: u16,
        headers: &[(&'static str, &str)],
        body: &[u8],
    ) -> (Self, Arc<std::sync::atomic::AtomicUsize>) {
        Self::status_after(status, headers, body, Duration::ZERO).await
    }

    async fn status_after(
        status: u16,
        headers: &[(&'static str, &str)],
        body: &[u8],
        delay: Duration,
    ) -> (Self, Arc<std::sync::atomic::AtomicUsize>) {
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reply = StatusReply {
            status: StatusCode::from_u16(status).expect("status"),
            headers: headers
                .iter()
                .map(|(name, value)| (*name, (*value).to_owned()))
                .collect(),
            body: Arc::new(body.to_vec()),
            hits: Arc::clone(&hits),
            delay,
        };
        let server = Self::start(Router::new().fallback(status_handler).with_state(reply)).await;
        (server, hits)
    }
}

/// The error one request of a scheduled kind gets from a wire that answers with
/// the given status.
async fn owner_error_of(
    status: u16,
    headers: &[(&'static str, &str)],
    body: &[u8],
) -> OwnerApiError {
    let (server, hits) = FakeServer::status(status, headers, body).await;
    let client = server.client(Duration::from_secs(2));
    let url = client.vehicle_probe_endpoint(VehicleId(7)).expect("URL");
    let error = client
        .execute_envelope_request::<Value>(client.envelope_request(TEST_TOKEN, url))
        .await
        .expect_err("an error status");
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one request, never retried"
    );
    error
}

#[tokio::test]
async fn r42_owner_wire_statuses_become_typed_errors() {
    use OwnerApiError::*;
    let cases: Vec<(u16, &[u8], OwnerApiError)> = vec![
        (404, b"{}", VehicleNotFound),
        (410, b"{}", Gone),
        (401, b"{}", HttpStatus(401)),
        (402, b"{}", HttpStatus(402)),
        (408, br#"{"error":"vehicle unavailable"}"#, HttpStatus(408)),
        (412, b"", HttpStatus(412)),
        (451, b"", HttpStatus(451)),
        (500, b"", HttpStatus(500)),
        (503, b"", HttpStatus(503)),
        (540, b"", HttpStatus(540)),
        // 405: only an error text that says "in service" is the car's state.
        (
            405,
            br#"{"response":null,"error":"vehicle is in service"}"#,
            VehicleInService,
        ),
        (
            405,
            br#"{"error":"Vehicle IS IN SERVICE","error_description":""}"#,
            VehicleInService,
        ),
        (
            405,
            br#"{"error":"","error_description":"car in service"}"#,
            VehicleInService,
        ),
        (405, b"the car is in service", VehicleInService),
        (405, br#"{"error":"method not allowed"}"#, HttpStatus(405)),
        (405, b"", HttpStatus(405)),
        // 403: whether the body names the Fleet API.
        (
            403,
            br#"{"error":"forbidden"}"#,
            Forbidden {
                fleet_api_named: false,
            },
        ),
        (
            403,
            b"",
            Forbidden {
                fleet_api_named: false,
            },
        ),
        (
            403,
            br#"{"error":"Fleet API required"}"#,
            Forbidden {
                fleet_api_named: true,
            },
        ),
        (
            403,
            b"Use the Fleet-API instead",
            Forbidden {
                fleet_api_named: true,
            },
        ),
    ];
    for (status, body, expected) in cases {
        assert_eq!(
            owner_error_of(status, &[], body).await,
            expected,
            "status {status}, body {:?}",
            String::from_utf8_lossy(body)
        );
    }
}

#[tokio::test]
async fn ct39_a_429_carries_the_wait_its_headers_ask_for() {
    // One request, never retried (R56); the value is the one RFC 9110 gives.
    for (headers, expected) in [
        (vec![("retry-after", "120")], 120),
        (vec![("retry-after", "0")], 0),
        (vec![("retry-after", "abc")], 0),
        (vec![("retry-after", "7200")], 3_600),
        (vec![("ratelimit-reset", "30")], 30),
        (vec![], 0),
    ] {
        assert_eq!(
            owner_error_of(429, &headers, b"").await,
            OwnerApiError::RateLimited {
                retry_after_seconds: expected
            },
            "{headers:?}"
        );
    }
}

#[test]
fn ct39_retry_after_parsing_follows_rfc_9110() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(RFC_EXAMPLE_EPOCH_SECONDS);
    let wait = |pairs: &[(&'static str, &str)]| retry_after_seconds_at(&headers_of(pairs), now);
    assert_eq!(wait(&[("retry-after", "120")]), 120);
    assert_eq!(wait(&[("retry-after", "0")]), 0, "zero means absent");
    assert_eq!(wait(&[("retry-after", "abc")]), 0);
    // An HTTP-date 90 s ahead, and one in the past.
    assert_eq!(
        wait(&[("retry-after", "Sun, 06 Nov 1994 08:51:07 GMT")]),
        90
    );
    assert_eq!(wait(&[("retry-after", "Sun, 06 Nov 1994 08:40:00 GMT")]), 0);
    assert_eq!(wait(&[("retry-after", "7200")]), 3_600);
    assert_eq!(wait(&[("ratelimit-reset", "30")]), 30);
}

#[tokio::test]
async fn r42_the_error_body_scan_is_bounded_and_the_body_is_never_kept() {
    // The first 4,096 bytes are read: a name inside them counts, one beyond them
    // does not, however large the body.
    assert_eq!(
        MAX_ERROR_BODY_BYTES, 4_096,
        "R42 names the first 4,096 bytes"
    );
    let filler = vec![b' '; 4_096 - 16];
    let mut inside = filler.clone();
    inside.extend_from_slice(b"FLEET API");
    inside.extend(std::iter::repeat_n(b'x', 1 << 20));
    assert_eq!(
        owner_error_of(403, &[], &inside).await,
        OwnerApiError::Forbidden {
            fleet_api_named: true
        }
    );
    let mut beyond = vec![b' '; 4_096];
    beyond.extend_from_slice(b"fleet api");
    assert_eq!(
        owner_error_of(403, &[], &beyond).await,
        OwnerApiError::Forbidden {
            fleet_api_named: false
        }
    );
    // The error carries no text of the body.
    let error = owner_error_of(403, &[], br#"{"error":"Fleet API secret-marker-77"}"#).await;
    assert!(!format!("{error:?} {error}").contains("secret-marker-77"));
}

#[test]
fn r42_fleet_api_naming_ignores_case_and_separators_and_reads_each_field_alone() {
    assert!(is_fleet_api_named(br#"{"error":"Fleet API"}"#));
    assert!(is_fleet_api_named(br#"{"error":"fleet-api"}"#));
    assert!(is_fleet_api_named(br#"{"error":"fleet_api"}"#));
    assert!(is_fleet_api_named(br#"{"error":"FleetAPI"}"#));
    assert!(is_fleet_api_named(
        br#"{"error":"forbidden","error_description":"see https://developer.tesla.com/docs/fleet-api"}"#
    ));
    // A word cannot form across the boundary of two fields.
    assert!(!is_fleet_api_named(
        br#"{"error":"fleet","error_description":"api"}"#
    ));
    // A JSON object is read by its two fields only.
    assert!(!is_fleet_api_named(br#"{"message":"Fleet API"}"#));
    // Anything else is text.
    assert!(is_fleet_api_named(b"<html>Fleet API</html>"));
    assert!(is_fleet_api_named(br#"["Fleet API"]"#));
    assert!(!is_fleet_api_named(b""));
    assert!(!is_fleet_api_named(br#"{"error":"fleet"}"#));
}

#[test]
fn r42_error_body_text_matches_ascii_case_insensitively() {
    assert!(is_owner_error_body(
        br#"{"error":"Vehicle Is In Service"}"#,
        "in service"
    ));
    assert!(is_vehicle_in_service_body(b"IN SERVICE"));
    assert!(!is_vehicle_in_service_body(br#"{"error":"in  service"}"#));
    assert!(!is_vehicle_in_service_body(br#"{"note":"in service"}"#));
    assert!(!is_owner_error_body(b"", "in service"));
}

#[tokio::test]
async fn r42_a_transport_failure_and_a_timeout_are_not_http_answers() {
    // Nothing listens: no HTTP status was received.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("port");
    let address = listener.local_addr().expect("address");
    drop(listener);
    let client = OwnerApi::for_fake_http(
        Url::parse(&format!("http://{address}/")).expect("URL"),
        Duration::from_millis(500),
    )
    .expect("client");
    let url = client.vehicle_probe_endpoint(VehicleId(7)).expect("URL");
    assert_eq!(
        client
            .execute_envelope_request::<Value>(client.envelope_request(TEST_TOKEN, url))
            .await
            .expect_err("refused connection"),
        OwnerApiError::Transport
    );
    // A response that never starts within the request timeout.
    let (server, _) = FakeServer::status_after(200, &[], b"{}", Duration::from_secs(5)).await;
    let client = server.client(Duration::from_millis(200));
    let url = client.vehicle_probe_endpoint(VehicleId(7)).expect("URL");
    assert_eq!(
        client
            .execute_envelope_request::<Value>(client.envelope_request(TEST_TOKEN, url))
            .await
            .expect_err("timeout"),
        OwnerApiError::RequestTimeout
    );
}

#[tokio::test]
async fn r55_an_oversized_response_is_discarded_unparsed() {
    let body = vec![b'x'; MAX_RESPONSE_BYTES + 1];
    let (server, _) = FakeServer::status(200, &[], &body).await;
    let client = server.client(Duration::from_secs(5));
    let url = client.vehicle_probe_endpoint(VehicleId(7)).expect("URL");
    let error = client
        .execute_envelope_request::<Value>(client.envelope_request(TEST_TOKEN, url))
        .await
        .expect_err("over the size limit");
    assert_eq!(error, OwnerApiError::ResponseTooLarge);
    assert!(!format!("{error:?}").contains('x'));
}

#[tokio::test]
async fn r55_a_list_with_one_malformed_record_is_rejected_whole() {
    // The list is one answer: a record that is vehicle-shaped but invalid makes
    // the whole list a failure, so no car is judged absent from a partial list.
    let state = FakeState::with_vehicles(
        r#"{"response":[
            {"vehicle_id":71,"id":70,"vin":"5YJ3E1EA7KF000001","state":"online"},
            {"id":72,"vin":"5YJ3E1EA7KF000002","state":"online"}
        ],"count":2}"#,
    );
    let fake = FakeServer::spawn(state).await;
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        fake.base_url.clone(),
        TEST_TOKEN,
        "test-refresh-token",
    );
    assert_eq!(
        fake.client(Duration::from_secs(2))
            .list_vehicles_with_legacy_auth_once(&auth)
            .await
            .expect_err("malformed record"),
        OwnerApiError::InvalidVehicleRecord
    );
    // A count that disagrees with the records is a failure too.
    let state = FakeState::with_vehicles(
        r#"{"response":[{"vehicle_id":71,"id":70,"vin":"5YJ3E1EA7KF000001","state":"online"}],"count":2}"#,
    );
    let fake = FakeServer::spawn(state).await;
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        fake.base_url.clone(),
        TEST_TOKEN,
        "test-refresh-token",
    );
    assert_eq!(
        fake.client(Duration::from_secs(2))
            .list_vehicles_with_legacy_auth_once(&auth)
            .await
            .expect_err("inconsistent count"),
        OwnerApiError::InvalidVehicleListCount
    );
}
