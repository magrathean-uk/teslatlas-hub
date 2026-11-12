// SPDX-License-Identifier: AGPL-3.0-only

//! Owner car discovery against a loopback stub Owner API (and, for the
//! resume, a loopback stub token endpoint); nothing here reaches Tesla. Every
//! identifier is invented.

use std::{sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}}, time::Duration};

use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener, task::JoinHandle};
use zeroize::Zeroizing;

use super::*;
use super::super::{OwnerSignIn, SIGN_IN_CODE_REUSED, WINDOW_CLOSED, sign_in_or_resume_owner_code_with_stub_for_test};
use crate::{
    auth::credential_vault::{CredentialEntry, fixtures::open_fixture_vault},
    config::{CollectorConfig, GeocoderConfig, ImportSourceConfig, TerrainConfig},
    db::ReplacePolicy,
    fleet_api::egress::test_support::TestWindow,
    tesla_stream::StreamRegion,
};

const ACCESS: &str = "discovery-access-0001";
const REFRESH: &str = "discovery-refresh-0001";
const VIN_ONE: &str = "5YJ3E1EA0SYNTH001";
const VIN_TWO: &str = "5YJ3E1EA0SYNTH002";
const NAME_ONE: &str = "Synthetic Car Alpha";
const NAME_TWO: &str = "Synthetic Car Beta";
const VERIFIER: &str = "synthetic-verifier-0301-abcdefghijklmnopqrstuvwxyz-0123456789";

fn two_cars() -> String {
    format!(r#"{{"response":[{{"id":7001,"vehicle_id":9001,"vin":"{VIN_ONE}","state":"asleep","display_name":"{NAME_ONE}"}},{{"id":7002,"vehicle_id":9002,"vin":"{VIN_TWO}","state":"online","display_name":"{NAME_TWO}"}}],"count":2}}"#)
}

/// How the stub answers every request.
#[derive(Clone)]
enum Reply {
    Json(u16, String),
    /// A declared length over the Owner API's answer bound.
    DeclaredOversize,
}

/// A loopback HTTP server that counts connections and keeps each request.
struct Stub {
    url: String,
    accepted: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Drop for Stub {
    fn drop(&mut self) { self.task.abort(); }
}

fn header_end(bytes: &[u8]) -> Option<usize> { bytes.windows(4).position(|window| window == b"\r\n\r\n").map(|at| at + 4) }

fn content_length(head: &[u8]) -> usize {
    std::str::from_utf8(head).unwrap_or_default().lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse().ok()).unwrap_or(0)
}

impl Stub {
    async fn spawn(reply: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("loopback stub");
        let url = format!("http://{}/", listener.local_addr().expect("stub address"));
        let accepted = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let task = {
            let (accepted, requests) = (Arc::clone(&accepted), Arc::clone(&requests));
            tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 4096];
                    loop {
                        if let Some(end) = header_end(&request) && request.len() >= end + content_length(&request[..end]) { break; }
                        match socket.read(&mut buffer).await { Ok(0) | Err(_) => break, Ok(read) => request.extend_from_slice(&buffer[..read]) }
                    }
                    requests.lock().expect("requests").push(String::from_utf8_lossy(&request).into_owned());
                    let response = match &reply {
                        Reply::Json(status, json) => format!("HTTP/1.1 {status} Synthetic\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len()).into_bytes(),
                        Reply::DeclaredOversize => format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", 4 * 1024 * 1024 + 1).into_bytes(),
                    };
                    let _ = socket.write_all(&response).await;
                    let _ = socket.shutdown().await;
                }
            })
        };
        Self { url, accepted, requests, task }
    }

    fn accepted(&self) -> usize { self.accepted.load(Ordering::SeqCst) }
}

struct Hub {
    root: tempfile::TempDir,
    store: HubStore,
    entry: CredentialVaultEntry,
}

/// A Hub with an empty store and its fixture vault.
fn hub() -> Hub {
    let root = crate::private_tempdir().expect("synthetic root");
    let store = HubStore::initialize(root.path()).expect("store");
    let entry = CredentialVaultEntry::for_test(open_fixture_vault(store.clone(), root.path()));
    Hub { root, store, entry }
}

impl Hub {
    /// Stores an own-login pair (origin hub_login, refresh owned by the Hub).
    fn seed_own_pair(&self) -> uuid::Uuid {
        let entry = CredentialEntry::own_login_fixture(Zeroizing::new(ACCESS.to_owned()), Zeroizing::new(REFRESH.to_owned())).expect("own pair");
        self.entry.vault().store_supplied_entry(entry, ReplacePolicy::ReplaceAny).expect("seeded own pair")
    }

