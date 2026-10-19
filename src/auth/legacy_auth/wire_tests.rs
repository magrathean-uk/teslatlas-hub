// SPDX-License-Identifier: AGPL-3.0-only

//! C5 part P1 spec tests for the token request and its answer on the wire
//! (spec tests 14, 15, 16, 51, 53, 69 and 92, with R33 to R36 and the status
//! rows of R45 that depend only on the answer). Every server is an in-process
//! loopback fake; nothing here uses the network.
//!
//! What the tests of spec 15 and 16 say about retry times, persistence and the
//! receipt belongs to the refresh attempt (part P3) and is asserted there; here
//! the class, error, state code and the resend of the same body are asserted.

use std::collections::VecDeque;

use axum::{body::Body, extract::Request, http::Version, response::Response};
use reqwest::{Certificate, header::HeaderMap};
use serde_json::{Value, json};

use super::*;
use crate::legacy_auth::wire::{
    AcceptedKind, AnswerBody, AttemptClass, Exchange, PreparedRequest, RawAnswer, StateCode,
    TokenErrorCode, classify_answer, classify_error_answer, exchange, parse_error_code,
};

const SUCCESS_BODY: &str = r#"{"access_token":"AT1","refresh_token":"refresh-1","id_token":"IDTOKEN-SECRET-9","expires_in":28800,"token_type":"Bearer"}"#;
const PRESENTED_ACCESS: &str = "AT0";
const PRESENTED_REFRESH: &str = "refresh-0";

fn client() -> Client {
    crate::crypto::install_default_provider();
    build_auth_client(true).expect("auth client")
}

/// One scripted answer of a fake token server.
#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    fn with_header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

/// What a fake server saw of one request.
#[derive(Clone, Debug)]
struct Seen {
    version: Version,
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.headers.iter().map(|(name, _)| name.clone()).collect();
        names.sort();
        names
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn json_body(&self) -> Value {
        serde_json::from_slice(&self.body).expect("a JSON request body")
    }
}

#[derive(Clone)]
struct Script {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    fallback: Reply,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Script {
    fn new(replies: Vec<Reply>) -> Self {
        let fallback = replies.last().cloned().expect("at least one reply");
        Self {
            replies: Arc::new(Mutex::new(replies.into())),
            fallback,
            seen: Arc::default(),
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn router(&self) -> Router {
        Router::new()
            .fallback(script_handler)
            .with_state(self.clone())
    }
}

async fn script_handler(State(script): State<Script>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    script.seen.lock().unwrap().push(Seen {
        version: parts.version,
        method: parts.method.to_string(),
        path: parts.uri.path().to_owned(),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or("[non-text]").to_owned(),
                )
            })
            .collect(),
        body: body.to_vec(),
    });
    let reply = script
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| script.fallback.clone());
    let mut response = Response::builder().status(reply.status);
    for (name, value) in reply.headers {
        response = response.header(name, value);
    }
    response.body(Body::from(reply.body)).unwrap()
}

/// A loopback token server and the endpoint URL to POST to.
async fn spawn(replies: Vec<Reply>) -> (Url, Script) {
    let script = Script::new(replies);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = script.router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (
        Url::parse(&format!("http://{address}/oauth2/v3/token")).unwrap(),
        script,
    )
}

fn complete(answer: Exchange) -> RawAnswer {
    match answer {
        Exchange::Answer(answer) => answer,
        Exchange::NotSent | Exchange::ConnectTimedOut => panic!("nothing was sent"),
        Exchange::Unknown => panic!("the outcome was unknown"),
    }
}

fn raw(status: u16, body: impl Into<Vec<u8>>) -> RawAnswer {
    RawAnswer {
        status: StatusCode::from_u16(status).unwrap(),
        headers: HeaderMap::new(),
        body: AnswerBody::Complete(Zeroizing::new(body.into())),
    }
}

fn classify(
    answer: &RawAnswer,
    pinned: Option<&str>,
) -> Result<AcceptedTokens, wire::FailedAnswer> {
    classify_answer(answer, PRESENTED_ACCESS, PRESENTED_REFRESH, pinned)
}

fn failure(answer: &RawAnswer) -> wire::FailedAnswer {
    match classify(answer, None) {
        Err(failure) => failure,
        Ok(_) => panic!("the answer was accepted"),
    }
}

fn success_with(patch: impl FnOnce(&mut Value)) -> String {
    let mut body: Value = serde_json::from_str(SUCCESS_BODY).unwrap();
    patch(&mut body);
    body.to_string()
}

fn version_is_semver_like(version: &str) -> bool {
    let (core, rest) = match version.find(['-', '+']) {
        Some(at) => (&version[..at], Some(&version[at + 1..])),
        None => (version, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        && rest.is_none_or(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'+')
        })
}

