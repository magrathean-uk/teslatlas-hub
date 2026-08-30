// SPDX-License-Identifier: AGPL-3.0-only

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::any,
};
use tokio::{net::TcpListener, task::JoinHandle};

use super::*;

const TEST_TOKEN: &str = "fleet-test-token";
const TEST_VIN: &str = "5YJ3E1EA7KF000001";

fn telemetry_ca_pem() -> String {
    let mut params = rcgen::CertificateParams::new(Vec::new()).expect("CA params");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages.push(rcgen::KeyUsagePurpose::KeyCertSign);
    params
        .self_signed(&rcgen::KeyPair::generate().expect("CA key"))
        .expect("CA certificate")
        .pem()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedRequest {
    method: String,
    path: String,
    authorization_ok: bool,
    content_type_json: bool,
    body: Vec<u8>,
}

#[derive(Clone, Copy)]
enum FakeResponse {
    Normal,
    Oversized,
    ProviderError,
    OversizedProviderError,
    UnauthorizedProviderError,
    Redirect,
    RateLimited,
    GatewayTimeout,
}

#[derive(Clone)]
struct FakeState {
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    response: FakeResponse,
}

struct FakeServer {
    base_url: Url,
    state: FakeState,
    _task: JoinHandle<()>,
}

impl FakeServer {
    async fn spawn(response: FakeResponse) -> Self {
        let state = FakeState {
            requests: Arc::new(Mutex::new(Vec::new())),
            response,
        };
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake listener");
        let address = listener.local_addr().expect("fake address");
        let router = Router::new()
            .fallback(any(fake_handler))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("fake server");
        });
        Self {
            base_url: Url::parse(&format!("http://{address}/")).expect("fake URL"),
            state,
            _task: task,
        }
    }

    fn fleet_client(&self) -> FleetApi {
        FleetApi::for_fake_http(self.base_url.clone(), Duration::from_secs(2))
            .expect("fake Fleet client")
    }

    fn proxy_client(&self) -> FleetCommandProxy {
        let base = FleetCommandProxyBase::parse_loopback_http_for_test(self.base_url.as_str())
            .expect("fake proxy base");
        FleetCommandProxy::for_fake_http(base, Duration::from_secs(2)).expect("fake proxy client")
    }
}

async fn fake_handler(State(state): State<FakeState>, request: Request<Body>) -> impl IntoResponse {
    let method = request.method().to_string();
    let route_path = request.uri().path().to_owned();
    let path = request
        .uri()
        .path_and_query()
        .map_or_else(|| route_path.clone(), |value| value.as_str().to_owned());
    let authorization_ok = request
        .headers()
        .get("authorization")
        .is_some_and(|value| value.as_bytes() == b"Bearer fleet-test-token");
    let content_type_json = request
        .headers()
        .get("content-type")
        .is_some_and(|value| value.as_bytes() == b"application/json");
    let body = to_bytes(request.into_body(), 4096)
        .await
        .expect("bounded request body")
        .to_vec();
    state
        .requests
        .lock()
        .expect("request ledger")
        .push(RecordedRequest {
            method,
            path: path.clone(),
            authorization_ok,
            content_type_json,
            body,
        });
    match state.response {
        FakeResponse::Oversized => {
            return (StatusCode::OK, vec![b'x'; MAX_RESPONSE_BYTES + 1]).into_response();
        }
        FakeResponse::ProviderError => {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    r#"{{"response":null,"error":"invalid_field","error_description":"configuration rejected","txid":"not-retained","vin":"{TEST_VIN}","token":"not-retained"}}"#
                ),
            )
                .into_response();
        }
        FakeResponse::OversizedProviderError => {
            return (
                StatusCode::BAD_REQUEST,
                vec![b'x'; MAX_ERROR_RESPONSE_BYTES + 1],
            )
                .into_response();
        }
        FakeResponse::UnauthorizedProviderError => {
            return (
                StatusCode::UNAUTHORIZED,
                r#"{"error":"token rejected","error_description":"not retained"}"#,
            )
                .into_response();
        }
        FakeResponse::Redirect => {
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [("location", "/followed")],
                "",
            )
                .into_response();
        }
        FakeResponse::RateLimited => {
            return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "17")], "").into_response();
        }
        FakeResponse::GatewayTimeout => {
            return (StatusCode::GATEWAY_TIMEOUT, "").into_response();
        }
        FakeResponse::Normal => {}
    }
    if route_path == "/oauth2/v3/token" {
        (
            StatusCode::OK,
            r#"{"access_token":"fleet-next-access","refresh_token":"fleet-next-refresh","expires_in":28800,"token_type":"Bearer"}"#,
        )
            .into_response()
    } else if route_path == "/api/1/vehicles" {
        (
            StatusCode::OK,
            format!(
                r#"{{"response":[{{"id":70,"vehicle_id":71,"vin":"{TEST_VIN}","state":"online","display_name":"Athena"}}],"count":1}}"#
            ),
        )
            .into_response()
    } else if route_path.ends_with("/vehicle_data") {
        (
            StatusCode::OK,
            r#"{"response":{"drive_state":{"timestamp":1700000000000},"charge_state":{"battery_level":80}}}"#,
        )
            .into_response()
    } else if route_path.ends_with("/wake_up") {
        (StatusCode::OK, r#"{"response":{"state":"online"}}"#).into_response()
    } else if route_path == "/api/1/vehicles/fleet_telemetry_config" {
        (
            StatusCode::OK,
            r#"{"response":{"updated_vehicles":1,"skipped_vehicles":{}}}"#,
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            r#"{"response":{"result":true,"reason":""}}"#,
        )
            .into_response()
    }
}

