// SPDX-License-Identifier: AGPL-3.0-only

//! The token request and its answer on the wire (C5 R10 to R12, R33 to R36,
//! R45 rows 1, 4 and 6 to 14, R46, R47, R53).
//!
//! Nothing here keeps state. `PreparedRequest::send` performs one POST and
//! reports what the Hub can honestly say about it; `classify_answer` turns a
//! complete answer into the validated pair or the failure class of the outcome
//! table. Scheduling, fencing and persistence belong to the refresh attempt.

use std::{fmt, time::Duration};

use futures_util::StreamExt;
use reqwest::{
    Client, ClientBuilder, StatusCode,
    header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, USER_AGENT},
    redirect::Policy,
};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use url::Url;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[cfg(test)]
use super::vocabulary::{RetryReason, SignOutReason};
use super::{
    LegacyAuthError,
    endpoint::{is_access_token, is_refresh_token, parse_jwt},
};
use crate::teslamate_token::MAX_LEGACY_TOKEN_PLAINTEXT_BYTES;

/// The connect timeout of the auth client, TCP and TLS handshake (R33). F11's
/// initial retransmission timeout is 1 s and doubles, so three SYN attempts take
/// 7 s; 10 s allows three tries before the host counts as unreachable, which is
/// a cheap, retryable "nothing was sent".
pub(crate) const AUTH_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The total timeout of one token request including the body read (R33). A
/// timeout after the request was sent makes the rotation outcome unknown and
/// forces a new sign-in (R45 row 6), so the budget leans long: 60 s is about
/// thirty times a normal exchange on a slow link, and it still bounds how long
/// other callers wait behind the single flight (R15).
pub(crate) const AUTH_TOTAL_TIMEOUT: Duration = Duration::from_secs(60);

/// The most body bytes read from a token answer (R34). An answer carries three
/// tokens of a few kilobytes each (F4); 64 KiB is more than ten times that and
/// bounds memory. With decompression disabled (R11) the bound holds for the
/// bytes on the wire and in memory alike. The 65,537th byte stops the read.
pub(crate) const MAX_TOKEN_RESPONSE_BYTES: usize = 64 * 1024;

/// The longest token a successor answer may carry (R46 with R23). A pair is
/// persisted before it is used, and the encrypted token store refuses a token
/// longer than its own plaintext limit, so a longer successor could never be
/// stored and the unit would retry its persistence for the life of the process
/// (R24). It is refused as unusable instead, as row 4 of the outcome table.
const MAX_TOKEN_BYTES: usize = MAX_LEGACY_TOKEN_PLAINTEXT_BYTES;

/// The Owner API client id of the refresh grant (R10, F3).
const CLIENT_ID: &str = "ownerapi";

/// The scope of the refresh grant (R10, F3). It equals the Owner API login's
/// grant, so F6's rule that a refresh must not widen scope holds, and
/// `offline_access` keeps a refresh token in the answer (F9).
const REFRESH_SCOPE: &str = "openid email offline_access";

/// The Hub's own product token and version, in F10's `product/version` form
/// (R11). A product token cannot contain a space, hence the hyphen.
pub(crate) const HUB_USER_AGENT: &str = concat!("Teslatlas-Hub/", env!("CARGO_PKG_VERSION"));

/// The three headers the token request sets, and no others (R11): the Hub's
/// own `User-Agent`, `Content-Type` for the JSON body, and `Accept` stating the
/// true answer type (F6) in place of the client's default `*/*` (F16). The
/// request is a public-client grant, so no client authentication is sent.
pub(crate) const REFRESH_REQUEST_HEADERS: [(HeaderName, &str); 3] = [
    (USER_AGENT, HUB_USER_AGENT),
    (CONTENT_TYPE, "application/json"),
    (ACCEPT, "application/json"),
];

