// SPDX-License-Identifier: AGPL-3.0-only

//! C5 part P3 spec tests: one refresh attempt under the kept fencing, the
//! schedule that follows it, the retry and stop rules of the outcome table, the
//! persistence retry, the detached exchange and shutdown (spec tests 18 to 27,
//! 29 to 32, 43 to 50, 52, 54 to 58, 70, 77, 78, 81, 88 and 94 (a) and (b)).
//!
//! Every test uses a temporary store, a manual clock and an in-process fake
//! over loopback (a token endpoint and the products endpoint in one router, so
//! the order of requests is one log); nothing leaves the machine. Tokens, times
//! and rows are the synthetic fixtures of spec section 8: `t0` is
//! 2027-01-15T08:00:00Z, the stored pair is `(JG, refresh-0)`, the successor
//! access token is `AT1`. The clock never moves unless a test moves it.

use std::{
    collections::VecDeque,
    sync::atomic::{AtomicI64, AtomicUsize},
    time::Duration,
};

use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use tokio::{net::TcpListener, task::JoinHandle};
use url::Url;

use super::*;
use crate::{
    legacy_auth::{LegacyAuthFuse, RefusalReason},
    owner_api::{OwnerApi, OwnerApiAuthError, Vehicle},
};

/// 2027-01-15T08:00:00Z.
const T0: i64 = 1_800_000_000;

/// The body of a successful refresh (spec section 8).
const SUCCESS: &str = r#"{"access_token":"AT1","refresh_token":"refresh-1","id_token":"IDTOKEN-SECRET-9","expires_in":28800,"token_type":"Bearer"}"#;

/// Strings that must never appear in anything the unit shows (test 69).
const SECRETS: [&str; 7] = [
    "AT1",
    "AT2",
    "refresh-0",
    "refresh-1",
    "refresh-2",
    "refresh-9",
    "IDTOKEN-SECRET-9",
];

/// A clock the test moves. A wait of the unit moves it instead of waiting.
struct ManualEnv(AtomicI64);

impl ManualEnv {
    fn new(now: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(now)))
    }

    fn set(&self, now: i64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl AuthEnv for ManualEnv {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    fn sleep(
        &self,
        seconds: i64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        self.0.fetch_add(seconds, Ordering::SeqCst);
        Box::pin(async {})
    }

    fn spread_unit(&self) -> f64 {
        0.0
    }
}

/// What the fake answers to a token request.
#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    delay: Duration,
    hang: bool,
    /// The answer is held back, once the request has been counted, until the test
    /// releases the gate (B2 fix round, P3b).
    gate: Option<Arc<tokio::sync::Notify>>,
    /// The body fails after the status, as a reset connection does: the outcome of
    /// the request is unknown (C5 R45 row 6).
    reset: bool,
}

impl Reply {
    fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
            delay: Duration::ZERO,
            hang: false,
            gate: None,
            reset: false,
        }
    }

    /// Hold the answer back until `gate` is released.
    fn held_by(mut self, gate: &Arc<tokio::sync::Notify>) -> Self {
        self.gate = Some(Arc::clone(gate));
        self
    }

    /// An answer whose body fails after the status line.
    fn reset() -> Self {
        Self {
            reset: true,
            ..Self::json(200, "")
        }
    }

    fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    fn hang() -> Self {
        Self {
            hang: true,
            ..Self::json(200, "")
        }
    }
}

/// One request the fake saw.
#[derive(Clone, Debug)]
struct Seen {
    /// `"token"` or `"products"`.
    kind: &'static str,
    /// The `refresh_token` member of a token request's body.
    refresh: Option<String>,
    authorization: Option<String>,
}

#[derive(Clone)]
struct Backend {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    fallback: Arc<Mutex<Reply>>,
    log: Arc<Mutex<Vec<Seen>>>,
    products_status: Arc<Mutex<u16>>,
    /// The clock the `Date` header of every answer is read from.
    clock: Arc<ManualEnv>,
}

impl Backend {
    fn token_posts(&self) -> Vec<Seen> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| seen.kind == "token")
            .cloned()
            .collect()
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|seen| seen.kind)
            .collect()
    }

    fn products(&self) -> Vec<Seen> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| seen.kind == "products")
            .cloned()
            .collect()
    }

    /// Queue replies for the next token requests; the last one repeats.
    fn script(&self, replies: Vec<Reply>) {
        let mut replies: VecDeque<Reply> = replies.into();
        if let Some(last) = replies.back().cloned() {
            *self.fallback.lock().unwrap() = last;
        }
        let mut queue = self.replies.lock().unwrap();
        queue.clear();
        queue.append(&mut replies);
    }
}

/// An IMF-fixdate (RFC 9110 section 5.6.7) for an epoch second.
fn imf_fixdate(epoch: i64) -> String {
    let moment = time::OffsetDateTime::from_unix_timestamp(epoch).expect("a date");
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
            [usize::from(moment.weekday().number_days_from_monday())],
        moment.day(),
        [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
        ][usize::from(u8::from(moment.month())) - 1],
        moment.year(),
        moment.hour(),
        moment.minute(),
        moment.second()
    )
}

fn into_response(reply: &Reply, server_now: i64) -> Response {
    let mut response = Response::builder().status(StatusCode::from_u16(reply.status).unwrap());
    // The server's clock is the test clock, so an answer never looks skewed
    // unless the test says so.
    if !reply
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("date"))
    {
        response = response.header("date", imf_fixdate(server_now));
    }
    for (name, value) in &reply.headers {
        response = response.header(name, value);
    }
    let body = if reply.reset {
        axum::body::Body::from_stream(futures_util::stream::once(async {
            Err::<axum::body::Bytes, std::io::Error>(std::io::Error::other("connection reset"))
        }))
    } else {
        axum::body::Body::from(reply.body.clone())
    };
    response.body(body).unwrap()
}

async fn token_handler(State(backend): State<Backend>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 1 << 20).await.unwrap_or_default();
    let refresh = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value.get("refresh_token")?.as_str().map(str::to_owned));
    backend.log.lock().unwrap().push(Seen {
        kind: "token",
        refresh,
        authorization: parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    });
    let reply = {
        let mut queue = backend.replies.lock().unwrap();
        queue
            .pop_front()
            .unwrap_or_else(|| backend.fallback.lock().unwrap().clone())
    };
    if reply.hang {
        std::future::pending::<()>().await;
    }
    if let Some(gate) = &reply.gate {
        gate.notified().await;
    }
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    into_response(&reply, backend.clock.now())
}

async fn products_handler(State(backend): State<Backend>, headers: HeaderMap) -> Response {
    backend.log.lock().unwrap().push(Seen {
        kind: "products",
        refresh: None,
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    });
    let status = *backend.products_status.lock().unwrap();
    (
        StatusCode::from_u16(status).unwrap(),
        [("date", imf_fixdate(backend.clock.now()))],
        r#"{"response":[],"count":0}"#,
    )
        .into_response()
}