#[test]
fn regions_are_fixed_to_official_hosts() {
    assert_eq!(
        FleetRegion::NorthAmericaAndAsiaPacific.base_url(),
        "https://fleet-api.prd.na.vn.cloud.tesla.com/"
    );
    assert_eq!(
        FleetRegion::EuropeMiddleEastAndAfrica.base_url(),
        "https://fleet-api.prd.eu.vn.cloud.tesla.com/"
    );
    assert_eq!(
        FleetRegion::China.base_url(),
        "https://fleet-api.prd.cn.vn.cloud.tesla.cn/"
    );
    assert_eq!(
        FleetRegion::EuropeMiddleEastAndAfrica.auth_token_url(),
        "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token"
    );
    assert_eq!(
        FleetRegion::China.auth_token_url(),
        "https://auth.tesla.cn/oauth2/v3/token"
    );
    assert!(FleetApi::new(FleetRegion::EuropeMiddleEastAndAfrica, Duration::ZERO).is_err());
}

#[test]
fn access_token_and_vin_are_bounded_and_redacted() {
    let token = FleetAccessToken::new(TEST_TOKEN).expect("token");
    assert_eq!(format!("{token:?}"), "FleetAccessToken([redacted])");
    assert!(!format!("{token:?}").contains(TEST_TOKEN));
    assert!(FleetAccessToken::new("bad token").is_err());
    assert!(FleetRefreshToken::new("bad token").is_err());
    assert_eq!(
        format!("{:?}", FleetClientId::parse("client-id").expect("client")),
        "FleetClientId([redacted])"
    );

    let vin = VehicleVin::parse(&TEST_VIN.to_ascii_lowercase()).expect("VIN");
    assert_eq!(vin.as_str(), TEST_VIN);
    for invalid in ["", "../wake_up", "5YJ3E1EA7KF00000I", "5YJ3E1EA7KF000001x"] {
        assert!(VehicleVin::parse(invalid).is_err(), "accepted {invalid}");
    }
}

#[tokio::test]
async fn fleet_refresh_uses_official_form_contract_without_redirects() {
    let fake = FakeServer::spawn(FakeResponse::Normal).await;
    let endpoint = fake.base_url.join("oauth2/v3/token").expect("auth URL");
    let auth =
        FleetAuthApi::for_fake_http(endpoint, Duration::from_secs(2)).expect("Fleet auth client");
    let refreshed = auth
        .refresh(
            &FleetClientId::parse("client-id").expect("client id"),
            &FleetRefreshToken::new("old-refresh").expect("refresh token"),
        )
        .await
        .expect("refreshed tokens");
    assert_eq!(refreshed.expires_in_seconds, 28_800);
    assert_eq!(refreshed.access_token.expose(), "fleet-next-access");
    assert_eq!(refreshed.refresh_token.expose(), "fleet-next-refresh");
    let requests = fake.state.requests.lock().expect("ledger");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/oauth2/v3/token");
    assert_eq!(
        requests[0].body,
        b"grant_type=refresh_token&client_id=client-id&refresh_token=old-refresh"
    );
}