fn assert_policy_request(seen: &Seen) {
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.path, "/oauth2/v3/token");
    assert_eq!(seen.version, Version::HTTP_11);
    let mut names = seen.header_names();
    if let Some(at) = names.iter().position(|name| name == "connection") {
        names.remove(at);
    }
    assert_eq!(
        names,
        [
            "accept",
            "content-length",
            "content-type",
            "host",
            "user-agent"
        ]
    );
    assert_eq!(seen.header("accept"), Some("application/json"));
    assert_eq!(seen.header("content-type"), Some("application/json"));
    let agent = seen.header("user-agent").unwrap();
    let version = agent
        .strip_prefix("Teslatlas-Hub/")
        .unwrap_or_else(|| panic!("user agent {agent}"));
    assert!(version_is_semver_like(version), "user agent {agent}");
    assert_eq!(
        agent,
        format!("Teslatlas-Hub/{}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        seen.json_body(),
        json!({
            "grant_type": "refresh_token",
            "client_id": "ownerapi",
            "refresh_token": "refresh-0",
            "scope": "openid email offline_access",
        })
    );
}

#[tokio::test]
async fn t14_the_request_has_the_exact_body_and_the_three_policy_headers() {
    let (endpoint, script) = spawn(vec![Reply::new(200, SUCCESS_BODY)]).await;
    // Load the pair as production does, so the body carries the stored token.
    let auth = LegacyAuth::from_persisted_state(
        jg(),
        PRESENTED_REFRESH,
        1_800_028_800,
        1_800_021_600,
        &AuthEndpointConfig {
            mode: AuthMode::Tesla,
            region: None,
            token_url: None,
            allow_loopback_http: true,
        },
    )
    .unwrap()
    .with_wire_override_for_test(endpoint);
    // The asserted endpoint is the pre-mapping one; the request goes to the fake.
    assert_eq!(
        auth.token_endpoint().unwrap().as_str(),
        "https://auth.tesla.com/oauth2/v3/token"
    );
    let answer =
        complete(exchange(&client(), auth.request_url().unwrap(), auth.refresh_token()).await);
    assert_eq!(answer.status, StatusCode::OK);
    let seen = script.seen();
    assert_eq!(seen.len(), 1, "exactly one POST");
    assert_policy_request(&seen[0]);
    assert_eq!(REFRESH_REQUEST_HEADERS.len(), 3);
    assert_eq!(
        HUB_USER_AGENT,
        format!("Teslatlas-Hub/{}", env!("CARGO_PKG_VERSION"))
    );
}

#[tokio::test]
async fn t15_a_redirect_is_a_final_answer_and_is_never_followed() {
    let (target, target_script) = spawn(vec![Reply::new(200, SUCCESS_BODY)]).await;
    let (endpoint, script) = spawn(vec![
        Reply::new(307, "redirect").with_header("location", target.to_string()),
    ])
    .await;
    let client = client();
    let answer = complete(exchange(&client, endpoint.clone(), PRESENTED_REFRESH).await);
    assert_eq!(answer.status, StatusCode::TEMPORARY_REDIRECT);
    assert!(
        target_script.seen().is_empty(),
        "the redirect target saw nothing"
    );
    let failed = failure(&answer);
    assert_eq!(failed.class, AttemptClass::Misrouted);
    assert_eq!(failed.error, LegacyAuthError::HttpStatus(307));
    assert_eq!(
        failed.class.state_code(),
        StateCode {
            state: "retrying",
            reason: Some("token_endpoint_misrouted")
        }
    );
    assert!(!failed.class.consumes_input());
    // The token was not consumed: the next attempt sends the same body again.
    complete(exchange(&client, endpoint, PRESENTED_REFRESH).await);
    let seen = script.seen();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].body, seen[1].body);
    assert_eq!(seen[1].json_body()["refresh_token"], "refresh-0");
    assert!(target_script.seen().is_empty());
}

#[test]
fn t16_a_successor_from_another_issuer_is_unusable() {
    let body = success_with(|body| body["access_token"] = json!(jc()));
    let answer = raw(200, body);
    let global = "https://auth.tesla.com/oauth2/v3";
    let failed = match classify(&answer, Some(global)) {
        Err(failed) => failed,
        Ok(_) => panic!("accepted a successor from the China issuer"),
    };
    assert_eq!(failed.class, AttemptClass::ConsumedWithoutSuccessor);
    assert_eq!(failed.error, LegacyAuthError::InvalidResponse);
    assert_eq!(
        failed.class.state_code(),
        StateCode {
            state: "sign_in_required",
            reason: Some("refresh_response_unusable")
        }
    );
    assert!(failed.class.consumes_input());
    // The same answer is accepted at the China issuer, in provider mode, and
    // for an opaque successor in either issuer.
    assert!(classify(&answer, Some("https://auth.tesla.cn/oauth2/v3")).is_ok());
    assert!(classify(&answer, None).is_ok());
    assert!(classify(&raw(200, SUCCESS_BODY), Some(global)).is_ok());
    // A JWT without a string issuer shows no other issuer.
    let no_issuer = success_with(|body| body["access_token"] = json!(jwt(json!({"exp": 1}))));
    assert!(classify(&raw(200, no_issuer), Some(global)).is_ok());
    // A JWT from the pinned issuer passes.
    let same = success_with(|body| body["access_token"] = json!(jg()));
    assert!(classify(&raw(200, same), Some(global)).is_ok());
}