    fn config(&self, base: &str) -> HubConfig {
        let mut collector = CollectorConfig::default();
        collector.owner_api_base_url = Some(base.to_owned());
        collector.stream_region = Some(StreamRegion::Global);
        HubConfig {
            data_dir: self.root.path().to_path_buf(),
            bind: "127.0.0.1:8080".parse().expect("bind"),
            tls: None,
            http: Default::default(),
            pairing: Default::default(),
            backup: Default::default(),
            collector,
            geocoder: GeocoderConfig { enabled: false, ..GeocoderConfig::default() },
            import_source: ImportSourceConfig::default(),
            terrain: TerrainConfig::default(),
        }
    }

    async fn discover(&self, stub: &Stub, selection: OwnerCarSelection) -> Result<OwnerDiscovery, CredentialCommandError> {
        let config = self.config(&stub.url);
        let base = url::Url::parse(&stub.url).expect("stub URL");
        discover_with(&self.entry, &self.store, &config, selection, &stub.url, move |_, _| {
            Ok(OwnerApi::for_fake_http(base, Duration::from_secs(5))?)
        }).await
    }

    fn cars(&self) -> usize { self.store.configured_tesla_vehicles().expect("configured cars").len() }

    fn pair_generation(&self) -> Option<uuid::Uuid> {
        self.entry.vault().stored_generation(CredentialSlot::OwnerApi).expect("stored generation")
    }
}

fn assert_secret_free(text: &str) {
    for secret in [ACCESS, REFRESH, VIN_ONE, VIN_TWO, NAME_ONE, NAME_TWO, VERIFIER] {
        assert!(!text.contains(secret), "a token, VIN or name appears: {text}");
    }
}

/// Outside an Open window (never opened, closed by the operator, past its
/// deadline) discovery refuses `window_closed` before any connection; the
/// stored pair stays and no car is configured.
#[tokio::test]
async fn owner_discovery_outside_an_open_window_refuses_before_any_connection() {
    for case in ["never_opened", "operator_closed", "past_deadline"] {
        let hub = hub();
        let generation = hub.seed_own_pair();
        let stub = Stub::spawn(Reply::Json(200, two_cars())).await;
        let window = match case {
            "never_opened" => TestWindow::closed(),
            "operator_closed" => { let window = TestWindow::open(600); window.close(); window }
            _ => { let window = TestWindow::open(60); window.advance(120); window }
        };
        let _route = window.route(&stub.url);
        let error = hub.discover(&stub, OwnerCarSelection::Vehicle(7001)).await.unwrap_err();
        assert_eq!(error.code(), WINDOW_CLOSED, "case {case}");
        assert_eq!(stub.accepted(), 0, "case {case}: no connection opened");
        assert_eq!(hub.cars(), 0, "case {case}: no car configured");
        assert_eq!(hub.pair_generation(), Some(generation), "case {case}: the pair is intact");
        assert_secret_free(&error.to_string());
    }
}

/// Inside an Open window a listed `--vehicle-id` is configured with one list
/// request carrying the Hub's own pair, so the collector's start condition
/// (a configured car and an admitted own pair) holds. An unlisted id, and no
/// choice among several cars, are refused and configure nothing. `--all-vehicles`
/// configures every listed car.
#[tokio::test]
async fn owner_discovery_configures_a_listed_car_and_refuses_an_unlisted_one() {
    let window = TestWindow::open(600);
    let hub = hub();
    let generation = hub.seed_own_pair();
    let stub = Stub::spawn(Reply::Json(200, two_cars())).await;
    let _route = window.route(&stub.url);

    let error = hub.discover(&stub, OwnerCarSelection::Vehicle(7999)).await.unwrap_err();
    assert_eq!(error.code(), DISCOVERY_VEHICLE_NOT_LISTED);
    assert_eq!((stub.accepted(), hub.cars()), (1, 0), "one list, no car configured");
    let error = hub.discover(&stub, OwnerCarSelection::Implicit).await.unwrap_err();
    assert_eq!(error.code(), DISCOVERY_SELECTION_REQUIRED);
    assert_eq!((stub.accepted(), hub.cars()), (2, 0), "one list, no car configured");

    let configured = hub.discover(&stub, OwnerCarSelection::Vehicle(7001)).await.expect("listed car configured");
    assert_eq!(configured.vehicles_configured, 1);
    assert_eq!(stub.accepted(), 3);
    let cars = hub.store.configured_tesla_vehicles().expect("configured cars");
    assert_eq!(cars.len(), 1);
    assert_eq!(cars[0].1, 7001, "the chosen car");
    assert_eq!(hub.pair_generation(), Some(generation), "discovery never refreshes the pair");
    let status = super::super::credential_status(&hub.store, hub.root.path(), 1_800_000_000_000).expect("credential status");
    assert!(status.legacy_credentials.present && !matches!(status.legacy_credentials.store_state, "source_credential_disabled" | "unknown_origin"),
        "the collector's start condition holds: an admitted own pair and a configured car");
    for request in stub.requests.lock().expect("requests").iter() {
        let head = request.to_ascii_lowercase();
        assert!(head.starts_with("get /api/1/products "), "only the products list is requested");
        assert!(head.contains(&format!("authorization: bearer {}", ACCESS.to_ascii_lowercase())), "the Hub's own access token");
    }

    let all = self::hub();
    all.seed_own_pair();
    let configured = all.discover(&stub, OwnerCarSelection::All).await.expect("every car configured");
    assert_eq!((configured.vehicles_configured, all.cars()), (2, 2));
}