/// The auth client builder (R11, R12, R33), also used by tests that add a trust
/// root.
///
/// - redirects are never followed, because following one would resend the
///   refresh token to a host the pin or the owner did not choose (R12);
/// - the 10 s and 60 s timeouts of R33, separate from the data client;
/// - decompression off, so no `Accept-Encoding` is sent and the size cap counts
///   the same bytes on the wire and in memory (R11);
/// - no cookie store (none is ever enabled) and no idle connections, so every
///   request opens a fresh connection and a stale pooled connection cannot
///   fail after the body was sent and turn an ordinary request into row 6;
/// - HTTP/1.1 only, because one small POST gains nothing from multiplexing and
///   one protocol keeps the wire identical to what the tests capture.
///
/// `allow_loopback_http` is the runtime test flag of `OwnerApi::build`: without
/// it the client refuses any scheme but `https` (R7).
pub(crate) fn auth_client_builder(allow_loopback_http: bool) -> ClientBuilder {
    auth_client_builder_with_timeouts(
        allow_loopback_http,
        AUTH_CONNECT_TIMEOUT,
        AUTH_TOTAL_TIMEOUT,
    )
}

/// The builder of [`auth_client_builder`] with the two timeouts given, so a
/// test can prove the mechanism in milliseconds instead of waiting out R33.
pub(crate) fn auth_client_builder_with_timeouts(
    allow_loopback_http: bool,
    connect_timeout: Duration,
    total_timeout: Duration,
) -> ClientBuilder {
    Client::builder()
        .https_only(!allow_loopback_http)
        .redirect(Policy::none())
        .connect_timeout(connect_timeout)
        .timeout(total_timeout)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .pool_max_idle_per_host(0)
        .http1_only()
}

/// Build the auth client (R11).
pub(crate) fn build_auth_client(allow_loopback_http: bool) -> Result<Client, reqwest::Error> {
    auth_client_builder(allow_loopback_http).build()
}

/// The request body (R10): exactly four members and no others.
#[derive(Serialize)]
struct TokenRequest<'a> {
    grant_type: &'static str,
    client_id: &'static str,
    refresh_token: &'a str,
    scope: &'static str,
}

/// One token request, built and not yet sent. Building it never touches the
/// network, so the caller can run its last authority check between `new` and
/// `send` (R17).
pub(crate) struct PreparedRequest {
    request: reqwest::RequestBuilder,
}

/// A writer that only counts, to measure the request body before it is built.
struct CountingWriter(usize);

impl std::io::Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The request body of R10 (CL-16): the four-member JSON measured first, then
/// serialised straight into one zeroising buffer of exactly that length, so the
/// refresh token is never copied into a larger or intermediate allocation.
/// Accepted limit: the escaped-string scratch of the serialiser and the HTTP and
/// TLS layers' own buffers are not wiped from here.
pub(super) fn token_request_body(refresh_token: &str) -> Option<Zeroizing<Vec<u8>>> {
    let request = TokenRequest {
        grant_type: "refresh_token",
        client_id: CLIENT_ID,
        refresh_token,
        scope: REFRESH_SCOPE,
    };
    let mut counter = CountingWriter(0);
    serde_json::to_writer(&mut counter, &request).ok()?;
    let mut body = Zeroizing::new(Vec::with_capacity(counter.0));
    serde_json::to_writer(&mut *body, &request).ok()?;
    #[cfg(test)]
    probe::measured(&body);
    (body.len() == counter.0).then_some(body)
}

/// Test-only record of the request body's allocation at the two points the
/// production path passes through, so a test can prove through
/// `PreparedRequest::new` that one buffer is built and moved on whole.
#[cfg(test)]
pub(super) mod probe {
    use std::cell::Cell;

    /// Pointer, capacity and length of a body buffer.
    pub(in crate::auth::legacy_auth) type Allocation = (usize, usize, usize);

    thread_local! {
        static MEASURED: Cell<Option<Allocation>> = const { Cell::new(None) };
        static HANDED: Cell<Option<Allocation>> = const { Cell::new(None) };
    }

    fn of(body: &[u8], capacity: usize) -> Allocation {
        (body.as_ptr() as usize, capacity, body.len())
    }

    pub(super) fn measured(body: &super::Zeroizing<Vec<u8>>) {
        MEASURED.set(Some(of(body, body.capacity())));
    }

    pub(super) fn handed(body: &super::Zeroizing<Vec<u8>>) {
        HANDED.set(Some(of(body, body.capacity())));
    }

    /// Forget both records; the next read shows only what happens after.
    pub(in crate::auth::legacy_auth) fn clear() {
        MEASURED.set(None);
        HANDED.set(None);
    }

    /// The buffer `token_request_body` returned, and the buffer `from_body`
    /// received just before it called `client.post`.
    pub(in crate::auth::legacy_auth) fn read() -> (Option<Allocation>, Option<Allocation>) {
        (MEASURED.get(), HANDED.get())
    }
}