#[test]
fn t51_the_token_type_is_bearer_or_absent() {
    for token_type in [
        json!("Bearer"),
        json!("BEARER"),
        json!("bearer"),
        json!("bEaReR"),
    ] {
        let body = success_with(|body| body["token_type"] = token_type.clone());
        let accepted = classify(&raw(200, body), None).unwrap_or_else(|_| panic!("{token_type}"));
        assert_eq!(accepted.kind, AcceptedKind::Rotated);
        // What is stored is always "Bearer" (R47), whatever the case received.
        let auth = LegacyAuth::for_test(
            Url::parse("http://127.0.0.1:9/oauth2/v3").unwrap(),
            "AT0",
            PRESENTED_REFRESH,
        );
        assert_eq!(auth.token_type(), "Bearer");
    }
    let absent = success_with(|body| {
        body.as_object_mut().unwrap().remove("token_type");
    });
    assert!(classify(&raw(200, absent), None).is_ok());
    let null = success_with(|body| body["token_type"] = Value::Null);
    assert!(classify(&raw(200, null), None).is_ok());
    for token_type in [
        json!("mac"),
        json!("Basic"),
        json!(""),
        json!(7),
        json!(["Bearer"]),
    ] {
        let body = success_with(|body| body["token_type"] = token_type.clone());
        let failed = match classify(&raw(200, body), None) {
            Err(failed) => failed,
            Ok(_) => panic!("accepted token_type {token_type}"),
        };
        assert_eq!(failed.class, AttemptClass::ConsumedWithoutSuccessor);
        assert_eq!(
            failed.class.state_code(),
            StateCode {
                state: "sign_in_required",
                reason: Some("refresh_response_unusable")
            }
        );
    }
}

#[test]
fn t53_invalid_tokens_in_a_2xx_are_unusable() {
    let cases = [
        ("refresh_token", json!("a\nb")),
        ("refresh_token", json!("")),
        ("refresh_token", json!("tok\u{e9}n")),
        ("refresh_token", json!(5)),
        ("access_token", json!("a b")),
        ("access_token", json!("")),
        ("access_token", json!("a\nb")),
        ("access_token", json!("=")),
        ("access_token", json!(5)),
        ("access_token", json!(["AT1"])),
        ("access_token", Value::Null),
    ];
    for (member, value) in cases {
        let body = success_with(|body| body[member] = value.clone());
        let failed = match classify(&raw(200, body), None) {
            Err(failed) => failed,
            Ok(_) => panic!("accepted {member} {value}"),
        };
        assert_eq!(
            failed.class,
            AttemptClass::ConsumedWithoutSuccessor,
            "{member} {value}"
        );
        assert_eq!(failed.error, LegacyAuthError::InvalidResponse);
        assert_eq!(
            failed.class.state_code(),
            StateCode {
                state: "sign_in_required",
                reason: Some("refresh_response_unusable")
            }
        );
    }
    // A missing access token, a non-object body, an empty body and bad JSON.
    let missing = success_with(|body| {
        body.as_object_mut().unwrap().remove("access_token");
    });
    for body in [
        missing.as_str(),
        "[]",
        r#"["AT1","refresh-1"]"#,
        "",
        "not json",
        "null",
        "{",
    ] {
        let failed = failure(&raw(200, body));
        assert_eq!(
            failed.class,
            AttemptClass::ConsumedWithoutSuccessor,
            "{body:?}"
        );
        assert_eq!(failed.error, LegacyAuthError::InvalidResponse, "{body:?}");
    }
    // A token longer than the store keeps cannot be persisted before use.
    let long = "a".repeat(crate::teslamate_token::MAX_LEGACY_TOKEN_PLAINTEXT_BYTES + 1);
    let body = success_with(|body| body["access_token"] = json!(long));
    assert_eq!(
        failure(&raw(200, body)).error,
        LegacyAuthError::InvalidResponse
    );
    let at_limit = "a".repeat(crate::teslamate_token::MAX_LEGACY_TOKEN_PLAINTEXT_BYTES);
    let body = success_with(|body| body["access_token"] = json!(at_limit));
    assert!(classify(&raw(200, body), None).is_ok());
}

