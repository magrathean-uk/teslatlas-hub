// SPDX-License-Identifier: AGPL-3.0-only

//! C5 part P4 spec tests: rejections, the fuse and the collector boundary (spec
//! tests 28, 33 to 42, 59, 61 to 63, 64 (b), 71, 73, 75, 76, 79 (rejection
//! cases), 82, 86, 90, 91 and 93, and the credential identity of the B2
//! clarification).
//!
//! Every test uses a temporary store, the manual clock of the refresh tests and
//! an in-process fake over loopback that stands for the token endpoint and the
//! Owner API in one router, so the order of requests is one log with the time
//! each arrived. Nothing leaves the machine. Tokens, times and rows are the
//! synthetic fixtures of spec section 8: `t0` is 2027-01-15T08:00:00Z, the
//! stored pair is `(JG, refresh-0)`, the successor access tokens are `AT1`,
//! `AT2`, and so on, issued in order by the token endpoint.

use std::{
    collections::{HashSet, VecDeque},
    sync::atomic::AtomicBool,
};

use super::*;
use crate::{
    legacy_auth::{AuthMode, RejectionAction, RejectionContext, RequestScope, state::FuseSnapshot},
    owner_api::{LegacyVehicleAction, VehicleId},
};

/// One request the fake saw, with the time (the manual clock) it arrived.
#[derive(Clone, Debug)]
struct Arrived {
    /// `"token"`, `"products"`, `"probe"` or `"action"`.
    kind: &'static str,
    at: i64,
    /// The bearer token of a GET or POST to the Owner API.
    bearer: Option<String>,
    /// The `refresh_token` member of a token request.
    refresh: Option<String>,
    /// The car a request was about.
    car: Option<u64>,
}

/// What the fake does, set by the test.
struct FakeState {
    env: Arc<ManualEnv>,
    /// Access tokens answered 401 on any Owner API read.
    rejected: Mutex<HashSet<String>>,
    /// Every access token is answered 401.
    reject_all: AtomicBool,
    /// Statuses and bodies for the next products requests, then the default.
    products_script: Mutex<VecDeque<(u16, String)>>,
    /// Headers added to every answer of the products endpoint (a `Retry-After` for
    /// a 429, C5-S4).
    products_headers: Mutex<Vec<(String, String)>>,
    /// Cars whose own record is answered 401 whatever the token.
    probe_rejected: Mutex<HashSet<u64>>,
    /// The answer to a wake-up, in place of the default.
    action_answer: Mutex<Option<(u16, String)>>,
    /// Replies for the next token requests, then a rotating success.
    token_script: Mutex<VecDeque<Reply>>,
    token_calls: AtomicUsize,
    /// After serving a token request the clock moves on by this many seconds, as
    /// time does while an exchange runs.
    advance_after_token: AtomicI64,
    /// When a request carrying this token arrives the clock is set to this time
    /// before it is answered.
    set_clock_on: Mutex<Option<(String, i64)>>,
    /// The `Date` of every answer, when it is not the test clock.
    server_date: Mutex<Option<i64>>,
    log: Mutex<Vec<Arrived>>,
    /// A store whose status row is read when a request arrives, so a test can
    /// see the fuse as it stood while the unit was sending.
    watch: Mutex<Option<crate::db::HubStore>>,
    rows_seen: Mutex<Vec<(&'static str, u8)>>,
}

#[derive(Clone)]
struct Fake(Arc<FakeState>);

impl Fake {
    fn arrivals(&self, kind: &str) -> Vec<Arrived> {
        self.0
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|arrived| arrived.kind == kind)
            .cloned()
            .collect()
    }

    fn all(&self) -> Vec<Arrived> {
        self.0.log.lock().unwrap().clone()
    }

    fn posts(&self) -> Vec<Arrived> {
        self.arrivals("token")
    }

    fn reject(&self, token: &str) {
        self.0.rejected.lock().unwrap().insert(token.to_owned());
    }

    fn accept_all(&self) {
        self.0.rejected.lock().unwrap().clear();
        self.0.reject_all.store(false, Ordering::SeqCst);
    }

    fn reject_everything(&self) {
        self.0.reject_all.store(true, Ordering::SeqCst);
    }

    fn script_products(&self, statuses: &[u16]) {
        let mut script = self.0.products_script.lock().unwrap();
        script.clear();
        script.extend(statuses.iter().map(|status| (*status, "{}".to_owned())));
    }

    fn script_tokens(&self, replies: Vec<Reply>) {
        let mut script = self.0.token_script.lock().unwrap();
        script.clear();
        script.extend(replies);
    }

    /// Whether the fuse stored when the `n`th request of `kind` arrived held
    /// this count.
    fn consecutive_when(&self, kind: &'static str, which: usize) -> Option<u8> {
        self.0
            .rows_seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(seen, _)| *seen == kind)
            .nth(which)
            .map(|(_, consecutive)| *consecutive)
    }
}

fn bearer_of(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_owned)
}

fn dated(state: &FakeState, mut response: Response) -> Response {
    let date = state
        .server_date
        .lock()
        .unwrap()
        .unwrap_or_else(|| state.env.now());
    response
        .headers_mut()
        .insert("date", imf_fixdate(date).parse().unwrap());
    response
}

/// Record an arrival, run the test's hooks, and say whether the token is one the
/// fake refuses.
fn arrive(
    state: &FakeState,
    kind: &'static str,
    headers: &HeaderMap,
    car: Option<u64>,
) -> (Option<String>, bool) {
    let bearer = bearer_of(headers);
    if let Some((token, at)) = state.set_clock_on.lock().unwrap().clone()
        && bearer.as_deref() == Some(token.as_str())
    {
        state.env.set(at);
    }
    state.log.lock().unwrap().push(Arrived {
        kind,
        at: state.env.now(),
        bearer: bearer.clone(),
        refresh: None,
        car,
    });
    if let Some(store) = state.watch.lock().unwrap().as_ref() {
        let consecutive = store
            .load_collector_auth_row()
            .ok()
            .flatten()
            .map_or(0, |row| row.fuse_consecutive);
        state.rows_seen.lock().unwrap().push((kind, consecutive));
    }
    let refused = state.reject_all.load(Ordering::SeqCst)
        || bearer
            .as_ref()
            .is_some_and(|token| state.rejected.lock().unwrap().contains(token));
    (bearer, refused)
}

async fn products(State(fake): State<Fake>, headers: HeaderMap) -> Response {
    let state = &fake.0;
    let (_, refused) = arrive(state, "products", &headers, None);
    let scripted = state.products_script.lock().unwrap().pop_front();
    let mut response = match scripted {
        Some((status, body)) => (StatusCode::from_u16(status).unwrap(), body).into_response(),
        None if refused => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
        None => (StatusCode::OK, r#"{"response":[],"count":0}"#).into_response(),
    };
    for (name, value) in state.products_headers.lock().unwrap().iter() {
        response.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    dated(state, response)
}

async fn probe(
    State(fake): State<Fake>,
    axum::extract::Path(car): axum::extract::Path<u64>,
    headers: HeaderMap,
) -> Response {
    let state = &fake.0;
    let (_, refused) = arrive(state, "probe", &headers, Some(car));
    let response = if refused || state.probe_rejected.lock().unwrap().contains(&car) {
        (StatusCode::UNAUTHORIZED, "unauthorized").into_response()
    } else {
        (
            StatusCode::OK,
            r#"{"response":{"state":"online","in_service":false}}"#,
        )
            .into_response()
    };
    dated(state, response)
}

async fn wake(
    State(fake): State<Fake>,
    axum::extract::Path(car): axum::extract::Path<u64>,
    headers: HeaderMap,
) -> Response {
    let state = &fake.0;
    let (_, refused) = arrive(state, "action", &headers, Some(car));
    let scripted = state.action_answer.lock().unwrap().clone();
    let response = match scripted {
        Some((status, body)) => (StatusCode::from_u16(status).unwrap(), body).into_response(),
        None if refused => (StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
        None => (StatusCode::OK, r#"{"response":{"state":"online"}}"#).into_response(),
    };
    dated(state, response)
}

async fn token(State(fake): State<Fake>, request: Request) -> Response {
    let state = &fake.0;
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 1 << 20).await.unwrap_or_default();
    let refresh = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value.get("refresh_token")?.as_str().map(str::to_owned));
    state.log.lock().unwrap().push(Arrived {
        kind: "token",
        at: state.env.now(),
        bearer: bearer_of(&parts.headers),
        refresh,
        car: None,
    });
    let n = state.token_calls.fetch_add(1, Ordering::SeqCst) + 1;
    let scripted = state.token_script.lock().unwrap().pop_front();
    let reply = scripted.unwrap_or_else(|| {
        Reply::json(
            200,
            &format!(
                r#"{{"access_token":"AT{n}","refresh_token":"refresh-{n}","id_token":"IDTOKEN-SECRET-9","expires_in":28800,"token_type":"Bearer"}}"#
            ),
        )
    });
    if reply.hang {
        std::future::pending::<()>().await;
    }
    if !reply.delay.is_zero() {
        tokio::time::sleep(reply.delay).await;
    }
    let response = into_response(&reply, state.env.now());
    let advance = state.advance_after_token.load(Ordering::SeqCst);
    if advance > 0 {
        state.env.set(state.env.now() + advance);
    }
    response
}

/// A temporary store, a clock, the fake and the rig that holds them.
struct Bed {
    rig: Rig,
    fake: Fake,
}

async fn new_bed() -> Bed {
    crate::crypto::install_default_provider();
    let env = ManualEnv::new(T0);
    let directory = crate::private_tempdir().expect("data directory");
    let store = crate::db::HubStore::initialize(directory.path()).expect("Hub store");
    let fake = Fake(Arc::new(FakeState {
        env: env.clone(),
        rejected: Mutex::default(),
        reject_all: AtomicBool::new(false),
        products_script: Mutex::default(),
        products_headers: Mutex::default(),
        probe_rejected: Mutex::default(),
        action_answer: Mutex::default(),
        token_script: Mutex::default(),
        token_calls: AtomicUsize::new(0),
        advance_after_token: AtomicI64::new(0),
        set_clock_on: Mutex::default(),
        server_date: Mutex::default(),
        log: Mutex::default(),
        watch: Mutex::default(),
        rows_seen: Mutex::default(),
    }));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let address = listener.local_addr().expect("address");
    let router = Router::new()
        .route("/oauth2/v3/token", post(token))
        .route("/api/1/products", get(products))
        .route("/api/1/vehicles/{id}", get(probe))
        .route("/api/1/vehicles/{id}/wake_up", post(wake))
        .with_state(fake.clone());
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    // The rig's own fake is not used; its fields are what the helpers read.
    let backend = Backend {
        replies: Arc::default(),
        fallback: Arc::new(Mutex::new(Reply::json(200, SUCCESS))),
        log: Arc::default(),
        products_status: Arc::new(Mutex::new(200)),
        clock: env.clone(),
    };
    Bed {
        rig: Rig {
            directory,
            store,
            backend,
            base: Url::parse(&format!("http://{address}/")).unwrap(),
            env,
            server,
        },
        fake,
    }
}

impl Bed {
    fn api(&self) -> OwnerApi {
        self.rig.api()
    }

    /// A managed manager over the stored pair, region not configured.
    fn manager(&self) -> LegacyAuthManager {
        self.rig.manager()
    }

    /// A managed manager with a configured region, for opaque stored tokens.
    fn manager_global(&self) -> LegacyAuthManager {
        self.rig.manager_with(
            Some(crate::tesla_stream::StreamRegion::Global),
            self.rig.token_url(),
        )
    }

    /// The same, built as an observer.
    fn observer(&self) -> LegacyAuthManager {
        LegacyAuthManager::from_hub_teslamate_store_for_test(
            self.rig.store.clone(),
            self.rig.directory.path(),
            &self.rig.endpoint(None),
            Some(self.rig.token_url()),
            true,
            self.rig.env.clone(),
        )
        .expect("observer")
    }

    fn at(&self, now: i64) {
        self.rig.at(now);
    }

    fn now(&self) -> i64 {
        self.rig.env.now()
    }

    async fn list(
        &self,
        manager: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
    ) -> Result<Vec<Vehicle>, OwnerApiAuthError> {
        self.api()
            .list_vehicles_with_legacy_auth_fused(manager, fuse)
            .await
    }

    async fn probe(
        &self,
        manager: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        car: u64,
    ) -> Result<bool, OwnerApiAuthError> {
        self.api()
            .vehicle_probe_with_legacy_auth_fused(manager, fuse, VehicleId::from_test(car))
            .await
    }

    async fn wake(
        &self,
        manager: &mut LegacyAuthManager,
        fuse: &mut LegacyAuthFuse,
        car: u64,
    ) -> Result<crate::owner_api::LegacyVehicleActionResult, OwnerApiAuthError> {
        self.api()
            .execute_vehicle_action_with_legacy_auth_fused(
                manager,
                fuse,
                VehicleId::from_test(car),
                LegacyVehicleAction::Wake,
            )
            .await
    }

    /// The stored `fuse_consecutive`, or 0 without a row.
    fn stored_consecutive(&self) -> u8 {
        self.rig
            .store
            .load_collector_auth_row()
            .expect("row")
            .map_or(0, |row| row.fuse_consecutive)
    }

    /// Whether the stored status row holds the digest of the access token the stored
    /// pair holds now (hub fix pass 3): the rejection it counted is of the token still
    /// in use, so a restart owes the forced refresh. False without a row or a digest.
    fn stored_rejection_is_of_the_stored_token(&self) -> bool {
        let digest = self
            .rig
            .store
            .load_collector_auth_row()
            .expect("row")
            .and_then(|row| row.fuse_rejected_access_digest);
        digest.is_some()
            && digest
                == Some(crate::teslamate_token::legacy_access_token_digest(
                    &self.rig.stored_pair().0,
                ))
    }

    /// Store a pair as the owner would: an external credential write.
    fn external_write(&self, access: &str, refresh: &str) {
        let key = crate::teslamate_credentials::load_key(self.rig.directory.path()).expect("key");
        let tokens =
            OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned()).expect("tokens");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
                .expect("encrypt");
        self.rig
            .store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::imported(access, refresh).expect("row"),
            )
            .expect("external write");
    }
}