/// A temporary store, a fake and a clock.
struct Rig {
    directory: tempfile::TempDir,
    store: crate::db::HubStore,
    backend: Backend,
    base: Url,
    env: Arc<ManualEnv>,
    server: JoinHandle<()>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn new_rig() -> Rig {
    crate::crypto::install_default_provider();
    let env = ManualEnv::new(T0);
    let directory = crate::private_tempdir().expect("data directory");
    let store = crate::db::HubStore::initialize(directory.path()).expect("Hub store");
    let backend = Backend {
        replies: Arc::default(),
        fallback: Arc::new(Mutex::new(Reply::json(200, SUCCESS))),
        log: Arc::default(),
        products_status: Arc::new(Mutex::new(200)),
        clock: env.clone(),
    };
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new()
        .route("/oauth2/v3/token", post(token_handler))
        .route("/api/1/products", get(products_handler))
        .with_state(backend.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Rig {
        directory,
        store,
        backend,
        base: Url::parse(&format!("http://{address}/")).unwrap(),
        env,
        server,
    }
}

fn jg() -> String {
    jwt_for_test(
        serde_json::json!({"iss": "https://auth.tesla.com/oauth2/v3", "exp": 1_800_028_800_i64}),
    )
}

fn jc() -> String {
    jwt_for_test(
        serde_json::json!({"iss": "https://auth.tesla.cn/oauth2/v3", "exp": 1_800_028_800_i64}),
    )
}

/// A JWT of the global issuer with this `exp`.
fn jg_exp(exp: i64) -> String {
    jwt_for_test(serde_json::json!({"iss": "https://auth.tesla.com/oauth2/v3", "exp": exp}))
}

impl Rig {
    /// Store the pair `(access, refresh)`: `schedule` `None` is the imported
    /// form.
    fn seed(&self, access: &str, refresh: &str, schedule: Option<(i64, i64)>) {
        seed_pair(
            &self.store,
            self.directory.path(),
            access,
            refresh,
            schedule,
        );
    }

    /// The fixture pair `(JG, refresh-0)`, due at `t0` and usable until
    /// `t0 + 28,500` (the token's `exp` less the allowance).
    fn seed_due(&self) {
        self.seed(&jg(), "refresh-0", Some((T0 + 28_800, T0)));
    }

    fn endpoint(&self, region: Option<crate::tesla_stream::StreamRegion>) -> AuthEndpointConfig {
        AuthEndpointConfig {
            mode: crate::legacy_auth::AuthMode::Tesla,
            region,
            token_url: None,
            allow_loopback_http: true,
        }
    }

    fn token_url(&self) -> Url {
        self.base.join("oauth2/v3/token").unwrap()
    }

    fn manager(&self) -> LegacyAuthManager {
        self.manager_with(None, self.token_url())
    }

    fn manager_with(
        &self,
        region: Option<crate::tesla_stream::StreamRegion>,
        loopback: Url,
    ) -> LegacyAuthManager {
        LegacyAuthManager::from_hub_teslamate_store_for_test(
            self.store.clone(),
            self.directory.path(),
            &self.endpoint(region),
            Some(loopback),
            false,
            self.env.clone(),
        )
        .expect("manager")
    }

    fn client(&self) -> Client {
        crate::legacy_auth::build_auth_client(true).expect("auth client")
    }

    /// A manager started over a store whose attempt is unresolved (R57 step 3):
    /// it is built in its stopped state and holds the stop `signed_out(
    /// refresh_outcome_unknown)` (C5-B2).
    fn restarted_stopped(&self) -> LegacyAuthManager {
        let manager = LegacyAuthManager::from_hub_teslamate_store_for_test(
            self.store.clone(),
            self.directory.path(),
            &self.endpoint(None),
            Some(self.token_url()),
            false,
            self.env.clone(),
        )
        .expect("a stopped manager");
        assert!(matches!(
            manager.access_token_for_sensitive_use(),
            Err(LegacyAuthManagerError::Auth(LegacyAuthError::SignedOut {
                reason: crate::legacy_auth::SignOutReason::RefreshOutcomeUnknown
            }))
        ));
        manager
    }

    /// An auth client with a short total budget, to prove the timeout paths in
    /// milliseconds on the same builder the production client uses.
    fn short_client(&self, connect: Duration, total: Duration) -> Client {
        crate::legacy_auth::auth_client_builder_with_timeouts(true, connect, total)
            .build()
            .expect("short auth client")
    }

    fn api(&self) -> OwnerApi {
        OwnerApi::for_fake_http(self.base.clone(), Duration::from_secs(5)).expect("Owner API")
    }

    fn at(&self, now: i64) {
        self.env.set(now);
    }

    fn row(&self) -> crate::db::CollectorAuthRow {
        self.store
            .load_collector_auth_row()
            .expect("status row")
            .expect("a status row")
    }

    /// The state and reason of the stored status row.
    fn state(&self) -> (String, Option<String>) {
        let row = self.row();
        (row.state, row.reason)
    }

    fn receipts(&self) -> Vec<(String, Option<i64>, Option<i64>, bool)> {
        self.store
            .legacy_token_receipts_for_test()
            .expect("receipts")
    }

    /// The pair the store holds: access, refresh, `expires_at`, `next_refresh_at`.
    fn stored_pair(&self) -> (String, String, i64, i64) {
        let stored = self
            .store
            .load_teslamate_legacy_tokens()
            .expect("load")
            .expect("a pair");
        let key = crate::teslamate_credentials::load_key(self.directory.path()).expect("key");
        let tokens = crate::teslamate_token::decrypt_legacy_owner_tokens(
            key.as_bytes(),
            stored.access(),
            stored.refresh(),
        )
        .expect("decrypt");
        (
            tokens.access_token().to_owned(),
            tokens.refresh_token().to_owned(),
            stored.expires_at(),
            stored.next_refresh_at(),
        )
    }

    fn stored_generation(&self) -> uuid::Uuid {
        self.store
            .load_teslamate_legacy_tokens()
            .expect("load")
            .expect("a pair")
            .credential_generation()
            .expect("bound generation")
    }

    /// Test 69: nothing the unit shows contains a secret.
    fn scan(&self, manager: &LegacyAuthManager, also: &str) {
        let rendered = format!(
            "{manager:?} {:?} {:?} {also}",
            self.store.load_collector_auth_row(),
            self.receipts()
        );
        for secret in SECRETS {
            assert!(!rendered.contains(secret), "{secret} appears in {rendered}");
        }
        for token in [jg(), jc()] {
            let payload = token.split('.').nth(1).unwrap().to_owned();
            assert!(!rendered.contains(&payload), "a JWT payload appears");
        }
    }

    async fn refresh(&self, manager: &mut LegacyAuthManager) -> Result<(), LegacyAuthManagerError> {
        manager
            .refresh_if_due(&self.client(), SystemTime::now())
            .await
    }

    async fn list(
        &self,
        manager: &mut LegacyAuthManager,
    ) -> Result<Vec<Vehicle>, OwnerApiAuthError> {
        let mut fuse = LegacyAuthFuse::default();
        self.api()
            .list_vehicles_with_legacy_auth_fused(manager, &mut fuse)
            .await
    }
}

fn auth_error(error: LegacyAuthManagerError) -> LegacyAuthError {
    match error {
        LegacyAuthManagerError::Auth(error) => error,
        other => panic!("unexpected error {other:?}"),
    }
}

fn row_of(rig: &Rig) -> (String, Option<String>, Option<i64>, bool) {
    let row = rig.row();
    (row.state, row.reason, row.retry_at, row.collecting)
}

// ---------------------------------------------------------------------------
// Test 18: one attempt, persisted before use
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t18_a_due_refresh_is_persisted_before_the_next_request_carries_the_new_token() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    let g0 = rig.stored_generation();
    rig.list(&mut manager).await.expect("products");

    assert_eq!(rig.backend.kinds(), ["token", "products"]);
    let posts = rig.backend.token_posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].refresh.as_deref(), Some("refresh-0"));
    assert_eq!(
        rig.backend.products()[0].authorization.as_deref(),
        Some("Bearer AT1")
    );
    // Persisted once, with the times of the schedule.
    assert_eq!(
        rig.stored_pair(),
        (
            "AT1".into(),
            "refresh-1".into(),
            1_800_028_800,
            1_800_021_600
        )
    );
    assert_eq!(manager.write_log.lock().unwrap().as_slice(), [T0]);
    assert_ne!(rig.stored_generation(), g0);
    assert_eq!(manager.generation(), rig.stored_generation());
    // The attempt is closed and the successor's input is fenced (K2).
    assert_eq!(
        rig.receipts(),
        [("success".to_owned(), Some(200), None, true)]
    );
    assert_eq!(rig.store.legacy_fenced_inputs_for_test().unwrap(), [g0]);
    assert_eq!(manager.auth.retry_at(), None);
    assert_eq!(manager.auth.retry_reason, None);
    assert_eq!(row_of(&rig), ("ready".into(), None, None, true));
    rig.scan(&manager, "");
}

// ---------------------------------------------------------------------------
// Tests 19 to 22 and 78: the schedule end to end
// ---------------------------------------------------------------------------

/// Run one refresh whose answer is `body`, from the due fixture, and return the
/// rig and the manager after it.
async fn refreshed_with(body: &str) -> (Rig, LegacyAuthManager) {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![Reply::json(200, body)]);
    let mut manager = rig.manager();
    rig.refresh(&mut manager).await.expect("refresh");
    (rig, manager)
}

#[tokio::test]
async fn t19_an_answer_without_expires_in_uses_the_jwt_exp() {
    let token = jg();
    let body = serde_json::json!({"access_token": token, "refresh_token": "refresh-1"}).to_string();
    let (rig, manager) = refreshed_with(&body).await;
    let (_, _, expires, next) = rig.stored_pair();
    assert_eq!((expires, next), (1_800_028_500, 1_800_021_375));
    assert_eq!(manager.auth.usable_until(), Some(1_800_028_500));
}

#[tokio::test]
async fn t20_an_opaque_answer_without_expires_in_is_eight_hours() {
    let rig = new_rig().await;
    rig.seed("AT0", "refresh-0", Some((T0 + 28_800, T0)));
    rig.backend.script(vec![Reply::json(
        200,
        r#"{"access_token":"AT1","refresh_token":"refresh-1"}"#,
    )]);
    let mut manager = rig.manager_with(
        Some(crate::tesla_stream::StreamRegion::Global),
        rig.token_url(),
    );
    rig.refresh(&mut manager).await.expect("refresh");
    let (_, _, expires, next) = rig.stored_pair();
    assert_eq!((expires, next), (1_800_028_800, 1_800_021_600));
}

#[tokio::test]
async fn t21_a_thirty_second_token_is_short_and_is_used_only_while_it_lives() {
    let body = SUCCESS.replace("28800", "30");
    let (rig, mut manager) = refreshed_with(&body).await;
    assert_eq!(
        rig.stored_pair(),
        ("AT1".into(), "refresh-1".into(), T0 + 30, T0 + 23)
    );
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("short_token_lifetime".into()),
            Some(T0 + 60),
            true
        )
    );
    // A request at t0 + 29 carries AT1; at t0 + 30 it is not sent.
    rig.at(T0 + 29);
    rig.list(&mut manager).await.expect("products at t0 + 29");
    assert_eq!(
        rig.backend.products()[0].authorization.as_deref(),
        Some("Bearer AT1")
    );
    rig.at(T0 + 30);
    let error = rig
        .list(&mut manager)
        .await
        .expect_err("not sent at t0 + 30");
    assert!(
        matches!(
            error,
            OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(LegacyAuthError::AuthUnavailable {
                retry_at: Some(retry_at)
            })) if retry_at == T0 + 60
        ),
        "{error:?}"
    );
    assert_eq!(rig.backend.products().len(), 1, "nothing was sent");
    assert_eq!(rig.backend.token_posts().len(), 1);
    // The status says nothing collects now.
    let _ = rig.refresh(&mut manager).await;
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("short_token_lifetime".into()),
            Some(T0 + 60),
            false
        )
    );
    assert_eq!(
        rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthWaiting,
        ))
    );
}

#[tokio::test]
async fn t21_a_very_long_lifetime_is_held_to_a_day_and_a_digit_string_is_read() {
    let (rig, _manager) = refreshed_with(&SUCCESS.replace("28800", "10000000")).await;
    assert_eq!(
        rig.stored_pair(),
        (
            "AT1".into(),
            "refresh-1".into(),
            1_800_086_400,
            1_800_064_800
        )
    );
    assert_eq!(rig.state().0, "ready");
    let (rig, _manager) = refreshed_with(
        &SUCCESS
            .replace("28800", "\\\"28800\\\"")
            .replace("\\\"", "\""),
    )
    .await;
    assert_eq!(
        rig.stored_pair(),
        (
            "AT1".into(),
            "refresh-1".into(),
            1_800_028_800,
            1_800_021_600
        )
    );
}