#[test]
fn r45_rows_2_3_and_14_are_told_apart_by_the_pair() {
    // Row 2: a different refresh token, whatever the access token.
    let rotated = classify(&raw(200, SUCCESS_BODY), None).unwrap();
    assert_eq!(rotated.kind, AcceptedKind::Rotated);
    assert_eq!(rotated.access_token.as_str(), "AT1");
    assert_eq!(rotated.refresh_token.as_str(), "refresh-1");
    assert_eq!(rotated.expires_in, Some(json!(28800)));
    // Row 3: no refresh token, or the same one, and a new access token: the
    // presented refresh token stays the current one.
    for patch in [
        success_with(|body| {
            body.as_object_mut().unwrap().remove("refresh_token");
        }),
        success_with(|body| body["refresh_token"] = json!("refresh-0")),
        success_with(|body| body["refresh_token"] = Value::Null),
    ] {
        let accepted = classify(&raw(200, patch), None).unwrap();
        assert_eq!(accepted.kind, AcceptedKind::NotRotated);
        assert_eq!(accepted.refresh_token.as_str(), "refresh-0");
        assert_eq!(accepted.access_token.as_str(), "AT1");
        assert_eq!(accepted.kind.class(), AttemptClass::NotRotated);
        assert!(!accepted.kind.class().consumes_input());
    }
    // Row 14: the pair is the one presented.
    let same = success_with(|body| {
        body["access_token"] = json!("AT0");
        body["refresh_token"] = json!("refresh-0");
    });
    let accepted = classify(&raw(200, same), None).unwrap();
    assert_eq!(accepted.kind, AcceptedKind::NotRenewed);
    assert_eq!(
        accepted.kind.class().state_code(),
        StateCode {
            state: "retrying",
            reason: Some("refresh_not_renewed")
        }
    );
    // The lifetime hint is kept as received for the schedule to read.
    let hint = classify(
        &raw(
            200,
            success_with(|body| body["expires_in"] = json!("28800")),
        ),
        None,
    )
    .unwrap();
    assert_eq!(hint.expires_in, Some(json!("28800")));
    let none = classify(
        &raw(
            200,
            success_with(|body| {
                body.as_object_mut().unwrap().remove("expires_in");
            }),
        ),
        None,
    )
    .unwrap();
    assert_eq!(none.expires_in, None);
}