/// The error of a request, as the unit's own.
fn legacy_error(error: OwnerApiAuthError) -> LegacyAuthError {
    match error {
        OwnerApiAuthError::Auth(LegacyAuthManagerError::Auth(error)) => error,
        other => panic!("not an error of the unit: {other:?}"),
    }
}

fn unavailable_at(error: OwnerApiAuthError) -> Option<i64> {
    match legacy_error(error) {
        LegacyAuthError::AuthUnavailable { retry_at } => retry_at,
        other => panic!("not AuthUnavailable: {other:?}"),
    }
}

/// A refreshed-form fixture that is not due for a long while, with the token
/// `jg()`: usable until `t0 + 28,500`.
fn seed_quiet(bed: &Bed) {
    bed.rig
        .seed(&jg(), "refresh-0", Some((T0 + 28_800, T0 + 21_600)));
}

/// The context of a request carrying `carried` while the unit holds `current`.
fn context(scope: RequestScope, carried: uuid::Uuid, current: uuid::Uuid) -> RejectionContext {
    RejectionContext {
        scope,
        generation: carried,
        current,
        token_usable: true,
        remedy_pending: false,
        refresh_possible: true,
        holder: crate::legacy_auth::HolderKind::Managed,
        access_digest: gen_n(0xacce55),
        sign_in: None,
    }
}

fn gen_n(n: u128) -> uuid::Uuid {
    uuid::Uuid::from_u128(n)
}

// ---------------------------------------------------------------------------
// The fuse alone: R40, R41, R41a, R42 with exact inputs
// ---------------------------------------------------------------------------

fn car(n: u64) -> RequestScope {
    RequestScope::Vehicle(VehicleId::from_test(n))
}

#[test]
fn t34_a_storm_of_requests_carrying_one_generation_is_one_piece_of_evidence() {
    let mut fuse = LegacyAuthFuse::default();
    let (g0, g1) = (gen_n(1), gen_n(2));
    // Five cars' requests carrying g0 all get a 401 at t0. Each is a per-vehicle
    // 401 that may mean only that car's access was withdrawn (R40 step 5).
    for n in 1..=5 {
        assert_eq!(
            fuse.record_rejection(T0, &context(car(n), g0, g0)),
            RejectionAction::Confirm
        );
    }
    // Exactly one confirmation read starts; the others are held off for 60 s (R41a).
    assert_eq!(fuse.begin_confirmation(T0), Ok(()));
    for _ in 0..4 {
        assert_eq!(fuse.begin_confirmation(T0), Err(T0 + 60));
    }
    assert_eq!(fuse.take_confirmation_count(), 1);
    // The read is a 401: one counted rejection, and the force of a refresh.
    assert_eq!(
        fuse.count_rejection(T0, &context(RequestScope::Account, g0, g0)),
        RejectionAction::ForceRefreshThenRetry
    );
    assert_eq!(fuse.consecutive(), 1);
    // The rest of the storm is the same evidence: it waits for the action under way.
    for n in 2..=5 {
        assert_eq!(
            fuse.record_rejection(T0, &context(car(n), g0, g0)),
            RejectionAction::Coalesced
        );
    }
    assert_eq!(fuse.consecutive(), 1);
    // After the refresh a request that still carries g0 is superseded (R40 step 1).
    assert_eq!(
        fuse.record_rejection(T0, &context(car(2), g0, g1)),
        RejectionAction::RetryWithCurrent
    );
}

#[test]
fn t36_a_success_with_the_current_generation_resets_the_count() {
    let mut fuse = LegacyAuthFuse::default();
    let (g0, g1) = (gen_n(1), gen_n(2));
    assert_eq!(
        fuse.record_rejection(T0, &context(RequestScope::Account, g0, g0)),
        RejectionAction::ForceRefreshThenRetry
    );
    assert_eq!(fuse.consecutive(), 1);
    // The refresh gave g1; a 2xx carrying g1 disproves the rejection (R42).
    fuse.record_success(g1, RequestScope::Account);
    assert_eq!(fuse.snapshot(), FuseSnapshot::default());
    // A later 401 of g1 is the first again.
    assert_eq!(
        fuse.record_rejection(T0 + 100, &context(RequestScope::Account, g1, g1)),
        RejectionAction::ForceRefreshThenRetry
    );
    assert_eq!(fuse.consecutive(), 1);
}

#[test]
fn t37_a_rejection_is_forgotten_after_an_hour_without_another() {
    let (g0, g1, g2) = (gen_n(1), gen_n(2), gen_n(3));
    let account = |generation| context(RequestScope::Account, generation, generation);
    // 3,601 s after the last counted rejection the count starts again.
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(
        fuse.record_rejection(T0, &account(g0)),
        RejectionAction::ForceRefreshThenRetry
    );
    assert_eq!(
        fuse.record_rejection(T0 + 3_601, &account(g1)),
        RejectionAction::ForceRefreshThenRetry
    );
    assert_eq!(fuse.consecutive(), 1);
    // 3,600 s after it, the boundary, the first still counts (the window is closed
    // on that side).
    let mut fuse = LegacyAuthFuse::default();
    fuse.record_rejection(T0, &account(g0));
    assert_eq!(
        fuse.record_rejection(T0 + 3_600, &account(g1)),
        RejectionAction::PauseThenRefresh { until: T0 + 3_900 }
    );
    assert_eq!(fuse.consecutive(), 2);
    // The third rejected generation stops the account.
    assert_eq!(
        fuse.record_rejection(T0 + 3_900, &account(g2)),
        RejectionAction::Stop(SignOutReason::AccessTokenRejected)
    );
    assert_eq!(fuse.consecutive(), 3);
    assert!(fuse.is_blown());
    // A reset clears everything, the stop mirror included.
    fuse.reset();
    assert!(!fuse.is_blown());
    assert_eq!(fuse.snapshot(), FuseSnapshot::default());
}

#[test]
fn t38_a_stale_generation_is_not_counted_and_the_request_goes_again() {
    let mut fuse = LegacyAuthFuse::default();
    let (g0, g1) = (gen_n(1), gen_n(2));
    assert_eq!(
        fuse.record_rejection(T0, &context(RequestScope::Account, g0, g1)),
        RejectionAction::RetryWithCurrent
    );
    assert_eq!(fuse.snapshot(), FuseSnapshot::default());
}

#[test]
fn t39_an_expired_token_or_a_remedy_under_way_is_not_evidence() {
    let g0 = gen_n(1);
    let mut fuse = LegacyAuthFuse::default();
    let expired = RejectionContext {
        token_usable: false,
        ..context(RequestScope::Account, g0, g0)
    };
    assert_eq!(
        fuse.record_rejection(T0, &expired),
        RejectionAction::NotCounted
    );
    let pending = RejectionContext {
        remedy_pending: true,
        ..context(RequestScope::Account, g0, g0)
    };
    assert_eq!(
        fuse.record_rejection(T0, &pending),
        RejectionAction::NotCounted
    );
    assert_eq!(fuse.snapshot(), FuseSnapshot::default());
}

#[test]
fn the_columns_of_the_table_of_r41_for_each_holder() {
    use crate::legacy_auth::HolderKind;
    let g0 = gen_n(1);
    // Observer: the first counted rejection waits for the refresh owner's next pair.
    let mut fuse = LegacyAuthFuse::default();
    let observer = RejectionContext {
        holder: HolderKind::Observer,
        refresh_possible: false,
        ..context(RequestScope::Account, g0, g0)
    };
    assert_eq!(
        fuse.record_rejection(T0, &observer),
        RejectionAction::ObserverWait
    );
    // Access-token-only: a pause and one confirming read, which refuses after it.
    let mut fuse = LegacyAuthFuse::default();
    let only = RejectionContext {
        holder: HolderKind::AccessTokenOnly,
        refresh_possible: false,
        ..context(RequestScope::Account, g0, g0)
    };
    assert_eq!(
        fuse.record_rejection(T0, &only),
        RejectionAction::PauseThenConfirm { until: T0 + 300 }
    );
    // During the pause the same evidence is coalesced.
    assert_eq!(
        fuse.record_rejection(T0 + 100, &only),
        RejectionAction::Coalesced
    );
    // After it the same token is rejected again: that is the confirmation's answer.
    assert_eq!(
        fuse.record_rejection(T0 + 300, &only),
        RejectionAction::Stop(SignOutReason::ProviderTokenRejected)
    );
    // In `sign_in_required` no refresh is possible: any counted rejection stops.
    let mut fuse = LegacyAuthFuse::default();
    let sign_in = RejectionContext {
        sign_in: Some(SignOutReason::RefreshTokenRejected),
        refresh_possible: false,
        ..context(RequestScope::Account, g0, g0)
    };
    assert_eq!(
        fuse.record_rejection(T0, &sign_in),
        RejectionAction::Stop(SignOutReason::RefreshTokenRejected)
    );
}