impl PreparedRequest {
    /// Build the POST of R10 and R11 to `endpoint` for `refresh_token`.
    pub(crate) fn new(client: &Client, endpoint: Url, refresh_token: &str) -> Option<Self> {
        Some(Self::from_body(
            client,
            endpoint,
            token_request_body(refresh_token)?,
        ))
    }

    /// The POST with `body` moved into the client as it is, with no copy: the
    /// allocation passes once to the request, which owns it from then on.
    pub(super) fn from_body(client: &Client, endpoint: Url, mut body: Zeroizing<Vec<u8>>) -> Self {
        #[cfg(test)]
        probe::handed(&body);
        let mut request = client.post(endpoint);
        for (name, value) in REFRESH_REQUEST_HEADERS {
            request = request.header(name, value);
        }
        Self {
            request: request.body(std::mem::take(&mut *body)),
        }
    }

    /// Send the request and read the whole answer within R34.
    pub(crate) async fn send(self) -> Exchange {
        #[cfg(test)]
        crate::api::test_egress_guard::refuse_production_request(&self.request);
        let response = match self.request.send().await {
            Ok(response) => response,
            Err(error) => return Exchange::from_send_error(&error),
        };
        let status = response.status();
        let headers = response.headers().clone();
        if response
            .content_length()
            .is_some_and(|length| length > MAX_TOKEN_RESPONSE_BYTES as u64)
        {
            return Exchange::Answer(RawAnswer {
                status,
                headers,
                body: AnswerBody::TooLarge,
            });
        }
        // One accumulator of the cap's size, allocated after the declared
        // length passed and checked before every append, so the answer is never
        // moved into a larger buffer that would leave an unwiped copy (CL-16).
        let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_TOKEN_RESPONSE_BYTES));
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                // A reset, timeout or read error before a complete body: the
                // request was sent, so the outcome is unknown (R45 row 6).
                return Exchange::Unknown;
            };
            if chunk.len() > MAX_TOKEN_RESPONSE_BYTES - bytes.len() {
                return Exchange::Answer(RawAnswer {
                    status,
                    headers,
                    body: AnswerBody::TooLarge,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        Exchange::Answer(RawAnswer {
            status,
            headers,
            body: AnswerBody::Complete(bytes),
        })
    }
}

/// Build and send one token request (R10, R11). The attempt builds its request
/// itself (it needs the prepared form for the guard and the send time); this is
/// the one-call form the wire tests use.
#[cfg(test)]
pub(crate) async fn exchange(client: &Client, endpoint: Url, refresh_token: &str) -> Exchange {
    match PreparedRequest::new(client, endpoint, refresh_token) {
        Some(prepared) => prepared.send().await,
        None => Exchange::NotSent,
    }
}

/// What the Hub knows after one token request.
pub(crate) enum Exchange {
    /// Row 1: nothing reached the server (DNS, TCP or TLS handshake failed, or
    /// the connect timeout passed), so the refresh token is untouched.
    NotSent,
    /// Row 1, the connect timeout: the handshake never completed within the
    /// connect budget (R33), so nothing was sent either. It is a separate case
    /// only because the receipt records `timeout` where a refused or failed
    /// connection records `transport_error` (R45 row 1).
    ConnectTimedOut,
    /// Row 6: the request was handed to the client and no complete status and
    /// body came back, so whether the server rotated the token is unknown.
    Unknown,
    /// A complete status and body, the body within R34 or marked too large.
    Answer(RawAnswer),
}

impl Exchange {
    /// R33: an error the client reports as connection-phase (reqwest
    /// `is_connect`, which includes the connect timeout and a TLS handshake
    /// failure) proves the body was never sent, because no connection is
    /// pooled (R11). A builder error rejected the request before any send.
    /// Every other error after the request was handed over is row 6.
    fn from_send_error(error: &reqwest::Error) -> Self {
        if error.is_connect() && error.is_timeout() {
            Self::ConnectTimedOut
        } else if error.is_connect() || error.is_builder() {
            Self::NotSent
        } else {
            Self::Unknown
        }
    }
}

/// The body of an answer.
pub(crate) enum AnswerBody {
    /// All of the body, at most `MAX_TOKEN_RESPONSE_BYTES`.
    Complete(Zeroizing<Vec<u8>>),
    /// More than `MAX_TOKEN_RESPONSE_BYTES`; the read stopped at the cap.
    TooLarge,
}

/// A complete answer from the token endpoint. Headers stay available to the
/// caller for `Retry-After` (R31) and `Date` (R55). It has no `Debug` that
/// shows the body (R53).
pub(crate) struct RawAnswer {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: AnswerBody,
}

impl fmt::Debug for RawAnswer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawAnswer")
            .field("status", &self.status.as_u16())
            .field("body_too_large", &matches!(self.body, AnswerBody::TooLarge))
            .finish()
    }
}