#[test]
fn r35_only_the_error_member_is_read_and_only_when_a_recognised_string() {
    let code = |body: &str| {
        parse_error_code(&AnswerBody::Complete(Zeroizing::new(
            body.as_bytes().to_vec(),
        )))
    };
    assert_eq!(
        code(r#"{"error":"invalid_request"}"#),
        Some(TokenErrorCode::InvalidRequest)
    );
    assert_eq!(
        code(r#"{"error":"invalid_client"}"#),
        Some(TokenErrorCode::InvalidClient)
    );
    assert_eq!(
        code(r#"{"error":"invalid_grant"}"#),
        Some(TokenErrorCode::InvalidGrant)
    );
    assert_eq!(
        code(r#"{"error":"unauthorized_client"}"#),
        Some(TokenErrorCode::UnauthorizedClient)
    );
    assert_eq!(
        code(r#"{"error":"unsupported_grant_type"}"#),
        Some(TokenErrorCode::UnsupportedGrantType)
    );
    assert_eq!(
        code(r#"{"error":"invalid_scope"}"#),
        Some(TokenErrorCode::InvalidScope)
    );
    assert_eq!(
        code(r#"{"error":"login_required"}"#),
        Some(TokenErrorCode::LoginRequired)
    );
    for unrecognised in [
        r#"{"error":"Invalid_Grant"}"#,
        r#"{"error":"server_error"}"#,
        r#"{"error":5}"#,
        r#"{"error":["invalid_grant"]}"#,
        r#"{"error":null}"#,
        r#"{"error_description":"invalid_grant"}"#,
        r#"["invalid_grant"]"#,
        "invalid_grant",
        "",
        "{",
    ] {
        assert_eq!(code(unrecognised), None, "{unrecognised}");
    }
    // Other members are ignored: the error member decides.
    assert_eq!(
        code(
            r#"{"error_description":"invalid_client","error":"invalid_grant","extra":{"a":[1,2]}}"#
        ),
        Some(TokenErrorCode::InvalidGrant)
    );
    // An oversize body counts as no recognised code.
    assert_eq!(parse_error_code(&AnswerBody::TooLarge), None);
}

#[test]
fn r45_error_answers_map_to_rows_7_to_13() {
    use AttemptClass::{
        Blocked, ClientRefused, Dead, Misrouted, Refused, RequestRejected, Transient,
    };
    let fleet_named = |status| classify_error_answer(status, None, true);
    let cases: Vec<(u16, &str, AttemptClass, LegacyAuthError, StateCode)> = {
        let retrying = |reason| StateCode {
            state: "retrying",
            reason: Some(reason),
        };
        let sign_in = StateCode {
            state: "sign_in_required",
            reason: Some("refresh_token_rejected"),
        };
        let mut cases = vec![
            // Row 7.
            (
                400,
                r#"{"error":"invalid_client"}"#,
                ClientRefused,
                LegacyAuthError::ClientRefused,
                retrying("client_check"),
            ),
            (
                401,
                r#"{"error":"unauthorized_client"}"#,
                ClientRefused,
                LegacyAuthError::ClientRefused,
                retrying("client_check"),
            ),
            (
                400,
                r#"{"error":"unauthorized_client"}"#,
                ClientRefused,
                LegacyAuthError::ClientRefused,
                retrying("client_check"),
            ),
            // Row 8.
            (
                400,
                r#"{"error":"invalid_grant"}"#,
                Dead,
                LegacyAuthError::RefreshTokenRejected,
                sign_in,
            ),
            (
                401,
                r#"{"error":"login_required"}"#,
                Dead,
                LegacyAuthError::RefreshTokenRejected,
                sign_in,
            ),
            (
                400,
                r#"{"error":"login_required"}"#,
                Dead,
                LegacyAuthError::RefreshTokenRejected,
                sign_in,
            ),
            (
                401,
                r#"{"error":"invalid_grant"}"#,
                Dead,
                LegacyAuthError::RefreshTokenRejected,
                sign_in,
            ),
            // Row 9: 400 with the three request codes, any other or no code.
            (
                401,
                "",
                RequestRejected,
                LegacyAuthError::HttpStatus(401),
                retrying("refresh_request_rejected"),
            ),
            (
                401,
                "{}",
                RequestRejected,
                LegacyAuthError::HttpStatus(401),
                retrying("refresh_request_rejected"),
            ),
            (
                401,
                r#"{"error":"server_error"}"#,
                RequestRejected,
                LegacyAuthError::HttpStatus(401),
                retrying("refresh_request_rejected"),
            ),
            // Row 11: a 401 with a recognised code that no row lists.
            (
                401,
                r#"{"error":"invalid_request"}"#,
                Misrouted,
                LegacyAuthError::HttpStatus(401),
                retrying("token_endpoint_misrouted"),
            ),
            // Row 10 and 12.
            (
                403,
                r#"{"error":"forbidden","error_description":"Use the Fleet API"}"#,
                Refused,
                LegacyAuthError::OwnerApiRefused {
                    reason: RefusalReason::OwnerApi,
                },
                StateCode {
                    state: "api_refused",
                    reason: Some("owner_api"),
                },
            ),
            (
                403,
                "This endpoint is retired: see the fleet-api documentation",
                Refused,
                LegacyAuthError::OwnerApiRefused {
                    reason: RefusalReason::OwnerApi,
                },
                StateCode {
                    state: "api_refused",
                    reason: Some("owner_api"),
                },
            ),
            (
                403,
                r#"{"error":"fleet_api_required"}"#,
                Refused,
                LegacyAuthError::OwnerApiRefused {
                    reason: RefusalReason::OwnerApi,
                },
                StateCode {
                    state: "api_refused",
                    reason: Some("owner_api"),
                },
            ),
            (
                403,
                "Forbidden",
                Blocked,
                LegacyAuthError::HttpStatus(403),
                retrying("refresh_blocked"),
            ),
            (
                403,
                r#"{"error":"invalid_grant"}"#,
                Blocked,
                LegacyAuthError::HttpStatus(403),
                retrying("refresh_blocked"),
            ),
            // Row 13 and a 4xx or 3xx of row 11.
            (
                404,
                "",
                Misrouted,
                LegacyAuthError::HttpStatus(404),
                retrying("token_endpoint_misrouted"),
            ),
        ];
        for status in [400_u16, 402, 405, 410, 415, 418, 422] {
            let (class, state) = if status == 400 {
                (RequestRejected, retrying("refresh_request_rejected"))
            } else {
                (Misrouted, retrying("token_endpoint_misrouted"))
            };
            cases.push((
                status,
                "",
                class,
                LegacyAuthError::HttpStatus(status),
                state,
            ));
        }
        for status in [301_u16, 302, 307, 308] {
            cases.push((
                status,
                "",
                Misrouted,
                LegacyAuthError::HttpStatus(status),
                retrying("token_endpoint_misrouted"),
            ));
        }
        for status in [408_u16, 425, 429, 500, 502, 503, 504, 599] {
            cases.push((
                status,
                "",
                Transient,
                LegacyAuthError::HttpStatus(status),
                retrying("tesla_unavailable"),
            ));
        }
        cases
    };
    for code in [
        "invalid_request",
        "unsupported_grant_type",
        "invalid_scope",
        "weird",
    ] {
        let failed = failure(&raw(400, format!(r#"{{"error":"{code}"}}"#)));
        assert_eq!(failed.class, RequestRejected, "{code}");
        assert_eq!(failed.error, LegacyAuthError::HttpStatus(400), "{code}");
    }
    for (status, body, class, error, state) in cases {
        let failed = failure(&raw(status, body));
        assert_eq!(failed.class, class, "{status} {body}");
        assert_eq!(failed.error, error, "{status} {body}");
        assert_eq!(failed.class.state_code(), state, "{status} {body}");
    }
    // The Fleet-API test runs only for a 403: error_description is never read
    // for another status.
    let described = r#"{"error":"invalid_request","error_description":"Fleet API"}"#;
    assert_eq!(failure(&raw(400, described)).class, RequestRejected);
    assert_eq!(failure(&raw(500, described)).class, Transient);
    assert_eq!(fleet_named(403).class, Refused);
    assert_eq!(fleet_named(400).class, RequestRejected);
    // The consumed inputs of the table.
    for (class, consumed) in [
        (AttemptClass::NotSent, false),
        (AttemptClass::Rotated, true),
        (AttemptClass::NotRotated, false),
        (AttemptClass::ConsumedWithoutSuccessor, true),
        (AttemptClass::Unknown, true),
        (AttemptClass::ClientRefused, false),
        (AttemptClass::Dead, true),
        (AttemptClass::RequestRejected, false),
        (AttemptClass::Refused, false),
        (AttemptClass::Misrouted, false),
        (AttemptClass::Blocked, false),
        (AttemptClass::Transient, false),
        (AttemptClass::NotRenewed, false),
    ] {
        assert_eq!(class.consumes_input(), consumed, "{class:?}");
    }
}

#[tokio::test]
async fn r34_the_body_cap_is_exactly_65536_bytes() {
    assert_eq!(MAX_TOKEN_RESPONSE_BYTES, 65_536);
    let client = client();
    let pad = |total: usize| {
        let mut body = br#"{"error":"invalid_grant","pad":""#.to_vec();
        let tail = br#""}"#;
        body.resize(total - tail.len(), b'x');
        body.extend_from_slice(tail);
        assert_eq!(body.len(), total);
        body
    };
    // At the cap the body is read and its error code recognised.
    let (endpoint, _script) = spawn(vec![Reply::new(400, pad(MAX_TOKEN_RESPONSE_BYTES))]).await;
    let answer = complete(exchange(&client, endpoint, PRESENTED_REFRESH).await);
    assert!(matches!(answer.body, AnswerBody::Complete(ref bytes) if bytes.len() == 65_536));
    assert_eq!(failure(&answer).class, AttemptClass::Dead);
    // One byte more: an oversize error body is "no recognised code" (R35).
    let (endpoint, _script) = spawn(vec![Reply::new(400, pad(MAX_TOKEN_RESPONSE_BYTES + 1))]).await;
    let answer = complete(exchange(&client, endpoint, PRESENTED_REFRESH).await);
    assert!(matches!(answer.body, AnswerBody::TooLarge));
    assert_eq!(failure(&answer).class, AttemptClass::RequestRejected);
    // An oversize 2xx is row 4 with ResponseTooLarge.
    let (endpoint, _script) = spawn(vec![Reply::new(200, pad(MAX_TOKEN_RESPONSE_BYTES + 1))]).await;
    let answer = complete(exchange(&client, endpoint, PRESENTED_REFRESH).await);
    let failed = failure(&answer);
    assert_eq!(failed.class, AttemptClass::ConsumedWithoutSuccessor);
    assert_eq!(failed.error, LegacyAuthError::ResponseTooLarge);
}

#[tokio::test]
async fn r34_a_chunked_answer_over_the_cap_stops_before_the_end_of_the_stream() {
    let (issuer, _task) = raw_chunked_token_server(
        vec![vec![b'x'; MAX_TOKEN_RESPONSE_BYTES], vec![b'y']],
        false,
    )
    .await;
    let endpoint = issuer.join("token").unwrap();
    let answer = timeout(
        Duration::from_secs(1),
        exchange(&client(), endpoint, PRESENTED_REFRESH),
    )
    .await
    .expect("the cap must end the read before the chunked answer ends");
    let answer = complete(answer);
    assert!(matches!(answer.body, AnswerBody::TooLarge));
    assert_eq!(failure(&answer).error, LegacyAuthError::ResponseTooLarge);
}

#[tokio::test]
async fn a_split_chunked_answer_within_the_cap_is_read_whole() {
    let body = SUCCESS_BODY.to_owned();
    let split_at = body.len() / 2;
    let (issuer, _task) = raw_chunked_token_server(
        vec![
            body.as_bytes()[..split_at].to_vec(),
            body.as_bytes()[split_at..].to_vec(),
        ],
        true,
    )
    .await;
    let answer =
        complete(exchange(&client(), issuer.join("token").unwrap(), PRESENTED_REFRESH).await);
    // CL-16: one accumulator of the cap's size holds the answer whole, however
    // the chunks arrive.
    assert!(matches!(
        answer.body,
        AnswerBody::Complete(ref bytes)
            if bytes.capacity() == MAX_TOKEN_RESPONSE_BYTES && bytes.as_slice() == body.as_bytes()
    ));
    let accepted = classify(&answer, None).unwrap();
    assert_eq!(accepted.access_token.as_str(), "AT1");
}

#[test]
fn the_token_request_buffer_is_built_once_at_its_exact_length() {
    use crate::legacy_auth::wire::token_request_body;
    let plain = "r".repeat(700);
    let escaped = format!("a\"b\\c\n{}\u{e9}\u{1f600}", "q".repeat(40));
    for token in [plain.as_str(), escaped.as_str()] {
        let body = token_request_body(token).expect("a body");
        assert_eq!(body.capacity(), body.len(), "exact allocation");
        let json: Value = serde_json::from_slice(&body).expect("JSON");
        let members = json.as_object().expect("an object");
        assert_eq!(members.len(), 4);
        assert_eq!(members["grant_type"], "refresh_token");
        assert_eq!(members["client_id"], "ownerapi");
        assert_eq!(members["scope"], "openid email offline_access");
        assert_eq!(members["refresh_token"], token);
    }
    // The 700-byte token alone makes the body longer than 700 bytes.
    assert!(token_request_body(&plain).unwrap().len() > 700);
}

#[tokio::test]
async fn r33_a_refused_connection_sent_nothing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let endpoint = Url::parse(&format!("http://{address}/oauth2/v3/token")).unwrap();
    assert!(matches!(
        exchange(&client(), endpoint, PRESENTED_REFRESH).await,
        Exchange::NotSent
    ));
    // A scheme the client refuses is rejected before anything is sent.
    let https_only = build_auth_client(false).unwrap();
    let plain = Url::parse("http://127.0.0.1:9/oauth2/v3/token").unwrap();
    assert!(matches!(
        exchange(&https_only, plain, PRESENTED_REFRESH).await,
        Exchange::NotSent
    ));
}

#[tokio::test]
async fn r33_a_sent_request_without_a_complete_answer_is_unknown() {
    // Closed after the request was read, before any status.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = socket.read(&mut request).await;
        drop(socket);
    });
    let endpoint = Url::parse(&format!("http://{address}/oauth2/v3/token")).unwrap();
    assert!(matches!(
        exchange(&client(), endpoint, PRESENTED_REFRESH).await,
        Exchange::Unknown
    ));
    task.await.unwrap();

    // A status and part of a body, then a reset: the body is incomplete.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = socket.read(&mut request).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\n\r\n{\"access_token\"")
            .await
            .unwrap();
        drop(socket);
    });
    let endpoint = Url::parse(&format!("http://{address}/oauth2/v3/token")).unwrap();
    assert!(matches!(
        exchange(&client(), endpoint, PRESENTED_REFRESH).await,
        Exchange::Unknown
    ));
    task.await.unwrap();
}

/// R33: the total budget bounds a server that never answers, and the outcome
/// is then unknown. The mechanism is proved with a 300 ms budget on the same
/// builder the production client uses; the spec's values are asserted next to it.
#[tokio::test]
async fn r33_a_server_that_never_answers_ends_at_the_total_timeout_as_unknown() {
    assert_eq!(AUTH_CONNECT_TIMEOUT, Duration::from_secs(10));
    assert_eq!(AUTH_TOTAL_TIMEOUT, Duration::from_secs(60));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _ = socket.read(&mut request).await;
        std::future::pending::<()>().await;
    });
    crate::crypto::install_default_provider();
    let short = wire::auth_client_builder_with_timeouts(
        true,
        Duration::from_secs(5),
        Duration::from_millis(300),
    )
    .build()
    .unwrap();
    let endpoint = Url::parse(&format!("http://{address}/oauth2/v3/token")).unwrap();
    let started = std::time::Instant::now();
    let outcome = exchange(&short, endpoint, PRESENTED_REFRESH).await;
    assert!(matches!(outcome, Exchange::Unknown));
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert!(started.elapsed() < Duration::from_secs(5));
    task.abort();
}

/// C5T-04, R33: a connection-phase error is a request that sent nothing, and a
/// connect timeout is told from a refusal so the receipt can say which it was.
/// The connect timeout covers the TLS handshake, so a listener that accepts and
/// stays silent stalls it on any host, with no injection.
#[tokio::test]
async fn r33_a_tls_handshake_that_never_completes_is_a_connect_timeout_that_sent_nothing() {
    crate::crypto::install_default_provider();
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = silent.local_addr().unwrap();
    let holder = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = silent.accept().await {
            held.push(socket);
        }
    });
    assert_eq!(AUTH_CONNECT_TIMEOUT, Duration::from_secs(10));
    let short = wire::auth_client_builder_with_timeouts(
        false,
        Duration::from_millis(200),
        Duration::from_secs(5),
    )
    .build()
    .unwrap();
    let endpoint = Url::parse(&format!("https://{address}/oauth2/v3/token")).unwrap();
    let started = std::time::Instant::now();
    let outcome = exchange(&short, endpoint, PRESENTED_REFRESH).await;
    assert!(
        matches!(outcome, Exchange::ConnectTimedOut),
        "a connect timeout"
    );
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "not the total budget"
    );
    holder.abort();
}

