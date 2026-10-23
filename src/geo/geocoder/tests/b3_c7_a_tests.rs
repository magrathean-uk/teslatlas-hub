// SPDX-License-Identifier: AGPL-3.0-only

// C7 part A tests (spec sections 7.1 and 7.4, C7 T1-T14, C7 T55 and C7 T57-T63): the
// reverse request URL, the exact request, status and redirect handling,
// admission before and after the reserved wait, pacing and the limiter. Every
// request goes to a loopback stub on an ephemeral port through a geocoder with
// its own private limiter; no test reaches the public host. Bodies are
// hand-written synthetic text.

use crate::config::{ConfigError, GeocoderConfig};
use crate::db::HubStore;
use crate::geocoder::{
    EgressGuard, EgressGuardError, FIXED_USER_AGENT, GeocodedAddress, Geocoder, GeocoderError,
    MAX_RESPONSE_BYTES, MIN_REQUEST_INTERVAL, RequestFailure, SharedLimiter, global_limiter,
    is_public_host, parse_response, request_interval, retry_after_seconds, reverse_request_url,
    seeded_limiter, slot_free, wait_for_slot_every,
};
use crate::location::Wgs84Point;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RETRY_AFTER};
use std::future::Future;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

/// C7 fixture F1, the answer of a building-level lookup.
const F1: &str = r#"{"place_id":7001,"licence":"Data (c) test contributors, ODbL 1.0","osm_type":"way","osm_id":"880001","lat":"52.4100000","lon":"-1.5200000","place_rank":30,"category":"building","type":"house","addresstype":"building","name":"","display_name":"14, Fernhill Road, Larkfield, Ashcombe, West Wexshire, England, AB1 2CD, United Kingdom","address":{"house_number":"14","road":"Fernhill Road","suburb":"Larkfield","city":"Ashcombe","county":"West Wexshire","state":"England","ISO3166-2-lvl4":"GB-ENG","postcode":"AB1 2CD","country":"United Kingdom","country_code":"gb"}}"#;

const HTML: &str =
    "<!doctype html><html><head><title>Example</title></head><body>Maintenance</body></html>";

/// The URL of C7 T1, C7 T3 and C7 T7 for point (51.5, -0.25) and language `en-GB`,
/// without the scheme and authority.
const T1_TARGET: &str =
    "/reverse?format=jsonv2&lat=51.5&lon=-0.25&zoom=18&addressdetails=1&accept-language=en-GB";

/// Text that no error of a request may contain (C7 T14).
const FORBIDDEN_IN_ERRORS: [&str; 11] = [
    "51.5",
    "-0.25",
    "52.4",
    "Unable",
    "message",
    "lat=",
    "pw@",
    "key=abc",
    "Maintenance",
    "120",
    "nominatim.openstreetmap.org",
];

fn point(latitude: f64, longitude: f64) -> Wgs84Point {
    Wgs84Point::new(latitude, longitude).expect("a valid point")
}

fn url(text: &str) -> Url {
    Url::parse(text).expect("a valid URL")
}

/// Run an async test body under the C7 timeout rule: `requests` x 1 s + 5 s,
/// where `requests` counts the requests through one geocoder, and never more
/// than the 10 s cap of the spec.
async fn within<T>(requests: u64, body: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs((requests + 5).min(10)), body)
        .await
        .expect("the test finishes within its timeout")
}

// ---- the loopback stub ------------------------------------------------------

/// One request the stub received: its head text and the instant it arrived.
#[derive(Clone)]
struct Captured {
    head: String,
    at: Instant,
}

/// What the stub answers.
#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    /// The `Content-Length` to declare when it differs from the body.
    declared_length: Option<usize>,
}

fn reply(status: u16, body: &str) -> Reply {
    Reply {
        status,
        headers: vec![("Content-Type", "application/json".to_owned())],
        body: body.as_bytes().to_vec(),
        declared_length: None,
    }
}

impl Reply {
    fn with_header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_owned()));
        self
    }
}

struct Stub {
    endpoint: String,
    requests: Arc<Mutex<Vec<Captured>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Stub {
    async fn start(answer: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let endpoint = format!("http://{}", listener.local_addr().expect("address"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(serve(stream, answer.clone(), Arc::clone(&captured)));
            }
        });
        Self {
            endpoint,
            requests,
            task,
        }
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn captured(&self) -> Vec<Captured> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut stream: TcpStream, answer: Reply, captured: Arc<Mutex<Vec<Captured>>>) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
        }
    }
    captured.lock().unwrap().push(Captured {
        head: String::from_utf8_lossy(&bytes).into_owned(),
        at: Instant::now(),
    });
    let mut head = format!("HTTP/1.1 {} Stub\r\n", answer.status);
    for (name, value) in &answer.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        answer.declared_length.unwrap_or(answer.body.len())
    ));
    // The client may close at once (it does not read the body of an error
    // status), so write errors end the answer and are not failures.
    if stream.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    let _ = stream.write_all(&answer.body).await;
    let _ = stream.shutdown().await;
}