#[test]
fn the_fuse_survives_a_snapshot_and_its_debug_shows_no_generation() {
    let mut fuse = LegacyAuthFuse::default();
    let g0 = gen_n(7);
    fuse.record_rejection(T0, &context(RequestScope::Account, g0, g0));
    let restored = LegacyAuthFuse::from_snapshot(fuse.snapshot());
    assert_eq!(restored.snapshot(), fuse.snapshot());
    let shown = format!("{fuse:?}");
    assert!(!shown.contains(&g0.to_string()), "{shown}");
    // A confirmation read is spaced from the one before it.
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(fuse.begin_confirmation(T0), Ok(()));
    fuse.finish_confirmation();
    assert_eq!(fuse.begin_confirmation(T0 + 59), Err(T0 + 60));
    assert_eq!(fuse.begin_confirmation(T0 + 60), Ok(()));
}

// ---------------------------------------------------------------------------
// The wrappers against the fake: tests 33, 35, 36, 37, 39, 41, 42
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t33_a_forced_refresh_waits_for_the_minimum_interval_after_a_success() {
    let bed = new_bed().await;
    bed.rig.seed_due();
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    // A refresh at t0, then the token it gave is refused at t0 + 10.
    bed.list(&mut manager, &mut fuse).await.expect("first list");
    assert_eq!(bed.fake.posts().len(), 1);
    bed.fake.reject("AT1");
    bed.fake.reject("AT2");
    bed.at(T0 + 10);
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("refused twice");
    // The rejection counted and forced a refresh, which was sent at t0 + 60, not
    // earlier (R19); its answer was refused as well, which is the second count.
    let posts = bed.fake.posts();
    assert_eq!(
        posts.iter().map(|post| post.at).collect::<Vec<_>>(),
        [T0, T0 + 60]
    );
    assert_eq!(fuse.consecutive(), 2);
    assert_eq!(unavailable_at(error), Some(T0 + 60 + 300));
    // Nothing was sent to the token endpoint in between.
    let seen = bed.fake.all();
    assert_eq!(
        seen.iter().map(|arrived| arrived.kind).collect::<Vec<_>>(),
        ["token", "products", "products", "token", "products"]
    );
}

#[tokio::test]
async fn t35_three_rejected_generations_sign_the_account_out_until_credentials_change() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    bed.fake.reject_everything();
    bed.fake.0.advance_after_token.store(1, Ordering::SeqCst);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let g0 = manager.generation();

    // t0: a 401 of g0, a refresh, a 401 of g1 one second later: the pause begins.
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("refused twice");
    assert_eq!(unavailable_at(error), Some(T0 + 301));
    assert_eq!(bed.fake.posts().len(), 1);
    let row = bed.rig.row();
    assert_eq!(
        (
            row.state.as_str(),
            row.fuse_consecutive,
            row.fuse_pause_until
        ),
        ("awaiting_confirmation", 2, Some(T0 + 301))
    );
    assert_eq!(
        bed.rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthWaiting,
        ))
    );
    // Requests wait for the pause: nothing is sent at t0 + 300.
    bed.at(T0 + 300);
    let before = bed.fake.all().len();
    let error = bed.list(&mut manager, &mut fuse).await.expect_err("paused");
    assert_eq!(unavailable_at(error), Some(T0 + 301));
    assert_eq!(bed.fake.all().len(), before);

    // t0 + 301: the forced refresh, then the third rejected generation.
    bed.at(T0 + 301);
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("signed out");
    assert_eq!(legacy_error(error), LegacyAuthError::AccessTokenRejected);
    let posts = bed.fake.posts();
    assert_eq!(
        posts.iter().map(|post| post.at).collect::<Vec<_>>(),
        [T0, T0 + 301]
    );
    let g2 = manager.generation();
    assert_ne!(g2, g0);
    let row = bed.rig.row();
    assert_eq!(
        (row.state.as_str(), row.reason.as_deref()),
        ("signed_out", Some("access_token_rejected"))
    );
    assert_eq!(row.stopped_generation, Some(g2));
    assert_eq!(row.fuse_consecutive, 3);
    assert!(!row.collecting);
    assert_eq!(
        bed.rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::SignedOut)
    );
    assert!(fuse.is_blown());

    // No Owner API, stream or token request in the next 24 simulated hours.
    let sent = bed.fake.all().len();
    for step in 0..48 {
        bed.at(T0 + 302 + step * 1_800);
        assert!(bed.list(&mut manager, &mut fuse).await.is_err());
        assert!(bed.probe(&mut manager, &mut fuse, 1).await.is_err());
        assert!(bed.rig.refresh(&mut manager).await.is_err());
        assert!(manager.access_token_for_sensitive_use().is_err());
        assert!(
            manager
                .refresh_now(&bed.rig.client(), SystemTime::now())
                .await
                .is_err()
        );
    }
    assert_eq!(bed.fake.all().len(), sent);
    bed.rig.scan(&manager, "");
}

#[tokio::test]
async fn t41_new_credentials_lift_a_sign_out_reset_the_fuse_and_refresh_on_first_need() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    bed.fake.reject_everything();
    bed.fake.0.advance_after_token.store(1, Ordering::SeqCst);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    let _ = bed.list(&mut manager, &mut fuse).await;
    bed.at(T0 + 301);
    let _ = bed.list(&mut manager, &mut fuse).await;
    assert_eq!(bed.rig.state().0, "signed_out");
    // The owner signs in again at t0 + 1,000: an external write of a new pair.
    bed.fake.accept_all();
    bed.at(T0 + 1_000);
    bed.external_write("AT9", "refresh-9");
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("the new pair is loaded and used");
    assert_eq!(fuse.consecutive(), 0);
    assert!(!fuse.is_blown());
    let row = bed.rig.row();
    assert_eq!((row.state.as_str(), row.fuse_consecutive), ("ready", 0));
    assert_eq!(row.stopped_generation, None);
    assert_eq!(
        bed.fake
            .arrivals("products")
            .last()
            .unwrap()
            .bearer
            .as_deref(),
        Some("AT9")
    );
    // An imported pair is refreshed on its first need (R26).
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("second list");
    let posts = bed.fake.posts();
    assert_eq!(posts.len(), 3);
    assert_eq!(posts[2].refresh.as_deref(), Some("refresh-9"));
}

#[tokio::test]
async fn t42_the_owner_retry_lifts_a_rejected_access_token_and_sends_one_request() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    bed.fake.reject_everything();
    bed.fake.0.advance_after_token.store(1, Ordering::SeqCst);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let _ = bed.list(&mut manager, &mut fuse).await;
    bed.at(T0 + 301);
    let _ = bed.list(&mut manager, &mut fuse).await;
    assert_eq!(bed.rig.state().0, "signed_out");
    // The owner's retry control: the stop is lifted and the fuse is zero.
    bed.fake.accept_all();
    let sent = bed.fake.all().len();
    manager
        .owner_retry()
        .expect("an access token's rejection lifts");
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("one request");
    assert_eq!(fuse.consecutive(), 0);
    assert_eq!(bed.rig.state().0, "ready");
    assert_eq!(
        bed.fake.all().len(),
        sent + 1,
        "exactly one Owner API request"
    );
    assert_eq!(bed.fake.posts().len(), 2, "no token request");
    assert_eq!(bed.stored_consecutive(), 0);
}