#[test]
fn proxy_requires_loopback_https_without_url_secrets() {
    assert!(FleetCommandProxyBase::parse("https://127.0.0.1:4443/").is_ok());
    assert!(FleetCommandProxyBase::parse("https://[::1]:4443/").is_ok());
    assert!(FleetCommandProxyBase::parse("https://localhost:4443/").is_ok());
    for invalid in [
        "http://127.0.0.1:4443/",
        "https://192.0.2.1:4443/",
        "https://token@127.0.0.1:4443/",
        "https://127.0.0.1:4443/?token=bad",
    ] {
        assert!(
            FleetCommandProxyBase::parse(invalid).is_err(),
            "accepted {invalid}"
        );
    }
}

#[test]
fn proxy_custom_root_uses_exclusive_reqwest_trust_mode() {
    crate::crypto::install_default_provider();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .expect("test certificate")
        .cert
        .pem();

    command_proxy_client_builder(Duration::from_secs(2), Some(certificate.as_bytes()), false)
        .expect("custom-root builder")
        .tls_danger_accept_invalid_hostnames(true)
        .build()
        .expect("exclusive-root mode permits the reqwest-only hostname override");

    assert!(
        command_proxy_client_builder(Duration::from_secs(2), None, false)
            .expect("platform-root builder")
            .tls_danger_accept_invalid_hostnames(true)
            .build()
            .is_err(),
        "no custom root must retain reqwest's platform verifier mode"
    );
}

#[tokio::test]
async fn wake_uses_exact_vin_path_and_bearer() {
    let fake = FakeServer::spawn(FakeResponse::Normal).await;
    let result = fake
        .fleet_client()
        .wake(
            &FleetAccessToken::new(TEST_TOKEN).expect("token"),
            &VehicleVin::parse(TEST_VIN).expect("VIN"),
        )
        .await
        .expect("wake");
    assert_eq!(result.state, "online");
    assert_eq!(
        *fake.state.requests.lock().expect("ledger"),
        vec![RecordedRequest {
            method: "POST".to_owned(),
            path: format!("/api/1/vehicles/{TEST_VIN}/wake_up"),
            authorization_ok: true,
            content_type_json: false,
            body: Vec::new(),
        }]
    );
}

#[tokio::test]
async fn fleet_reads_discovery_and_vehicle_data_without_wake() {
    let fake = FakeServer::spawn(FakeResponse::Normal).await;
    let client = fake.fleet_client();
    let token = FleetAccessToken::new(TEST_TOKEN).expect("token");
    let vehicles = client.list_vehicles(&token).await.expect("vehicles");
    assert_eq!(vehicles.len(), 1);
    assert_eq!(vehicles[0].id.get(), 70);
    let data = client
        .vehicle_data(
            &token,
            vehicles[0].id,
            &VehicleVin::parse(TEST_VIN).expect("VIN"),
        )
        .await
        .expect("vehicle data");
    assert_eq!(data.fields()["charge_state"]["battery_level"], 80);
    let requests = fake.state.requests.lock().expect("ledger");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].path, "/api/1/vehicles");
    assert_eq!(
        requests[1].path,
        format!(
            "/api/1/vehicles/{TEST_VIN}/vehicle_data?endpoints={}",
            VEHICLE_DATA_ENDPOINTS.replace(';', "%3B")
        )
    );
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[tokio::test]
async fn typed_commands_use_exact_proxy_paths_and_bodies() {
    let fake = FakeServer::spawn(FakeResponse::Normal).await;
    let proxy = fake.proxy_client();
    let token = FleetAccessToken::new(TEST_TOKEN).expect("token");
    let vin = VehicleVin::parse(TEST_VIN).expect("VIN");
    let commands = [
        (
            FleetCommand::ClimateStart,
            "auto_conditioning_start",
            b"{}".as_slice(),
        ),
        (
            FleetCommand::ClimateStop,
            "auto_conditioning_stop",
            b"{}".as_slice(),
        ),
        (FleetCommand::Lock, "door_lock", b"{}".as_slice()),
        (FleetCommand::Unlock, "door_unlock", b"{}".as_slice()),
        (FleetCommand::ChargeStart, "charge_start", b"{}".as_slice()),
        (FleetCommand::ChargeStop, "charge_stop", b"{}".as_slice()),
        (FleetCommand::FlashLights, "flash_lights", b"{}".as_slice()),
        (FleetCommand::HonkHorn, "honk_horn", b"{}".as_slice()),
        (
            FleetCommand::SetChargeLimit { percent: 80 },
            "set_charge_limit",
            br#"{"percent":80}"#.as_slice(),
        ),
    ];
    for (command, _, _) in commands {
        assert_eq!(
            proxy.execute(&token, &vin, command).await.expect("command"),
            FleetCommandResult {
                result: true,
                reason: None,
            }
        );
    }
    let requests = fake.state.requests.lock().expect("ledger");
    assert_eq!(requests.len(), commands.len());
    for (request, (_, endpoint, body)) in requests.iter().zip(commands) {
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.path,
            format!("/api/1/vehicles/{TEST_VIN}/command/{endpoint}")
        );
        assert!(request.authorization_ok);
        assert!(request.content_type_json);
        assert_eq!(request.body, body);
    }
}