#[tokio::test]
async fn t21_an_expires_in_that_is_not_a_lifetime_falls_back_to_eight_hours() {
    for value in ["0", "-5", "1.5", "null"] {
        let rig = new_rig().await;
        rig.seed("AT0", "refresh-0", Some((T0 + 28_800, T0)));
        rig.backend
            .script(vec![Reply::json(200, &SUCCESS.replace("28800", value))]);
        let mut manager = rig.manager_with(
            Some(crate::tesla_stream::StreamRegion::Global),
            rig.token_url(),
        );
        rig.refresh(&mut manager).await.expect("refresh");
        let (_, _, expires, next) = rig.stored_pair();
        assert_eq!((expires, next), (1_800_028_800, 1_800_021_600), "{value}");
    }
}

#[tokio::test]
async fn t22_an_exp_that_is_already_past_is_stored_at_the_minimum_span_and_never_sent() {
    let token = jg_exp(1_799_999_000);
    let body = serde_json::json!({"access_token": token, "refresh_token": "refresh-1"}).to_string();
    let (rig, mut manager) = refreshed_with(&body).await;
    assert_eq!(
        (rig.stored_pair().2, rig.stored_pair().3),
        (1_800_000_002, 1_800_000_001)
    );
    assert_eq!(manager.auth.usable_until(), Some(1_799_998_700));
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("short_token_lifetime".into()),
            Some(T0 + 60),
            false
        )
    );
    let error = rig.list(&mut manager).await.expect_err("unusable");
    assert!(matches!(
        error,
        OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
            LegacyAuthError::AuthUnavailable { .. }
        ))
    ));
    assert!(
        rig.backend.products().is_empty(),
        "no request carries the new token"
    );
}

#[tokio::test]
async fn t78_the_token_s_own_exp_bounds_usability_below_expires_in() {
    let token = jg_exp(1_800_003_600);
    let body =
        serde_json::json!({"access_token": token, "refresh_token": "refresh-1", "expires_in": 28_800})
            .to_string();
    let (rig, mut manager) = refreshed_with(&body).await;
    assert_eq!(
        (rig.stored_pair().2, rig.stored_pair().3),
        (1_800_003_300, 1_800_002_475)
    );
    assert_eq!(manager.auth.usable_until(), Some(1_800_003_300));
    assert_eq!(row_of(&rig).0, "ready");
    rig.at(1_800_003_299);
    assert!(manager.access_token_for_sensitive_use().is_ok());
    rig.at(1_800_003_300);
    assert!(manager.access_token_for_sensitive_use().is_err());
    let _ = &mut manager;
}

// ---------------------------------------------------------------------------
// Tests 25 to 27: the imported form
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t26_an_imported_pair_is_refreshed_on_the_first_need_before_the_request() {
    let rig = new_rig().await;
    rig.seed(&jg(), "refresh-0", None);
    let mut manager = rig.manager();
    // Nothing is sent when the manager is built (R26).
    assert!(rig.backend.kinds().is_empty());
    rig.list(&mut manager).await.expect("products");
    assert_eq!(rig.backend.kinds(), ["token", "products"]);
    assert_eq!(
        rig.backend.products()[0].authorization.as_deref(),
        Some("Bearer AT1")
    );
    assert!(!manager.auth.startup_refresh_pending);
    // A second request refreshes nothing more.
    rig.list(&mut manager).await.expect("products again");
    assert_eq!(rig.backend.token_posts().len(), 1);
}

#[tokio::test]
async fn t27_a_failed_startup_refresh_still_collects_with_the_stored_token_and_retries_in_a_minute()
{
    let rig = new_rig().await;
    rig.seed(&jg(), "refresh-0", None);
    rig.backend.script(vec![Reply::json(503, "down")]);
    let mut manager = rig.manager();
    rig.list(&mut manager)
        .await
        .expect("products with the stored token");
    assert_eq!(
        rig.backend.products()[0].authorization.as_deref(),
        Some(format!("Bearer {}", jg()).as_str())
    );
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("tesla_unavailable".into()),
            Some(T0 + 60),
            true
        )
    );
    // Within the minute nothing more is sent to the token endpoint.
    rig.at(T0 + 10);
    rig.list(&mut manager).await.expect("products at t0 + 10");
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert!(manager.auth.startup_refresh_pending);
    // At t0 + 60 the next attempt presents the same refresh token (a 503 consumed nothing).
    rig.at(T0 + 60);
    rig.list(&mut manager).await.expect("products at t0 + 60");
    let posts = rig.backend.token_posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[1].refresh.as_deref(), Some("refresh-0"));
}

// ---------------------------------------------------------------------------
// Test 29: one flight
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t29_ten_concurrent_callers_share_one_exchange_and_then_see_the_new_token() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![
        Reply::json(200, SUCCESS).after(Duration::from_millis(300)),
    ]);
    let manager = Arc::new(tokio::sync::Mutex::new(rig.manager()));
    let api = rig.api();
    let mut callers = Vec::new();
    for _ in 0..10 {
        let manager = Arc::clone(&manager);
        let api = api.clone();
        callers.push(tokio::spawn(async move {
            let mut guard = manager.lock().await;
            let mut fuse = LegacyAuthFuse::default();
            api.list_vehicles_with_legacy_auth_fused(&mut guard, &mut fuse)
                .await
        }));
    }
    for caller in callers {
        caller.await.expect("task").expect("products");
    }
    assert_eq!(rig.backend.token_posts().len(), 1, "exactly one exchange");
    let kinds = rig.backend.kinds();
    assert_eq!(kinds[0], "token");
    assert_eq!(kinds.len(), 11, "{kinds:?}");
    // Every request made while the exchange was in flight waited for it.
    for seen in rig.backend.products() {
        assert_eq!(seen.authorization.as_deref(), Some("Bearer AT1"));
    }
}

// ---------------------------------------------------------------------------
// Tests 30 to 32: persistence retry
// ---------------------------------------------------------------------------

fn plant(manager: &LegacyAuthManager, faults: &[PersistFault]) {
    manager
        .persistence
        .hub()
        .expect("a store")
        .faults
        .lock()
        .unwrap()
        .extend(faults.iter().copied());
}

#[tokio::test]
async fn t30_a_write_that_fails_twice_is_retried_at_five_and_fifteen_seconds_without_a_second_request()
 {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    plant(&manager, &[PersistFault::Fail, PersistFault::Fail]);
    rig.refresh(&mut manager)
        .await
        .expect("the attempt is not a failure");
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(
        row_of(&rig),
        ("persistence_pending".into(), None, None, true)
    );
    // Requests during that time carry the old token.
    for now in [T0 + 1, T0 + 4] {
        rig.at(now);
        rig.list(&mut manager).await.expect("products");
        assert_eq!(
            rig.backend
                .products()
                .last()
                .unwrap()
                .authorization
                .as_deref(),
            Some(format!("Bearer {}", jg()).as_str())
        );
    }
    rig.at(T0 + 5);
    rig.list(&mut manager).await.expect("products at t0 + 5");
    assert_eq!(
        rig.backend
            .products()
            .last()
            .unwrap()
            .authorization
            .as_deref(),
        Some(format!("Bearer {}", jg()).as_str()),
        "the second write failed too"
    );
    rig.at(T0 + 14);
    rig.refresh(&mut manager).await.expect("not due yet");
    rig.at(T0 + 15);
    rig.list(&mut manager).await.expect("products at t0 + 15");
    assert_eq!(
        rig.backend
            .products()
            .last()
            .unwrap()
            .authorization
            .as_deref(),
        Some("Bearer AT1")
    );
    assert_eq!(
        manager.write_log.lock().unwrap().as_slice(),
        [T0, T0 + 5, T0 + 15]
    );
    assert_eq!(rig.backend.token_posts().len(), 1, "no second request");
    assert_eq!(
        rig.stored_pair(),
        ("AT1".into(), "refresh-1".into(), T0 + 28_800, T0 + 21_600)
    );
    assert_eq!(row_of(&rig).0, "ready");
    rig.scan(&manager, "");
}

#[tokio::test]
async fn t31_a_pending_write_past_usability_sends_nothing_and_collects_nothing() {
    let rig = new_rig().await;
    rig.seed("AT0", "refresh-0", Some((T0 + 10, T0)));
    let mut manager = rig.manager_with(
        Some(crate::tesla_stream::StreamRegion::Global),
        rig.token_url(),
    );
    plant(&manager, &[PersistFault::Fail; 12]);
    // Usable until the stored `expires_at`, t0 + 10.
    rig.refresh(&mut manager).await.expect("attempt");
    rig.at(T0 + 11);
    let error = rig.list(&mut manager).await.expect_err("not sent");
    assert!(matches!(
        error,
        OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(
            LegacyAuthError::AuthUnavailable { .. }
        ))
    ));
    assert!(rig.backend.products().is_empty());
    assert_eq!(rig.backend.token_posts().len(), 1, "no second POST");
    assert_eq!(
        row_of(&rig),
        ("persistence_pending".into(), None, None, false)
    );
    assert_eq!(
        rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthWaiting,
        ))
    );
}

#[tokio::test]
async fn t32_an_ambiguous_write_is_recognised_by_its_generation_and_not_written_twice() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    plant(&manager, &[PersistFault::Ambiguous]);
    rig.refresh(&mut manager).await.expect("attempt");
    assert_eq!(row_of(&rig).0, "persistence_pending");
    // The write committed, so the store already holds the successor.
    assert_eq!(rig.stored_pair().0, "AT1");
    rig.at(T0 + 5);
    rig.refresh(&mut manager).await.expect("retry");
    assert_eq!(
        manager.write_log.lock().unwrap().as_slice(),
        [T0],
        "no second write"
    );
    assert_eq!(row_of(&rig).0, "ready");
    assert_eq!(manager.access_token(), "AT1");
    assert_eq!(manager.generation(), rig.stored_generation());
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
}