/// The value of header `name` in a captured request head.
fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

// ---- geocoders and guards -----------------------------------------------------

fn config_for(endpoint: &str, language: &str) -> GeocoderConfig {
    GeocoderConfig {
        enabled: true,
        endpoint: Some(endpoint.to_owned()),
        language: language.to_owned(),
        timeout_seconds: 2,
    }
}

/// A test-mode geocoder with a fresh private limiter, returned with that limiter.
fn geocoder_with_limiter(endpoint: &str, language: &str) -> (Geocoder, SharedLimiter) {
    let limiter: SharedLimiter = Arc::new(Mutex::new(None));
    let geocoder = Geocoder::build(&config_for(endpoint, language), true, Arc::clone(&limiter))
        .expect("geocoder");
    (geocoder, limiter)
}

fn geocoder_for(endpoint: &str) -> Geocoder {
    geocoder_with_limiter(endpoint, "en-GB").0
}

/// A guard that permits its first `permitted` checks, refuses every later one
/// and counts all of them.
struct CountingGuard {
    permitted: usize,
    checks: AtomicUsize,
}

impl CountingGuard {
    fn permitting() -> Self {
        Self::permitting_first(usize::MAX)
    }

    fn refusing() -> Self {
        Self::permitting_first(0)
    }

    fn permitting_first(permitted: usize) -> Self {
        Self {
            permitted,
            checks: AtomicUsize::new(0),
        }
    }

    fn checks(&self) -> usize {
        self.checks.load(Ordering::SeqCst)
    }
}

impl EgressGuard for CountingGuard {
    fn assert_egress_allowed(&self) -> Result<(), EgressGuardError> {
        let seen = self.checks.fetch_add(1, Ordering::SeqCst);
        if seen < self.permitted {
            Ok(())
        } else {
            Err(EgressGuardError)
        }
    }
}

fn assert_text_is_safe(error: &GeocoderError) {
    for text in [error.to_string(), format!("{error:?}")] {
        for forbidden in FORBIDDEN_IN_ERRORS {
            assert!(
                !text.contains(forbidden),
                "error text {text:?} contains {forbidden:?}"
            );
        }
    }
}

// ---- C7 T1 to C7 T6: the URL --------------------------------------------------------

#[test]
fn c7_t01_to_t06_url() {
    // C7 T1: a base endpoint gets the six parameters in order.
    let built = reverse_request_url(&url("http://127.0.0.1:8080"), point(51.5, -0.25), "en-GB")
        .expect("URL");
    assert_eq!(
        built.as_str(),
        "http://127.0.0.1:8080/reverse?format=jsonv2&lat=51.5&lon=-0.25&zoom=18&addressdetails=1&accept-language=en-GB"
    );

    // C7 T2: the path rule.
    for (endpoint, path) in [
        ("https://geo.example.test/nominatim", "/nominatim/reverse"),
        ("https://geo.example.test/nominatim/", "/nominatim/reverse"),
        ("https://geo.example.test/reverse", "/reverse"),
        ("https://geo.example.test/reverse/", "/reverse"),
        (
            "https://geo.example.test/api/reverse.php",
            "/api/reverse.php",
        ),
        ("https://geo.example.test/Reverse", "/Reverse/reverse"),
        ("https://geo.example.test", "/reverse"),
    ] {
        let built = reverse_request_url(&url(endpoint), point(51.5, -0.25), "en-GB")
            .unwrap_or_else(|_| panic!("{endpoint} is accepted"));
        assert_eq!(built.path(), path, "{endpoint}");
    }

    // C7 T3: userinfo and a non-empty query are refused, a fragment and an empty
    // query are dropped.
    for endpoint in [
        "https://user:pw@geo.example.test/base",
        "https://user@geo.example.test/base",
        "https://geo.example.test/base?key=abc",
        "https://user:pw@geo.example.test/base?key=abc&zoom=3#frag",
    ] {
        let refused = reverse_request_url(&url(endpoint), point(51.5, -0.25), "en-GB");
        assert!(
            matches!(refused, Err(GeocoderError::InvalidEndpoint)),
            "{endpoint}"
        );
    }
    for endpoint in [
        "https://geo.example.test/base#frag",
        "https://geo.example.test/base?",
    ] {
        let built = reverse_request_url(&url(endpoint), point(51.5, -0.25), "en-GB")
            .unwrap_or_else(|_| panic!("{endpoint} is accepted"));
        assert_eq!(
            built.as_str(),
            "https://geo.example.test/base/reverse?format=jsonv2&lat=51.5&lon=-0.25&zoom=18&addressdetails=1&accept-language=en-GB",
            "{endpoint}"
        );
    }

    // C7 T4: the language is form-encoded.
    let built = reverse_request_url(
        &url("https://geo.example.test/base"),
        point(51.5, -0.25),
        "de,en;q=0.5",
    )
    .expect("URL");
    assert!(
        built
            .as_str()
            .ends_with("&accept-language=de%2Cen%3Bq%3D0.5"),
        "{built}"
    );

    // C7 T5: the coordinate text.
    let endpoint = url("https://geo.example.test");
    for (given, lat, lon) in [
        (point(-0.0, 0.0), "lat=0", "lon=0"),
        (
            point(0.000_000_1, 179.999_999_9),
            "lat=0.0000001",
            "lon=179.9999999",
        ),
        (
            point(-33.86785, 151.20732),
            "lat=-33.86785",
            "lon=151.20732",
        ),
    ] {
        let built = reverse_request_url(&endpoint, given, "en-GB").expect("URL");
        let query = built.query().expect("query");
        let pairs: Vec<&str> = query.split('&').collect();
        assert_eq!(pairs[1], lat, "{query}");
        assert_eq!(pairs[2], lon, "{query}");
    }

    // C7 T6: a URL that cannot be a base.
    assert!(matches!(
        reverse_request_url(&url("mailto:geo@example.test"), point(51.5, -0.25), "en-GB"),
        Err(GeocoderError::InvalidEndpoint)
    ));
}