/// The recognised `error` codes of a token answer (R35): F6's six and F9's
/// `login_required`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenErrorCode {
    InvalidRequest,
    InvalidClient,
    InvalidGrant,
    UnauthorizedClient,
    UnsupportedGrantType,
    InvalidScope,
    LoginRequired,
}

/// Read the `error` code of a non-2xx answer (R35): the body is parsed as a
/// JSON object and only its `error` member is read, and only when it is a
/// string. A non-JSON body, an oversize body or any other value is "no
/// recognised code". Nothing else of the body is read, kept or logged here.
pub(crate) fn parse_error_code(body: &AnswerBody) -> Option<TokenErrorCode> {
    let AnswerBody::Complete(bytes) = body else {
        return None;
    };
    if !is_json_object_text(bytes) {
        return None;
    }
    #[derive(Deserialize)]
    struct ErrorBody {
        error: Option<String>,
    }
    let parsed: ErrorBody = serde_json::from_slice(bytes).ok()?;
    match parsed.error?.as_str() {
        "invalid_request" => Some(TokenErrorCode::InvalidRequest),
        "invalid_client" => Some(TokenErrorCode::InvalidClient),
        "invalid_grant" => Some(TokenErrorCode::InvalidGrant),
        "unauthorized_client" => Some(TokenErrorCode::UnauthorizedClient),
        "unsupported_grant_type" => Some(TokenErrorCode::UnsupportedGrantType),
        "invalid_scope" => Some(TokenErrorCode::InvalidScope),
        "login_required" => Some(TokenErrorCode::LoginRequired),
        _ => None,
    }
}

/// Whether the first byte of a body that is not JSON whitespace opens an
/// object. A derived struct reader also accepts a JSON array, read by position,
/// which is not a token answer.
fn is_json_object_text(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .find(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        == Some(&b'{')
}

/// The class of a token attempt, one per row of the outcome table (R45). The
/// class fixes what happens to the refresh token and which state the account
/// is in; the schedule that follows a retryable class is the refresh attempt's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptClass {
    /// Row 1: nothing was sent. Not a failed answer: the attempt reports it by
    /// its own kind (`ExchangeKind::NotSent`), so only the tests of the table
    /// construct this class.
    #[cfg_attr(not(test), allow(dead_code))]
    NotSent,
    /// Row 2: a new refresh token was issued. Reported by `ExchangeKind::Persisted`
    /// and `Pending`; constructed by the tests of the table only.
    #[cfg_attr(not(test), allow(dead_code))]
    Rotated,
    /// Row 3: no new refresh token; the presented one stays valid (F6). Reported
    /// the same way; constructed by the tests of the table only.
    #[cfg_attr(not(test), allow(dead_code))]
    NotRotated,
    /// Row 4: a grant was issued that the Hub cannot use.
    ConsumedWithoutSuccessor,
    /// Row 6: the outcome of a sent request is unknown.
    Unknown,
    /// Row 7: `invalid_client` or `unauthorized_client`.
    ClientRefused,
    /// Row 8: `invalid_grant` or `login_required`.
    Dead,
    /// Row 9: the request shape was rejected.
    RequestRejected,
    /// Row 10: a 403 that names the Fleet API.
    Refused,
    /// Row 11: a redirect, 404, 405, 410, 415 or another unlisted 4xx.
    Misrouted,
    /// Row 12: a 403 that does not name the Fleet API.
    Blocked,
    /// Row 13: 408, 425, 429 or a 5xx.
    Transient,
    /// Row 14: the endpoint returned the pair it was given.
    NotRenewed,
}

/// A state code with its optional reason code (R52): the last column of the
/// outcome table, which the wire tests pin row by row. The attempt itself sets
/// the state through the authority, so this table has no caller outside them.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StateCode {
    pub state: &'static str,
    pub reason: Option<&'static str>,
}