#[tokio::test]
async fn r33a_shutdown_tries_a_pending_write_once_more_without_waiting_for_its_time() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    plant(&manager, &[PersistFault::Fail]);
    rig.refresh(&mut manager).await.expect("attempt");
    assert_eq!(row_of(&rig).0, "persistence_pending");
    // Two seconds later the retry is not yet due (5 s), but shutdown tries.
    rig.at(T0 + 2);
    manager.shutdown().await.expect("an orderly shutdown");
    assert_eq!(rig.stored_pair().0, "AT1");
    assert_eq!(manager.write_log.lock().unwrap().as_slice(), [T0, T0 + 2]);
    assert_eq!(row_of(&rig).0, "ready");
}

// ---------------------------------------------------------------------------
// Tests 43 to 58: the outcome table
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t43_a_dead_refresh_token_signs_in_again_but_collects_until_the_access_token_expires() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend
        .script(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
    let mut manager = rig.manager();
    let g0 = rig.stored_generation();
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("dead"));
    assert_eq!(error, LegacyAuthError::RefreshTokenRejected);
    assert_eq!(
        rig.state(),
        (
            "sign_in_required".into(),
            Some("refresh_token_rejected".into())
        )
    );
    assert_eq!(
        rig.receipts(),
        [("authentication_rejected".to_owned(), Some(400), None, true)]
    );
    assert_eq!(
        rig.store.legacy_fenced_inputs_for_test().unwrap(),
        [g0],
        "the input stays fenced"
    );
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    // While the access token is usable the owner is told (attention) and the Hub
    // stays ready: the lease is kept fresh at each simulated evaluation time.
    let lease = rig
        .store
        .acquire_supervised_collector_lease(T0 * 1_000)
        .expect("lease");
    let view_at = |now: i64| {
        rig.store
            .heartbeat_supervised_collector_lease(
                lease,
                crate::db::SupervisedCollectorState::Active,
                now * 1_000,
            )
            .expect("fresh authority");
        rig.store
            .collector_readiness_view(&crate::db::ReadinessContext::context_free(
                true,
                now * 1_000,
            ))
    };
    assert_eq!(
        rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthSignInRequired,
        ))
    );
    let usable = view_at(T0 + 1);
    assert_eq!(usable.service, Ok(()));
    assert_eq!(
        usable.attention,
        Some(crate::db::CollectionAttentionCode::CollectorAuthSignInRequired)
    );
    // Collection goes on with the stored token until it expires at t0 + 28,500.
    rig.at(T0 + 28_499);
    rig.list(&mut manager).await.expect("products while usable");
    assert_eq!(
        rig.backend.products()[0].authorization.as_deref(),
        Some(format!("Bearer {}", jg()).as_str())
    );
    // The owner's retry cannot lift a sign-in stop (R44).
    assert_eq!(
        manager.owner_retry(),
        Err(LegacyAuthError::RetryNotPossible {
            reason: SignOutReason::RefreshTokenRejected
        })
    );
    rig.at(T0 + 28_500);
    let error = rig.list(&mut manager).await.expect_err("signed out");
    assert!(
        matches!(
            error,
            OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(LegacyAuthError::SignedOut {
                reason: SignOutReason::RefreshTokenRejected
            }))
        ),
        "{error:?}"
    );
    assert_eq!(
        rig.state(),
        ("signed_out".into(), Some("refresh_token_rejected".into()))
    );
    assert!(!rig.row().collecting);
    // Signed out is the service failure, with no attention beside it.
    let expired = view_at(T0 + 28_500);
    assert_eq!(
        expired.service.err().map(|failure| failure.code),
        Some(crate::db::ReadinessReasonCode::CollectorAuthTerminal)
    );
    assert_eq!(expired.attention, None);
    // No token request in the next 24 hours, however often it is asked.
    for step in 0..48 {
        rig.at(T0 + 28_500 + step * 1_800);
        let _ = rig.refresh(&mut manager).await;
        let _ = manager.refresh_now(&rig.client(), SystemTime::now()).await;
    }
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(
        manager.owner_retry(),
        Err(LegacyAuthError::RetryNotPossible {
            reason: SignOutReason::RefreshTokenRejected
        })
    );
    rig.scan(&manager, &format!("{error:?}"));
}

#[tokio::test]
async fn t44_login_required_with_a_401_is_the_same_dead_token() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend
        .script(vec![Reply::json(401, r#"{"error":"login_required"}"#)]);
    let mut manager = rig.manager();
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("dead"));
    assert_eq!(error, LegacyAuthError::RefreshTokenRejected);
    assert_eq!(
        rig.state(),
        (
            "sign_in_required".into(),
            Some("refresh_token_rejected".into())
        )
    );
    assert_eq!(
        rig.receipts(),
        [("authentication_rejected".to_owned(), Some(401), None, true)]
    );
}

#[tokio::test]
async fn t45_repeated_503_retries_at_the_documented_times_with_the_same_refresh_token() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![Reply::json(503, "down")]);
    let mut manager = rig.manager();
    let mut expected = vec![T0, T0 + 60, T0 + 180, T0 + 420, T0 + 720, T0 + 1_020];
    let mut sent = Vec::new();
    for (attempt, at) in expected.clone().into_iter().enumerate() {
        // One second early, nothing is sent.
        if attempt > 0 {
            rig.at(at - 1);
            let before = rig.backend.token_posts().len();
            rig.refresh(&mut manager).await.expect("deferred");
            assert_eq!(rig.backend.token_posts().len(), before, "early at {at}");
        }
        rig.at(at);
        let error = auth_error(rig.refresh(&mut manager).await.expect_err("503"));
        assert_eq!(error, LegacyAuthError::HttpStatus(503));
        sent.push(at);
        // Collection continues with the current token while it is usable.
        assert!(manager.access_token_for_sensitive_use().is_ok());
        assert_eq!(row_of(&rig).1.as_deref(), Some("tesla_unavailable"));
    }
    expected.truncate(sent.len());
    assert_eq!(sent, expected);
    let posts = rig.backend.token_posts();
    assert_eq!(posts.len(), 6);
    assert!(
        posts
            .iter()
            .all(|post| post.refresh.as_deref() == Some("refresh-0"))
    );
    // Each is closed `http_error` 503 with the input released.
    let receipts = rig.receipts();
    assert_eq!(receipts.len(), 6);
    assert!(
        receipts
            .iter()
            .all(|receipt| *receipt == ("http_error".to_owned(), Some(503), None, false))
    );
    assert!(
        rig.store
            .legacy_fenced_inputs_for_test()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn t46_retry_after_sets_the_next_attempt_and_the_receipt_keeps_it() {
    for (header, expected_delay, stored) in [
        ("7200", 3_600, 7_200),
        ("5", 60, 5),
        ("Fri, 15 Jan 2027 08:15:00 GMT", 900, 900),
    ] {
        let rig = new_rig().await;
        rig.seed_due();
        rig.backend.script(vec![
            Reply::json(429, "slow down").with_header("retry-after", header),
        ]);
        let mut manager = rig.manager();
        let _ = rig.refresh(&mut manager).await;
        assert_eq!(row_of(&rig).2, Some(T0 + expected_delay), "{header}");
        assert_eq!(
            rig.receipts(),
            [("http_error".to_owned(), Some(429), Some(stored), false)],
            "{header}"
        );
    }
}

#[tokio::test]
async fn t47_a_refused_connection_sent_nothing_so_the_token_is_reused_after_a_minute() {
    let rig = new_rig().await;
    rig.seed_due();
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        Url::parse(&format!("http://{address}/oauth2/v3/token")).unwrap()
    };
    let mut manager = rig.manager_with(None, dead);
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("refused"));
    assert_eq!(error, LegacyAuthError::Transport);
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("network".into()),
            Some(T0 + 60),
            true
        )
    );
    assert_eq!(
        rig.receipts(),
        [("transport_error".to_owned(), None, None, false)]
    );
    assert!(
        rig.store
            .legacy_fenced_inputs_for_test()
            .unwrap()
            .is_empty()
    );
    // At +60 the same refresh token is presented again, to the live server.
    let live = rig.manager_with(None, rig.token_url());
    drop(live);
    manager = rig.manager_with(None, rig.token_url());
    rig.at(T0 + 60);
    rig.refresh(&mut manager).await.expect("succeeds");
    assert_eq!(
        rig.backend.token_posts()[0].refresh.as_deref(),
        Some("refresh-0")
    );
}

#[tokio::test]
async fn t48_a_request_that_is_never_answered_leaves_the_outcome_unknown_for_good() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![Reply::hang()]);
    let mut manager = rig.manager();
    let client = rig.short_client(Duration::from_secs(5), Duration::from_millis(300));
    let error = auth_error(
        manager
            .refresh_if_due(&client, SystemTime::now())
            .await
            .expect_err("no answer"),
    );
    assert_eq!(error, LegacyAuthError::RotationOutcomeUnknown);
    assert_eq!(
        rig.state(),
        (
            "sign_in_required".into(),
            Some("refresh_outcome_unknown".into())
        )
    );
    // The receipt stays started: unresolved, with its input fenced.
    assert_eq!(rig.receipts(), [("started".to_owned(), None, None, true)]);
    assert!(rig.store.has_unresolved_legacy_refresh().unwrap());
    assert_eq!(rig.store.legacy_fenced_inputs_for_test().unwrap().len(), 1);
    // Collection continues while the token is usable; no further POST ever.
    rig.at(T0 + 100);
    rig.list(&mut manager).await.expect("products");
    for step in 1..=20 {
        rig.at(T0 + step * 4_000);
        let _ = rig.refresh(&mut manager).await;
    }
    assert_eq!(rig.backend.token_posts().len(), 1);
    // After a restart nothing is sent and the state is a sign-out. The manager is
    // built in that stopped state (C5-B2), so the collector can start and show it.
    rig.at(T0 + 200);
    let mut restarted = rig.restarted_stopped();
    assert_eq!(
        rig.state(),
        ("signed_out".into(), Some("refresh_outcome_unknown".into()))
    );
    for step in 1..=5 {
        rig.at(T0 + 200 + step * 4_000);
        assert!(rig.list(&mut restarted).await.is_err());
        let _ = rig.refresh(&mut restarted).await;
    }
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert!(
        rig.backend.products().len() <= 1,
        "no Owner API request after the restart"
    );
    rig.scan(&manager, "");
}