#[tokio::test]
async fn fleet_telemetry_configuration_uses_fixed_proxy_path_and_typed_body() {
    let fake = FakeServer::spawn(FakeResponse::Normal).await;
    let certificate = telemetry_ca_pem();
    let config =
        FleetTelemetryConfigBuilder::new("telemetry.example.com", 443, certificate, 1_900_000_000)
            .with_recommended_fields()
            .build()
            .expect("telemetry config");
    let vins = FleetTelemetryVins::parse([TEST_VIN]).expect("VIN list");
    let result = fake
        .proxy_client()
        .configure_fleet_telemetry(
            &FleetAccessToken::new(TEST_TOKEN).expect("token"),
            &vins,
            &config,
        )
        .await
        .expect("telemetry configuration");
    assert!(result.skipped_vehicles.is_empty());

    let requests = fake.state.requests.lock().expect("ledger");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/1/vehicles/fleet_telemetry_config");
    assert!(requests[0].authorization_ok);
    assert!(requests[0].content_type_json);
    let body: Value = serde_json::from_slice(&requests[0].body).expect("JSON body");
    assert_eq!(body["vins"][0], TEST_VIN);
    assert_eq!(body["config"]["hostname"], "telemetry.example.com");
    assert_eq!(body["config"]["port"], 443);
    assert_eq!(body["config"]["exp"], 1_900_000_000u64);
    assert_eq!(body["config"]["delivery_policy"], "latest");
    assert_eq!(body["config"]["fields"]["Location"]["interval_seconds"], 5);
    assert_eq!(
        body["config"]["fields"]["Version"]["interval_seconds"],
        3600
    );
    assert_eq!(result.updated_vehicles, 1);
}

#[tokio::test]
async fn fleet_telemetry_removal_uses_one_validated_vin_and_no_body() {
    let fake = FakeServer::spawn(FakeResponse::Normal).await;
    fake.proxy_client()
        .remove_fleet_telemetry(
            &FleetAccessToken::new(TEST_TOKEN).expect("token"),
            &VehicleVin::parse(TEST_VIN).expect("VIN"),
        )
        .await
        .expect("telemetry removal");

    let requests = fake.state.requests.lock().expect("ledger");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(
        requests[0].path,
        format!("/api/1/vehicles/{TEST_VIN}/fleet_telemetry_config")
    );
    assert!(requests[0].authorization_ok);
    assert!(!requests[0].content_type_json);
    assert!(requests[0].body.is_empty());
}