#[tokio::test]
async fn t73_the_owner_retry_lifts_only_the_stops_whose_refresh_token_is_valid() {
    let bed = new_bed().await;
    bed.rig.seed_due();
    bed.fake
        .script_tokens(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
    let mut manager = bed.manager();
    let _ = bed.rig.refresh(&mut manager).await;
    assert_eq!(bed.rig.state().0, "sign_in_required");
    let state_before = bed.rig.row();
    let sent = bed.fake.all().len();
    assert_eq!(
        manager.owner_retry(),
        Err(LegacyAuthError::RetryNotPossible {
            reason: SignOutReason::RefreshTokenRejected
        })
    );
    assert_eq!(bed.rig.row(), state_before, "the state is unchanged");
    assert_eq!(bed.fake.all().len(), sent, "no POST");
    // The same after a restart: the stop is restored and the retry still says no.
    drop(manager);
    let mut restarted = bed.manager();
    assert_eq!(
        restarted.owner_retry(),
        Err(LegacyAuthError::RetryNotPossible {
            reason: SignOutReason::RefreshTokenRejected
        })
    );
    assert_eq!(bed.fake.posts().len(), 1);
}

#[tokio::test]
async fn t93_a_rejection_in_sign_in_required_signs_out_at_once_without_a_request_to_the_token_endpoint()
 {
    let bed = new_bed().await;
    bed.rig.seed_due();
    bed.fake
        .script_tokens(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    // At t0 the refresh token is found dead; collection goes on with the stored token.
    let _ = bed.rig.refresh(&mut manager).await;
    assert_eq!(bed.rig.state().0, "sign_in_required");
    bed.fake.reject(&jg());
    bed.at(T0 + 100);
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("refused while sign-in is needed");
    assert_eq!(
        legacy_error(error),
        LegacyAuthError::SignedOut {
            reason: SignOutReason::RefreshTokenRejected
        }
    );
    assert_eq!(
        bed.rig.state(),
        ("signed_out".into(), Some("refresh_token_rejected".into()))
    );
    assert_eq!(bed.fake.posts().len(), 1, "no POST");
}

#[tokio::test]
async fn t71_a_success_clears_the_pending_retry_so_later_rejections_count() {
    let bed = new_bed().await;
    bed.rig.seed_due();
    // The token endpoint is down at t0, so a retry is pending.
    bed.fake.script_tokens(vec![Reply::json(503, "down")]);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("with the stored token");
    assert_eq!(manager.auth.retry_at(), Some(T0 + 60));
    // t0 + 60: the retry succeeds and clears the retry fields (R23).
    bed.at(T0 + 60);
    bed.rig.refresh(&mut manager).await.expect("refresh");
    assert_eq!(manager.auth.retry_at(), None);
    assert!(manager.auth.retry_reason.is_none());
    // AT1 and every later token are refused: the rejections now count, and the
    // third rejected generation signs the account out.
    bed.fake.reject_everything();
    bed.fake.0.advance_after_token.store(1, Ordering::SeqCst);
    bed.at(T0 + 100);
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("count 1 and 2");
    // The forced refresh waited for the minimum interval (t0 + 120).
    assert_eq!(
        bed.fake
            .posts()
            .iter()
            .map(|post| post.at)
            .collect::<Vec<_>>(),
        [T0, T0 + 60, T0 + 120]
    );
    let until = unavailable_at(error).expect("a pause");
    assert_eq!(until, T0 + 121 + 300);
    bed.at(until);
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("count 3");
    assert_eq!(legacy_error(error), LegacyAuthError::AccessTokenRejected);
    assert_eq!(
        bed.rig.state(),
        ("signed_out".into(), Some("access_token_rejected".into()))
    );
}

#[tokio::test]
async fn t86_a_scheduled_refresh_inside_the_pause_waits_for_the_pause_to_end() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    bed.fake.reject_everything();
    bed.fake.0.advance_after_token.store(1, Ordering::SeqCst);
    // The first refresh answers a 300 s token, whose own refresh falls due at t0 + 225.
    bed.fake.script_tokens(vec![Reply::json(
        200,
        r#"{"access_token":"AT1","refresh_token":"refresh-1","expires_in":300,"token_type":"Bearer"}"#,
    )]);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let error = bed.list(&mut manager, &mut fuse).await.expect_err("paused");
    assert_eq!(unavailable_at(error), Some(T0 + 301));
    assert_eq!(manager.auth.next_refresh_at(), T0 + 225);
    // t0 + 250: the schedule has come, but the pause has not ended: no POST.
    bed.at(T0 + 250);
    bed.rig.refresh(&mut manager).await.expect("no attempt");
    let _ = bed.list(&mut manager, &mut fuse).await;
    assert_eq!(bed.fake.posts().len(), 1);
    // t0 + 301: exactly one POST (the forced one and the scheduled one are one).
    bed.at(T0 + 301);
    let _ = bed.list(&mut manager, &mut fuse).await;
    let posts = bed.fake.posts();
    assert_eq!(
        posts.iter().map(|post| post.at).collect::<Vec<_>>(),
        [T0, T0 + 301]
    );
}

#[tokio::test]
async fn t39_a_401_to_a_token_known_to_be_expired_is_not_counted() {
    let bed = new_bed().await;
    // A refresh is due and failing: a retry is pending, and the token's own life
    // ends at t0 (its `exp` is 300 s later, less the allowance).
    bed.rig
        .seed(&jg_exp(T0 + 300), "refresh-0", Some((T0, T0 - 7_200)));
    bed.fake
        .script_tokens(vec![Reply::json(503, "down"), Reply::json(503, "down")]);
    bed.fake.reject(&jg_exp(T0 + 300));
    // The request leaves at t0 - 1 and is answered at t0 + 5.
    *bed.fake.0.set_clock_on.lock().unwrap() = Some((jg_exp(T0 + 300), T0 + 5));
    bed.at(T0 - 1);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let error = bed.list(&mut manager, &mut fuse).await.expect_err("401");
    assert!(matches!(
        legacy_error(error),
        LegacyAuthError::AuthUnavailable { .. }
    ));
    assert_eq!(fuse.consecutive(), 0, "not counted");
    assert_eq!(bed.fake.arrivals("products").len(), 1);
    // The remedy goes on: at the retry time the refresh is sent (the gates allow).
    bed.at(T0 + 59);
    bed.rig.refresh(&mut manager).await.ok();
    assert_eq!(bed.fake.posts().len(), 2);
}

#[tokio::test]
async fn t37_a_rejection_counts_again_an_hour_after_the_last_one() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    // Each list is refused; the answer to each resend is a 503, which is no
    // success and no rejection.
    bed.fake.script_products(&[401, 503]);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let error = bed.list(&mut manager, &mut fuse).await.expect_err("503");
    assert!(matches!(
        error,
        OwnerApiAuthError::Owner(crate::owner_api::OwnerApiError::HttpStatus(503))
    ));
    assert_eq!(fuse.consecutive(), 1);
    bed.fake.script_products(&[401, 503]);
    bed.at(T0 + 3_601);
    let _ = bed.list(&mut manager, &mut fuse).await;
    assert_eq!(fuse.consecutive(), 1, "the first rejection was forgotten");
    assert_eq!(bed.stored_consecutive(), 1);
}

// ---------------------------------------------------------------------------
// Observer (R28), the refusal check (R49), data-plane answers (R48), the
// per-vehicle 401 (R41a) and the crash loop (R57 step 7)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t28_an_observer_never_refreshes_waits_when_rejected_and_resumes_on_a_new_pair() {
    let bed = new_bed().await;
    bed.rig.seed(&jg(), "refresh-0", None);
    let mut manager = bed.observer();
    let mut fuse = LegacyAuthFuse::default();
    // Twenty-four simulated hours: the stored token is used while it is usable
    // (until t0 + 28,500) and no token request is ever sent.
    for hour in 0..24 {
        bed.at(T0 + hour * 3_600);
        let listed = bed.list(&mut manager, &mut fuse).await;
        assert_eq!(listed.is_ok(), hour < 8, "hour {hour}: {listed:?}");
    }
    assert!(bed.fake.posts().is_empty());
    assert!(
        bed.fake
            .arrivals("products")
            .iter()
            .all(|seen| seen.bearer.as_deref() == Some(jg().as_str()))
    );
    // Fresh clock, same pair: a products 401 makes the observer wait.
    bed.at(T0 + 7_200);
    bed.fake.reject(&jg());
    let sent = bed.fake.all().len();
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("rejected");
    assert_eq!(unavailable_at(error), Some(T0 + 7_260));
    assert_eq!(
        bed.rig.state(),
        ("observer_waiting".into(), Some("token_rejected".into()))
    );
    assert!(!bed.rig.row().collecting);
    assert_eq!(
        bed.rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthWaiting,
        ))
    );
    // No further Owner API request, and no stream admission (the token read fails).
    let _ = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("waiting");
    let _ = bed
        .probe(&mut manager, &mut fuse, 1)
        .await
        .expect_err("waiting");
    assert!(manager.access_token_for_sensitive_use().is_err());
    assert_eq!(
        bed.fake.all().len(),
        sent + 1,
        "only the rejected request was sent"
    );
    // The refresh owner's next pair: an external write. Within 60 s the next
    // request carries it and the state is ready.
    let fresh = jg_exp(T0 + 40_000);
    bed.external_write(&fresh, "refresh-5");
    bed.at(T0 + 7_230);
    assert!(
        bed.list(&mut manager, &mut fuse).await.is_err(),
        "not looked at yet"
    );
    bed.at(T0 + 7_260);
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("the new pair");
    assert_eq!(
        bed.fake
            .arrivals("products")
            .last()
            .unwrap()
            .bearer
            .as_deref(),
        Some(fresh.as_str())
    );
    assert_eq!(bed.rig.state().0, "ready");
    assert!(bed.fake.posts().is_empty(), "an observer never posts");
}

#[tokio::test]
async fn t59_a_403_naming_the_fleet_api_is_returned_unchanged_and_changes_nothing() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    bed.fake
        .0
        .products_script
        .lock()
        .unwrap()
        .push_back((403, r#"{"error":"Use the Fleet API"}"#.to_owned()));
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let before_row = bed.rig.row();
    let before_pair = bed.rig.stored_pair();
    let before_generation = bed.rig.stored_generation();
    let error = bed.list(&mut manager, &mut fuse).await.expect_err("403");
    assert!(matches!(
        error,
        OwnerApiAuthError::Owner(crate::owner_api::OwnerApiError::Forbidden {
            fleet_api_named: true
        })
    ));
    assert!(bed.fake.posts().is_empty());
    assert_eq!(fuse.consecutive(), 0);
    assert_eq!(bed.rig.row(), before_row, "no state change");
    assert_eq!(
        bed.rig.stored_pair(),
        before_pair,
        "the pair row is as it was"
    );
    assert_eq!(bed.rig.stored_generation(), before_generation);
}

#[tokio::test]
async fn t61_the_refusal_check_refreshes_once_per_generation_and_waits_for_the_interval() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    let mut manager = bed.manager();
    let client = bed.rig.client();
    let g0 = manager.generation();
    // A plain 403 carrying g0: one POST, and the pair that comes back.
    let RefusalRefresh::Refreshed(g1) = manager.refusal_check_refresh(&client, Some(g0)).await
    else {
        panic!("a refresh");
    };
    assert_ne!(g1, g0);
    assert_eq!(bed.fake.posts().len(), 1);
    // A second ask for g0 needs no exchange: the pair is already newer.
    let RefusalRefresh::Refreshed(again) = manager.refusal_check_refresh(&client, Some(g0)).await
    else {
        panic!("the current generation");
    };
    assert_eq!(again, g1);
    assert_eq!(bed.fake.posts().len(), 1);
    // An ask for g1 within 60 s of the last success is sent at that success + 60.
    bed.at(T0 + 10);
    let RefusalRefresh::Refreshed(g2) = manager.refusal_check_refresh(&client, Some(g1)).await
    else {
        panic!("a second refresh");
    };
    assert_ne!(g2, g1);
    let posts = bed.fake.posts();
    assert_eq!(
        posts.iter().map(|post| post.at).collect::<Vec<_>>(),
        [T0, T0 + 60]
    );
}