#[tokio::test]
async fn t49_a_handshake_that_never_completes_is_a_timeout_that_sent_nothing() {
    let rig = new_rig().await;
    rig.seed_due();
    // A listener that accepts the connection and then says nothing: the TLS
    // handshake over `https` never completes, so the connect timeout (which covers
    // the handshake, R33) ends it. This holds on any host, with no injection
    // (C5T-04). The budget is 200 ms on the same builder the production client
    // uses; the production values are asserted in the wire tests.
    let silent = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("silent listener");
    let address = silent.local_addr().expect("address");
    let holder = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = silent.accept().await {
            held.push(socket);
        }
    });
    let mut manager = rig.manager_with(
        None,
        Url::parse(&format!("https://{address}/oauth2/v3/token")).unwrap(),
    );
    let client = rig.short_client(Duration::from_millis(200), Duration::from_secs(5));
    let started = std::time::Instant::now();
    let error = auth_error(
        manager
            .refresh_if_due(&client, SystemTime::now())
            .await
            .expect_err("no connection"),
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "ended by the connect timeout"
    );
    holder.abort();
    assert_eq!(error, LegacyAuthError::Transport);
    assert_eq!(rig.receipts(), [("timeout".to_owned(), None, None, false)]);
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("network".into()),
            Some(T0 + 60),
            true
        )
    );
    assert!(
        rig.store
            .legacy_fenced_inputs_for_test()
            .unwrap()
            .is_empty()
    );
    assert!(
        rig.backend.token_posts().is_empty(),
        "nothing reached the server"
    );
}

#[tokio::test]
async fn t50_an_answer_over_the_size_cap_is_unusable_and_leaves_the_attempt_unresolved() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut body = String::from(r#"{"access_token":""#);
    body.push_str(&"a".repeat(65_537 - body.len() - 2));
    body.push_str("\"}");
    assert_eq!(body.len(), 65_537);
    rig.backend.script(vec![Reply::json(200, &body)]);
    let mut manager = rig.manager();
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("too large"));
    assert_eq!(error, LegacyAuthError::ResponseTooLarge);
    assert_eq!(
        rig.state(),
        (
            "sign_in_required".into(),
            Some("refresh_response_unusable".into())
        )
    );
    assert_eq!(rig.receipts(), [("started".to_owned(), None, None, true)]);
    assert!(
        manager.write_log.lock().unwrap().is_empty(),
        "no persist call"
    );
    assert_eq!(rig.stored_pair().0, jg());
    // After a restart the attempt is unresolved: a sign-out, nothing sent. The
    // manager is built in that stopped state (C5-B2).
    let mut restarted = rig.restarted_stopped();
    assert_eq!(
        rig.state(),
        ("signed_out".into(), Some("refresh_outcome_unknown".into()))
    );
    assert!(rig.list(&mut restarted).await.is_err());
    assert_eq!(rig.backend.token_posts().len(), 1);
}

#[tokio::test]
async fn t52_an_answer_without_a_new_refresh_token_keeps_the_presented_one() {
    for body in [
        r#"{"access_token":"AT1","expires_in":28800,"token_type":"Bearer"}"#,
        r#"{"access_token":"AT1","refresh_token":"refresh-0","expires_in":28800}"#,
    ] {
        let rig = new_rig().await;
        rig.seed_due();
        rig.backend.script(vec![Reply::json(200, body)]);
        let mut manager = rig.manager();
        rig.refresh(&mut manager).await.expect("refresh");
        assert_eq!(
            rig.stored_pair(),
            (
                "AT1".into(),
                "refresh-0".into(),
                1_800_028_800,
                1_800_021_600
            )
        );
        assert_eq!(row_of(&rig).0, "ready");
        // The next refresh presents refresh-0 again (nothing was consumed).
        rig.backend.script(vec![Reply::json(
            200,
            r#"{"access_token":"AT2","expires_in":28800}"#,
        )]);
        rig.at(T0 + 21_600);
        rig.refresh(&mut manager).await.expect("second refresh");
        let posts = rig.backend.token_posts();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[1].refresh.as_deref(), Some("refresh-0"));
    }
}

#[tokio::test]
async fn t54_the_same_pair_returned_is_not_renewed_nothing_is_persisted_and_the_input_is_released()
{
    let rig = new_rig().await;
    rig.seed_due();
    let body = serde_json::json!({
        "access_token": jg(), "refresh_token": "refresh-0", "expires_in": 28_800
    })
    .to_string();
    rig.backend.script(vec![Reply::json(200, &body)]);
    let mut manager = rig.manager();
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("not renewed"));
    assert_eq!(error, LegacyAuthError::RefreshNotRenewed);
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("refresh_not_renewed".into()),
            Some(T0 + 60),
            true
        )
    );
    assert_eq!(
        rig.receipts(),
        [("success".to_owned(), Some(200), None, false)]
    );
    assert!(manager.write_log.lock().unwrap().is_empty(), "no persist");
    assert!(
        rig.store
            .legacy_fenced_inputs_for_test()
            .unwrap()
            .is_empty()
    );
    rig.at(T0 + 60);
    let _ = rig.refresh(&mut manager).await;
    let posts = rig.backend.token_posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[1].refresh.as_deref(), Some("refresh-0"));
}

#[tokio::test]
async fn t55_two_consecutive_invalid_client_answers_are_an_api_refusal_that_survives_a_restart() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend
        .script(vec![Reply::json(400, r#"{"error":"invalid_client"}"#)]);
    let mut manager = rig.manager();
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("first answer"));
    assert_eq!(error, LegacyAuthError::ClientRefused);
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("client_check".into()),
            Some(T0 + 60),
            true
        )
    );
    assert_eq!(
        rig.receipts(),
        [("http_error".to_owned(), Some(400), None, false)]
    );
    // Early: nothing is sent.
    rig.at(T0 + 59);
    rig.refresh(&mut manager).await.expect("deferred");
    assert_eq!(rig.backend.token_posts().len(), 1);
    rig.at(T0 + 60);
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("second answer"));
    assert_eq!(
        error,
        LegacyAuthError::OwnerApiRefused {
            reason: RefusalReason::Client
        }
    );
    assert_eq!(rig.state(), ("api_refused".into(), Some("client".into())));
    let row = rig.row();
    assert_eq!(row.stopped_mode.as_deref(), Some("legacy"));
    assert_eq!(row.stopped_generation, None);
    assert_eq!(rig.backend.token_posts().len(), 2);
    assert_eq!(rig.receipts().len(), 2);

    // Test 94 (a): a restart with the same configuration restores the refusal
    // before any request, and sends nothing in the next 24 hours.
    let before = rig.row();
    let mut restarted = rig.manager();
    assert_eq!(
        restarted.auth.stop,
        Some(AuthStop::ApiRefused(RefusalReason::Client))
    );
    assert_eq!(rig.row(), before, "the status row is unchanged");
    for step in 0..48 {
        rig.at(T0 + 60 + step * 1_800);
        let error = restarted
            .refresh_if_due(&rig.client(), SystemTime::now())
            .await;
        assert!(matches!(
            error,
            Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::OwnerApiRefused { .. }
            ))
        ));
        assert!(restarted.access_token_for_sensitive_use().is_err());
    }
    assert!(rig.list(&mut restarted).await.is_err());
    assert_eq!(rig.backend.token_posts().len(), 2);
    assert!(rig.backend.products().is_empty());
    // The owner's retry lifts it: requests resume and the next token request
    // presents refresh-0 (R60 row 7).
    rig.backend.script(vec![Reply::json(200, SUCCESS)]);
    rig.at(T0 + 100_000);
    restarted.owner_retry().expect("lifted");
    assert_eq!(rig.row().stopped_mode, None);
    assert_eq!(rig.state().0, "ready");
    rig.list(&mut restarted).await.expect("products resume");
    let posts = rig.backend.token_posts();
    assert_eq!(posts.len(), 3);
    assert_eq!(posts[2].refresh.as_deref(), Some("refresh-0"));
    rig.scan(&restarted, &format!("{error:?}"));
}

#[tokio::test]
async fn t55_any_other_answer_between_the_two_resets_the_count() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![
        Reply::json(400, r#"{"error":"invalid_client"}"#),
        Reply::json(503, "down"),
        Reply::json(400, r#"{"error":"invalid_client"}"#),
    ]);
    let mut manager = rig.manager();
    let _ = rig.refresh(&mut manager).await;
    rig.at(T0 + 60);
    let _ = rig.refresh(&mut manager).await;
    assert_eq!(manager.auth.client_refusals, 0);
    rig.at(T0 + 180);
    let _ = rig.refresh(&mut manager).await;
    assert_eq!(
        rig.state(),
        ("retrying".into(), Some("client_check".into()))
    );
    assert_eq!(manager.auth.stop, None);
}

#[tokio::test]
async fn t56_a_rejected_request_shape_is_retried_hourly() {
    for (status, body) in [(400, r#"{"error":"invalid_scope"}"#), (401, "oops")] {
        let rig = new_rig().await;
        rig.seed_due();
        rig.backend.script(vec![Reply::json(status, body)]);
        let mut manager = rig.manager();
        let _ = rig.refresh(&mut manager).await;
        assert_eq!(
            row_of(&rig),
            (
                "retrying".into(),
                Some("refresh_request_rejected".into()),
                Some(T0 + 3_600),
                true
            ),
            "{status}"
        );
        assert_eq!(
            rig.receipts(),
            [("http_error".to_owned(), Some(status as i64), None, false)]
        );
        // Not before the hour, then the same token again.
        rig.at(T0 + 3_599);
        rig.refresh(&mut manager).await.expect("deferred");
        assert_eq!(rig.backend.token_posts().len(), 1);
    }
}

#[tokio::test]
async fn t57_a_missing_token_endpoint_is_retried_hourly() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![Reply::json(404, "")]);
    let mut manager = rig.manager();
    let _ = rig.refresh(&mut manager).await;
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("token_endpoint_misrouted".into()),
            Some(T0 + 3_600),
            true
        )
    );
}

