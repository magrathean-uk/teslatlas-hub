// SPDX-License-Identifier: AGPL-3.0-only

// C7 part C tests for the geocoder module (spec section 7.5, C7 T65 to C7 T71):
// the back-off schedule, the durable provider state, cache-first lookup during a
// pause, the restart, a second endpoint, the answers that are not refusals and
// the pre-claim gate. Every provider is a loopback stub on an ephemeral port
// that answers a script; every time is a `looked_up_at_ms` or a `now_ms` the test
// passes, never the clock; each geocoder has its own private limiter; no test
// reaches the public host. Bodies are hand-written synthetic text.

use super::b3_c7_b::LabelledResult;

use crate::config::GeocoderConfig;
use crate::db::{GEOCODER_PROVIDER_STATE_KEY, HubStore};
use crate::geocoder::{
    EgressGuard, EgressGuardError, GeocodedAddress, Geocoder, GeocoderError, LookupGate,
    SharedLimiter, backoff_pause, lookup_gate_with, parse_response, seeded_limiter,
};
use crate::location::Wgs84Point;
use serde_json::{Value, json};
use std::future::Future;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// C7 fixture F1, the answer of a building-level lookup.
const F1: &str = r#"{"place_id":7001,"licence":"Data (c) test contributors, ODbL 1.0","osm_type":"way","osm_id":"880001","lat":"52.4100000","lon":"-1.5200000","place_rank":30,"category":"building","type":"house","addresstype":"building","name":"","display_name":"14, Fernhill Road, Larkfield, Ashcombe, West Wexshire, England, AB1 2CD, United Kingdom","address":{"house_number":"14","road":"Fernhill Road","suburb":"Larkfield","city":"Ashcombe","county":"West Wexshire","state":"England","ISO3166-2-lvl4":"GB-ENG","postcode":"AB1 2CD","country":"United Kingdom","country_code":"gb"}}"#;

fn point(latitude: f64, longitude: f64) -> Wgs84Point {
    Wgs84Point::new(latitude, longitude).expect("a valid point")
}

/// Run an async test body under the C7 timeout rule: `requests` x 1 s + 5 s.
async fn within<T>(seconds: u64, body: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(seconds), body)
        .await
        .expect("the test finishes within its timeout")
}

async fn within_labelled<T>(test: &str, seconds: u64, body: impl Future<Output = T>) -> T {
    let completed = tokio::time::timeout(Duration::from_secs(seconds), body).await;
    assert!(
        completed.is_ok(),
        "assertion failed: {test}: the provider-state scenario completes within its bound"
    );
    match completed {
        Ok(value) => value,
        Err(_) => unreachable!("labelled completion assertion passed"),
    }
}

// ---- the scripted loopback stub -------------------------------------------------

/// One answer of the script.
#[derive(Clone)]
struct Answer {
    status: u16,
    retry_after: Option<&'static str>,
    body: &'static str,
    /// How long the stub holds the answer back.
    delay: Duration,
    /// A `Content-Length` to announce in place of the body's own length.
    claimed_length: Option<usize>,
}

fn answer(status: u16, body: &'static str) -> Answer {
    Answer {
        status,
        retry_after: None,
        body,
        delay: Duration::ZERO,
        claimed_length: None,
    }
}

impl Answer {
    fn retry_after(mut self, value: &'static str) -> Self {
        self.retry_after = Some(value);
        self
    }

    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    fn claiming_length(mut self, length: usize) -> Self {
        self.claimed_length = Some(length);
        self
    }
}

/// The spec's shorthand "429 (120)": status 429 with `Retry-After: 120` and `{}`.
fn refusal(status: u16) -> Answer {
    answer(status, "{}")
}