// ---- C7 T58: the public host ------------------------------------------------------

#[test]
fn c7_t58_public_host() {
    for endpoint in [
        "https://nominatim.openstreetmap.org/",
        "https://NOMINATIM.OpenStreetMap.org/",
        "https://nominatim.openstreetmap.org./",
        "https://nominatim.openstreetmap.org:8443/base/",
        "http://nominatim.openstreetmap.org/",
    ] {
        assert!(is_public_host(&url(endpoint)), "{endpoint}");
    }
    for endpoint in [
        "https://nominatim.openstreetmap.org.example.test/",
        "https://other-nominatim.openstreetmap.org/",
        "https://openstreetmap.org/",
        "https://geo.example.test/nominatim.openstreetmap.org/",
        "http://127.0.0.1:8080/",
    ] {
        assert!(!is_public_host(&url(endpoint)), "{endpoint}");
    }
}

// ---- C7 T7: the request head -------------------------------------------------------

#[tokio::test]
async fn c7_t07_headers() {
    within(1, async {
        let stub = Stub::start(reply(200, F1)).await;
        let result = geocoder_for(&stub.endpoint)
            .reverse_with_egress_guard(point(51.5, -0.25), &CountingGuard::permitting())
            .await
            .expect("an address");
        assert_eq!(result, parse_response(F1.as_bytes()).unwrap());

        let seen = stub.captured();
        assert_eq!(seen.len(), 1);
        let head = &seen[0].head;
        assert_eq!(
            head.lines().next(),
            Some(format!("GET {T1_TARGET} HTTP/1.1").as_str())
        );
        assert_eq!(header_value(head, "user-agent"), Some(FIXED_USER_AGENT));
        assert_eq!(header_value(head, "accept"), Some("application/json"));
        assert_eq!(header_value(head, "accept-language"), Some("en-GB"));
        for absent in [
            "referer",
            "cookie",
            "authorization",
            "from",
            "transfer-encoding",
        ] {
            assert_eq!(header_value(head, absent), None, "{absent}");
        }
        // A GET carries no body.
        assert!(matches!(
            header_value(head, "content-length"),
            None | Some("0")
        ));
    })
    .await;
}

// ---- C7 T8: statuses -----------------------------------------------------------------