#[tokio::test]
async fn r33_a_server_whose_certificate_is_not_trusted_is_a_request_that_sent_nothing() {
    crate::crypto::install_default_provider();
    let identity = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        identity.cert.pem().into_bytes(),
        identity.signing_key.serialize_pem().into_bytes(),
    )
    .await
    .unwrap();
    let script = Script::new(vec![Reply::new(200, SUCCESS_BODY)]);
    let router = script.router();
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener.into_std().unwrap(), tls)
            .unwrap()
            .serve(router.into_make_service())
            .await
            .unwrap();
    });
    // The production client with no root added for the self-signed server: the
    // handshake fails, which is a connection-phase error (R33), so the body was
    // never sent and a network blip cannot become row 6.
    let production = build_auth_client(false).unwrap();
    let endpoint = Url::parse(&format!("https://{address}/oauth2/v3/token")).unwrap();
    assert!(matches!(
        exchange(&production, endpoint, PRESENTED_REFRESH).await,
        Exchange::NotSent
    ));
    assert!(script.seen().is_empty(), "no request reached the server");
    server.abort();
}

#[tokio::test]
async fn t92_no_cookie_and_no_compression_header_is_ever_sent() {
    let (endpoint, script) = spawn(vec![
        Reply::new(200, SUCCESS_BODY).with_header("set-cookie", "sid=session-1; Path=/"),
        Reply::new(200, SUCCESS_BODY),
    ])
    .await;
    let client = client();
    complete(exchange(&client, endpoint.clone(), PRESENTED_REFRESH).await);
    complete(exchange(&client, endpoint, PRESENTED_REFRESH).await);
    let seen = script.seen();
    assert_eq!(seen.len(), 2);
    for request in &seen {
        assert!(request.header("cookie").is_none(), "a cookie was sent");
        assert!(
            request.header("accept-encoding").is_none(),
            "accept-encoding was sent"
        );
        assert!(request.header("authorization").is_none());
        assert_policy_request(request);
    }
}