#[tokio::test]
async fn t62_a_403_on_a_resident_action_fails_the_action_and_reports_nothing() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    *bed.fake.0.action_answer.lock().unwrap() =
        Some((403, r#"{"error":"Use the Fleet API"}"#.to_owned()));
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let before_row = bed.rig.row();
    let error = bed
        .wake(&mut manager, &mut fuse, 70)
        .await
        .expect_err("403");
    assert!(matches!(
        error,
        OwnerApiAuthError::Owner(crate::owner_api::OwnerApiError::HttpStatus(403))
    ));
    assert!(bed.fake.posts().is_empty());
    assert_eq!(bed.rig.row(), before_row);
    assert_eq!(fuse.consecutive(), 0);
    // A 401 to an action is evidence like any 401, but the action is never sent again.
    *bed.fake.0.action_answer.lock().unwrap() = None;
    bed.fake.reject(&jg());
    let error = bed
        .wake(&mut manager, &mut fuse, 70)
        .await
        .expect_err("401");
    assert!(matches!(
        legacy_error(error),
        LegacyAuthError::AuthUnavailable { .. }
    ));
    assert_eq!(
        bed.fake.arrivals("action").len(),
        2,
        "each action was sent once"
    );
    assert_eq!(
        bed.fake.arrivals("products").len(),
        1,
        "an action's 401 is confirmed by one list, as a car's read is"
    );
}

#[tokio::test]
async fn t63_an_observer_s_refusal_check_sends_nothing_and_changes_no_state() {
    let bed = new_bed().await;
    bed.rig.seed(&jg(), "refresh-0", None);
    let mut manager = bed.observer();
    let before = bed.rig.row();
    let client = bed.rig.client();
    let outcome = manager
        .refusal_check_refresh(&client, Some(manager.generation()))
        .await;
    assert!(
        matches!(outcome, RefusalRefresh::ObserverRefreshDisabled),
        "{outcome:?}"
    );
    assert!(bed.fake.all().is_empty());
    assert_eq!(bed.rig.row(), before);
}

#[tokio::test]
async fn t75_a_cars_own_401_is_confirmed_by_the_list_before_the_account_is_blamed() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let g0 = manager.generation();
    // Car 1's own record is refused; the list is fine.
    bed.fake.0.probe_rejected.lock().unwrap().insert(1);
    let error = bed
        .probe(&mut manager, &mut fuse, 1)
        .await
        .expect_err("car 1");
    assert_eq!(legacy_error(error), LegacyAuthError::VehicleAccessRejected);
    assert_eq!(
        bed.fake
            .arrivals("probe")
            .iter()
            .map(|seen| seen.car)
            .collect::<Vec<_>>(),
        [Some(1)]
    );
    assert!(bed.fake.posts().is_empty());
    assert_eq!(fuse.consecutive(), 0);
    assert_eq!(fuse.vehicle_mark(VehicleId::from_test(1)), Some(g0));
    // Car 2 is still sent and carries g0.
    bed.probe(&mut manager, &mut fuse, 2)
        .await
        .expect("car 2 reads");
    assert_eq!(
        bed.fake.arrivals("probe").last().unwrap().bearer.as_deref(),
        Some(jg().as_str())
    );
    // Car 1's next 401 with g0 needs no confirmation read and no POST.
    let reads = bed.fake.arrivals("products").len();
    assert_eq!(reads, 1);
    let error = bed
        .probe(&mut manager, &mut fuse, 1)
        .await
        .expect_err("marked");
    assert_eq!(legacy_error(error), LegacyAuthError::VehicleAccessRejected);
    assert_eq!(bed.fake.arrivals("products").len(), reads);
    assert!(bed.fake.posts().is_empty());
    // A scheduled refresh to g1: car 1's next 401 is confirmed again.
    bed.at(T0 + 21_600);
    bed.rig
        .refresh(&mut manager)
        .await
        .expect("scheduled refresh");
    assert_eq!(bed.fake.posts().len(), 1);
    let _ = bed.probe(&mut manager, &mut fuse, 1).await.expect_err("g1");
    assert_eq!(bed.fake.arrivals("products").len(), reads + 1);
    assert_eq!(
        fuse.vehicle_mark(VehicleId::from_test(1)),
        Some(manager.generation())
    );
    // A 2xx for car 1 clears its mark.
    bed.fake.0.probe_rejected.lock().unwrap().clear();
    bed.probe(&mut manager, &mut fuse, 1).await.expect("2xx");
    assert_eq!(fuse.vehicle_mark(VehicleId::from_test(1)), None);
    // After a restart car 1's first 401 is confirmed again, and rotates nothing.
    drop(manager);
    let mut restarted = bed.manager_global();
    let mut fresh_fuse = LegacyAuthFuse::default();
    bed.fake.0.probe_rejected.lock().unwrap().insert(1);
    let posts = bed.fake.posts().len();
    let reads = bed.fake.arrivals("products").len();
    let error = bed
        .probe(&mut restarted, &mut fresh_fuse, 1)
        .await
        .expect_err("car 1 again");
    assert_eq!(legacy_error(error), LegacyAuthError::VehicleAccessRejected);
    assert_eq!(bed.fake.arrivals("products").len(), reads + 1);
    assert_eq!(bed.fake.posts().len(), posts);
}

#[tokio::test]
async fn t76_a_crash_loop_cannot_evade_the_fuse() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    // t0: a 401 of g0, a forced POST that succeeds and is persisted, and the
    // process is killed before the resend is answered.
    bed.fake.script_products(&[401, 503]);
    {
        let mut manager = bed.manager_global();
        let mut fuse = LegacyAuthFuse::default();
        let _ = bed.list(&mut manager, &mut fuse).await;
        assert_eq!(bed.fake.posts().len(), 1);
        assert_eq!(bed.stored_consecutive(), 1);
    }
    // Restart at t0 + 10: the fuse is restored (consecutive 1, rejected g0), and the
    // pair is the one the forced refresh persisted before the process was killed.
    bed.fake.reject_everything();
    bed.at(T0 + 10);
    let mut manager = bed.manager_global();
    assert!(manager.test_pair_matches("AT1", "refresh-1").expect("pair"));
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(manager.status_now().fuse.consecutive, 1);
    // t0 + 11: a 401 of g1 is the second count; the pause runs to t0 + 311.
    bed.at(T0 + 11);
    let error = bed.list(&mut manager, &mut fuse).await.expect_err("paused");
    assert_eq!(unavailable_at(error), Some(T0 + 311));
    assert_eq!(bed.rig.state().0, "awaiting_confirmation");
    // No POST before t0 + 311, however often it is asked.
    for at in [T0 + 12, T0 + 100, T0 + 310] {
        bed.at(at);
        let _ = bed.list(&mut manager, &mut fuse).await;
        let _ = bed.rig.refresh(&mut manager).await;
        assert_eq!(bed.fake.posts().len(), 1, "at {at}");
    }
    // Variant: killed again at t0 + 100 and restarted: the POST is sent at t0 + 311.
    drop(manager);
    bed.at(T0 + 100);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(bed.rig.state().0, "awaiting_confirmation");
    bed.at(T0 + 311);
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("signed out");
    assert_eq!(legacy_error(error), LegacyAuthError::AccessTokenRejected);
    let posts = bed.fake.posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[1].at, T0 + 311);
    assert_eq!(
        bed.rig.state(),
        ("signed_out".into(), Some("access_token_rejected".into()))
    );
}

#[tokio::test]
async fn the_fuse_is_stored_as_it_changes_and_the_refresh_does_not_reset_it() {
    // The status row holds the fuse while it counts (R52a), and a refresh that
    // the fuse forced does not clear it (R42): the 2xx that follows does.
    let bed = new_bed().await;
    seed_quiet(&bed);
    *bed.fake.0.watch.lock().unwrap() = Some(bed.rig.store.clone());
    bed.fake.script_products(&[401]);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("refreshed and resent");
    // When the resend arrived the stored fuse held the count of the rejection.
    assert_eq!(bed.fake.consecutive_when("products", 0), Some(0));
    assert_eq!(bed.fake.consecutive_when("products", 1), Some(1));
    // The 2xx reset it.
    assert_eq!(bed.stored_consecutive(), 0);
    assert_eq!(fuse.consecutive(), 0);
    // Test 36: a later 401 of the new generation is the first again (the count was
    // cleared by the 2xx, not carried over by the refresh), as the unit sends again.
    bed.fake.reject("AT1");
    bed.at(T0 + 100);
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("refreshed and resent again");
    assert_eq!(bed.fake.consecutive_when("products", 3), Some(1));
    assert_eq!(bed.stored_consecutive(), 0);
}

#[tokio::test]
async fn the_credential_identity_changes_with_an_external_write_and_never_with_a_refresh() {
    let bed = new_bed().await;
    bed.rig.seed_due();
    let identity = || bed.rig.store.collector_credential_identity(false).unwrap();
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let before = identity();
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("a refresh and a list");
    assert_eq!(bed.fake.posts().len(), 1);
    // The Hub's own refresh changed the pair but not the identity (B2 clarification;
    // pairs with C3 CT72 (g)).
    assert_eq!(identity(), before);
    // The owner storing credentials does.
    drop(manager);
    bed.external_write("AT9", "refresh-9");
    assert_ne!(identity(), before);
}

#[tokio::test]
async fn t64b_a_refusal_c3_detected_is_restored_for_the_same_mode_and_not_for_another() {
    let insert = |bed: &Bed, mode: &str, state: &str, reason: &str| {
        bed.rig
            .store
            .open()
            .expect("catalogue")
            .execute(
                "INSERT INTO collector_account_state
                    (singleton_id, mode, config_fingerprint, state, reason_code, since_ms, updated_at_ms)
                 VALUES (1, ?1, ?2, ?3, ?4, 1000, 1000)",
                rusqlite::params![mode, "0".repeat(64), state, reason],
            )
            .expect("C3 account row");
    };
    // Legacy configuration, C3's row says the Owner API was refused: restored.
    let bed = new_bed().await;
    seed_quiet(&bed);
    insert(&bed, "legacy", "owner_api_refused", "owner_api_refused");
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(
        bed.rig.state().0,
        "ready",
        "before the collector's start path"
    );
    manager.apply_collector_start(&CollectorStartView {
        reset_refusal: false,
        account: Some(("legacy", "owner_api_refused", "owner_api_refused")),
    });
    let row = bed.rig.row();
    assert_eq!(
        (
            row.state.as_str(),
            row.reason.as_deref(),
            row.stopped_mode.as_deref()
        ),
        ("api_refused", Some("owner_api"), Some("legacy"))
    );
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("refused");
    assert_eq!(
        legacy_error(error),
        LegacyAuthError::OwnerApiRefused {
            reason: RefusalReason::OwnerApi
        }
    );
    assert!(bed.fake.all().is_empty(), "no requests");
    assert_eq!(manager.wait_view(T0), AuthWait::Stopped);
    // A client refusal reason is restored as such, and a restart holds it with no
    // C3 row at all (step 5 (b)).
    drop(manager);
    let restarted = bed.manager();
    assert_eq!(restarted.status_now().state, AuthState::ApiRefused);

    // The same row for Provider mode: not restored.
    let bed = new_bed().await;
    seed_quiet(&bed);
    insert(&bed, "legacy", "owner_api_refused", "owner_api_refused");
    let mut manager = bed.manager();
    manager.apply_collector_start(&CollectorStartView {
        reset_refusal: false,
        account: Some(("provider", "owner_api_refused", "owner_api_refused")),
    });
    assert_eq!(bed.rig.state().0, "ready");

    // C3 reset its row at this start: the refusal this unit restored is cleared.
    let bed = new_bed().await;
    seed_quiet(&bed);
    insert(&bed, "legacy", "collecting", "config_changed");
    {
        let mut manager = bed.manager();
        manager.apply_collector_start(&CollectorStartView {
            reset_refusal: false,
            account: Some(("legacy", "provider_refused", "provider_client_refused")),
        });
        let row = bed.rig.row();
        assert_eq!(
            (row.state.as_str(), row.reason.as_deref()),
            ("api_refused", Some("client"))
        );
    }
    let mut restarted = bed.manager();
    assert_eq!(restarted.status_now().state, AuthState::ApiRefused);
    restarted.apply_collector_start(&CollectorStartView {
        reset_refusal: true,
        account: Some(("legacy", "collecting", "config_changed")),
    });
    assert_eq!(bed.rig.state().0, "ready");
    assert_eq!(bed.rig.row().stopped_mode, None);
}

// ---------------------------------------------------------------------------
// Access-token-only providers (R8a, test 79, rejection cases) and the clock
// ---------------------------------------------------------------------------

fn access_only(bed: &Bed, token: &str) -> LegacyAuthManager {
    let auth = LegacyAuth::from_persisted_state(
        token,
        "",
        0,
        0,
        &AuthEndpointConfig {
            mode: AuthMode::Provider,
            region: None,
            token_url: None,
            allow_loopback_http: false,
        },
    )
    .expect("an access-token-only pair");
    LegacyAuthManager::for_test_with_env(auth, Arc::new(|_, _| Ok(())), bed.rig.env.clone())
}