/// Answers the requests it receives with the script in order and counts them.
struct Stub {
    endpoint: String,
    count: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Stub {
    async fn start(script: Vec<Answer>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let endpoint = format!("http://{}", listener.local_addr().expect("address"));
        let count = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&count);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let index = counted.fetch_add(1, Ordering::SeqCst);
                // An answer past the end of the script is a test failure through
                // the request count; it still gets a reply so the client finishes.
                let next = script
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| answer(599, "{}"));
                tokio::spawn(serve(stream, next));
            }
        });
        Self {
            endpoint,
            count,
            task,
        }
    }

    fn requests(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut stream: TcpStream, next: Answer) {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(count) => bytes.extend_from_slice(&buffer[..count]),
        }
    }
    tokio::time::sleep(next.delay).await;
    let mut head = format!(
        "HTTP/1.1 {} Stub\r\nContent-Type: application/json\r\n",
        next.status
    );
    if let Some(value) = next.retry_after {
        head.push_str(&format!("Retry-After: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        next.claimed_length.unwrap_or(next.body.len())
    ));
    if stream.write_all(head.as_bytes()).await.is_err() {
        return;
    }
    let _ = stream.write_all(next.body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

// ---- geocoders, stores and records ----------------------------------------------

fn config_for(endpoint: &str, timeout_seconds: u64) -> GeocoderConfig {
    GeocoderConfig {
        enabled: true,
        endpoint: Some(endpoint.to_owned()),
        language: "en-GB".to_owned(),
        timeout_seconds,
    }
}

/// A test-mode geocoder with a fresh private limiter, returned with that limiter.
fn geocoder_with_limiter(endpoint: &str, timeout_seconds: u64) -> (Geocoder, SharedLimiter) {
    let limiter: SharedLimiter = Arc::new(Mutex::new(None));
    let geocoder = Geocoder::build(
        &config_for(endpoint, timeout_seconds),
        true,
        Arc::clone(&limiter),
    )
    .expect("geocoder");
    (geocoder, limiter)
}

fn geocoder_for(endpoint: &str) -> Geocoder {
    geocoder_with_limiter(endpoint, 2).0
}

fn store_in(temporary: &tempfile::TempDir) -> HubStore {
    HubStore::initialize(temporary.path()).expect("store")
}

fn private_store() -> (tempfile::TempDir, HubStore) {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let store = store_in(&temporary);
    (temporary, store)
}

/// The stored provider-state text, as the storage getter returns it.
fn stored(store: &HubStore) -> Option<String> {
    store.geocoder_provider_state().expect("the row reads")
}

/// The stored record, parsed.
fn record(store: &HubStore) -> Option<Value> {
    stored(store).map(|text| serde_json::from_str(&text).expect("the record is JSON"))
}

/// The record of C7 R60 with the given values.
fn expected_record(
    endpoint: &str,
    host: &str,
    status: u16,
    refusals: u32,
    since_ms: i64,
    until_ms: i64,
) -> Value {
    json!({
        "v": 1,
        "endpoint": endpoint,
        "host": host,
        "status": status,
        "refusals": refusals,
        "since_ms": since_ms,
        "until_ms": until_ms,
    })
}

/// Make the provider-state row unreadable: its table is renamed away.
fn aside_metadata_table(store: &HubStore) {
    store
        .open()
        .expect("connection")
        .execute_batch("ALTER TABLE hub_metadata RENAME TO hub_metadata_aside")
        .expect("rename");
    assert!(store.geocoder_provider_state().is_err());
}

fn lookup_rows(store: &HubStore) -> i64 {
    store
        .open()
        .expect("connection")
        .query_row("SELECT COUNT(*) FROM address_lookup_cache", [], |row| {
            row.get(0)
        })
        .expect("count")
}

/// A guard that permits its first `permitted` checks, refuses the rest and counts
/// them all.
struct CountingGuard {
    permitted: usize,
    checks: AtomicUsize,
}

impl CountingGuard {
    fn new() -> Self {
        Self::permitting_first(usize::MAX)
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
        if self.checks.fetch_add(1, Ordering::SeqCst) < self.permitted {
            Ok(())
        } else {
            Err(EgressGuardError)
        }
    }
}

// ---- C7 T65: the back-off schedule ---------------------------------------------------

#[test]
fn c7_t65_backoff() {
    let seconds =
        |refusals: u32, retry_after: Option<u64>| backoff_pause(refusals, retry_after).as_secs();
    // 60 s doubled per refusal, capped at 86,400 s.
    assert_eq!(seconds(1, None), 60);
    assert_eq!(seconds(2, None), 120);
    assert_eq!(seconds(3, None), 240);
    assert_eq!(seconds(11, None), 61_440);
    assert_eq!(seconds(12, None), 86_400);
    assert_eq!(seconds(u32::MAX, None), 86_400);
    // A longer `Retry-After` is honoured, a shorter one never shortens the
    // schedule, and a huge one is capped.
    assert_eq!(seconds(1, Some(600)), 600);
    assert_eq!(seconds(3, Some(10)), 240);
    assert_eq!(seconds(1, Some(u64::MAX)), 86_400);
    // No sub-second part.
    assert_eq!(backoff_pause(1, None), Duration::from_secs(60));
}

// ---- C7 T66: refusal, pause, second refusal, recovery ------------------------------

#[tokio::test]
async fn c7_t66_refusal_recovery() {
    within(8, async {
        let stub = Stub::start(vec![
            refusal(429).retry_after("120"),
            refusal(403),
            answer(200, F1),
        ])
        .await;
        let (_temporary, store) = private_store();
        let (geocoder, limiter) = geocoder_with_limiter(&stub.endpoint, 2);
        let endpoint = geocoder.lookup_binding().endpoint.clone();

        // (a) The first refusal: 429 with a requested delay of 120 s.
        let error = geocoder
            .reverse_cached(&store, point(52.41, -1.52), 1_000_000)
            .await
            .expect_err("a refusal");
        assert!(matches!(error, GeocoderError::HttpStatus(429)), "{error:?}");
        assert_eq!(stub.requests(), 1);
        assert_eq!(
            record(&store),
            Some(expected_record(
                &endpoint,
                "127.0.0.1",
                429,
                1,
                1_000_000,
                1_120_000
            ))
        );
        // The record is compact JSON in the specified field order.
        assert_eq!(
            stored(&store).expect("record"),
            format!(
                r#"{{"v":1,"endpoint":"{endpoint}","host":"127.0.0.1","status":429,"refusals":1,"since_ms":1000000,"until_ms":1120000}}"#
            )
        );
        assert_eq!(
            GEOCODER_PROVIDER_STATE_KEY, "geocoder_provider_state",
            "the one metadata key"
        );

        // (b) One millisecond before the pause ends: no admission check, no slot,
        // no request, and the record is unchanged.
        let written = stored(&store);
        let slot = *limiter.lock().unwrap();
        let guard = CountingGuard::new();
        let error = geocoder
            .reverse_cached_with_egress_guard(&store, point(52.42, -1.52), 1_119_999, &guard)
            .await
            .expect_err("backing off");
        assert!(matches!(error, GeocoderError::BackingOff), "{error:?}");
        assert_eq!(
            error.to_string(),
            "geocoder provider refused requests; waiting before the next attempt"
        );
        assert_eq!(guard.checks(), 0);
        assert_eq!(*limiter.lock().unwrap(), slot);
        assert_eq!(stub.requests(), 1);
        assert_eq!(stored(&store), written);

        // (c) When the pause ends the next lookup is sent; a second refusal
        // doubles the pause and keeps the start.
        let error = geocoder
            .reverse_cached(&store, point(52.43, -1.52), 1_120_000)
            .await
            .expect_err("a second refusal");
        assert!(matches!(error, GeocoderError::HttpStatus(403)), "{error:?}");
        assert_eq!(stub.requests(), 2);
        assert_eq!(
            record(&store),
            Some(expected_record(
                &endpoint,
                "127.0.0.1",
                403,
                2,
                1_000_000,
                1_240_000
            ))
        );

        // (d) An answer ends the refusal: the address is stored and the record
        // is deleted.
        let rows_before = lookup_rows(&store);
        let address = geocoder
            .reverse_cached(&store, point(52.44, -1.52), 1_240_000)
            .await
            .expect("an address");
        assert_eq!(address, parse_response(F1.as_bytes()).unwrap());
        assert_eq!(stub.requests(), 3);
        assert_eq!(stored(&store), None);
        assert_eq!(lookup_rows(&store), rows_before + 1);

        // (e) A record that cannot be read fails closed: no request is sent.
        aside_metadata_table(&store);
        let error = geocoder
            .reverse_cached(&store, point(52.45, -1.52), 1_240_001)
            .await
            .expect_err("an unreadable record");
        assert!(matches!(error, GeocoderError::Cache(_)), "{error:?}");
        assert_eq!(stub.requests(), 3);
    })
    .await;
}

// ---- C7 T67: cache hits during a pause ---------------------------------------------

#[tokio::test]
async fn c7_t67_cache_during_pause() {
    within(7, async {
        let stub = Stub::start(vec![answer(200, F1), refusal(429)]).await;
        let (_temporary, store) = private_store();
        let geocoder = geocoder_for(&stub.endpoint);
        let first = point(52.41, -1.52);
        let second = point(52.42, -1.52);

        // (a) A first lookup, cached.
        let cached: GeocodedAddress = geocoder
            .reverse_cached(&store, first, 1_000)
            .await
            .expect("an address");
        assert_eq!(stub.requests(), 1);

        // (b) A refusal for another point starts a pause until 62,000.
        let error = geocoder
            .reverse_cached(&store, second, 2_000)
            .await
            .expect_err("a refusal");
        assert!(matches!(error, GeocoderError::HttpStatus(429)), "{error:?}");
        assert_eq!(stub.requests(), 2);
        let endpoint = geocoder.lookup_binding().endpoint.clone();
        assert_eq!(
            record(&store),
            Some(expected_record(
                &endpoint,
                "127.0.0.1",
                429,
                1,
                2_000,
                62_000
            ))
        );

        // (c) The cached point is still served during the pause, without any
        // admission check or request, and the record is unchanged.
        let written = stored(&store);
        let guard = CountingGuard::new();
        let again = geocoder
            .reverse_cached_with_egress_guard(&store, first, 3_000, &guard)
            .await
            .labelled_ok(
                "c7_t67_cache_during_pause",
                "the cache answers during the pause",
            );
        assert_eq!(again, cached);
        assert_eq!(guard.checks(), 0);
        assert_eq!(stub.requests(), 2);
        assert_eq!(stored(&store), written);

        // (d) An uncached point waits for the pause to end.
        let error = geocoder
            .reverse_cached(&store, second, 4_000)
            .await
            .expect_err("backing off");
        assert!(matches!(error, GeocoderError::BackingOff), "{error:?}");
        assert_eq!(stub.requests(), 2);
    })
    .await;
}

// ---- C7 T68: the pause survives a restart -------------------------------------------

#[tokio::test]
async fn c7_t68_restart() {
    within(7, async {
        let stub = Stub::start(vec![refusal(429), answer(200, F1)]).await;
        let temporary = crate::private_tempdir().expect("temporary directory");
        let store = store_in(&temporary);
        let geocoder = geocoder_for(&stub.endpoint);

        // (a) The refusal at 1,000,000 starts a pause until 1,060,000.
        let error = geocoder
            .reverse_cached(&store, point(52.41, -1.52), 1_000_000)
            .await
            .expect_err("a refusal");
        assert!(matches!(error, GeocoderError::HttpStatus(429)), "{error:?}");
        let endpoint = geocoder.lookup_binding().endpoint.clone();
        assert_eq!(
            record(&store),
            Some(expected_record(
                &endpoint,
                "127.0.0.1",
                429,
                1,
                1_000_000,
                1_060_000
            ))
        );

        // A restart: the geocoder and the store are dropped and reopened, with a
        // fresh limiter.
        drop(geocoder);
        drop(store);
        let reopened = store_in(&temporary);
        let geocoder = geocoder_for(&stub.endpoint);

        // (b) The pause is still in force halfway through.
        let error = geocoder
            .reverse_cached(&reopened, point(52.42, -1.52), 1_030_000)
            .await
            .expect_err("backing off after the restart");
        assert!(matches!(error, GeocoderError::BackingOff), "{error:?}");
        assert_eq!(stub.requests(), 1);

        // (c) At its end the lookup is sent and its answer clears the record.
        geocoder
            .reverse_cached(&reopened, point(52.42, -1.52), 1_060_000)
            .await
            .expect("an address");
        assert_eq!(stub.requests(), 2);
        assert_eq!(stored(&reopened), None);
    })
    .await;
}

// ---- C7 T69: another endpoint -----------------------------------------------------------

#[tokio::test]
async fn c7_t69_new_endpoint() {
    within(7, async {
        let refusing = Stub::start(vec![refusal(429)]).await;
        let answering = Stub::start(vec![answer(200, F1)]).await;
        let (_temporary, store) = private_store();
        let first = geocoder_for(&refusing.endpoint);
        let second = geocoder_for(&answering.endpoint);
        assert_ne!(
            first.lookup_binding().endpoint,
            second.lookup_binding().endpoint
        );

        // The first endpoint refuses and the record names it.
        let error = first
            .reverse_cached(&store, point(52.41, -1.52), 1_000)
            .await
            .expect_err("a refusal");
        assert!(matches!(error, GeocoderError::HttpStatus(429)), "{error:?}");
        assert_eq!(
            record(&store),
            Some(expected_record(
                &first.lookup_binding().endpoint,
                "127.0.0.1",
                429,
                1,
                1_000,
                61_000
            ))
        );

        // The second endpoint ignores that pause, is asked, and its answer ends the
        // record of the first.
        second
            .reverse_cached(&store, point(52.42, -1.52), 2_000)
            .await
            .labelled_ok("c7_t69_new_endpoint", "an address from the other endpoint");
        assert_eq!(answering.requests(), 1);
        assert_eq!(refusing.requests(), 1);
        assert_eq!(stored(&store), None);
    })
    .await;
}

// ---- C7 T70: answers that are not refusals ------------------------------------------

/// A 429 at 1,000 ms that stores a record, then `second` at 61,000 ms, when the pause
/// is over. Returns the second call's error, the requests the stub saw and the
/// record afterwards.
async fn answer_after_refusal(second: Answer) -> (GeocoderError, usize, Option<Value>) {
    let stub = Stub::start(vec![refusal(429), second]).await;
    let (_temporary, store) = private_store();
    let geocoder = geocoder_for(&stub.endpoint);
    let error = geocoder
        .reverse_cached(&store, point(52.41, -1.52), 1_000)
        .await
        .expect_err("a refusal");
    assert!(matches!(error, GeocoderError::HttpStatus(429)), "{error:?}");
    assert!(record(&store).is_some());
    let error = geocoder
        .reverse_cached(&store, point(52.42, -1.52), 61_000)
        .await
        .expect_err("an answer that is not an address");
    (error, stub.requests(), record(&store))
}

#[tokio::test]
async fn c7_t70_other_answers() {
    // (a) to (c): a failure status that is not a refusal starts no pause.
    for (status, retry_after, body) in [
        (500, None, "{}"),
        (503, Some("60"), "{}"),
        (404, None, r#"{"error":"Unable to geocode"}"#),
    ] {
        within_labelled("c7_t70_other_answers", 6, async {
            let mut scripted = answer(status, body);
            if let Some(value) = retry_after {
                scripted = scripted.retry_after(value);
            }
            let stub = Stub::start(vec![scripted]).await;
            let (_temporary, store) = private_store();
            let error = geocoder_for(&stub.endpoint)
                .reverse_cached(&store, point(52.41, -1.52), 1_000)
                .await
                .expect_err("an error status");
            assert!(
                matches!(error, GeocoderError::HttpStatus(code) if code == status),
                "{status}: {error:?}"
            );
            assert_eq!(stub.requests(), 1);
            assert_eq!(stored(&store), None, "{status}");
        })
        .await;
    }

    // (d) A 429 and then a 500: the second answer is not a refusal and clears the
    // record.
    within_labelled("c7_t70_other_answers", 7, async {
        let stub = Stub::start(vec![refusal(429), refusal(500)]).await;
        let (_temporary, store) = private_store();
        let geocoder = geocoder_for(&stub.endpoint);
        let error = geocoder
            .reverse_cached(&store, point(52.41, -1.52), 1_000)
            .await
            .expect_err("a refusal");
        assert!(matches!(error, GeocoderError::HttpStatus(429)), "{error:?}");
        assert!(record(&store).is_some());
        let error = geocoder
            .reverse_cached(&store, point(52.42, -1.52), 61_000)
            .await
            .expect_err("a server error");
        assert!(matches!(error, GeocoderError::HttpStatus(500)), "{error:?}");
        assert_eq!(stub.requests(), 2);
        assert_eq!(stored(&store), None);
    })
    .await;

    // (e) A timeout says nothing about the refusal: the record stays as it was.
    within_labelled("c7_t70_other_answers", 8, async {
        let stub = Stub::start(vec![
            refusal(429),
            answer(200, F1).delayed(Duration::from_secs(3)),
        ])
        .await;
        let (_temporary, store) = private_store();
        let (geocoder, _) = geocoder_with_limiter(&stub.endpoint, 1);
        let error = geocoder
            .reverse_cached(&store, point(52.41, -1.52), 1_000)
            .await
            .expect_err("a refusal");
        assert!(matches!(error, GeocoderError::HttpStatus(429)), "{error:?}");
        let endpoint = geocoder.lookup_binding().endpoint.clone();
        let expected = Some(expected_record(
            &endpoint,
            "127.0.0.1",
            429,
            1,
            1_000,
            61_000,
        ));
        assert_eq!(record(&store), expected);
        let error = geocoder
            .reverse_cached(&store, point(52.42, -1.52), 61_000)
            .await
            .expect_err("a timeout");
        assert!(matches!(error, GeocoderError::Timeout), "{error:?}");
        assert_eq!(stub.requests(), 2);
        assert_eq!(record(&store), expected);
    })
    .await;

    // (f) Admission lost after the provider answered: the store is not written
    // either way. A refusal is not recorded and the lookup reports the lost
    // admission; a record that an answer would have cleared stays for the next
    // answer. The two checks before the request are permitted.
    within_labelled("c7_t70_other_answers", 7, async {
        let stub = Stub::start(vec![refusal(429), refusal(500)]).await;
        let (_temporary, store) = private_store();
        let geocoder = geocoder_for(&stub.endpoint);
        let guard = CountingGuard::permitting_first(2);
        let error = geocoder
            .reverse_cached_with_egress_guard(&store, point(52.41, -1.52), 1_000, &guard)
            .await
            .expect_err("admission lost after the answer");
        assert!(matches!(error, GeocoderError::EgressDenied), "{error:?}");
        assert_eq!(stub.requests(), 1);
        assert_eq!(stored(&store), None);

        let written = gate_record("https://geo.example.test:443/", 61_000);
        store
            .set_geocoder_provider_state(Some(&written))
            .expect("record");
        let guard = CountingGuard::permitting_first(2);
        let error = geocoder
            .reverse_cached_with_egress_guard(&store, point(52.42, -1.52), 61_000, &guard)
            .await
            .expect_err("a server error");
        assert!(matches!(error, GeocoderError::HttpStatus(500)), "{error:?}");
        assert_eq!(stub.requests(), 2);
        assert_eq!(stored(&store).as_deref(), Some(written.as_str()));
    })
    .await;

    // (g) to (j): an answer that is not an address, whatever its fault, is still an
    // answer, so it clears the record as a status other than 403 or 429 does.
    within_labelled("c7_t70_other_answers", 7, async {
        let (error, requests, left) =
            answer_after_refusal(answer(200, r#"{"error":"Unable to geocode"}"#)).await;
        assert!(matches!(error, GeocoderError::NoResult), "{error:?}");
        assert_eq!((requests, left), (2, None));
    })
    .await;
    within_labelled("c7_t70_other_answers", 7, async {
        let (error, requests, left) = answer_after_refusal(answer(200, "[]")).await;
        assert!(matches!(error, GeocoderError::ResponseRead), "{error:?}");
        assert_eq!((requests, left), (2, None));
    })
    .await;
    within_labelled("c7_t70_other_answers", 7, async {
        let (error, requests, left) = answer_after_refusal(answer(200, "{}")).await;
        assert!(
            matches!(error, GeocoderError::MalformedResponse),
            "{error:?}"
        );
        assert_eq!((requests, left), (2, None));
    })
    .await;
    within_labelled("c7_t70_other_answers", 7, async {
        let (error, requests, left) =
            answer_after_refusal(answer(200, "{}").claiming_length(256 * 1024 + 1)).await;
        assert!(
            matches!(error, GeocoderError::ResponseTooLarge),
            "{error:?}"
        );
        assert_eq!((requests, left), (2, None));
    })
    .await;

    // (k) A refusal whose record cannot be stored is reported as a store failure,
    // after one request, and leaves no row.
    within_labelled("c7_t70_other_answers", 6, async {
        let stub = Stub::start(vec![refusal(429)]).await;
        let (_temporary, store) = private_store();
        store
            .open()
            .expect("connection")
            .execute_batch(&format!(
                "CREATE TRIGGER refuse_state BEFORE INSERT ON hub_metadata
                 WHEN NEW.key = '{GEOCODER_PROVIDER_STATE_KEY}'
                 BEGIN SELECT RAISE(ABORT, 'store refused'); END"
            ))
            .expect("trigger");
        let error = geocoder_for(&stub.endpoint)
            .reverse_cached(&store, point(52.41, -1.52), 1_000)
            .await
            .expect_err("a refusal that cannot be stored");
        assert!(
            matches!(error, GeocoderError::Cache(_)),
            concat!(
                "assertion failed: c7_t70_other_answers: ",
                "a refusal whose state cannot be stored reports the cache error; {error:?}"
            ),
            error = error
        );
        assert_eq!(stub.requests(), 1);
        assert_eq!(stored(&store), None);
    })
    .await;

    // (l) A record that cannot be deleted does not change the answer's outcome: the
    // error is the answer's own and the stale record stays for the next answer.
    within_labelled("c7_t70_other_answers", 7, async {
        let stub = Stub::start(vec![refusal(429), refusal(500)]).await;
        let (_temporary, store) = private_store();
        let geocoder = geocoder_for(&stub.endpoint);
        geocoder
            .reverse_cached(&store, point(52.41, -1.52), 1_000)
            .await
            .expect_err("a refusal");
        let kept = record(&store);
        assert!(kept.is_some());
        store
            .open()
            .expect("connection")
            .execute_batch(&format!(
                "CREATE TRIGGER keep_state BEFORE DELETE ON hub_metadata
                 WHEN OLD.key = '{GEOCODER_PROVIDER_STATE_KEY}'
                 BEGIN SELECT RAISE(ABORT, 'store refused'); END"
            ))
            .expect("trigger");
        let lookup_store = store.clone();
        let lookup_geocoder = geocoder.clone();
        let mut lookup = tokio::spawn(async move {
            lookup_geocoder
                .reverse_cached(&lookup_store, point(52.42, -1.52), 61_000)
                .await
        });
        let completed = tokio::time::timeout(Duration::from_secs(7), &mut lookup).await;
        if completed.is_err() {
            lookup.abort();
        }
        assert!(
            completed.is_ok(),
            concat!(
                "assertion failed: c7_t70_other_answers: ",
                "failed state deletion leaves a bounded server-error answer"
            )
        );
        let joined = match completed {
            Ok(joined) => joined,
            Err(_) => unreachable!("labelled lookup completion assertion passed"),
        };
        assert!(
            joined.is_ok(),
            concat!(
                "assertion failed: c7_t70_other_answers: ",
                "failed state deletion preserves the answer without a panic"
            )
        );
        let answer = match joined {
            Ok(answer) => answer,
            Err(_) => unreachable!("labelled task assertion passed"),
        };
        let error = answer.labelled_err(
            "c7_t70_other_answers",
            "a server error after failed state deletion",
        );
        assert!(matches!(error, GeocoderError::HttpStatus(500)), "{error:?}");
        assert_eq!(stub.requests(), 2);
        assert_eq!(record(&store), kept);
    })
    .await;
}

// ---- C7 T71: the pre-claim gate --------------------------------------------------------

/// A hand-written record text for the gate tests.
fn gate_record(endpoint: &str, until_ms: i64) -> String {
    format!(
        r#"{{"v":1,"endpoint":"{endpoint}","host":"geo.example.test","status":429,"refusals":1,"since_ms":1000,"until_ms":{until_ms}}}"#
    )
}

#[test]
fn c7_t71_gate() {
    let (_temporary, store) = private_store();
    let c1 = GeocoderConfig {
        endpoint: Some("https://geo.example.test/".to_owned()),
        ..GeocoderConfig::default()
    };
    let c0 = GeocoderConfig::default();
    let e1 = "https://geo.example.test:443/";
    let t = Instant::now();
    let fresh = || -> SharedLimiter { Arc::new(Mutex::new(None)) };
    let gate = |limiter: &SharedLimiter, config: &GeocoderConfig, now_ms: i64, now: Instant| {
        lookup_gate_with(limiter, config, &store, now_ms, now)
    };
    let set = |value: Option<&str>| store.set_geocoder_provider_state(value).expect("write");

    // No record.
    assert_eq!(gate(&fresh(), &c1, 1_000, t), LookupGate::Open);

    // A record for this endpoint: in force until its `until_ms`, exactly.
    set(Some(&gate_record(e1, 2_000)));
    assert_eq!(
        gate(&fresh(), &c1, 1_999, t),
        LookupGate::BackingOff { until_ms: 2_000 }
    );
    assert_eq!(gate(&fresh(), &c1, 2_000, t), LookupGate::Open);
    assert_eq!(gate(&fresh(), &c1, 2_001, t), LookupGate::Open);
    // The record comes before the limiter: a taken slot does not hide it.
    assert_eq!(
        gate(&seeded_limiter(t), &c1, 1_000, t),
        LookupGate::BackingOff { until_ms: 2_000 }
    );

    // A record for another endpoint blocks nothing.
    set(Some(&gate_record("https://other.example.test:443/", 2_000)));
    assert_eq!(gate(&fresh(), &c1, 1_000, t), LookupGate::Open);

    // A value that is not a valid record is treated as absent.
    for text in [
        "not json".to_owned(),
        String::new(),
        "[]".to_owned(),
        gate_record(e1, 2_000).replace(r#""v":1"#, r#""v":2"#),
        gate_record(e1, 2_000).replace(r#""status":429"#, r#""status":500"#),
        gate_record(e1, 2_000).replace(r#""refusals":1"#, r#""refusals":0"#),
        gate_record(e1, 2_000).replace(r#""refusals":1"#, r#""refusals":"1""#),
    ] {
        set(Some(&text));
        assert_eq!(gate(&fresh(), &c1, 1_000, t), LookupGate::Open, "{text}");
    }

    // A pause that ends before it began is invalid, although it ends after now.
    set(Some(&gate_record(e1, 500)));
    assert_eq!(gate(&fresh(), &c1, 100, t), LookupGate::Open);

    // A pause longer than 24 h past now counts as damaged: 86,401,001 is expired,
    // 86,401,000 is the longest pause still in force.
    set(Some(&gate_record(e1, 86_401_001)));
    assert_eq!(gate(&fresh(), &c1, 1_000, t), LookupGate::Open);
    set(Some(&gate_record(e1, 86_401_000)));
    assert_eq!(
        gate(&fresh(), &c1, 1_000, t),
        LookupGate::BackingOff {
            until_ms: 86_401_000
        }
    );
    set(None);

    // Pacing: 15 s for the public host, 1 s for any other.
    let taken = seeded_limiter(t);
    assert_eq!(gate(&taken, &c0, 1_000, t), LookupGate::Paced);
    assert_eq!(
        gate(&taken, &c0, 1_000, t + Duration::from_secs(14)),
        LookupGate::Paced
    );
    assert_eq!(
        gate(&taken, &c0, 1_000, t + Duration::from_secs(15)),
        LookupGate::Open
    );
    assert_eq!(
        gate(&taken, &c1, 1_000, t + Duration::from_millis(500)),
        LookupGate::Paced
    );
    assert_eq!(
        gate(&taken, &c1, 1_000, t + Duration::from_secs(1)),
        LookupGate::Open
    );
    // The gate reserves nothing.
    assert_eq!(*taken.lock().unwrap(), Some(t));

    // A configuration that does not build is `Open`: the kept path reports it.
    let broken = GeocoderConfig {
        endpoint: Some("http://127.0.0.1:1/".to_owned()),
        ..GeocoderConfig::default()
    };
    assert_eq!(gate(&taken, &broken, 1_000, t), LookupGate::Open);
    let bad_language = GeocoderConfig {
        language: String::new(),
        ..c1.clone()
    };
    assert_eq!(gate(&taken, &bad_language, 1_000, t), LookupGate::Open);

    // A poisoned limiter is `Open`: the wrapper fails closed on the same fault.
    let poisoned: SharedLimiter = Arc::new(Mutex::new(None));
    let holder = Arc::clone(&poisoned);
    let join = std::thread::spawn(move || {
        let _held = holder.lock().unwrap();
        panic!("poison the limiter for the test");
    })
    .join();
    assert!(join.is_err());
    assert_eq!(gate(&poisoned, &c1, 1_000, t), LookupGate::Open);

    // The record is one row of the metadata table under the one key.
    set(Some(&gate_record(e1, 2_000)));
    assert_eq!(
        gate(&fresh(), &c1, 1_000, t),
        LookupGate::BackingOff { until_ms: 2_000 }
    );
    let value: String = store
        .open()
        .expect("connection")
        .query_row(
            "SELECT value FROM hub_metadata WHERE key = 'geocoder_provider_state'",
            [],
            |row| row.get(0),
        )
        .expect("the row");
    assert_eq!(value, gate_record(e1, 2_000));

    // An unreadable record is `Open`: the wrapper fails closed on the same fault.
    aside_metadata_table(&store);
    assert_eq!(gate(&fresh(), &c1, 1_000, t), LookupGate::Open);
}