#[cfg(test)]
impl StateCode {
    const fn plain(state: &'static str) -> Self {
        Self {
            state,
            reason: None,
        }
    }

    const fn retrying(reason: RetryReason) -> Self {
        Self {
            state: "retrying",
            reason: Some(reason.code()),
        }
    }

    const fn sign_in_required(reason: SignOutReason) -> Self {
        Self {
            state: "sign_in_required",
            reason: Some(reason.code()),
        }
    }
}

impl AttemptClass {
    /// The state this class puts the account in, the last column of R45. Row 7
    /// is shown for its first answer; the second consecutive one is
    /// `api_refused(client)`, decided by the attempt that counts them.
    #[cfg(test)]
    pub(crate) const fn state_code(self) -> StateCode {
        match self {
            Self::NotSent => StateCode::retrying(RetryReason::Network),
            Self::Rotated | Self::NotRotated => StateCode::plain("ready"),
            Self::ConsumedWithoutSuccessor => {
                StateCode::sign_in_required(SignOutReason::RefreshResponseUnusable)
            }
            Self::Unknown => StateCode::sign_in_required(SignOutReason::RefreshOutcomeUnknown),
            Self::ClientRefused => StateCode::retrying(RetryReason::ClientCheck),
            Self::Dead => StateCode::sign_in_required(SignOutReason::RefreshTokenRejected),
            Self::RequestRejected => StateCode::retrying(RetryReason::RefreshRequestRejected),
            Self::Refused => StateCode {
                state: "api_refused",
                reason: Some("owner_api"),
            },
            Self::Misrouted => StateCode::retrying(RetryReason::TokenEndpointMisrouted),
            Self::Blocked => StateCode::retrying(RetryReason::RefreshBlocked),
            Self::Transient => StateCode::retrying(RetryReason::TeslaUnavailable),
            Self::NotRenewed => StateCode::retrying(RetryReason::RefreshNotRenewed),
        }
    }

    /// Whether the presented refresh token is spent for good after this class:
    /// the answer issued a new one, was unusable after a 2xx, is unknown, or
    /// said the token is dead (the "Input" column of R45).
    #[cfg(test)]
    pub(crate) const fn consumes_input(self) -> bool {
        matches!(
            self,
            Self::Rotated | Self::ConsumedWithoutSuccessor | Self::Unknown | Self::Dead
        )
    }
}

/// A failed attempt: its class and the error the caller sees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FailedAnswer {
    pub class: AttemptClass,
    pub error: LegacyAuthError,
}

/// The kind of an accepted 2xx answer (R45 rows 2, 3 and 14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AcceptedKind {
    /// Row 2: the refresh token differs from the presented one.
    Rotated,
    /// Row 3: no new refresh token (or the same one) and a new access token.
    NotRotated,
    /// Row 14: the pair equals the presented pair.
    NotRenewed,
}

impl AcceptedKind {
    /// The attempt class of this kind (rows 2, 3 and 14), which the wire tests
    /// pin against the outcome table.
    #[cfg(test)]
    pub(crate) const fn class(self) -> AttemptClass {
        match self {
            Self::Rotated => AttemptClass::Rotated,
            Self::NotRotated => AttemptClass::NotRotated,
            Self::NotRenewed => AttemptClass::NotRenewed,
        }
    }
}

/// A 2xx answer that passed R46, R47 and R9. The `id_token` was parsed only to
/// be dropped. The lifetime hint is kept raw: choosing the lifetime (R20) is
/// the schedule's decision.
#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct AcceptedTokens {
    pub access_token: Zeroizing<String>,
    /// The new refresh token, or the presented one when the answer carried
    /// none or the same one.
    pub refresh_token: Zeroizing<String>,
    #[zeroize(skip)]
    pub kind: AcceptedKind,
    /// The `expires_in` member as received, if any; not a secret.
    #[zeroize(skip)]
    pub expires_in: Option<Value>,
}

impl fmt::Debug for AcceptedTokens {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcceptedTokens")
            .field("kind", &self.kind)
            .finish()
    }
}

/// A string member that is zeroised on drop, or any other JSON value. JSON
/// `null` is read as an absent member (through `Option`), because it carries
/// no value to use or to refuse.
enum Member {
    Text(Zeroizing<String>),
    Other,
}