#[tokio::test]
async fn t79_an_access_token_only_pair_expires_by_its_own_exp_and_never_posts() {
    let bed = new_bed().await;
    let token = jwt_for_test(
        serde_json::json!({"iss": "https://issuer.example.test/oauth2/v3", "exp": T0 + 600}),
    );
    let mut manager = access_only(&bed, &token);
    let mut fuse = LegacyAuthFuse::default();
    bed.at(T0 + 299);
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("usable until exp - 300");
    // At t0 + 300 the token is past its usability: signed out, expired.
    bed.at(T0 + 300);
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("expired");
    assert_eq!(legacy_error(error), LegacyAuthError::ProviderTokenExpired);
    let status = manager.status_now();
    assert_eq!(
        (status.state, status.reason.map(|reason| reason.code())),
        (AuthState::SignedOut, Some("provider_token_expired"))
    );
    // No token request in the next 24 hours, whatever asks.
    for step in 0..48 {
        bed.at(T0 + 300 + step * 1_800);
        let _ = manager
            .refresh_now(&bed.rig.client(), SystemTime::now())
            .await;
        let _ = bed.list(&mut manager, &mut fuse).await;
    }
    assert!(bed.fake.posts().is_empty());
}

#[tokio::test]
async fn t79_a_rejected_access_token_only_token_pauses_then_one_read_decides() {
    // The 401 starts a pause; after it one read with the same token decides.
    for accepted in [false, true] {
        let bed = new_bed().await;
        let mut manager = access_only(&bed, "provider-token-0001");
        let mut fuse = LegacyAuthFuse::default();
        bed.fake.reject("provider-token-0001");
        let error = bed.list(&mut manager, &mut fuse).await.expect_err("401");
        assert_eq!(unavailable_at(error), Some(T0 + 300));
        assert_eq!(manager.status_now().state, AuthState::AwaitingConfirmation);
        // Nothing is sent during the pause.
        bed.at(T0 + 299);
        let sent = bed.fake.all().len();
        assert!(bed.list(&mut manager, &mut fuse).await.is_err());
        assert_eq!(bed.fake.all().len(), sent);
        // At t0 + 300 the one confirming read.
        bed.at(T0 + 300);
        if accepted {
            bed.fake.accept_all();
            bed.list(&mut manager, &mut fuse).await.expect("2xx");
            assert_eq!(manager.status_now().state, AuthState::Ready);
            assert_eq!(fuse.consecutive(), 0);
        } else {
            let error = bed
                .list(&mut manager, &mut fuse)
                .await
                .expect_err("401 again");
            assert_eq!(legacy_error(error), LegacyAuthError::ProviderTokenRejected);
            let status = manager.status_now();
            assert_eq!(
                (status.state, status.reason.map(|reason| reason.code())),
                (AuthState::SignedOut, Some("provider_token_rejected"))
            );
            // The owner's retry lifts it (R44).
            bed.fake.accept_all();
            manager.owner_retry().expect("lifts");
            bed.list(&mut manager, &mut fuse)
                .await
                .expect("one request");
            assert_eq!(manager.status_now().state, AuthState::Ready);
        }
        assert!(bed.fake.posts().is_empty(), "no refresh path");
    }
}

#[tokio::test]
async fn t82_an_observer_on_a_fast_clock_is_told_it_is_waiting_because_of_the_clock() {
    let bed = new_bed().await;
    bed.rig.seed(&jg(), "refresh-0", None);
    let mut manager = bed.observer();
    let mut fuse = LegacyAuthFuse::default();
    // The local clock is 2 h ahead of the server's: an answer received at local
    // t0 + 28,400 carries the server's `Date`, t0 + 21,200.
    *bed.fake.0.server_date.lock().unwrap() = Some(T0 + 21_200);
    bed.at(T0 + 28_400);
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("still usable");
    assert!(bed.rig.row().clock_skew, "the skew flag is stored");
    // At local t0 + 28,500 the token is past usability: the reason is the clock.
    bed.at(T0 + 28_500);
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("waiting");
    assert!(matches!(
        legacy_error(error),
        LegacyAuthError::AuthUnavailable { .. }
    ));
    assert_eq!(
        bed.rig.state(),
        ("observer_waiting".into(), Some("clock_skew".into()))
    );
    assert_eq!(
        bed.rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthWaiting,
        ))
    );
    let sent = bed.fake.all().len();
    let _ = bed.list(&mut manager, &mut fuse).await;
    assert_eq!(bed.fake.all().len(), sent, "no further request");
}

#[tokio::test]
async fn t91_an_observer_stops_collecting_at_the_expiry_of_an_imported_token_and_resumes_on_a_new_pair()
 {
    let bed = new_bed().await;
    bed.rig.seed(&jg(), "refresh-0", None);
    let mut manager = bed.observer();
    let mut fuse = LegacyAuthFuse::default();
    bed.at(T0 + 28_500);
    let _ = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("expired");
    assert_eq!(
        bed.rig.state(),
        ("observer_waiting".into(), Some("token_expired".into()))
    );
    assert_eq!(
        bed.rig.store.collector_auth_readiness().unwrap(),
        Some(crate::db::CollectorAuthReadiness::Attention(
            crate::db::CollectionAttentionCode::CollectorAuthWaiting,
        ))
    );
    assert!(
        bed.fake.all().is_empty(),
        "no token request, no Owner API request"
    );
    // An external write restores `ready` within 60 s.
    let fresh = jg_exp(T0 + 60_000);
    bed.external_write(&fresh, "refresh-5");
    bed.at(T0 + 28_520);
    assert!(bed.list(&mut manager, &mut fuse).await.is_err());
    bed.at(T0 + 28_560);
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("the new pair");
    assert_eq!(bed.rig.state().0, "ready");
    assert!(bed.fake.posts().is_empty());
}

// ---------------------------------------------------------------------------
// R39: what the collector may do with its next request
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t90_the_collector_s_wait_follows_the_unit_s_own_times() {
    let bed = new_bed().await;
    // A token whose usability ends at t0 and whose refresh is failing with a
    // `Retry-After`: the whole account waits for the retry.
    bed.rig
        .seed(&jg_exp(T0 + 300), "refresh-0", Some((T0, T0 - 7_200)));
    bed.fake.script_tokens(vec![
        Reply::json(503, "down").with_header("retry-after", "90"),
    ]);
    let mut manager = bed.manager();
    assert_eq!(manager.wait_view(T0 - 1), AuthWait::Ready, "usable");
    let _ = bed.rig.refresh(&mut manager).await;
    assert_eq!(manager.auth.retry_at(), Some(T0 + 90));
    let secs = Duration::from_secs;
    assert_eq!(manager.wait_view(T0), AuthWait::For(secs(90)));
    // A retry time that has just passed is never a spin: at least one second.
    assert_eq!(manager.wait_view(T0 + 95), AuthWait::For(secs(1)));

    // A wait is never longer than an hour, whatever the clock says: the token's
    // own life ended long ago, the retry is an hour away and the clock reads
    // 10,000 s before it.
    let bed = new_bed().await;
    bed.rig
        .seed(&jg_exp(T0 - 100_000), "refresh-0", Some((T0, T0 - 7_200)));
    bed.fake.script_tokens(vec![
        Reply::json(503, "down").with_header("retry-after", "3600"),
    ]);
    let mut manager = bed.manager();
    let _ = bed.rig.refresh(&mut manager).await;
    assert_eq!(manager.auth.retry_at(), Some(T0 + 3_600));
    assert_eq!(
        manager.wait_view(T0 + 3_600 - 10_000),
        AuthWait::For(secs(3_600))
    );

    // An exchange in flight: the caller waits for its result, bounded by the
    // exchange's own timeout, and the dropped caller does not end it.
    let bed = new_bed().await;
    bed.rig.seed_due();
    bed.fake.script_tokens(vec![Reply::hang()]);
    let manager = Arc::new(tokio::sync::Mutex::new(bed.manager()));
    let client = bed.rig.client();
    let started = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move {
            let mut guard = manager.lock().await;
            let _ = guard.refresh_if_due(&client, SystemTime::now()).await;
        }
    });
    while bed.fake.posts().is_empty() {
        tokio::task::yield_now().await;
    }
    started.abort();
    let _ = started.await;
    assert_eq!(
        manager.lock().await.wait_view(T0),
        AuthWait::UntilExchangeEnds {
            at_most: Duration::from_secs(60)
        }
    );

    // Signed out: the collector stops.
    let bed = new_bed().await;
    seed_quiet(&bed);
    bed.fake.reject_everything();
    bed.fake.0.advance_after_token.store(1, Ordering::SeqCst);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let _ = bed.list(&mut manager, &mut fuse).await;
    bed.at(T0 + 301);
    let _ = bed.list(&mut manager, &mut fuse).await;
    assert_eq!(manager.wait_view(bed.now()), AuthWait::Stopped);
    // A refusal, and a configuration error, stop it too.
    assert!(manager.stop_view(bed.now()).is_some());
}

#[tokio::test]
async fn a_token_past_its_usability_is_due_whatever_the_stored_schedule_says() {
    // A row whose `next_refresh_at` lies beyond the token's own `exp` (R25) leaves the
    // account without a usable token until the schedule comes, unless the refresh is
    // owed at once; the unit has a refresh path, so it is.
    let bed = new_bed().await;
    bed.rig.seed(
        &jg_exp(T0 + 300),
        "refresh-0",
        Some((T0 + 20_000, T0 + 10_000)),
    );
    let mut manager = bed.manager();
    assert_eq!(manager.auth.usable_until(), Some(T0));
    assert!(!manager.auth.is_due(T0 - 1), "usable and not yet scheduled");
    assert!(manager.auth.is_due(T0), "the token has expired");
    // And it is refreshed, not waited for: the first request that needs it refreshes.
    bed.at(T0);
    let mut fuse = LegacyAuthFuse::default();
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("a refresh, then the list");
    assert_eq!(bed.fake.posts().len(), 1);
    // A pair with no refresh path is never due: it signs out instead (R8a).
    let only = access_only(&bed, "provider-token-0001");
    assert!(!only.auth.is_due(T0 + 1_000_000));
}

// ---------------------------------------------------------------------------
// B2 P3: R57 step 6, the refusal overlay and the stops it may not erase
// ---------------------------------------------------------------------------

/// C3's account row, as an earlier run left it.
fn p3_insert_c3(bed: &Bed, mode: &str, state: &str, reason: &str) {
    bed.rig
        .store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO collector_account_state
                (singleton_id, mode, config_fingerprint, state, reason_code, since_ms, updated_at_ms)
             VALUES (1, ?1, ?2, ?3, ?4, 1000, 1000)",
            rusqlite::params![mode, "0".repeat(64), state, reason],
        )
        .expect("C3 account row");
}

/// What the collector's start path tells the unit when C3's row holds a refusal of
/// the configured mode and no reset happened.
const P3_REFUSED_VIEW: CollectorStartView<'static> = CollectorStartView {
    reset_refusal: false,
    account: Some(("legacy", "owner_api_refused", "owner_api_refused")),
};