#[test]
fn fleet_telemetry_configuration_parses_reason_grouped_skips() {
    let requested = FleetTelemetryVins::parse([TEST_VIN, "5YJ3E1EA7KF317001", "5YJ3E1EA7KF317002"])
        .expect("requested VINs");
    let wire: FleetTelemetryConfigureWire = serde_json::from_value(serde_json::json!({
        "updated_vehicles": 1,
        "skipped_vehicles": {
            "missing_key": ["5YJ3E1EA7KF317001"],
            "unsupported_firmware": ["5YJ3E1EA7KF317002"]
        }
    }))
    .expect("Tesla response shape");
    let result = wire.into_result(&requested).expect("validated result");
    assert_eq!(result.updated_vehicles, 1);
    assert_eq!(result.skipped_vehicles.len(), 2);
    assert_eq!(result.skipped_vehicles[0].reason, "missing_key");
    assert_eq!(result.skipped_vehicles[1].reason, "unsupported_firmware");

    let inconsistent: FleetTelemetryConfigureWire = serde_json::from_value(serde_json::json!({
        "updated_vehicles": 1,
        "skipped_vehicles": {}
    }))
    .expect("response shape");
    assert_eq!(
        inconsistent.into_result(&requested),
        Err(FleetApiError::InvalidResponse)
    );
}

#[test]
fn fleet_telemetry_builder_rejects_unsafe_or_ambiguous_values() {
    let certificate = telemetry_ca_pem();
    assert_eq!(
        FleetTelemetryConfigBuilder::new(
            "https://telemetry.example.com",
            443,
            certificate.as_str(),
            1,
        )
        .with_recommended_fields()
        .build()
        .expect_err("URL is not a hostname"),
        FleetApiConfigError::InvalidTelemetryHostname
    );
    assert_eq!(
        FleetTelemetryConfigBuilder::new("telemetry.example.com", 0, certificate.as_str(), 1)
            .with_recommended_fields()
            .build()
            .expect_err("zero port"),
        FleetApiConfigError::InvalidTelemetryPort
    );
    assert_eq!(
        FleetTelemetryConfigBuilder::new("telemetry.example.com", 443, "not a certificate", 1)
            .with_recommended_fields()
            .build()
            .expect_err("invalid CA"),
        FleetApiConfigError::InvalidTelemetryCa
    );
    assert_eq!(
        FleetTelemetryConfigBuilder::new("telemetry.example.com", 443, certificate.as_str(), 1)
            .build()
            .expect_err("empty fields"),
        FleetApiConfigError::InvalidTelemetryFields
    );
    let minimum_delta = FleetTelemetryFieldConfig::new(60)
        .expect("field interval")
        .with_minimum_delta(1.0)
        .expect("minimum delta");
    assert_eq!(
        FleetTelemetryConfigBuilder::new("telemetry.example.com", 443, certificate.as_str(), 1,)
            .field(FleetTelemetryField::VehicleSpeed, minimum_delta)
            .build()
            .expect_err("minimum delta is Location-only"),
        FleetApiConfigError::InvalidTelemetryMinimumDelta
    );
    assert_eq!(
        FleetTelemetryVins::parse([TEST_VIN, TEST_VIN]).expect_err("duplicate VIN"),
        FleetApiConfigError::DuplicateTelemetryVin
    );
}

#[test]
fn fleet_telemetry_debug_redacts_ca() {
    let config = FleetTelemetryConfigBuilder::new(
        "telemetry.example.com",
        443,
        "-----BEGIN CERTIFICATE-----\nsecret-ca\n-----END CERTIFICATE-----",
        1,
    );
    assert!(!format!("{config:?}").contains("secret-ca"));
}

#[test]
fn fleet_telemetry_typed_policy_matches_ingest_mapping_policy() {
    let typed = serde_json::to_value(FleetTelemetryConfigBuilder::recommended_fields())
        .expect("typed field policy JSON");
    assert_eq!(
        typed,
        crate::fleet_telemetry::recommended_cheap_fields_config()["fields"]
    );
}

#[tokio::test]
async fn invalid_charge_limit_fails_before_transport() {
    let fake = FakeServer::spawn(FakeResponse::Normal).await;
    let error = fake
        .proxy_client()
        .execute(
            &FleetAccessToken::new(TEST_TOKEN).expect("token"),
            &VehicleVin::parse(TEST_VIN).expect("VIN"),
            FleetCommand::SetChargeLimit { percent: 49 },
        )
        .await
        .expect_err("invalid limit");
    assert_eq!(error, FleetApiError::InvalidChargeLimit);
    assert!(fake.state.requests.lock().expect("ledger").is_empty());
}

#[tokio::test]
async fn responses_are_bounded() {
    let fake = FakeServer::spawn(FakeResponse::Oversized).await;
    let error = fake
        .fleet_client()
        .wake(
            &FleetAccessToken::new(TEST_TOKEN).expect("token"),
            &VehicleVin::parse(TEST_VIN).expect("VIN"),
        )
        .await
        .expect_err("oversized response");
    assert_eq!(error, FleetApiError::ResponseTooLarge);
}