#[tokio::test]
async fn t58_a_403_naming_the_fleet_api_is_a_refusal_that_survives_a_restart() {
    for body in [
        r#"{"error":"Please use the Fleet API"}"#,
        "see developer.tesla.com/docs/fleet-api",
    ] {
        let rig = new_rig().await;
        rig.seed_due();
        rig.backend.script(vec![Reply::json(403, body)]);
        let mut manager = rig.manager();
        let error = auth_error(rig.refresh(&mut manager).await.expect_err("refused"));
        assert_eq!(
            error,
            LegacyAuthError::OwnerApiRefused {
                reason: RefusalReason::OwnerApi
            }
        );
        assert_eq!(
            rig.state(),
            ("api_refused".into(), Some("owner_api".into()))
        );
        assert_eq!(rig.row().stopped_mode.as_deref(), Some("legacy"));
        assert_eq!(
            rig.receipts(),
            [("http_error".to_owned(), Some(403), None, false)]
        );
        // Test 94 (b): restored after a restart, nothing sent.
        let mut restarted = rig.manager();
        assert_eq!(
            restarted.auth.stop,
            Some(AuthStop::ApiRefused(RefusalReason::OwnerApi))
        );
        assert!(rig.list(&mut restarted).await.is_err());
        let _ = rig.refresh(&mut restarted).await;
        assert_eq!(rig.backend.token_posts().len(), 1);
        assert!(rig.backend.products().is_empty());
        rig.scan(&restarted, "");
    }
    // A 403 that does not name the Fleet API is a block that is retried.
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![Reply::json(403, "blocked")]);
    let mut manager = rig.manager();
    let _ = rig.refresh(&mut manager).await;
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("refresh_blocked".into()),
            Some(T0 + 60),
            true
        )
    );
}

// ---------------------------------------------------------------------------
// Test 70: China end to end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t70_a_china_pair_is_refreshed_at_the_china_endpoint_and_keeps_the_china_region() {
    let rig = new_rig().await;
    rig.seed(&jc(), "refresh-0", None);
    let mut manager = rig.manager();
    assert_eq!(manager.region(), crate::tesla_stream::StreamRegion::China);
    assert_eq!(
        manager.auth.token_endpoint().unwrap().as_str(),
        "https://auth.tesla.cn/oauth2/v3/token"
    );
    rig.refresh(&mut manager).await.expect("refresh");
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(rig.stored_pair().0, "AT1");
    assert_eq!(manager.region(), crate::tesla_stream::StreamRegion::China);
    assert_eq!(
        crate::tesla_stream::streaming_endpoint(manager.region()),
        crate::tesla_stream::streaming_endpoint(crate::tesla_stream::StreamRegion::China)
    );
    assert_ne!(
        crate::tesla_stream::streaming_endpoint(crate::tesla_stream::StreamRegion::China),
        crate::tesla_stream::streaming_endpoint(crate::tesla_stream::StreamRegion::Global)
    );
}

// ---------------------------------------------------------------------------
// Test 77: detached exchange and shutdown
// ---------------------------------------------------------------------------

/// Wait until the fake has seen `count` token requests.
async fn seen_posts(rig: &Rig, count: usize) {
    for _ in 0..400 {
        if rig.backend.token_posts().len() >= count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the token request was never seen");
}

#[tokio::test]
async fn t77_a_shutdown_during_an_exchange_waits_for_it_persists_it_and_starts_nothing_new() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![
        Reply::json(200, SUCCESS).after(Duration::from_millis(400)),
    ]);
    let manager = rig.manager();
    let flag = manager.shutdown_flag();
    let manager = Arc::new(tokio::sync::Mutex::new(manager));
    let client = rig.client();
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(&rig, 1).await;
    flag.store(true, Ordering::Release);
    caller.await.expect("task").expect("the exchange completes");
    manager
        .lock()
        .await
        .shutdown()
        .await
        .expect("an orderly shutdown");
    // The answer was persisted and the pair adopted.
    assert_eq!(rig.stored_pair().0, "AT1");
    assert_eq!(manager.lock().await.access_token(), "AT1");
    // No new request starts once shutdown has begun, even a forced one.
    rig.at(T0 + 100);
    manager
        .lock()
        .await
        .refresh_now(&rig.client(), SystemTime::now())
        .await
        .expect("nothing sent");
    assert_eq!(rig.backend.token_posts().len(), 1);
    // At the next start the state is ready with the new pair. The successor's
    // access token is opaque, so the region is the configured one (R4).
    let next = rig.manager_with(
        Some(crate::tesla_stream::StreamRegion::Global),
        rig.token_url(),
    );
    assert_eq!(next.access_token(), "AT1");
    assert_eq!(rig.state().0, "ready");
}

#[tokio::test]
async fn t77_a_dropped_caller_does_not_abort_the_exchange_and_its_result_is_adopted() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![
        Reply::json(200, SUCCESS).after(Duration::from_millis(400)),
    ]);
    let manager = Arc::new(tokio::sync::Mutex::new(rig.manager()));
    let client = rig.client();
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(&rig, 1).await;
    caller.abort();
    let _ = caller.await;
    // The exchange still completes and is persisted by its own task.
    for _ in 0..400 {
        if rig.stored_pair().0 == "AT1" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(rig.stored_pair().0, "AT1");
    // The next holder applies the result before any token use.
    let mut guard = manager.lock().await;
    rig.at(T0 + 1);
    rig.list(&mut guard).await.expect("products");
    assert_eq!(
        rig.backend.products()[0].authorization.as_deref(),
        Some("Bearer AT1")
    );
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(guard.generation(), rig.stored_generation());
}

#[tokio::test]
async fn t77_a_server_that_never_answers_is_an_unknown_outcome_at_the_next_start() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![Reply::hang()]);
    let mut manager = rig.manager();
    let client = rig.short_client(Duration::from_secs(5), Duration::from_millis(250));
    let error = manager.refresh_if_due(&client, SystemTime::now()).await;
    assert!(matches!(
        error,
        Err(LegacyAuthManagerError::Auth(
            LegacyAuthError::RotationOutcomeUnknown
        ))
    ));
    // The unknown outcome is the unit's state, already known to this process; it
    // is not a sensitive outcome of the shutdown and is not returned as one.
    manager.shutdown().await.expect("an orderly shutdown");
    let mut restarted = rig.restarted_stopped();
    assert_eq!(
        rig.state(),
        ("signed_out".into(), Some("refresh_outcome_unknown".into()))
    );
    assert!(rig.list(&mut restarted).await.is_err());
    assert_eq!(rig.backend.token_posts().len(), 1);
}

// ---------------------------------------------------------------------------
// Test 81: collection while sign-in is required
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t81_requests_continue_while_sign_in_is_required_and_stop_with_the_access_token() {
    let rig = new_rig().await;
    // Usable until `expires_at`, t0 + 7,200.
    rig.seed(&jg(), "refresh-0", Some((T0 + 7_200, T0)));
    rig.backend.script(vec![Reply::hang()]);
    let mut manager = rig.manager();
    let client = rig.short_client(Duration::from_secs(5), Duration::from_millis(250));
    let _ = manager.refresh_if_due(&client, SystemTime::now()).await;
    assert_eq!(
        rig.state(),
        (
            "sign_in_required".into(),
            Some("refresh_outcome_unknown".into())
        )
    );
    // The owner is told at once, as attention: the Hub stays ready while the
    // access token is usable. The lease is kept fresh at each simulated
    // evaluation time.
    let lease = rig
        .store
        .acquire_supervised_collector_lease(T0 * 1_000)
        .expect("lease");
    let view_at = |now: i64| {
        rig.store
            .heartbeat_supervised_collector_lease(
                lease,
                crate::db::SupervisedCollectorState::Active,
                now * 1_000,
            )
            .expect("fresh authority");
        rig.store
            .collector_readiness_view(&crate::db::ReadinessContext::context_free(
                true,
                now * 1_000,
            ))
    };
    assert_eq!(
        rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthSignInRequired,
        ))
    );
    for now in [T0 + 1, T0 + 3_600, T0 + 7_199] {
        rig.at(now);
        let usable = view_at(now);
        assert_eq!(
            usable.service,
            Ok(()),
            "ready while the token is usable at {now}"
        );
        assert_eq!(
            usable.attention,
            Some(crate::db::CollectionAttentionCode::CollectorAuthSignInRequired)
        );
        rig.list(&mut manager).await.expect("products");
        assert_eq!(
            rig.backend
                .products()
                .last()
                .unwrap()
                .authorization
                .as_deref(),
            Some(format!("Bearer {}", jg()).as_str())
        );
    }
    assert!(rig.row().collecting);
    rig.at(T0 + 7_200);
    assert!(rig.list(&mut manager).await.is_err());
    assert_eq!(
        rig.state(),
        ("signed_out".into(), Some("refresh_outcome_unknown".into()))
    );
    assert!(!rig.row().collecting);
    assert_eq!(
        rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::SignedOut)
    );
    let expired = view_at(T0 + 7_200);
    assert_eq!(
        expired.service.err().map(|failure| failure.code),
        Some(crate::db::ReadinessReasonCode::CollectorAuthTerminal),
        "signed out is terminal from the moment the token expires"
    );
    assert_eq!(expired.attention, None);
    assert!(
        crate::legacy_auth::state::AuthState::SignedOut.lease_is_terminal(),
        "a sign-out puts the lease in `auth_terminal`"
    );
    assert_eq!(rig.backend.token_posts().len(), 1);
}