/// A manager signed out with `refresh_token_rejected`, the attempt resolved: the
/// refresh token was found dead, and the access token was then rejected too.
async fn p3_signed_out_bed() -> (Bed, LegacyAuthManager) {
    let bed = new_bed().await;
    bed.rig.seed_due();
    bed.fake
        .script_tokens(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let _ = bed.rig.refresh(&mut manager).await;
    bed.fake.reject(&jg());
    bed.at(T0 + 100);
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("signed out");
    assert_eq!(
        legacy_error(error),
        LegacyAuthError::SignedOut {
            reason: SignOutReason::RefreshTokenRejected
        }
    );
    assert_eq!(
        bed.rig.state(),
        ("signed_out".into(), Some("refresh_token_rejected".into()))
    );
    assert!(!bed.rig.store.has_unresolved_legacy_refresh().unwrap());
    (bed, manager)
}

#[tokio::test]
async fn cs_p3_step6_keeps_signed_out_after_retry() {
    let (bed, manager) = p3_signed_out_bed().await;
    drop(manager);
    p3_insert_c3(&bed, "legacy", "owner_api_refused", "owner_api_refused");
    // A restart: the matching-generation signed-out stop is restored, with no
    // unresolved attempt behind it, and the collector's start path then reports C3's
    // refusal for the configured mode.
    let mut restarted = bed.manager();
    let row_before = bed.rig.row();
    let status_before = restarted.status_now();
    let posts = bed.fake.posts().len();
    restarted.apply_collector_start(&P3_REFUSED_VIEW);
    // Nothing changed and nothing was written: the stop, its reason, its generation
    // and the row are the same, the handout is blocked and no retry can lift it.
    assert_eq!(bed.rig.row(), row_before);
    assert_eq!(restarted.status_now(), status_before);
    assert!(matches!(
        restarted.access_token_for_sensitive_use(),
        Err(LegacyAuthManagerError::Auth(LegacyAuthError::SignedOut {
            reason: SignOutReason::RefreshTokenRejected
        }))
    ));
    assert_eq!(
        restarted.owner_retry(),
        Err(LegacyAuthError::RetryNotPossible {
            reason: SignOutReason::RefreshTokenRejected
        })
    );
    assert_eq!(bed.rig.row(), row_before);
    assert_eq!(bed.fake.posts().len(), posts, "no POST");
    // The control: a refusal this unit may take (a ready unit) is overlaid, and the
    // owner's retry does lift it.
    let ready = new_bed().await;
    seed_quiet(&ready);
    p3_insert_c3(&ready, "legacy", "owner_api_refused", "owner_api_refused");
    let mut manager = ready.manager();
    manager.apply_collector_start(&P3_REFUSED_VIEW);
    assert_eq!(ready.rig.state().0, "api_refused");
    manager
        .owner_retry()
        .expect("a refusal lifts on the owner's retry");
    assert_eq!(ready.rig.state().0, "ready");
}

/// A manager whose refresh ended as row 4 (no answer), row 6 (an unusable
/// answer) or row 8 (a dead refresh token) of C5's outcome table, restarted over
/// its store, with C3's refusal row beside it.
async fn p3_outcome_bed(row: u8) -> (Bed, LegacyAuthManager) {
    let bed = new_bed().await;
    bed.rig.seed_due();
    let mut manager = bed.manager();
    match row {
        4 => {
            bed.fake.script_tokens(vec![Reply::hang()]);
            let client = bed
                .rig
                .short_client(Duration::from_secs(5), Duration::from_millis(300));
            let error = manager
                .refresh_if_due(&client, SystemTime::now())
                .await
                .expect_err("no answer");
            assert!(matches!(
                error,
                LegacyAuthManagerError::Auth(LegacyAuthError::RotationOutcomeUnknown)
            ));
        }
        6 => {
            let mut body = String::from(r#"{"access_token":""#);
            body.push_str(&"a".repeat(65_537 - body.len() - 2));
            body.push_str("\"}");
            bed.fake.script_tokens(vec![Reply::json(200, &body)]);
            let _ = bed.rig.refresh(&mut manager).await;
        }
        _ => {
            bed.fake
                .script_tokens(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
            let _ = bed.rig.refresh(&mut manager).await;
        }
    }
    assert_eq!(bed.rig.state().0, "sign_in_required", "row {row}");
    assert_eq!(
        bed.rig.store.has_unresolved_legacy_refresh().unwrap(),
        row != 8,
        "row {row}"
    );
    drop(manager);
    p3_insert_c3(&bed, "legacy", "owner_api_refused", "owner_api_refused");
    let restarted = bed.manager();
    (bed, restarted)
}

#[tokio::test]
async fn cs_p3_step6_sign_in_and_unresolved_overlay_cannot_replay() {
    for row in [4_u8, 6, 8] {
        let (bed, mut manager) = p3_outcome_bed(row).await;
        let mut fuse = LegacyAuthFuse::default();
        let posts = bed.fake.posts().len();
        // Rows 4 and 6 restart as the unit's own stopped state; row 8 as the sign-in
        // stop. Both are open to the refusal overlay (the unresolved attempt is
        // there, or the stop is a sign-in).
        manager.apply_collector_start(&P3_REFUSED_VIEW);
        assert_eq!(
            bed.rig.state().0,
            "api_refused",
            "row {row}: the overlay is applied"
        );
        let error = bed
            .list(&mut manager, &mut fuse)
            .await
            .expect_err("blocked");
        assert_eq!(
            legacy_error(error),
            LegacyAuthError::OwnerApiRefused {
                reason: RefusalReason::OwnerApi
            },
            "row {row}"
        );
        assert_eq!(
            bed.fake.posts().len(),
            posts,
            "row {row}: no POST under the overlay"
        );
        assert!(
            bed.fake.arrivals("products").is_empty(),
            "row {row}: no request"
        );
        // The refusal lifts (the owner's retry, as the start path's reset would). The
        // former sign-in is not rebuilt.
        manager.owner_retry().expect("the refusal lifts");
        let after_lift = bed.rig.state();
        if row == 8 {
            // The fence of the dead input stays: a due refresh meets it before any
            // transport and derives the unknown-outcome sign-out, never a replay.
            let error = bed
                .list(&mut manager, &mut fuse)
                .await
                .expect_err("the due refresh meets its fence");
            assert_eq!(
                legacy_error(error),
                LegacyAuthError::SignedOut {
                    reason: SignOutReason::RefreshOutcomeUnknown
                },
                "row 8 (state after the lift {after_lift:?})"
            );
        } else {
            // Rows 4 and 6 rederive the unknown-outcome sign-out at the lift.
            assert_eq!(
                after_lift,
                ("signed_out".into(), Some("refresh_outcome_unknown".into())),
                "row {row}"
            );
            let error = bed
                .list(&mut manager, &mut fuse)
                .await
                .expect_err("signed out");
            assert_eq!(
                legacy_error(error),
                LegacyAuthError::SignedOut {
                    reason: SignOutReason::RefreshOutcomeUnknown
                },
                "row {row}"
            );
        }
        assert_eq!(
            bed.rig.state(),
            ("signed_out".into(), Some("refresh_outcome_unknown".into())),
            "row {row}"
        );
        assert_eq!(
            bed.fake.posts().len(),
            posts,
            "row {row}: the consumed input is never replayed"
        );
        assert!(bed.fake.arrivals("products").is_empty(), "row {row}");
    }
}

#[tokio::test]
async fn cs_p3_row8_overlay_lift_allows_usable_traffic_until_refresh_due() {
    // Row 8 reached by a forced refresh BEFORE the schedule makes one due: the
    // refresh token is dead and its input fenced, but the access token is usable
    // and the next refresh is hours away. After the C3 refusal overlays the sign-in
    // and lifts, usable traffic resumes with the current access token and no token
    // request; the moment the refresh falls due it meets the fence before any
    // transport and derives the unknown-outcome sign-out (R43 to R45, R57 step 6).
    let bed = new_bed().await;
    seed_quiet(&bed);
    let due = T0 + 21_600;
    bed.fake
        .script_tokens(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
    let mut manager = bed.manager();
    let _ = manager
        .refresh_now(&bed.rig.client(), SystemTime::now())
        .await;
    assert_eq!(bed.rig.state().0, "sign_in_required");
    assert!(!bed.rig.store.has_unresolved_legacy_refresh().unwrap());
    let fences = bed.rig.store.legacy_fenced_inputs_for_test().unwrap();
    assert_eq!(fences.len(), 1, "the dead input is fenced");
    assert_eq!(bed.fake.posts().len(), 1, "the one forced refresh");
    drop(manager);
    p3_insert_c3(&bed, "legacy", "owner_api_refused", "owner_api_refused");
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    manager.apply_collector_start(&P3_REFUSED_VIEW);
    assert_eq!(bed.rig.state().0, "api_refused", "the overlay is applied");
    bed.at(T0 + 10);
    assert!(bed.list(&mut manager, &mut fuse).await.is_err());
    assert!(
        bed.fake.arrivals("products").is_empty(),
        "no request under the overlay"
    );
    manager.owner_retry().expect("the refusal lifts");
    assert_ne!(bed.rig.state().0, "api_refused");
    // Usable traffic resumes while the refresh is not due: the current access
    // token, no token request, up to and including the last second before it.
    for (index, at) in [T0 + 20, due - 1].into_iter().enumerate() {
        bed.at(at);
        bed.list(&mut manager, &mut fuse)
            .await
            .unwrap_or_else(|error| panic!("usable traffic at {at}: {error:?}"));
        assert_eq!(bed.fake.arrivals("products").len(), index + 1, "at {at}");
        assert_eq!(bed.fake.posts().len(), 1, "no token request at {at}");
        assert_eq!(
            bed.fake.arrivals("products")[index].bearer.as_deref(),
            Some(jg().as_str()),
            "the current access token at {at}"
        );
    }
    // At the due second the refresh meets its fence before any transport.
    bed.at(due);
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("the due refresh meets its fence");
    assert_eq!(
        legacy_error(error),
        LegacyAuthError::SignedOut {
            reason: SignOutReason::RefreshOutcomeUnknown
        }
    );
    assert_eq!(
        bed.rig.state(),
        ("signed_out".into(), Some("refresh_outcome_unknown".into()))
    );
    assert_eq!(
        bed.fake.posts().len(),
        1,
        "the fenced input is not replayed"
    );
    assert_eq!(
        bed.fake.arrivals("products").len(),
        2,
        "no request after it"
    );
    assert_eq!(
        bed.rig.store.legacy_fenced_inputs_for_test().unwrap(),
        fences
    );
}

#[tokio::test]
async fn cs_p3_signed_out_guard_uses_unresolved_fact() {
    // The pure guard, over the three values of the fact and the sign-in control.
    // False: a sign-out stands, nothing is written.
    let (bed, manager) = p3_signed_out_bed().await;
    drop(manager);
    p3_insert_c3(&bed, "legacy", "owner_api_refused", "owner_api_refused");
    let mut unit = bed.manager();
    let row = bed.rig.row();
    unit.apply_collector_start(&P3_REFUSED_VIEW);
    assert_eq!(bed.rig.row(), row, "fact false: preserved");
    // Unreadable: the fact is no evidence, so the sign-out stands and nothing is
    // written. The receipt binding table is made unreadable for the read alone.
    bed.rig
        .store
        .open()
        .expect("catalogue")
        .execute_batch("ALTER TABLE legacy_refresh_receipt_bindings RENAME TO p3_hidden_bindings")
        .expect("hide the table");
    assert!(bed.rig.store.has_unresolved_legacy_refresh().is_err());
    unit.apply_collector_start(&P3_REFUSED_VIEW);
    bed.rig
        .store
        .open()
        .expect("catalogue")
        .execute_batch("ALTER TABLE p3_hidden_bindings RENAME TO legacy_refresh_receipt_bindings")
        .expect("restore the table");
    assert_eq!(
        bed.rig.row(),
        row,
        "fact unreadable: preserved, nothing written"
    );
    assert_eq!(unit.status_now().state, AuthState::SignedOut);
    // True: the overlay is allowed (a real unresolved attempt: row 4).
    let (bed, mut unit) = p3_outcome_bed(4).await;
    assert!(bed.rig.store.has_unresolved_legacy_refresh().unwrap());
    unit.apply_collector_start(&P3_REFUSED_VIEW);
    assert_eq!(bed.rig.state().0, "api_refused", "fact true: overlay");
    // The sign-in control: a sign-in stop with no unresolved attempt is overlaid.
    let (bed, mut unit) = p3_outcome_bed(8).await;
    assert!(!bed.rig.store.has_unresolved_legacy_refresh().unwrap());
    unit.apply_collector_start(&P3_REFUSED_VIEW);
    assert_eq!(bed.rig.state().0, "api_refused", "sign-in: overlay");
}

#[tokio::test]
async fn cs_p3_own_refusal_precedence_at_the_manager() {
    // The manager built over a store whose own refusal and a configuration error (an
    // opaque token with no region) both stand: the refusal is what the unit shows,
    // construction writes nothing over it, and the configuration it hid reappears
    // when the refusal lifts.
    let bed = new_bed().await;
    bed.rig.seed(
        "opaque-token-no-issuer-1",
        "refresh-0",
        Some((T0 + 28_800, T0 + 21_600)),
    );
    let own = crate::db::CollectorAuthRow {
        state: "api_refused".to_owned(),
        reason: Some("owner_api".to_owned()),
        collecting: false,
        retry_at: None,
        stopped_generation: None,
        stopped_mode: Some("legacy".to_owned()),
        fuse_consecutive: 0,
        fuse_generation: None,
        fuse_last_rejection_at: None,
        fuse_pause_until: None,
        fuse_rejected_access_digest: None,
        clock_skew: false,
    };
    bed.rig
        .store
        .write_collector_auth_row(&own, T0 * 1_000)
        .expect("own refusal");
    let mut manager = bed.manager();
    assert_eq!(
        bed.rig.row(),
        own,
        "construction wrote nothing over the refusal"
    );
    assert!(matches!(
        manager.stop_view(T0),
        Some(LegacyAuthError::OwnerApiRefused { .. })
    ));
    // The refusal lifts: the configuration the refusal hid reappears.
    manager.owner_retry().expect("lift");
    assert!(matches!(
        manager.stop_view(T0),
        Some(LegacyAuthError::RegionRequired)
    ));
    assert_eq!(bed.rig.state().0, "configuration_error");
    // A restart with the same facts publishes nothing over the unit's own state.
    drop(manager);
    let restarted = bed.manager();
    assert_eq!(restarted.status_now().state, AuthState::ConfigurationError);
}

#[tokio::test]
async fn cs_p3_own_refusal_precedence_with_unresolved_evidence_and_hidden_configuration() {
    // The three facts together: the unit's own refusal, an unresolved refresh
    // attempt and a configuration error (an opaque token with no region). The
    // refusal shows first; when it lifts the unresolved attempt is what stops the
    // account (R57 step 3 precedes step 4), and the configuration stays beneath it.
    // The live unit, the stored row and a rebuilt unit all say the same, and the
    // fenced input is never presented again.
    let bed = new_bed().await;
    bed.rig.seed(
        "opaque-token-no-issuer-1",
        "refresh-0",
        Some((T0 + 28_800, T0 + 21_600)),
    );
    // Construction binds the generation; the attempt is then started and never
    // closed, as a crash between the reservation and the answer leaves it.
    let generation = {
        let unit = bed.manager();
        unit.generation()
    };
    bed.rig
        .store
        .begin_legacy_refresh(generation)
        .expect("a started attempt");
    assert!(bed.rig.store.has_unresolved_legacy_refresh().unwrap());
    let own = crate::db::CollectorAuthRow {
        state: "api_refused".to_owned(),
        reason: Some("owner_api".to_owned()),
        collecting: false,
        retry_at: None,
        stopped_generation: None,
        stopped_mode: Some("legacy".to_owned()),
        fuse_consecutive: 0,
        fuse_generation: None,
        fuse_last_rejection_at: None,
        fuse_pause_until: None,
        fuse_rejected_access_digest: None,
        clock_skew: false,
    };
    bed.rig
        .store
        .write_collector_auth_row(&own, T0 * 1_000)
        .expect("own refusal");
    let fences = bed.rig.store.legacy_fenced_inputs_for_test().unwrap();
    let unknown = LegacyAuthError::SignedOut {
        reason: SignOutReason::RefreshOutcomeUnknown,
    };
    let mut manager = bed.manager();
    assert_eq!(bed.rig.row(), own, "construction wrote nothing over it");
    assert!(matches!(
        manager.stop_view(T0),
        Some(LegacyAuthError::OwnerApiRefused { .. })
    ));
    assert!(matches!(
        manager.access_token_for_sensitive_use(),
        Err(LegacyAuthManagerError::Auth(
            LegacyAuthError::OwnerApiRefused { .. }
        ))
    ));
    // The refusal lifts: the unknown-outcome sign-out stands ahead of the
    // configuration error, in the view, in every request error and in the row.
    manager.owner_retry().expect("lift");
    assert_eq!(manager.stop_view(T0), Some(unknown.clone()));
    assert_eq!(manager.status_now().state, AuthState::SignedOut);
    assert_eq!(
        bed.rig.state(),
        ("signed_out".into(), Some("refresh_outcome_unknown".into()))
    );
    assert!(matches!(
        manager.access_token_for_sensitive_use(),
        Err(LegacyAuthManagerError::Auth(ref error)) if *error == unknown
    ));
    assert!(matches!(
        bed.rig.refresh(&mut manager).await,
        Err(LegacyAuthManagerError::Auth(ref error)) if *error == unknown
    ));
    let live_row = bed.rig.row();
    assert!(live_row.stopped_generation.is_some());
    // A rebuilt unit restores the same state and writes the same row.
    drop(manager);
    let rebuilt = bed.manager();
    assert_eq!(rebuilt.stop_view(T0), Some(unknown.clone()));
    assert_eq!(bed.rig.row(), live_row);
    // The stop does not lift the configuration beneath it: the owner's retry cannot
    // replay the fenced input, and nothing was sent.
    assert_eq!(
        bed.rig.store.legacy_fenced_inputs_for_test().unwrap(),
        fences
    );
    assert!(bed.fake.posts().is_empty());
    // The control: with no unresolved attempt the configuration shows after the
    // lift (the test above).
}

#[tokio::test]
async fn cs_p3f2_a_restored_credential_stop_shows_ahead_of_a_configuration_error() {
    // R-P3F-3: one ranking, the restore's. A stored current-generation sign-out of
    // any reason (here a rejected refresh token, not an unresolved attempt) and a
    // configuration error (an opaque token with no region) both stand: the
    // sign-out is what the unit shows, so construction, a status publish and a
    // restart keep the row, with its stopped generation.
    let bed = new_bed().await;
    bed.rig.seed(
        "opaque-token-no-issuer-1",
        "refresh-0",
        Some((T0 + 28_800, T0 + 21_600)),
    );
    let generation = {
        let unit = bed.manager();
        unit.generation()
    };
    let stopped = crate::db::CollectorAuthRow {
        state: "signed_out".to_owned(),
        reason: Some("refresh_token_rejected".to_owned()),
        collecting: false,
        retry_at: None,
        stopped_generation: Some(generation),
        stopped_mode: None,
        fuse_consecutive: 0,
        fuse_generation: None,
        fuse_last_rejection_at: None,
        fuse_pause_until: None,
        fuse_rejected_access_digest: None,
        clock_skew: false,
    };
    bed.rig
        .store
        .write_collector_auth_row(&stopped, T0 * 1_000)
        .expect("stored sign-out");
    let rejected = LegacyAuthError::SignedOut {
        reason: SignOutReason::RefreshTokenRejected,
    };
    let mut manager = bed.manager();
    assert_eq!(bed.rig.row(), stopped, "construction kept the stored stop");
    assert_eq!(manager.status_now().state, AuthState::SignedOut);
    assert_eq!(manager.stop_view(T0), Some(rejected.clone()));
    // A status publish (servicing) writes the same row.
    manager.service();
    assert_eq!(bed.rig.row(), stopped, "a publish kept the stored stop");
    assert!(matches!(
        manager.access_token_for_sensitive_use(),
        Err(LegacyAuthManagerError::Auth(ref error)) if *error == rejected
    ));
    // A restart restores the same stop with its generation.
    drop(manager);
    let restarted = bed.manager();
    assert_eq!(restarted.stop_view(T0), Some(rejected));
    assert_eq!(bed.rig.row(), stopped, "a restart kept the stored stop");
}

#[tokio::test]
async fn cs_p3_external_write_with_a_configuration_error_lifts_the_unknown_outcome_stop() {
    // The unknown-outcome sign-out shows ahead of a configuration error, so the
    // external credential write that resolves the attempt (R44) must take the stop
    // with it when the new pair is itself unusable under the configuration: the
    // account then shows the configuration error, not the stop the write lifted.
    let bed = new_bed().await;
    seed_quiet(&bed);
    let generation = bed.manager().generation();
    bed.rig
        .store
        .begin_legacy_refresh(generation)
        .expect("a started attempt");
    let mut manager = bed.manager();
    assert!(matches!(
        manager.stop_view(T0),
        Some(LegacyAuthError::SignedOut {
            reason: SignOutReason::RefreshOutcomeUnknown
        })
    ));
    // The external write: a pair of its own generation, stored over the fenced one.
    let key = crate::teslamate_credentials::load_key(bed.rig.directory.path()).unwrap();
    let tokens = OwnerTokens::from_secret_parts(
        "opaque-token-no-issuer-2".to_owned(),
        "refresh-1".to_owned(),
    )
    .unwrap();
    let (cipher_access, cipher_refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens).unwrap();
    bed.rig
        .store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(
                cipher_access,
                cipher_refresh,
                T0 + 28_800,
                T0 + 21_600,
            )
            .unwrap()
            .with_credential_generation(
                crate::teslamate_token::legacy_refresh_credential_generation(&tokens),
            )
            .unwrap(),
        )
        .expect("an external write");
    assert!(!bed.rig.store.has_unresolved_legacy_refresh().unwrap());
    bed.at(T0 + 120);
    assert!(manager.recheck_external_write(), "the write is adopted");
    assert!(matches!(
        manager.stop_view(T0 + 120),
        Some(LegacyAuthError::RegionRequired)
    ));
    assert_eq!(bed.rig.state().0, "configuration_error");
    assert!(bed.fake.posts().is_empty());
}

#[path = "fix_tests.rs"]
mod fix_tests;

#[path = "r3_fix_tests.rs"]
mod r3_fix_tests;