impl<'de> Deserialize<'de> for Member {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MemberVisitor;
        impl<'de> Visitor<'de> for MemberVisitor {
            type Value = Member;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value")
            }

            fn visit_str<E>(self, value: &str) -> Result<Member, E> {
                Ok(Member::Text(Zeroizing::new(value.to_owned())))
            }

            fn visit_string<E>(self, value: String) -> Result<Member, E> {
                Ok(Member::Text(Zeroizing::new(value)))
            }

            fn visit_bool<E>(self, _: bool) -> Result<Member, E> {
                Ok(Member::Other)
            }

            fn visit_i64<E>(self, _: i64) -> Result<Member, E> {
                Ok(Member::Other)
            }

            fn visit_u64<E>(self, _: u64) -> Result<Member, E> {
                Ok(Member::Other)
            }

            fn visit_f64<E>(self, _: f64) -> Result<Member, E> {
                Ok(Member::Other)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Member, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Member::Other)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Member, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Member::Other)
            }
        }
        deserializer.deserialize_any(MemberVisitor)
    }
}

/// The members of a 2xx answer that this unit reads (F4).
#[derive(Deserialize)]
struct WireAnswer {
    access_token: Option<Member>,
    refresh_token: Option<Member>,
    /// Parsed only to be dropped and zeroised with the answer (R53).
    #[allow(dead_code)]
    id_token: Option<Member>,
    expires_in: Option<Value>,
    token_type: Option<Member>,
}

/// Turn a complete answer into the accepted pair or a failure class (R45 rows
/// 2 to 4 and 7 to 14).
///
/// `presented_access` and `presented_refresh` are the pair the request carried.
/// `pinned_issuer` is the issuer a JWT successor must carry (R9), `None` in
/// provider mode (R8).
pub(crate) fn classify_answer(
    answer: &RawAnswer,
    presented_access: &str,
    presented_refresh: &str,
    pinned_issuer: Option<&str>,
) -> Result<AcceptedTokens, FailedAnswer> {
    if answer.status.is_success() {
        return accept_tokens(
            &answer.body,
            presented_access,
            presented_refresh,
            pinned_issuer,
        );
    }
    let code = parse_error_code(&answer.body);
    let names_fleet_api = answer.status == StatusCode::FORBIDDEN
        && match &answer.body {
            AnswerBody::Complete(bytes) => crate::owner_api::is_fleet_api_named(bytes),
            AnswerBody::TooLarge => false,
        };
    Err(classify_error_answer(
        answer.status.as_u16(),
        code,
        names_fleet_api,
    ))
}

/// The class and error of a non-2xx answer (R45 rows 7 to 13).
///
/// A 401 with a recognised code other than the four of rows 7 and 8 matches no
/// listed row: row 9 lists "401 with no recognised code" only, so it is "another
/// 4xx not listed" and row 11. Rows 9 and 11 share one schedule, so the choice
/// changes only the reason shown to the owner.
pub(crate) fn classify_error_answer(
    status: u16,
    code: Option<TokenErrorCode>,
    names_fleet_api: bool,
) -> FailedAnswer {
    use TokenErrorCode::{InvalidClient, InvalidGrant, LoginRequired, UnauthorizedClient};
    let (class, error) = match (status, code) {
        (400 | 401, Some(InvalidClient | UnauthorizedClient)) => {
            (AttemptClass::ClientRefused, LegacyAuthError::ClientRefused)
        }
        (400 | 401, Some(InvalidGrant | LoginRequired)) => {
            (AttemptClass::Dead, LegacyAuthError::RefreshTokenRejected)
        }
        (400, _) | (401, None) => (
            AttemptClass::RequestRejected,
            LegacyAuthError::HttpStatus(status),
        ),
        (403, _) if names_fleet_api => (
            AttemptClass::Refused,
            LegacyAuthError::OwnerApiRefused {
                reason: super::vocabulary::RefusalReason::OwnerApi,
            },
        ),
        (403, _) => (AttemptClass::Blocked, LegacyAuthError::HttpStatus(status)),
        (408 | 425 | 429 | 500..=599, _) => {
            (AttemptClass::Transient, LegacyAuthError::HttpStatus(status))
        }
        _ => (AttemptClass::Misrouted, LegacyAuthError::HttpStatus(status)),
    };
    FailedAnswer { class, error }
}