// ---------------------------------------------------------------------------
// Test 88: the fence close variants
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t88_a_released_input_is_sent_again_and_a_dead_one_is_fenced_against_every_path() {
    // Row 9: closed `http_error`, binding and fence deleted, the next attempt
    // presents refresh-0 again.
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![
        Reply::json(400, r#"{"error":"invalid_request"}"#),
        Reply::json(200, SUCCESS),
    ]);
    let mut manager = rig.manager();
    let _ = rig.refresh(&mut manager).await;
    assert_eq!(
        rig.receipts(),
        [("http_error".to_owned(), Some(400), None, false)]
    );
    assert!(
        rig.store
            .legacy_fenced_inputs_for_test()
            .unwrap()
            .is_empty()
    );
    rig.at(T0 + 3_600);
    rig.refresh(&mut manager).await.expect("second attempt");
    assert_eq!(
        rig.backend.token_posts()[1].refresh.as_deref(),
        Some("refresh-0")
    );

    // Row 8: closed `authentication_rejected` with the input kept fenced: no
    // path can present it again, not even a direct reservation or a re-import.
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend
        .script(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
    let mut manager = rig.manager();
    let g0 = rig.stored_generation();
    let _ = rig.refresh(&mut manager).await;
    assert_eq!(
        rig.receipts(),
        [("authentication_rejected".to_owned(), Some(400), None, true)]
    );
    assert!(matches!(
        rig.store.begin_legacy_refresh(g0),
        Err(crate::db::StoreError::LegacyRefreshOutcomeUnknown)
    ));
    let key = crate::teslamate_credentials::load_key(rig.directory.path()).unwrap();
    let same = OwnerTokens::from_secret_parts(jg(), "refresh-0".to_owned()).unwrap();
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &same).unwrap();
    let same = crate::db::TeslaMateLegacyTokenStore::imported(access, refresh)
        .unwrap()
        .with_credential_generation(g0)
        .unwrap();
    assert!(matches!(
        rig.store.replace_teslamate_legacy_tokens(&same),
        Err(crate::db::StoreError::LegacyRefreshOutcomeUnknown)
    ));
}

// ---------------------------------------------------------------------------
// R44a, R62: a pair written under the unit (test 87 at the manager)
// ---------------------------------------------------------------------------

fn replace_with_new_login(rig: &Rig, access: &str, refresh: &str) {
    let key = crate::teslamate_credentials::load_key(rig.directory.path()).unwrap();
    let tokens = OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned()).unwrap();
    let (cipher_access, cipher_refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens).unwrap();
    rig.store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::imported(cipher_access, cipher_refresh)
                .unwrap()
                .with_credential_generation(
                    crate::teslamate_token::legacy_refresh_credential_generation(&tokens),
                )
                .unwrap(),
        )
        .expect("an external write");
}

#[tokio::test]
async fn t87_an_external_write_before_a_due_refresh_loads_the_new_pair_and_sends_nothing() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    let old = manager.generation();
    replace_with_new_login(&rig, &jg_exp(1_800_050_000), "refresh-2");
    rig.refresh(&mut manager).await.expect("adopts");
    assert!(rig.backend.token_posts().is_empty(), "no request");
    assert_ne!(manager.generation(), old);
    assert_eq!(manager.generation(), rig.stored_generation());
    assert_eq!(manager.refresh_token(), "refresh-2");
    assert_eq!(manager.auth.stop, None);
    assert!(
        manager.auth.startup_refresh_pending,
        "an imported pair is refreshed on first need"
    );
    // The next call refreshes the new pair.
    rig.refresh(&mut manager).await.expect("refresh");
    assert_eq!(
        rig.backend.token_posts()[0].refresh.as_deref(),
        Some("refresh-2")
    );
}

#[tokio::test]
async fn t87_an_external_write_during_an_exchange_discards_the_successor_and_signs_nothing_out() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![
        Reply::json(200, SUCCESS).after(Duration::from_millis(400)),
    ]);
    let manager = Arc::new(tokio::sync::Mutex::new(rig.manager()));
    let client = rig.client();
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(&rig, 1).await;
    // The owner enters a new login while the request is in flight. The store
    // resolves the started attempt (K4).
    replace_with_new_login(&rig, &jg_exp(1_800_050_000), "refresh-2");
    caller
        .await
        .expect("task")
        .expect("no failure to the caller");
    let mut guard = manager.lock().await;
    rig.at(T0 + 5);
    rig.refresh(&mut guard).await.expect("adopts at the retry");
    assert_eq!(guard.refresh_token(), "refresh-2");
    assert_eq!(guard.auth.stop, None);
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    assert_eq!(
        rig.stored_pair().1,
        "refresh-2",
        "the successor was never written"
    );
    assert_eq!(guard.generation(), rig.stored_generation());
    assert_ne!(rig.state().0, "signed_out");
}

// ---------------------------------------------------------------------------
// R17, R19, R38, R49: the gates at the manager
// ---------------------------------------------------------------------------

#[tokio::test]
async fn r17_a_guard_that_fails_after_the_reservation_cancels_it_and_sends_nothing() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    let checks = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&checks);
    // The check before the attempt passes; the one after the reservation fails.
    manager.sensitive_access = Arc::new(move || {
        if counted.fetch_add(1, Ordering::SeqCst) < 1 {
            Ok(())
        } else {
            Err(CredentialError::SensitiveAccessUnavailable)
        }
    });
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("refused"));
    assert_eq!(error, LegacyAuthError::SensitiveAccessUnavailable);
    assert!(rig.backend.token_posts().is_empty());
    // Closed with the kept cancel: `cancelled`, no status, input released.
    assert_eq!(
        rig.receipts(),
        [("cancelled".to_owned(), None, None, false)]
    );
    assert!(
        rig.store
            .legacy_fenced_inputs_for_test()
            .unwrap()
            .is_empty()
    );
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("sensitive_access".into()),
            Some(T0 + 60),
            true
        )
    );
    // The same refresh token is presented once the authority is back.
    manager.sensitive_access = permitted_sensitive_access();
    rig.at(T0 + 60);
    rig.refresh(&mut manager).await.expect("refresh");
    assert_eq!(
        rig.backend.token_posts()[0].refresh.as_deref(),
        Some("refresh-0")
    );
}

#[tokio::test]
async fn r19_a_forced_refresh_waits_for_the_minimum_interval_and_a_longer_gate_is_reported() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    rig.refresh(&mut manager).await.expect("first refresh");
    assert_eq!(rig.backend.token_posts().len(), 1);
    // Ten seconds later a forced refresh (a 401, a refusal check) waits until
    // 60 s after the send time of the last success, then sends.
    rig.at(T0 + 10);
    rig.backend.script(vec![Reply::json(
        200,
        r#"{"access_token":"AT2","refresh_token":"refresh-2","expires_in":28800}"#,
    )]);
    manager
        .refresh_now(&rig.client(), SystemTime::now())
        .await
        .expect("forced refresh");
    assert_eq!(
        rig.env.now(),
        T0 + 60,
        "the wait moved the clock to t0 + 60"
    );
    assert_eq!(rig.backend.token_posts().len(), 2);
    assert_eq!(
        rig.backend.token_posts()[1].refresh.as_deref(),
        Some("refresh-1")
    );
    assert_eq!(manager.access_token(), "AT2");
    // A retry pending for an hour is not waited for: the caller is told when.
    rig.backend.script(vec![Reply::json(404, "")]);
    rig.at(T0 + 200);
    let _ = manager.refresh_now(&rig.client(), SystemTime::now()).await;
    let reported = auth_error(
        manager
            .refresh_now(&rig.client(), SystemTime::now())
            .await
            .expect_err("gated"),
    );
    assert_eq!(
        reported,
        LegacyAuthError::AuthUnavailable {
            retry_at: Some(T0 + 200 + 3_600)
        }
    );
    assert_eq!(rig.env.now(), T0 + 200);
}

#[tokio::test]
async fn r38_a_clock_below_the_floor_sends_nothing_and_hands_out_no_token() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    rig.at(1_500_000_000);
    rig.refresh(&mut manager).await.expect("deferred");
    assert!(rig.backend.token_posts().is_empty());
    assert!(matches!(
        manager.access_token_for_sensitive_use(),
        Err(LegacyAuthManagerError::Auth(
            LegacyAuthError::AuthUnavailable { .. }
        ))
    ));
    assert!(rig.list(&mut manager).await.is_err());
    assert!(rig.backend.products().is_empty());
    assert_eq!(row_of(&rig), ("clock_invalid".into(), None, None, false));
    assert_eq!(
        rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthWaiting,
        ))
    );
    // When the clock reads t0 again, normal operation resumes.
    rig.at(T0);
    rig.refresh(&mut manager).await.expect("refresh");
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(row_of(&rig).0, "ready");
}

#[tokio::test]
async fn r14_an_observer_never_sends_a_token_request_for_any_trigger() {
    let rig = new_rig().await;
    rig.seed(&jg(), "refresh-0", None);
    let observer = LegacyAuthManager::from_hub_teslamate_store_for_test(
        rig.store.clone(),
        rig.directory.path(),
        &rig.endpoint(None),
        Some(rig.token_url()),
        true,
        rig.env.clone(),
    );
    let mut observer = observer.expect("observer");
    for step in 0..30 {
        rig.at(T0 + step * 3_600);
        rig.refresh(&mut observer)
            .await
            .expect("an observer skips the schedule");
        assert!(matches!(
            observer.refresh_now(&rig.client(), SystemTime::now()).await,
            Err(LegacyAuthManagerError::ObserverRefreshDisabled)
        ));
    }
    assert!(rig.backend.token_posts().is_empty());
    assert_eq!(rig.stored_pair().0, jg());
}