#[tokio::test]
async fn c7_t08_status() {
    within(1, async {
        let mut oversize = reply(500, "");
        oversize.body = vec![b'x'; MAX_RESPONSE_BYTES + 1];
        let cases = [
            (reply(404, r#"{"error":"Unable to geocode"}"#), 404),
            (reply(403, "{}"), 403),
            (reply(429, "{}"), 429),
            (reply(500, "{}"), 500),
            (reply(503, "{}"), 503),
            (
                reply(
                    400,
                    r#"{"error":{"code":400,"message":"lat=51.5 invalid"}}"#,
                ),
                400,
            ),
            // The body is never read: not `ResponseTooLarge`, not `Timeout`.
            (oversize, 500),
        ];
        for (answer, status) in cases {
            let stub = Stub::start(answer).await;
            let error = geocoder_for(&stub.endpoint)
                .reverse_with_egress_guard(point(51.5, -0.25), &CountingGuard::permitting())
                .await
                .expect_err("a refused status");
            assert!(
                matches!(error, GeocoderError::HttpStatus(code) if code == status),
                "status {status} gave {error:?}"
            );
            assert_eq!(stub.count(), 1, "status {status}");
        }
    })
    .await;
}

// ---- C7 T9: no redirect --------------------------------------------------------------

#[tokio::test]
async fn c7_t09_redirect() {
    within(1, async {
        let target = Stub::start(reply(200, F1)).await;
        let redirecting = Stub::start(
            reply(302, "").with_header("Location", &format!("{}/reverse", target.endpoint)),
        )
        .await;
        let error = geocoder_for(&redirecting.endpoint)
            .reverse_with_egress_guard(point(51.5, -0.25), &CountingGuard::permitting())
            .await
            .expect_err("a redirect is an error");
        assert!(matches!(error, GeocoderError::HttpStatus(302)), "{error:?}");
        assert_eq!(redirecting.count(), 1);
        assert_eq!(target.count(), 0);
    })
    .await;
}

// ---- C7 T10 and C7 T11: admission -----------------------------------------------------

#[tokio::test]
async fn c7_t10_admission() {
    within(1, async {
        // C7 T10: a success status with an `error` member is `NoResult`.
        let stub = Stub::start(reply(200, r#"{"error":"Unable to geocode"}"#)).await;
        let error = geocoder_for(&stub.endpoint)
            .reverse_with_egress_guard(point(51.5, -0.25), &CountingGuard::permitting())
            .await
            .expect_err("no result");
        assert!(matches!(error, GeocoderError::NoResult), "{error:?}");
        assert_eq!(stub.count(), 1);

        // C7 T11 first case: a guard that refuses from the start sends nothing
        // and reserves no slot (C7 R3).
        let stub = Stub::start(reply(200, F1)).await;
        let (geocoder, limiter) = geocoder_with_limiter(&stub.endpoint, "en-GB");
        let guard = CountingGuard::refusing();
        let error = geocoder
            .reverse_with_egress_guard(point(51.5, -0.25), &guard)
            .await
            .expect_err("refused");
        assert!(matches!(error, GeocoderError::EgressDenied), "{error:?}");
        assert_eq!(guard.checks(), 1);
        assert_eq!(stub.count(), 0);
        assert!(limiter.lock().unwrap().is_none());
    })
    .await;
}

#[tokio::test]
async fn c7_t11_revoke() {
    within(2, async {
        let stub = Stub::start(reply(200, F1)).await;
        let (geocoder, limiter) = geocoder_with_limiter(&stub.endpoint, "en-GB");
        let here = point(51.5, -0.25);

        // Call 1 succeeds and takes the first slot.
        geocoder
            .reverse_with_egress_guard(here, &CountingGuard::permitting())
            .await
            .expect("call 1");
        let first_slot = limiter.lock().unwrap().expect("a reserved slot");

        // Call 2 is refused before the wait: it reserves no slot.
        let refused = geocoder
            .reverse_with_egress_guard(here, &CountingGuard::refusing())
            .await
            .expect_err("call 2");
        assert!(matches!(refused, GeocoderError::EgressDenied));
        assert_eq!(*limiter.lock().unwrap(), Some(first_slot));

        // Call 3 takes the slot right after call 1's: one interval, not two.
        geocoder
            .reverse_with_egress_guard(here, &CountingGuard::permitting())
            .await
            .expect("call 3");
        let third_slot = limiter.lock().unwrap().expect("a reserved slot");
        assert_eq!(third_slot, first_slot + MIN_REQUEST_INTERVAL);
        assert_eq!(stub.count(), 2);
        assert!(stub.captured()[1].at >= third_slot);

        // Admission revoked after the slot was reserved (the second check): no
        // request is sent, and the reserved slot stays used.
        let revoked = CountingGuard::permitting_first(1);
        let error = geocoder
            .reverse_with_egress_guard(here, &revoked)
            .await
            .expect_err("revoked during the wait");
        assert!(matches!(error, GeocoderError::EgressDenied), "{error:?}");
        assert_eq!(revoked.checks(), 2);
        assert_eq!(stub.count(), 2);
        assert_eq!(
            *limiter.lock().unwrap(),
            Some(third_slot + MIN_REQUEST_INTERVAL)
        );
    })
    .await;
}

// ---- C7 T12: cache first -----------------------------------------------------------

#[tokio::test]
async fn c7_t12_cache() {
    within(1, async {
        let stub = Stub::start(reply(200, F1)).await;
        let temporary = crate::private_tempdir().expect("temporary directory");
        let here = point(52.41, -1.52);
        let expected = parse_response(F1.as_bytes()).unwrap();
        {
            let store = HubStore::initialize(temporary.path()).expect("store");
            let geocoder = geocoder_for(&stub.endpoint);
            let first = geocoder
                .reverse_cached(&store, here, 1000)
                .await
                .expect("first");
            assert_eq!(first, expected);
            assert_eq!(stub.count(), 1);
            let second = geocoder
                .reverse_cached(&store, here, 2000)
                .await
                .expect("second");
            assert_eq!(second, expected);
            assert_eq!(stub.count(), 1);

            // A cache hit returns before any admission check, wait or request.
            let guard = CountingGuard::refusing();
            let hit = geocoder
                .reverse_cached_with_egress_guard(&store, here, 3000, &guard)
                .await
                .expect("a hit under a refusing guard");
            assert_eq!(hit, expected);
            assert_eq!(guard.checks(), 0);
            assert_eq!(stub.count(), 1);
        }

        // After a restart a new geocoder and a reopened store still hit.
        let reopened = HubStore::initialize(temporary.path()).expect("reopened store");
        let again = geocoder_for(&stub.endpoint)
            .reverse_cached(&reopened, here, 4000)
            .await
            .expect("after restart");
        assert_eq!(again, expected);
        assert_eq!(stub.count(), 1);
    })
    .await;
}

// ---- C7 T13: rate --------------------------------------------------------------------

#[tokio::test]
async fn c7_t13_rate() {
    within(2, async {
        assert!(MIN_REQUEST_INTERVAL >= Duration::from_secs(1));
        assert_eq!(
            request_interval(&url("https://geo.example.test/")),
            MIN_REQUEST_INTERVAL
        );

        // Two requests through one geocoder: the second slot is exactly one
        // interval after the first, and the second request starts no earlier.
        let stub = Stub::start(reply(200, F1)).await;
        let (geocoder, limiter) = geocoder_with_limiter(&stub.endpoint, "en-GB");
        let guard = CountingGuard::permitting();
        geocoder
            .reverse_with_egress_guard(point(51.5, -0.25), &guard)
            .await
            .expect("first");
        let first_slot = limiter.lock().unwrap().expect("first slot");
        geocoder
            .reverse_with_egress_guard(point(51.5, -0.25), &guard)
            .await
            .expect("second");
        let second_slot = limiter.lock().unwrap().expect("second slot");
        assert_eq!(second_slot, first_slot + Duration::from_secs(1));
        let seen = stub.captured();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].at >= first_slot);
        assert!(seen[1].at >= second_slot);
    })
    .await;
}

// ---- C7 T14: errors carry no secret or location text -----------------------------------

#[tokio::test]
async fn c7_t14_safe_errors() {
    within(1, async {
        let here = point(51.5, -0.25);
        let mut errors: Vec<GeocoderError> = Vec::new();

        // C7 T3 and C7 T6.
        for endpoint in [
            "https://user:pw@geo.example.test/base",
            "https://user@geo.example.test/base",
            "https://geo.example.test/base?key=abc",
            "https://user:pw@geo.example.test/base?key=abc&zoom=3#frag",
            "mailto:geo@example.test",
        ] {
            errors.push(
                reverse_request_url(&url(endpoint), here, "en-GB").expect_err("a refused endpoint"),
            );
        }

        // C7 T8, C7 T9, C7 T10, C7 T55: refused answers whose bodies carry the forbidden text.
        let mut answers = vec![
            reply(404, r#"{"error":"Unable to geocode"}"#),
            reply(403, "{}").with_header("Retry-After", "120"),
            reply(429, "{}").with_header("Retry-After", "120"),
            reply(500, "{}"),
            reply(
                400,
                r#"{"error":{"code":400,"message":"lat=51.5 invalid"}}"#,
            ),
            reply(302, "").with_header("Location", "http://127.0.0.1:9/reverse?lat=51.5"),
            reply(200, r#"{"error":"Unable to geocode"}"#),
            reply(200, HTML),
            reply(200, "[]"),
        ];
        // C7 T55 also answers with a text/html content type.
        answers[7].headers = vec![("Content-Type", "text/html".to_owned())];
        for answer in answers {
            let stub = Stub::start(answer).await;
            errors.push(
                geocoder_for(&stub.endpoint)
                    .reverse_with_egress_guard(here, &CountingGuard::permitting())
                    .await
                    .expect_err("a failed lookup"),
            );
        }

        // C7 T11: a refusing guard.
        let stub = Stub::start(reply(200, F1)).await;
        errors.push(
            geocoder_for(&stub.endpoint)
                .reverse_with_egress_guard(here, &CountingGuard::refusing())
                .await
                .expect_err("refused"),
        );

        // C7 T57: a language that is not a header value.
        let mut geocoder = geocoder_for(&stub.endpoint);
        geocoder.language = "日本語".to_owned();
        errors.push(
            geocoder
                .reverse_with_egress_guard(here, &CountingGuard::permitting())
                .await
                .expect_err("a bad language"),
        );

        // C7 T59: the public host in a test build.
        let public = Geocoder::build(
            &config_for("https://nominatim.openstreetmap.org/", "en-GB"),
            false,
            Arc::new(Mutex::new(None)),
        )
        .expect("geocoder");
        // The guard permits only the first check, so a send that got past the
        // test-build guard would be refused before it left the machine.
        let error = public
            .reverse_with_egress_guard(here, &CountingGuard::permitting_first(1))
            .await
            .expect_err("the public host");
        assert!(matches!(error, GeocoderError::InvalidEndpoint));
        errors.push(error);

        assert_eq!(errors.len(), 5 + 9 + 1 + 1 + 1);
        for error in &errors {
            assert_text_is_safe(error);
        }
    })
    .await;
}

// ---- C7 T55: a success status that is not a reverse answer ------------------------------

#[tokio::test]
async fn c7_t55_wrong_format() {
    within(2, async {
        let mut html = reply(200, HTML);
        html.headers = vec![("Content-Type", "text/html".to_owned())];
        let stub = Stub::start(html).await;
        let error = geocoder_for(&stub.endpoint)
            .reverse_with_egress_guard(point(51.5, -0.25), &CountingGuard::permitting())
            .await
            .expect_err("an HTML page");
        assert!(matches!(error, GeocoderError::ResponseRead), "{error:?}");
        assert_eq!(stub.count(), 1);

        let stub = Stub::start(reply(200, "[]")).await;
        let error = geocoder_for(&stub.endpoint)
            .reverse_with_egress_guard(point(51.5, -0.25), &CountingGuard::permitting())
            .await
            .expect_err("an array");
        assert!(matches!(error, GeocoderError::ResponseRead), "{error:?}");
    })
    .await;
}

// ---- C7 T57: a language that cannot be a header value -------------------------------------

#[tokio::test]
async fn c7_t57_bad_language() {
    within(1, async {
        let stub = Stub::start(reply(200, F1)).await;
        let (mut geocoder, limiter) = geocoder_with_limiter(&stub.endpoint, "en-GB");
        geocoder.language = "日本語".to_owned();
        let guard = CountingGuard::permitting();
        let error = geocoder
            .reverse_with_egress_guard(point(51.5, -0.25), &guard)
            .await
            .expect_err("a bad language");
        assert!(
            matches!(
                error,
                GeocoderError::InvalidConfig(ConfigError::InvalidGeocoderLanguage)
            ),
            "{error:?}"
        );
        assert_eq!(guard.checks(), 0);
        assert_eq!(stub.count(), 0);
        assert!(limiter.lock().unwrap().is_none());
    })
    .await;
}

// ---- C7 T59: the test-build guard ---------------------------------------------------------

#[tokio::test]
async fn c7_t59_test_guard() {
    within(1, async {
        let here = point(51.5, -0.25);
        for endpoint in [
            "https://nominatim.openstreetmap.org/",
            "https://NOMINATIM.openstreetmap.org./base",
        ] {
            let public = Geocoder::build(
                &config_for(endpoint, "en-GB"),
                false,
                Arc::new(Mutex::new(None)),
            )
            .expect("geocoder");
            // Fail closed: the guard permits only its first check, so an
            // implementation that reached admission would be refused after the
            // wait and before any send; the public host is never reached.
            let guard = CountingGuard::permitting_first(1);
            let error = tokio::time::timeout(
                Duration::from_millis(500),
                public.reverse_with_egress_guard(here, &guard),
            )
            .await
            .expect("an immediate refusal")
            .expect_err("the public host is refused in a test build");
            assert!(
                matches!(error, GeocoderError::InvalidEndpoint),
                "{endpoint}"
            );
            assert_eq!(guard.checks(), 0, "{endpoint}");
            // The refusal also reserved no slot.
            assert!(public.limiter.lock().unwrap().is_none(), "{endpoint}");
        }

        // The cached entry point stops there too and writes nothing.
        let temporary = crate::private_tempdir().expect("temporary directory");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let public = Geocoder::build(
            &config_for("https://nominatim.openstreetmap.org/", "en-GB"),
            false,
            Arc::new(Mutex::new(None)),
        )
        .expect("geocoder");
        let guard = CountingGuard::permitting_first(1);
        let error = public
            .reverse_cached_with_egress_guard(&store, point(52.41, -1.52), 1000, &guard)
            .await
            .expect_err("the public host is refused in a test build");
        assert!(matches!(error, GeocoderError::InvalidEndpoint));
        assert_eq!(guard.checks(), 0);
        let connection = store.open().expect("connection");
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM address_lookup_cache", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(rows, 0);
        // It also wrote no provider-state row (C7 K-STATE-KEY).
        assert_eq!(store.geocoder_provider_state().expect("row"), None);

        // C7-C carry (C7 R56, C7 R57): the default configuration has no endpoint, which
        // means the public host, so the default-config geocoder is refused in a test
        // build before any admission check, rate wait or request, by both entry
        // points, and the cached one leaves no lookup row and no provider-state row.
        let default_geocoder = Geocoder::build(
            &GeocoderConfig::default(),
            false,
            Arc::new(Mutex::new(None)),
        )
        .expect("the default configuration builds");
        assert!(is_public_host(&default_geocoder.endpoint));
        let guard = CountingGuard::permitting_first(1);
        let error = tokio::time::timeout(
            Duration::from_millis(500),
            default_geocoder.reverse_with_egress_guard(here, &guard),
        )
        .await
        .expect("an immediate refusal")
        .expect_err("the public host is refused in a test build");
        assert!(matches!(error, GeocoderError::InvalidEndpoint));
        assert_eq!(guard.checks(), 0);
        assert!(default_geocoder.limiter.lock().unwrap().is_none());

        let temporary = crate::private_tempdir().expect("temporary directory");
        let store = HubStore::initialize(temporary.path()).expect("store");
        let error = default_geocoder
            .reverse_cached(&store, point(52.41, -1.52), 1000)
            .await
            .expect_err("the public host is refused in a test build");
        assert!(matches!(error, GeocoderError::InvalidEndpoint));
        let connection = store.open().expect("connection");
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM address_lookup_cache", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(rows, 0);
        assert_eq!(store.geocoder_provider_state().expect("row"), None);
        assert!(default_geocoder.limiter.lock().unwrap().is_none());
    })
    .await;
}

// ---- C7 T60: Retry-After -------------------------------------------------------------------

fn retry_after_of(values: &[&[u8]]) -> Option<u64> {
    let mut headers = HeaderMap::new();
    for value in values {
        headers.append(
            RETRY_AFTER,
            HeaderValue::from_bytes(value).expect("a header value"),
        );
    }
    retry_after_seconds(&headers)
}

#[tokio::test]
async fn c7_t60_retry_after() {
    // The header parser.
    assert_eq!(retry_after_of(&[b"120"]), Some(120));
    assert_eq!(retry_after_of(&[b" 30 "]), Some(30));
    assert_eq!(retry_after_of(&[b"\t30\t"]), Some(30));
    assert_eq!(retry_after_of(&[b"0"]), Some(0));
    assert_eq!(
        retry_after_of(&[b"99999999999999999999999"]),
        Some(u64::MAX)
    );
    for value in [
        &b"Wed, 21 Oct 2026 07:28:00 GMT"[..],
        b"-5",
        b"1.5",
        b"+7",
        b"",
        b"   ",
        b"12a",
    ] {
        assert_eq!(retry_after_of(&[value]), None, "{value:?}");
    }
    assert_eq!(retry_after_of(&[b"5", b"7"]), None);
    assert_eq!(retry_after_seconds(&HeaderMap::new()), None);
    let mut other = HeaderMap::new();
    other.insert(
        HeaderName::from_static("x-retry-after"),
        HeaderValue::from_static("5"),
    );
    assert_eq!(retry_after_seconds(&other), None);

    // `send_reverse` carries the delay for 403 and 429 only, and the public
    // result is still the status.
    within(1, async {
        let cases = [
            (403, Some("120"), Some(120)),
            (429, Some("120"), Some(120)),
            (429, Some("Wed, 21 Oct 2026 07:28:00 GMT"), None),
            (429, None, None),
            (500, Some("120"), None),
            (404, Some("120"), None),
        ];
        for (status, header, expected) in cases {
            let mut answer = reply(status, "{}");
            if let Some(value) = header {
                answer = answer.with_header("Retry-After", value);
            }
            let stub = Stub::start(answer).await;
            let failure: RequestFailure = geocoder_for(&stub.endpoint)
                .send_reverse(point(51.5, -0.25), &CountingGuard::permitting())
                .await
                .expect_err("a refused status");
            assert!(
                matches!(failure.error, GeocoderError::HttpStatus(code) if code == status),
                "{status}"
            );
            assert_eq!(failure.retry_after_seconds, expected, "{status} {header:?}");
        }
    })
    .await;
}

// ---- C7 T61: interval and reservation --------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn c7_t61_reservation() {
    assert_eq!(
        request_interval(&url("https://nominatim.openstreetmap.org/")),
        Duration::from_secs(15)
    );
    assert_eq!(
        request_interval(&url("https://geo.example.test/")),
        Duration::from_secs(1)
    );

    let started = Instant::now();
    let limiter: SharedLimiter = Arc::new(Mutex::new(Some(started)));
    wait_for_slot_every(&limiter, Duration::from_secs(15))
        .await
        .expect("first slot");
    assert_eq!(
        *limiter.lock().unwrap(),
        Some(started + Duration::from_secs(15))
    );
    wait_for_slot_every(&limiter, Duration::from_secs(15))
        .await
        .expect("second slot");
    assert_eq!(
        *limiter.lock().unwrap(),
        Some(started + Duration::from_secs(30))
    );

    // A limiter that holds nothing returns at once and holds an instant no
    // earlier than one taken just before the call and no later than one taken
    // just after it. The limiter stores real instants while the sleep runs on
    // the paused clock, so the upper bound is what shows that an empty limiter
    // reserved "now" and not "now + interval".
    let before = Instant::now();
    let fresh: SharedLimiter = Arc::new(Mutex::new(None));
    wait_for_slot_every(&fresh, Duration::from_secs(15))
        .await
        .expect("fresh slot");
    let slot = fresh.lock().unwrap().expect("a slot");
    assert!(slot >= before);
    assert!(slot <= Instant::now());
}

// ---- C7 T62: seeded limiter and slot check ------------------------------------------------------

#[test]
fn c7_t62_seed() {
    let start = Instant::now();
    assert_eq!(*seeded_limiter(start).lock().unwrap(), Some(start));

    let one_second = Duration::from_secs(1);
    let seeded = seeded_limiter(start);
    assert!(!slot_free(&seeded, one_second, start).unwrap());
    assert!(!slot_free(&seeded, one_second, start + Duration::from_millis(999)).unwrap());
    assert!(slot_free(&seeded, one_second, start + one_second).unwrap());

    let fifteen = Duration::from_secs(15);
    assert!(!slot_free(&seeded, fifteen, start + Duration::from_secs(14)).unwrap());
    assert!(slot_free(&seeded, fifteen, start + fifteen).unwrap());

    // The check reserves nothing.
    assert_eq!(*seeded.lock().unwrap(), Some(start));

    let empty: SharedLimiter = Arc::new(Mutex::new(None));
    assert!(slot_free(&empty, fifteen, start).unwrap());

    // A lock poisoned by a panicking holder is reported, not unwrapped.
    let poisoned: SharedLimiter = Arc::new(Mutex::new(None));
    let holder = Arc::clone(&poisoned);
    let join = std::thread::spawn(move || {
        let _held = holder.lock().unwrap();
        panic!("poison the limiter for the test");
    })
    .join();
    assert!(join.is_err());
    assert!(matches!(
        slot_free(&poisoned, one_second, start),
        Err(GeocoderError::RateLimiterPoisoned)
    ));

    // The process-wide limiter starts seeded: it holds a start instant already.
    let slot = global_limiter().lock().unwrap().expect("a seeded slot");
    assert!(slot <= Instant::now());
}

// ---- C7 T63: the fixed agent --------------------------------------------------------------------

#[tokio::test]
async fn c7_t63_agent() {
    assert_eq!(
        FIXED_USER_AGENT,
        concat!("teslatlas-hub/", env!("CARGO_PKG_VERSION"))
    );
    within(1, async {
        let stub = Stub::start(reply(200, F1)).await;
        let address: GeocodedAddress = geocoder_for(&stub.endpoint)
            .reverse_with_egress_guard(point(51.5, -0.25), &CountingGuard::permitting())
            .await
            .expect("an address");
        assert_eq!(address.osm_id, 880001);
        let seen = stub.captured();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            header_value(&seen[0].head, "user-agent"),
            Some(FIXED_USER_AGENT)
        );
    })
    .await;
}