/// Validate a 2xx answer (R46, R47, R9) and classify it as row 2, 3 or 14. Any
/// defect is row 4: a grant was issued, so the presented refresh token is
/// assumed spent, but the answer cannot be used.
fn accept_tokens(
    body: &AnswerBody,
    presented_access: &str,
    presented_refresh: &str,
    pinned_issuer: Option<&str>,
) -> Result<AcceptedTokens, FailedAnswer> {
    let unusable = |error| FailedAnswer {
        class: AttemptClass::ConsumedWithoutSuccessor,
        error,
    };
    let AnswerBody::Complete(bytes) = body else {
        return Err(unusable(LegacyAuthError::ResponseTooLarge));
    };
    let invalid = || unusable(LegacyAuthError::InvalidResponse);
    if !is_json_object_text(bytes) {
        return Err(invalid());
    }
    let wire: WireAnswer = serde_json::from_slice(bytes).map_err(|_| invalid())?;

    let access_token = match wire.access_token {
        Some(Member::Text(text)) if is_access_token(&text) && text.len() <= MAX_TOKEN_BYTES => text,
        _ => return Err(invalid()),
    };
    let new_refresh = match wire.refresh_token {
        None => None,
        Some(Member::Text(text)) if is_refresh_token(&text) && text.len() <= MAX_TOKEN_BYTES => {
            Some(text)
        }
        Some(_) => return Err(invalid()),
    };
    match wire.token_type {
        None => {}
        Some(Member::Text(text)) if text.eq_ignore_ascii_case("bearer") => {}
        Some(_) => return Err(invalid()),
    }
    if let Some(pinned) = pinned_issuer
        && let Some(issuer) = parse_jwt(&access_token).and_then(|claims| claims.iss)
        && issuer != pinned
    {
        // R9: a successor minted by another issuer could not be refreshed at
        // this endpoint. A JWT with no string `iss` shows no other issuer and
        // is accepted, as an absent `token_type` is (R47).
        return Err(invalid());
    }

    let rotated = new_refresh
        .as_deref()
        .is_some_and(|refresh| refresh.as_str() != presented_refresh);
    let kind = if rotated {
        AcceptedKind::Rotated
    } else if access_token.as_str() == presented_access {
        AcceptedKind::NotRenewed
    } else {
        AcceptedKind::NotRotated
    };
    let refresh_token = match new_refresh {
        Some(refresh) if rotated => refresh,
        _ => Zeroizing::new(presented_refresh.to_owned()),
    };
    Ok(AcceptedTokens {
        access_token,
        refresh_token,
        kind,
        expires_in: wire.expires_in,
    })
}

#[cfg(test)]
mod request_tests {
    use super::*;

    /// CL-16: through `PreparedRequest::new`, the one path production uses, the
    /// body is measured and filled once at its exact length, reaches
    /// `from_body` as that same allocation and passes to the request builder
    /// by `mem::take`, so the request's body is that allocation, not a copy.
    #[test]
    fn the_prepared_token_request_moves_the_measured_allocation_once() {
        crate::crypto::install_default_provider();
        let client = build_auth_client(true).expect("auth client");
        let plain = "r".repeat(700);
        let escaped = "refresh-\"quoted\"-\u{e9}-".to_owned() + &"r".repeat(700);
        for token in [plain.as_str(), escaped.as_str()] {
            let endpoint = Url::parse("http://127.0.0.1:9/oauth2/v3/token").expect("URL");
            probe::clear();
            let prepared = PreparedRequest::new(&client, endpoint, token).expect("prepared");
            let (measured, handed) = probe::read();
            let measured = measured.expect("the body was measured and filled");
            let handed = handed.expect("the body reached the client builder");
            assert_eq!(measured, handed, "the same allocation, never copied");
            let (address, capacity, length) = handed;
            assert_eq!(capacity, length, "exactly as long as the JSON");
            let request = prepared.request.build().expect("a built request");
            let moved = request
                .body()
                .and_then(reqwest::Body::as_bytes)
                .expect("an in-memory body");
            assert_eq!(moved.as_ptr() as usize, address, "no copy was made");
            assert_eq!(moved.len(), length);
            let json: Value = serde_json::from_slice(moved).expect("JSON");
            let members = json.as_object().expect("an object");
            assert_eq!(members.len(), 4);
            assert_eq!(members["refresh_token"], token);
            assert_eq!(members["grant_type"], "refresh_token");
        }
    }
}