#[tokio::test]
async fn r52a_the_status_row_is_written_on_each_change_and_never_when_nothing_changed() {
    let rig = new_rig().await;
    rig.seed(&jg(), "refresh-0", Some((T0 + 28_800, T0 + 21_600)));
    let mut manager = rig.manager();
    let updated = |rig: &Rig| {
        rig.store
            .open()
            .unwrap()
            .query_row(
                "SELECT updated_at_ms FROM collector_auth_state WHERE singleton_id = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    };
    // Written once at start, with the clock of the unit.
    let start = updated(&rig);
    assert_eq!(start, T0 * 1_000);
    // Calls that change nothing do not write.
    rig.at(T0 + 5);
    rig.list(&mut manager).await.expect("products");
    rig.at(T0 + 6);
    rig.list(&mut manager).await.expect("products");
    assert_eq!(updated(&rig), start);
    // An attempt changes the status (refreshing, then ready), and is written.
    rig.at(T0 + 21_600);
    rig.list(&mut manager).await.expect("products");
    assert!(updated(&rig) >= T0 * 1_000 + 21_600_000);
    assert_eq!(row_of(&rig).0, "ready");
}

// ---------------------------------------------------------------------------
// B2 fix round, P3b.6 (C5 R52, B2 clarification 3): the credential attention
// journal is written once per change, whatever the status row does
// ---------------------------------------------------------------------------

/// The journal lines of the credential attention at `level` in a captured log.
fn attention_lines(log: &str, level: &str) -> Vec<String> {
    log.lines()
        .filter(|line| line.contains("credential attention") && line.contains(level))
        .map(str::to_owned)
        .collect()
}

/// Drive the unit into `sign_in_required` by the row of the outcome table, with the
/// access token still usable, and return the reason it states.
async fn enter_sign_in_by(rig: &Rig, manager: &mut LegacyAuthManager, row: &str) -> &'static str {
    match row {
        "row 4" => {
            let mut body = String::from(r#"{"access_token":""#);
            body.push_str(&"a".repeat(65_537 - body.len() - 2));
            body.push_str("\"}");
            rig.backend.script(vec![Reply::json(200, &body)]);
            let _ = rig.refresh(manager).await;
            "refresh_response_unusable"
        }
        "row 6" => {
            rig.backend.script(vec![Reply::hang()]);
            let client = rig.short_client(Duration::from_secs(5), Duration::from_millis(250));
            let _ = manager.refresh_if_due(&client, SystemTime::now()).await;
            "refresh_outcome_unknown"
        }
        _ => {
            rig.backend
                .script(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
            let _ = rig.refresh(manager).await;
            "refresh_token_rejected"
        }
    }
}

#[tokio::test]
async fn credential_attention_transitions_are_logged_once_and_redacted() {
    let mut everything = String::new();
    // Every generation of every rig below, before and after an external write (B2
    // round 3, SEC-R3-N3): none may appear in a journal line.
    let mut generations: Vec<String> = Vec::new();
    // Rows 4, 6 and 8 while the access token is usable: the Hub stays ready, the
    // owner is told by the attention, and the journal says so once.
    for row in ["row 4", "row 6", "row 8"] {
        let rig = new_rig().await;
        rig.seed_due();
        crate::test_log_capture::arm();
        let mut manager = rig.manager();
        let reason = enter_sign_in_by(&rig, &mut manager, row).await;
        generations.push(manager.generation().to_string());
        assert_eq!(
            rig.state(),
            ("sign_in_required".into(), Some(reason.into())),
            "{row}"
        );
        let lease = rig
            .store
            .acquire_supervised_collector_lease(T0 * 1_000)
            .expect("lease");
        rig.store
            .heartbeat_supervised_collector_lease(
                lease,
                crate::db::SupervisedCollectorState::Active,
                (T0 + 1) * 1_000,
            )
            .expect("fresh authority");
        let usable =
            rig.store
                .collector_readiness_view(&crate::db::ReadinessContext::context_free(
                    true,
                    (T0 + 1) * 1_000,
                ));
        assert_eq!(usable.service, Ok(()), "{row}: ready while usable");
        assert_eq!(
            usable.attention,
            Some(crate::db::CollectionAttentionCode::CollectorAuthSignInRequired),
            "{row}"
        );
        // Servicing again does not repeat it.
        rig.at(T0 + 30);
        manager.service();
        let log = crate::test_log_capture::read();
        let warnings = attention_lines(&log, "WARN");
        assert_eq!(warnings.len(), 1, "{row}: {log}");
        assert!(
            warnings[0].contains("collector_auth_sign_in_required"),
            "{warnings:?}"
        );
        assert!(warnings[0].contains(reason), "{warnings:?}");
        assert!(warnings[0].contains("collector.decision"), "{warnings:?}");
        assert!(attention_lines(&log, "INFO").is_empty(), "{row}: {log}");
        everything.push_str(&crate::test_log_capture::disarm());

        if row != "row 8" {
            continue;
        }
        // From the dead-token rig on: the fuse, the retry fields and the failure of
        // the status write are not part of the identity.
        crate::test_log_capture::arm();
        manager.fail_status_writes(true);
        let attempts_before = manager.status_write_attempts().len();
        for step in 1..=3_u8 {
            manager.fuse = FuseSnapshot {
                consecutive: step,
                generation: Some(manager.generation()),
                last_rejection_at: Some(T0 + i64::from(step)),
                pause_until: None,
                rejected_access: None,
            };
            rig.at(T0 + 40 + i64::from(step) * 10);
            manager.service();
        }
        assert!(
            manager.status_write_attempts().len() > attempts_before,
            "the changes were offered to the failing store"
        );
        manager.fail_status_writes(false);
        rig.at(T0 + 120);
        manager.service();
        assert!(
            attention_lines(&crate::test_log_capture::read(), "").is_empty(),
            "a changed fuse, a retry and a failed status write add no line: {}",
            crate::test_log_capture::read()
        );
        // The owner signs in again (an external credential write): the attention
        // clears, once, with the line saying nothing of collection resuming.
        rig.seed(&jg(), "refresh-9", Some((T0 + 28_800, T0)));
        rig.at(T0 + 200);
        let _ = rig.refresh(&mut manager).await;
        generations.push(manager.generation().to_string());
        let cleared = crate::test_log_capture::read();
        let infos = attention_lines(&cleared, "INFO");
        assert_eq!(infos.len(), 1, "{cleared}");
        assert!(infos[0].contains("none"), "{infos:?}");
        assert!(attention_lines(&cleared, "WARN").is_empty(), "{cleared}");
        assert_eq!(rig.state().0, "ready");
        // The new pair is dead too: the attention is entered again, and journaled once.
        rig.backend
            .script(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
        rig.at(T0 + 400);
        let _ = rig.refresh(&mut manager).await;
        manager.service();
        generations.push(manager.generation().to_string());
        let again = crate::test_log_capture::read();
        assert_eq!(attention_lines(&again, "WARN").len(), 1, "{again}");
        assert_eq!(attention_lines(&again, "INFO").len(), 1, "{again}");
        everything.push_str(&crate::test_log_capture::disarm());
    }

    // A wait, a configuration error and a refusal.
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    crate::test_log_capture::arm();
    rig.at(100);
    manager.service();
    manager.service();
    let waiting = crate::test_log_capture::read();
    let warnings = attention_lines(&waiting, "WARN");
    assert_eq!(warnings.len(), 1, "an invalid host clock waits: {waiting}");
    assert!(
        warnings[0].contains("collector_auth_waiting"),
        "{warnings:?}"
    );
    rig.at(T0 + 1);
    manager.service();
    assert_eq!(
        attention_lines(&crate::test_log_capture::read(), "INFO").len(),
        1
    );
    manager.config_error = Some(ConfigurationReason::RegionRequired);
    manager.service();
    manager.service();
    let configuration = crate::test_log_capture::read();
    assert_eq!(
        attention_lines(&configuration, "WARN").len(),
        2,
        "{configuration}"
    );
    assert!(
        attention_lines(&configuration, "WARN")[1].contains("collector_auth_configuration_error"),
        "{configuration}"
    );
    manager.config_error = None;
    manager.service();
    assert_eq!(
        attention_lines(&crate::test_log_capture::read(), "INFO").len(),
        2
    );
    // A refusal is the C3 row's to journal: the unit adds no line of its own.
    let before = crate::test_log_capture::read();
    manager
        .auth
        .set_stop(AuthStop::ApiRefused(RefusalReason::OwnerApi));
    manager.service();
    manager.service();
    assert_eq!(
        crate::test_log_capture::read(),
        before,
        "an API refusal is no credential attention"
    );
    generations.push(manager.generation().to_string());
    everything.push_str(&crate::test_log_capture::disarm());

    // An attention the stored status restores is journaled once by the process that
    // restores it, and not again.
    let rig = new_rig().await;
    rig.seed_due();
    {
        let mut first = rig.manager();
        enter_sign_in_by(&rig, &mut first, "row 8").await;
    }
    crate::test_log_capture::arm();
    let mut restored = rig.manager();
    generations.push(restored.generation().to_string());
    restored.service();
    restored.service();
    let restart = crate::test_log_capture::disarm();
    let warnings = attention_lines(&restart, "WARN");
    assert_eq!(warnings.len(), 1, "once for the process: {restart}");
    assert!(
        warnings[0].contains("collector_auth_sign_in_required"),
        "{warnings:?}"
    );
    everything.push_str(&restart);

    // Nothing private is in any line: no token, no JWT payload, no generation.
    for secret in SECRETS {
        assert!(
            !everything.contains(secret),
            "{secret} appears in the journal"
        );
    }
    assert!(
        generations.len() >= 7,
        "every rig's generations were collected"
    );
    for generation in &generations {
        assert!(
            !everything.contains(generation),
            "a generation appears in the journal"
        );
    }
    for token in [jg(), jc()] {
        let payload = token.split('.').nth(1).unwrap().to_owned();
        assert!(
            !everything.contains(&payload),
            "a JWT payload appears in the journal"
        );
    }
}

#[path = "rejection_tests.rs"]
mod rejection_tests;