#[tokio::test]
async fn t92_a_tls_server_that_offers_http2_is_still_spoken_to_in_http11() {
    crate::crypto::install_default_provider();
    let identity = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        identity.cert.pem().into_bytes(),
        identity.signing_key.serialize_pem().into_bytes(),
    )
    .await
    .unwrap();
    let script = Script::new(vec![Reply::new(200, SUCCESS_BODY)]);
    let router = script.router();
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener.into_std().unwrap(), tls)
            .unwrap()
            .serve(router.into_make_service())
            .await
            .unwrap();
    });
    // The production builder, plus a trust root for the self-signed server.
    let client = wire::auth_client_builder(false)
        .add_root_certificate(Certificate::from_pem(identity.cert.pem().as_bytes()).unwrap())
        .build()
        .unwrap();
    let endpoint = Url::parse(&format!("https://{address}/oauth2/v3/token")).unwrap();
    let answer = complete(exchange(&client, endpoint, PRESENTED_REFRESH).await);
    assert_eq!(answer.status, StatusCode::OK);
    let seen = script.seen();
    assert_eq!(seen.len(), 1);
    assert_policy_request(&seen[0]);
    server.abort();
}

#[tokio::test]
async fn t84_the_request_carries_the_trimmed_refresh_token() {
    let (endpoint, script) = spawn(vec![Reply::new(200, SUCCESS_BODY)]).await;
    let auth = LegacyAuth::from_persisted_state(
        format!("{} ", jg()),
        "refresh-0\n",
        1_800_028_800,
        1_800_021_600,
        &AuthEndpointConfig {
            mode: AuthMode::Tesla,
            region: None,
            token_url: None,
            allow_loopback_http: true,
        },
    )
    .unwrap()
    .with_wire_override_for_test(endpoint);
    complete(exchange(&client(), auth.request_url().unwrap(), auth.refresh_token()).await);
    assert_eq!(script.seen()[0].json_body()["refresh_token"], "refresh-0");
}