/// An error answer or a malformed list leaves the stored pair intact and no
/// car configured, with a fixed code and no token, VIN or name in it.
#[tokio::test]
async fn owner_discovery_error_or_malformed_list_leaves_the_pair_and_no_car() {
    let cases = [
        ("unauthorised", Reply::Json(401, r#"{"error":"invalid_token"}"#.to_owned()), DISCOVERY_REFUSED),
        ("forbidden", Reply::Json(403, r#"{"error":"forbidden"}"#.to_owned()), DISCOVERY_REFUSED),
        ("server_error", Reply::Json(500, r#"{"error":"synthetic"}"#.to_owned()), DISCOVERY_REFUSED),
        ("rate_limited", Reply::Json(429, "{}".to_owned()), DISCOVERY_RATE_LIMITED),
        ("not_json", Reply::Json(200, "not-json".to_owned()), DISCOVERY_ANSWER_INVALID),
        ("no_response", Reply::Json(200, r#"{"count":0}"#.to_owned()), DISCOVERY_ANSWER_INVALID),
        ("count_mismatch", Reply::Json(200, two_cars().replace(r#""count":2"#, r#""count":3"#)), DISCOVERY_ANSWER_INVALID),
        ("bad_vin", Reply::Json(200, two_cars().replace(VIN_TWO, "SHORT")), DISCOVERY_ANSWER_INVALID),
        ("missing_id", Reply::Json(200, two_cars().replace(r#""id":7002,"#, "")), DISCOVERY_ANSWER_INVALID),
        ("oversize", Reply::DeclaredOversize, DISCOVERY_ANSWER_INVALID),
        ("no_cars", Reply::Json(200, r#"{"response":[],"count":0}"#.to_owned()), DISCOVERY_NO_VEHICLES),
    ];
    let window = TestWindow::open(600);
    for (case, reply, code) in cases {
        let hub = hub();
        let generation = hub.seed_own_pair();
        let stub = Stub::spawn(reply).await;
        let _route = window.route(&stub.url);
        let error = hub.discover(&stub, OwnerCarSelection::Vehicle(7001)).await.unwrap_err();
        assert_eq!(error.code(), code, "case {case}");
        assert_eq!(stub.accepted(), 1, "case {case}: one list, not retried");
        assert_eq!(hub.cars(), 0, "case {case}: no car configured");
        assert_eq!(hub.pair_generation(), Some(generation), "case {case}: the pair is intact");
        assert_secret_free(&format!("{error} {error:?}"));
    }
}

/// Only the Hub's own pair is used: with no pair, or with Fleet configured
/// (parked), or a bad selection, discovery refuses before any connection.
#[tokio::test]
async fn owner_discovery_uses_only_the_own_pair_and_refuses_locally() {
    let window = TestWindow::open(600);
    let stub = Stub::spawn(Reply::Json(200, two_cars())).await;
    let _route = window.route(&stub.url);

    let empty = hub();
    assert_eq!(empty.discover(&stub, OwnerCarSelection::Vehicle(7001)).await.unwrap_err().code(), DISCOVERY_PAIR_UNAVAILABLE);

    let hub = hub();
    hub.seed_own_pair();
    let mut fleet = hub.config(&stub.url);
    fleet.collector.provider = crate::config::CollectorProvider::Fleet;
    let error = discover_with(&hub.entry, &hub.store, &fleet, OwnerCarSelection::Vehicle(7001), &stub.url, |_, _| unreachable!("no client for Fleet")).await.unwrap_err();
    assert_eq!(error.code(), "provider_parked");
    assert_eq!(hub.discover(&stub, OwnerCarSelection::Vehicle(0)).await.unwrap_err().code(), DISCOVERY_SELECTION_INVALID);
    assert_eq!(owner_car_selection(Some(7001), true).unwrap_err().code(), DISCOVERY_SELECTION_INVALID);
    assert_eq!(owner_car_selection(Some(-1), false).unwrap_err().code(), DISCOVERY_SELECTION_INVALID);
    assert_eq!(owner_car_selection(None, true).expect("all"), OwnerCarSelection::All);
    assert_eq!(owner_car_selection(Some(7001), false).expect("one"), OwnerCarSelection::Vehicle(7001));
    assert_eq!(owner_car_selection(None, false).expect("implicit"), OwnerCarSelection::Implicit);
    assert_eq!(stub.accepted(), 0, "nothing sent");
    assert_eq!(hub.cars(), 0);
}

/// Discovery's logs name no token, VIN or name, on success or failure.
#[test]
fn owner_discovery_logs_carry_no_token_vin_or_name() {
    #[derive(Clone)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.lock().expect("log").extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink = Sink(Arc::clone(&logs));
    let subscriber = tracing_subscriber::fmt().without_time().with_ansi(false).with_max_level(tracing::Level::TRACE)
        .with_writer(move || sink.clone()).finish();
    let _default = tracing::subscriber::set_default(subscriber);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    runtime.block_on(async {
        let window = TestWindow::open(600);
        let stub = Stub::spawn(Reply::Json(200, two_cars())).await;
        let _route = window.route(&stub.url);
        let hub = hub();
        hub.seed_own_pair();
        hub.discover(&stub, OwnerCarSelection::All).await.expect("configured");
        let other = self::hub();
        other.seed_own_pair();
        other.discover(&stub, OwnerCarSelection::Implicit).await.unwrap_err();
    });
    let logs = logs.lock().expect("log");
    let logs = std::str::from_utf8(&logs).expect("UTF-8 logs");
    assert!(logs.contains("owner setup configured its cars") && logs.contains("owner setup discovery failed"), "both outcomes are logged");
    assert_secret_free(logs);
}

/// A failed discovery after a stored sign-in is finished without a second
/// Tesla login: the same material again resumes the sign-in with no token
/// request while that pair is still stored, then discovery succeeds. Other
/// material is still exchanged (and a used code still refused).
#[tokio::test]
async fn owner_discovery_repeat_setup_resumes_without_a_second_login() {
    let window = TestWindow::open(600);
    let answer = format!(r#"{{"access_token":"{ACCESS}","refresh_token":"{REFRESH}","expires_in":28800,"token_type":"Bearer"}}"#);
    let issuer = Stub::spawn(Reply::Json(200, answer)).await;
    let failing = Stub::spawn(Reply::Json(500, "{}".to_owned())).await;
    let listing = Stub::spawn(Reply::Json(200, two_cars())).await;
    let (_issuer_route, _failing_route, _listing_route) = (window.route(&issuer.url), window.route(&failing.url), window.route(&listing.url));
    let token = format!("{}oauth2/v3/token", issuer.url);
    let hub = hub();
    let redirect = super::super::OWNER_REDIRECT_URI;

    let first = sign_in_or_resume_owner_code_with_stub_for_test(&hub.entry, "code-discovery-0001", VERIFIER, redirect, &token).await.expect("exchanged");
    assert_eq!(first.sign_in(), "exchanged");
    assert_eq!(issuer.accepted(), 1);
    let generation = hub.pair_generation();
    assert_eq!(hub.discover(&failing, OwnerCarSelection::Vehicle(7002)).await.unwrap_err().code(), DISCOVERY_REFUSED);
    assert_eq!(hub.cars(), 0);

    let resumed = sign_in_or_resume_owner_code_with_stub_for_test(&hub.entry, "code-discovery-0001", VERIFIER, redirect, &token).await.expect("resumed");
    assert_eq!((resumed, resumed.aftercare()), (OwnerSignIn::Resumed, "unchanged"));
    assert_eq!(issuer.accepted(), 1, "no second token request");
    assert_eq!(hub.pair_generation(), generation, "the same pair");
    assert_eq!(hub.discover(&listing, OwnerCarSelection::Vehicle(7002)).await.expect("configured").vehicles_configured, 1);

    let other_verifier = "synthetic-verifier-0302-abcdefghijklmnopqrstuvwxyz-0123456789";
    let refused = sign_in_or_resume_owner_code_with_stub_for_test(&hub.entry, "code-discovery-0001", other_verifier, redirect, &token).await.unwrap_err();
    assert_eq!(refused.code(), SIGN_IN_CODE_REUSED, "other material is not resumed");
    assert_eq!(issuer.accepted(), 1);

    // Once the pair is replaced, the old material no longer resumes.
    hub.entry.vault().clear_credential_pair(CredentialSlot::OwnerApi).expect("cleared");
    let refused = sign_in_or_resume_owner_code_with_stub_for_test(&hub.entry, "code-discovery-0001", VERIFIER, redirect, &token).await.unwrap_err();
    assert_eq!(refused.code(), SIGN_IN_CODE_REUSED);
    assert_eq!(issuer.accepted(), 1);
}