#[tokio::test]
async fn redirects_are_not_followed_and_retry_after_is_preserved() {
    let token = FleetAccessToken::new(TEST_TOKEN).expect("token");
    let vin = VehicleVin::parse(TEST_VIN).expect("VIN");

    let redirected = FakeServer::spawn(FakeResponse::Redirect).await;
    assert_eq!(
        redirected
            .fleet_client()
            .wake(&token, &vin)
            .await
            .expect_err("redirect rejected"),
        FleetApiError::HttpStatus(307)
    );
    assert_eq!(redirected.state.requests.lock().expect("ledger").len(), 1);

    let limited = FakeServer::spawn(FakeResponse::RateLimited).await;
    assert_eq!(
        limited
            .fleet_client()
            .wake(&token, &vin)
            .await
            .expect_err("rate limit surfaced"),
        FleetApiError::RateLimited {
            retry_after_seconds: 17,
        }
    );

    let timed_out = FakeServer::spawn(FakeResponse::GatewayTimeout).await;
    assert_eq!(
        timed_out
            .fleet_client()
            .wake(&token, &vin)
            .await
            .expect_err("explicit timeout status surfaced"),
        FleetApiError::HttpStatus(504)
    );
}

#[tokio::test]
async fn provider_errors_are_bounded_allowlisted_and_keep_auth_status_plain() {
    let token = FleetAccessToken::new(TEST_TOKEN).expect("token");
    let vin = VehicleVin::parse(TEST_VIN).expect("VIN");

    let provider = FakeServer::spawn(FakeResponse::ProviderError).await;
    let error = provider
        .fleet_client()
        .wake(&token, &vin)
        .await
        .expect_err("provider error surfaced");
    assert_eq!(error.http_status(), Some(400));
    assert_eq!(error.provider_error(), Some(("invalid_field", None)));
    let retained = format!("{error:?}");
    assert!(!retained.contains(TEST_VIN));
    assert!(!retained.contains(TEST_TOKEN));
    assert!(!retained.contains("not-retained"));

    let oversized = FakeServer::spawn(FakeResponse::OversizedProviderError).await;
    assert_eq!(
        oversized
            .fleet_client()
            .wake(&token, &vin)
            .await
            .expect_err("oversized error body rejected"),
        FleetApiError::HttpStatus(400)
    );

    let unauthorized = FakeServer::spawn(FakeResponse::UnauthorizedProviderError).await;
    assert_eq!(
        unauthorized
            .fleet_client()
            .wake(&token, &vin)
            .await
            .expect_err("auth status remains matchable"),
        FleetApiError::HttpStatus(401)
    );
}

#[test]
fn sensitive_or_non_printable_provider_error_text_is_not_retained() {
    for body in [
        format!(r#"{{"error":"vehicle {TEST_VIN} not_found"}}"#),
        r#"{"error":"Bearer exposed-value"}"#.to_owned(),
        r#"{"error":"-----BEGIN CERTIFICATE-----"}"#.to_owned(),
        r#"{"error":"bad\nfield"}"#.to_owned(),
        r#"{"error":"opaque_secret_material_1234567890"}"#.to_owned(),
    ] {
        assert_eq!(parse_provider_error(body.as_bytes()), None);
    }
    assert_eq!(
        parse_provider_error(br#"{"error":"vehicle_offline","error_description":"do-not-retain"}"#),
        Some(("vehicle_offline".to_owned(), None))
    );
}

#[tokio::test]
async fn connection_failure_is_known_unsent() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve local port");
    let address = listener.local_addr().expect("local address");
    drop(listener);
    let endpoint = Url::parse(&format!("http://{address}/oauth2/v3/token")).expect("fake auth URL");
    let auth = FleetAuthApi::for_fake_http(endpoint, Duration::from_millis(250))
        .expect("Fleet auth client");

    assert_eq!(
        auth.refresh(
            &FleetClientId::parse("client-id").expect("client"),
            &FleetRefreshToken::new("old-refresh").expect("refresh token"),
        )
        .await
        .expect_err("connection must fail"),
        FleetApiError::RequestNotSent
    );
}