/// The prepared request is built before the last authority check and sent after
/// it (R17): building sends nothing.
#[tokio::test]
async fn building_the_request_sends_nothing() {
    let (endpoint, script) = spawn(vec![Reply::new(200, SUCCESS_BODY)]).await;
    let prepared = PreparedRequest::new(&client(), endpoint, PRESENTED_REFRESH).unwrap();
    assert!(script.seen().is_empty());
    complete(prepared.send().await);
    assert_eq!(script.seen().len(), 1);
}

#[tokio::test]
async fn t69_the_wire_path_logs_and_prints_no_secret() {
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let sink = Sink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let secrets = [
        "AT0",
        "AT1",
        "AT2",
        "refresh-0",
        "refresh-1",
        "refresh-2",
        "IDTOKEN-SECRET-9",
    ];
    let (endpoint, _script) = spawn(vec![
        Reply::new(200, SUCCESS_BODY),
        Reply::new(
            400,
            r#"{"error":"invalid_grant","error_description":"refresh-0 is dead"}"#,
        ),
        Reply::new(200, success_with(|body| body["token_type"] = json!("mac"))),
    ])
    .await;
    let client = client();
    let mut rendered = Vec::new();
    for _ in 0..3 {
        let answer = complete(exchange(&client, endpoint.clone(), PRESENTED_REFRESH).await);
        rendered.push(format!("{answer:?}"));
        match classify(&answer, None) {
            Ok(accepted) => {
                rendered.push(format!("{accepted:?}"));
                rendered.push(format!("{:?}", accepted.kind));
            }
            Err(failed) => {
                rendered.push(format!("{failed:?} {}", failed.error));
                rendered.push(format!("{:?}", failed.class.state_code()));
            }
        }
    }
    let logged = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    rendered.push(logged);
    for text in rendered {
        for secret in secrets {
            assert!(!text.contains(secret), "{secret} appeared in {text}");
        }
    }
}
