// SPDX-License-Identifier: AGPL-3.0-only

//! C5 part P1 spec tests for issuer, region and endpoint selection (spec
//! tests 1 to 13, 17, 74, 79 selection cases, 80, 83, 84) and the token grammar
//! and error vocabulary of R46 and R56. All inputs are synthetic and nothing
//! here uses the network.

use serde_json::json;

use super::*;
use crate::config::{CollectorConfig, HubConfig};

const REFRESH: &str = "refresh-0";
const GLOBAL_ENDPOINT: &str = "https://auth.tesla.com/oauth2/v3/token";
const CHINA_ENDPOINT: &str = "https://auth.tesla.cn/oauth2/v3/token";
const PROVIDER_TOKEN_URL: &str = "https://api.provider.example.test/oauth2/v3/token";

fn tesla(region: Option<StreamRegion>) -> AuthEndpointConfig {
    AuthEndpointConfig {
        mode: AuthMode::Tesla,
        region,
        token_url: None,
        allow_loopback_http: false,
    }
}

fn provider(token_url: Option<&str>, allow_loopback_http: bool) -> AuthEndpointConfig {
    AuthEndpointConfig {
        mode: AuthMode::Provider,
        region: None,
        token_url: token_url.map(str::to_owned),
        allow_loopback_http,
    }
}

/// The stored pair of the fixtures in refreshed form `(1800028800, 1800021600)`.
fn load(
    access: &str,
    refresh: &str,
    config: &AuthEndpointConfig,
) -> Result<LegacyAuth, LegacyAuthError> {
    LegacyAuth::from_persisted_state(access, refresh, 1_800_028_800, 1_800_021_600, config)
}

fn endpoint_text(auth: &LegacyAuth) -> &str {
    auth.token_endpoint().expect("an endpoint").as_str()
}

fn state_of(error: &LegacyAuthError) -> (&'static str, &'static str) {
    let state = error.load_failure_state().expect("a load failure state");
    (state.state, state.reason)
}

#[test]
fn t01_tesla_global_token_selects_the_global_endpoint() {
    let auth = load(&jg(), REFRESH, &tesla(None)).unwrap();
    assert_eq!(endpoint_text(&auth), GLOBAL_ENDPOINT);
    assert_eq!(auth.region(), StreamRegion::Global);
    assert_eq!(auth.endpoint_source(), EndpointSource::JwtIssuer);
}

#[test]
fn t02_tesla_china_token_selects_the_china_endpoint() {
    let auth = load(&jc(), REFRESH, &tesla(None)).unwrap();
    assert_eq!(endpoint_text(&auth), CHINA_ENDPOINT);
    assert_eq!(auth.region(), StreamRegion::China);
    assert_eq!(auth.endpoint_source(), EndpointSource::JwtIssuer);
}

#[test]
fn t03_a_global_token_with_a_china_region_is_a_conflict_before_any_network_use() {
    let error = load(&jg(), REFRESH, &tesla(Some(StreamRegion::China))).unwrap_err();
    assert_eq!(error, LegacyAuthError::RegionConflict);
    assert_eq!(state_of(&error), ("configuration_error", "region_conflict"));
    assert!(error.is_terminal_auth_failure());
    // The mirror conflict.
    assert_eq!(
        load(&jc(), REFRESH, &tesla(Some(StreamRegion::Global))).unwrap_err(),
        LegacyAuthError::RegionConflict
    );
}

#[test]
fn t04_a_china_token_with_a_china_region_selects_china() {
    let auth = load(&jc(), REFRESH, &tesla(Some(StreamRegion::China))).unwrap();
    assert_eq!(endpoint_text(&auth), CHINA_ENDPOINT);
    assert_eq!(auth.endpoint_source(), EndpointSource::JwtIssuer);
}

#[test]
fn t05_an_opaque_token_follows_the_configured_region() {
    let auth = load(OP, REFRESH, &tesla(Some(StreamRegion::China))).unwrap();
    assert_eq!(endpoint_text(&auth), CHINA_ENDPOINT);
    assert_eq!(auth.region(), StreamRegion::China);
    assert_eq!(auth.endpoint_source(), EndpointSource::ConfiguredRegion);
}

#[test]
fn t06_an_opaque_token_without_a_region_is_refused() {
    let error = load(OP, REFRESH, &tesla(None)).unwrap_err();
    assert_eq!(error, LegacyAuthError::RegionRequired);
    assert_eq!(state_of(&error), ("configuration_error", "region_required"));
}

#[test]
fn t07_an_unpinned_issuer_is_refused() {
    let error = load(&jx(), REFRESH, &tesla(None)).unwrap_err();
    assert_eq!(error, LegacyAuthError::InvalidIssuer);
    assert_eq!(
        state_of(&error),
        ("configuration_error", "issuer_not_allowed")
    );
    // A configured region does not make an unpinned issuer acceptable.
    assert_eq!(
        load(&jx(), REFRESH, &tesla(Some(StreamRegion::Global))).unwrap_err(),
        LegacyAuthError::InvalidIssuer
    );
}

#[test]
fn t08_issuer_comparison_is_exact() {
    for issuer in [
        "https://auth.tesla.com/oauth2/v3/",
        "HTTPS://AUTH.TESLA.COM/oauth2/v3",
        "https://auth.tesla.com/oauth2/v3 ",
        "https://auth.tesla.com/oauth2",
    ] {
        let token = jwt(json!({ "iss": issuer }));
        assert_eq!(
            load(&token, REFRESH, &tesla(None)).unwrap_err(),
            LegacyAuthError::InvalidIssuer,
            "accepted {issuer}"
        );
    }
}

#[test]
fn t09_a_padded_or_non_object_payload_is_opaque() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE};
    let header = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0";
    // Pad the payload until the padded base64url form carries `=`.
    let mut payload_text = String::from(r#"{"iss":"https://auth.tesla.com/oauth2/v3"}"#);
    let padded = loop {
        let candidate = URL_SAFE.encode(payload_text.as_bytes());
        if candidate.ends_with('=') {
            break candidate;
        }
        payload_text.insert(1, ' ');
    };
    let padded_token = format!("{header}.{padded}.c2ln");
    assert!(padded_token.contains('='));
    let array_token = jwt(json!([1]));
    for token in [padded_token, array_token] {
        assert_eq!(parse_jwt(&token), None, "recognised {token}");
        let auth = load(&token, REFRESH, &tesla(Some(StreamRegion::Global))).unwrap();
        assert_eq!(endpoint_text(&auth), GLOBAL_ENDPOINT);
        assert_eq!(auth.endpoint_source(), EndpointSource::ConfiguredRegion);
        assert_eq!(
            load(&token, REFRESH, &tesla(None)).unwrap_err(),
            LegacyAuthError::RegionRequired
        );
    }
}

#[test]
fn t09_jwt_shape_rules_of_r3() {
    let payload = "eyJpc3MiOiJhIn0";
    let header = "eyJhbGciOiJub25lIn0";
    for (token, recognised) in [
        (format!("{header}.{payload}.sig"), true),
        (format!("{header}.{payload}."), true),
        (format!("{header}.{payload}"), false),
        (format!("{header}.{payload}.sig.extra"), false),
        (format!(".{payload}.sig"), false),
        (format!("{header}..sig"), false),
        // The header must be a JSON object too.
        (format!("{}.{payload}.sig", "WzFd"), false),
        // Standard-alphabet characters are not base64url.
        (format!("{header}.ey+pc3MiOiJhIn0.sig"), false),
    ] {
        assert_eq!(parse_jwt(&token).is_some(), recognised, "token {token}");
    }
}

#[test]
fn t10_a_non_string_issuer_is_not_usable() {
    let token = jwt(json!({ "iss": 42 }));
    assert_eq!(parse_jwt(&token).unwrap().iss, None);
    assert_eq!(
        load(&token, REFRESH, &tesla(None)).unwrap_err(),
        LegacyAuthError::RegionRequired
    );
    let auth = load(&token, REFRESH, &tesla(Some(StreamRegion::China))).unwrap();
    assert_eq!(endpoint_text(&auth), CHINA_ENDPOINT);
    // A token with no `iss` at all behaves the same.
    let no_issuer = jwt(json!({ "exp": 1_800_028_800_i64 }));
    assert_eq!(
        load(&no_issuer, REFRESH, &tesla(None)).unwrap_err(),
        LegacyAuthError::RegionRequired
    );
}

#[test]
fn t11_provider_mode_uses_the_token_url_verbatim_without_an_issuer_pin() {
    let config = provider(Some(PROVIDER_TOKEN_URL), false);
    let auth = load(&jx(), REFRESH, &config).unwrap();
    assert_eq!(endpoint_text(&auth), PROVIDER_TOKEN_URL);
    assert_eq!(auth.endpoint_source(), EndpointSource::ProviderConfig);
    assert_eq!(auth.region(), StreamRegion::Global);

    // A configured region changes only the region, never the auth host.
    let china = AuthEndpointConfig {
        region: Some(StreamRegion::China),
        ..config.clone()
    };
    let auth = load(&jx(), REFRESH, &china).unwrap();
    assert_eq!(endpoint_text(&auth), PROVIDER_TOKEN_URL);
    assert_eq!(auth.region(), StreamRegion::China);

    // Provider mode takes a pinned Tesla token or an opaque one as well, and
    // never reads its issuer for the endpoint.
    for token in [jg(), jc(), OP.to_owned()] {
        let auth = load(&token, REFRESH, &config).unwrap();
        assert_eq!(endpoint_text(&auth), PROVIDER_TOKEN_URL);
    }
}

#[test]
fn t12_provider_token_urls_are_validated() {
    for invalid in [
        "http://api.provider.example.test/t",
        "https://u:p@api.provider.example.test/t",
        "https://u@api.provider.example.test/t",
        "https://api.provider.example.test/t?k=v",
        "https://api.provider.example.test/t?",
        "https://api.provider.example.test/t#f",
        "https://api.provider.example.test/t#",
        "notaurl",
        "",
        "ftp://api.provider.example.test/t",
        "mailto:someone@provider.example.test",
    ] {
        let error = load(&jx(), REFRESH, &provider(Some(invalid), false)).unwrap_err();
        assert_eq!(
            error,
            LegacyAuthError::TokenUrlInvalid,
            "accepted {invalid:?}"
        );
        assert_eq!(
            state_of(&error),
            ("configuration_error", "token_url_invalid")
        );
    }
    // Missing, with a non-empty refresh token.
    assert_eq!(
        load(&jx(), REFRESH, &provider(None, false)).unwrap_err(),
        LegacyAuthError::TokenUrlInvalid
    );
    // Loopback http: only with the runtime flag, only for an address literal.
    for loopback in [
        "http://127.0.0.1:9/t",
        "http://127.255.0.9:9/t",
        "http://[::1]:9/t",
    ] {
        assert_eq!(
            load(&jx(), REFRESH, &provider(Some(loopback), false)).unwrap_err(),
            LegacyAuthError::TokenUrlInvalid,
            "accepted {loopback} without the flag"
        );
        let auth = load(&jx(), REFRESH, &provider(Some(loopback), true)).unwrap();
        assert_eq!(auth.token_endpoint().unwrap().path(), "/t");
    }
    // The flag does not open any other http host, nor a host name.
    for other in [
        "http://api.provider.example.test/t",
        "http://localhost:9/t",
        "http://10.0.0.1:9/t",
    ] {
        assert_eq!(
            load(&jx(), REFRESH, &provider(Some(other), true)).unwrap_err(),
            LegacyAuthError::TokenUrlInvalid,
            "accepted {other} with the flag"
        );
    }
    // https stays valid with the flag.
    assert!(load(&jx(), REFRESH, &provider(Some(PROVIDER_TOKEN_URL), true)).is_ok());
}

#[test]
fn t13_tesla_mode_forbids_a_token_url() {
    let config = AuthEndpointConfig {
        token_url: Some(PROVIDER_TOKEN_URL.to_owned()),
        ..tesla(None)
    };
    let error = load(&jg(), REFRESH, &config).unwrap_err();
    assert_eq!(error, LegacyAuthError::TokenUrlNotAllowed);
    assert_eq!(
        state_of(&error),
        ("configuration_error", "token_url_not_allowed")
    );
    assert!(error.is_terminal_auth_failure());
}

#[test]
fn t17_the_region_decides_the_data_host() {
    let auth = load(&jc(), REFRESH, &tesla(None)).unwrap();
    assert_eq!(auth.region(), StreamRegion::China);
    let options = CollectorConfig::default()
        .owner_api_options_for_region(auth.region())
        .unwrap();
    assert_eq!(
        options.base_url.as_str(),
        "https://owner-api.vn.cloud.tesla.cn/"
    );
    let global = load(&jg(), REFRESH, &tesla(None)).unwrap();
    assert_eq!(
        CollectorConfig::default()
            .owner_api_options_for_region(global.region())
            .unwrap()
            .base_url
            .as_str(),
        "https://owner-api.teslamotors.com/"
    );
}

#[test]
fn t74_construction_takes_the_endpoint_configuration() {
    let config = provider(Some("https://api.provider.example.test/t"), false);
    let auth = LegacyAuth::from_persisted_state(jx(), REFRESH, 0, 0, &config).unwrap();
    assert_eq!(endpoint_text(&auth), "https://api.provider.example.test/t");
    let auth = LegacyAuth::from_access_token(jx(), REFRESH, &config).unwrap();
    assert_eq!(endpoint_text(&auth), "https://api.provider.example.test/t");

    for issuer in [
        "http://127.0.0.1:9/oauth2/v3",
        "http://127.0.0.1:9/oauth2/v3/",
    ] {
        let auth = LegacyAuth::for_test(Url::parse(issuer).unwrap(), "old-access", "old-refresh");
        assert_eq!(endpoint_text(&auth), "http://127.0.0.1:9/oauth2/v3/token");
        assert_eq!(auth.endpoint_source(), EndpointSource::TestOverride);
        assert_eq!(auth.region(), StreamRegion::Global);
    }
}

#[test]
fn t79_provider_selection_and_access_token_only() {
    // A provider with a token URL and a refresh token has an endpoint.
    let with_url = provider(Some(PROVIDER_TOKEN_URL), false);
    assert!(
        load(&jx(), REFRESH, &with_url)
            .unwrap()
            .token_endpoint()
            .is_some()
    );
    // Without one it is a configuration error.
    assert_eq!(
        load(&jx(), REFRESH, &provider(None, false)).unwrap_err(),
        LegacyAuthError::TokenUrlInvalid
    );
    // An empty refresh token is access-token-only: no token URL is needed and
    // no token request can be built, because there is no endpoint to send to.
    for config in [provider(None, false), with_url] {
        let auth = load(&jx(), "", &config).unwrap();
        assert!(auth.token_endpoint().is_none());
        assert!(auth.request_url().is_none());
        assert_eq!(auth.endpoint_source(), EndpointSource::ProviderConfig);
        assert!(!auth.startup_refresh_pending);
    }
    // Whitespace alone is empty after the trim of R46.
    assert!(
        load(&jx(), " \t\r\n", &provider(None, false))
            .unwrap()
            .token_endpoint()
            .is_none()
    );
    // Tesla mode with an empty refresh token is a stored-pair failure.
    let error = load(&jg(), "", &tesla(None)).unwrap_err();
    assert_eq!(error, LegacyAuthError::InvalidRefreshToken);
    assert_eq!(state_of(&error), ("signed_out", "stored_pair_invalid"));
}

#[test]
fn t80_a_configuration_without_a_region_leaves_the_region_unset() {
    let text = "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n";
    let (config, _) = HubConfig::from_exact_bytes(text.as_bytes()).unwrap();
    assert_eq!(config.collector.stream_region, None);
    let endpoint = AuthEndpointConfig::from_config(&config, false);
    assert_eq!(endpoint.region, None);
    assert_eq!(endpoint.mode, AuthMode::Tesla);
    assert_eq!(endpoint.token_url, None);
    assert!(!endpoint.allow_loopback_http);
    let auth = load(&jc(), REFRESH, &endpoint).unwrap();
    assert_eq!(endpoint_text(&auth), CHINA_ENDPOINT);
}

#[test]
fn the_endpoint_configuration_is_built_once_from_the_hub_configuration() {
    let parse = |extra: &str| {
        let text = format!("data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n{extra}");
        HubConfig::from_exact_bytes(text.as_bytes()).unwrap().0
    };
    // The default Owner API host is Tesla mode, a region key is the owner's.
    let config = parse("[collector]\nstream_region = 'china'\n");
    let endpoint = AuthEndpointConfig::from_config(&config, false);
    assert_eq!(endpoint.mode, AuthMode::Tesla);
    assert_eq!(endpoint.region, Some(StreamRegion::China));
    // A non-default Owner API origin is Provider mode and carries its token URL.
    let config = parse(
        "[collector]\nowner_api_base_url = 'https://api.provider.example.test/'\n\
         [collector.legacy_auth]\nenabled = true\ntoken_url = 'https://auth.provider.example.test/secret-route/token'\n",
    );
    let endpoint = AuthEndpointConfig::from_config(&config, true);
    assert_eq!(endpoint.mode, AuthMode::Provider);
    assert_eq!(
        endpoint.token_url.as_deref(),
        Some("https://auth.provider.example.test/secret-route/token")
    );
    assert!(endpoint.allow_loopback_http);
    // A token URL alone never makes a provider (C3 R1): Tesla mode forbids it.
    let config = parse(
        "[collector.legacy_auth]\nenabled = true\ntoken_url = 'https://auth.provider.example.test/t'\n",
    );
    let endpoint = AuthEndpointConfig::from_config(&config, false);
    assert_eq!(endpoint.mode, AuthMode::Tesla);
    assert_eq!(
        load(&jg(), REFRESH, &endpoint).unwrap_err(),
        LegacyAuthError::TokenUrlNotAllowed
    );
    // An existing file without the key keeps parsing.
    let config = parse("[collector.legacy_auth]\nenabled = true\n");
    assert_eq!(config.collector.legacy_auth.token_url, None);
}

#[test]
fn t83_an_out_of_range_exp_is_absent() {
    let issuer = "https://auth.tesla.com/oauth2/v3";
    for exp in [
        json!(1e300),
        json!(9_223_372_036_854_775_808_u64),
        json!(253_402_300_800_i64),
        json!(-1),
        json!("1800028800"),
        json!(null),
        json!([1]),
    ] {
        let token = jwt(json!({ "iss": issuer, "exp": exp }));
        let claims = parse_jwt(&token).expect("a JWT");
        assert_eq!(claims.exp, None, "exp {exp}");
        let auth = LegacyAuth::from_persisted_state(&token, REFRESH, 0, 0, &tesla(None)).unwrap();
        assert_eq!(auth.usable_until(), None, "exp {exp}");
    }
    // The bounds and the dropped fraction.
    for (exp, expected) in [
        (json!(0), Some(0)),
        (json!(253_402_300_799_i64), Some(253_402_300_799)),
        (json!(1_800_028_800.9), Some(1_800_028_800)),
        (json!(-0.5), Some(0)),
        (json!(1_800_028_800_u64), Some(1_800_028_800)),
    ] {
        let token = jwt(json!({ "iss": issuer, "exp": exp }));
        assert_eq!(parse_jwt(&token).unwrap().exp, expected, "exp {exp}");
    }
}

#[test]
fn r25_the_stored_schedule_and_the_token_exp_give_the_usable_time() {
    // Imported form: exp less the 300 s allowance, or unknown.
    let auth = LegacyAuth::from_persisted_state(jg(), REFRESH, 0, 0, &tesla(None)).unwrap();
    assert_eq!(auth.usable_until(), Some(1_800_028_500));
    assert!(auth.startup_refresh_pending);
    assert_eq!(auth.schedule_diagnostic(), None);
    let auth =
        LegacyAuth::from_persisted_state(OP, REFRESH, 0, 0, &tesla(Some(StreamRegion::Global)))
            .unwrap();
    assert_eq!(auth.usable_until(), None);
    // Refreshed form: the stored expiry, lowered to exp - 300 when that is earlier.
    let auth =
        LegacyAuth::from_persisted_state(jg(), REFRESH, 1_800_028_800, 1_800_021_600, &tesla(None))
            .unwrap();
    assert_eq!(
        (auth.expires_at(), auth.next_refresh_at()),
        (1_800_028_800, 1_800_021_600)
    );
    assert_eq!(auth.usable_until(), Some(1_800_028_500));
    assert!(!auth.startup_refresh_pending);
    let auth =
        LegacyAuth::from_persisted_state(jg(), REFRESH, 1_800_000_100, 1_800_000_050, &tesla(None))
            .unwrap();
    assert_eq!(auth.usable_until(), Some(1_800_000_100));
    // A schedule that fails the table's invariants loads as the imported form.
    let auth = LegacyAuth::from_persisted_state(jg(), REFRESH, 100, 200, &tesla(None)).unwrap();
    assert_eq!((auth.expires_at(), auth.next_refresh_at()), (0, 0));
    assert_eq!(
        auth.schedule_diagnostic(),
        Some(&LegacyAuthError::InvalidPersistedSchedule)
    );
    assert!(auth.startup_refresh_pending);
}

#[test]
fn t84_pasted_whitespace_is_trimmed_at_load() {
    let access = format!("{} ", jg());
    let auth = LegacyAuth::from_persisted_state(
        access,
        "refresh-0\n",
        1_800_028_800,
        1_800_021_600,
        &tesla(None),
    )
    .unwrap();
    assert_eq!(auth.access_token(), jg());
    assert_eq!(auth.refresh_token(), "refresh-0");
    for padded in ["\t\r\n refresh-0 \n", "refresh-0\r\n", " refresh-0"] {
        let auth = load(&jg(), padded, &tesla(None)).unwrap();
        assert_eq!(auth.refresh_token(), "refresh-0", "{padded:?}");
    }
    // Only SP, HTAB, CR and LF are trimmed: other whitespace is a defect.
    assert_eq!(
        load(&jg(), "refresh-0\u{000b}", &tesla(None)).unwrap_err(),
        LegacyAuthError::InvalidRefreshToken
    );
}

#[test]
fn r46_stored_tokens_must_meet_the_token_grammar() {
    let config = tesla(Some(StreamRegion::Global));
    for access in [
        "",
        " ",
        "a b",
        "a\u{0}b",
        "tok\u{e9}n",
        "=",
        "===",
        "a,b",
        "a\"b",
        "a\\b",
    ] {
        let error = load(access, REFRESH, &config).unwrap_err();
        assert_eq!(error, LegacyAuthError::InvalidAccessToken, "{access:?}");
        assert_eq!(state_of(&error), ("signed_out", "stored_pair_invalid"));
        assert!(error.is_terminal_auth_failure());
    }
    for refresh in ["", "a\nb", "a\u{0}b", "tok\u{e9}n", "a\u{7f}"] {
        assert_eq!(
            load(OP, refresh, &config).unwrap_err(),
            LegacyAuthError::InvalidRefreshToken,
            "{refresh:?}"
        );
    }
    // The b64token alphabet, trailing padding and VSCHAR (space included).
    // `=` is accepted anywhere after one other character (spec test 9: a JWT
    // with a padded payload loads as an opaque token).
    for access in ["abc", "a-b.c_d~e+f/g", "abc==", "A1", "a=b.c=.sig"] {
        assert!(load(access, REFRESH, &config).is_ok(), "{access:?}");
    }
    for refresh in ["a b", "!~", "x"] {
        assert!(load(OP, refresh, &config).is_ok(), "{refresh:?}");
    }
}

#[test]
fn r56_the_error_vocabulary_names_stops_and_waits() {
    use LegacyAuthError as E;
    let terminal = [
        E::RegionConflict,
        E::RegionRequired,
        E::InvalidIssuer,
        E::TokenUrlInvalid,
        E::TokenUrlNotAllowed,
        E::InvalidAccessToken,
        E::InvalidRefreshToken,
        E::AccessTokenRejected,
        E::ProviderTokenRejected,
        E::ProviderTokenExpired,
        E::OwnerApiRefused {
            reason: RefusalReason::OwnerApi,
        },
        E::OwnerApiRefused {
            reason: RefusalReason::Client,
        },
    ];
    let not_terminal = [
        E::InvalidResponse,
        E::ResponseTooLarge,
        E::Transport,
        E::RotationOutcomeUnknown,
        E::SensitiveRotationOutcomeUnknown,
        E::HttpStatus(503),
        E::RefreshDeferred,
        E::InvalidClock,
        E::InvalidPersistedSchedule,
        E::Persistence,
        E::SensitivePersistenceUnavailable,
        E::SensitiveAccessUnavailable,
        E::RefreshTokenRejected,
        E::ClientRefused,
        E::RefreshNotRenewed,
        E::RefreshNotPossible,
        E::VehicleAccessRejected,
        E::AuthUnavailable { retry_at: Some(7) },
        E::AuthUnavailable { retry_at: None },
        E::RetryNotPossible {
            reason: SignOutReason::AccessTokenRejected,
        },
    ];
    for error in &terminal {
        assert!(error.is_terminal_auth_failure(), "{error:?}");
    }
    for error in &not_terminal {
        assert!(!error.is_terminal_auth_failure(), "{error:?}");
    }
    // Fixed text: no variant's Display carries a value other than a status or
    // a reason code.
    assert_eq!(
        E::OwnerApiRefused {
            reason: RefusalReason::Client
        }
        .to_string(),
        "legacy owner api refused the account (client)"
    );
    assert_eq!(
        E::AuthUnavailable {
            retry_at: Some(1_800_000_000)
        }
        .to_string(),
        "legacy auth is unavailable"
    );
}

#[test]
fn r52_the_code_vocabularies_are_the_wire_codes() {
    assert_eq!(
        [
            SignOutReason::RefreshTokenRejected,
            SignOutReason::RefreshOutcomeUnknown,
            SignOutReason::RefreshResponseUnusable,
            SignOutReason::AccessTokenRejected,
            SignOutReason::StoredPairInvalid,
            SignOutReason::ProviderTokenRejected,
            SignOutReason::ProviderTokenExpired,
        ]
        .map(SignOutReason::code),
        [
            "refresh_token_rejected",
            "refresh_outcome_unknown",
            "refresh_response_unusable",
            "access_token_rejected",
            "stored_pair_invalid",
            "provider_token_rejected",
            "provider_token_expired",
        ]
    );
    assert_eq!(
        [RefusalReason::OwnerApi, RefusalReason::Client].map(RefusalReason::code),
        ["owner_api", "client"]
    );
    assert_eq!(
        [
            RetryReason::Network,
            RetryReason::SensitiveAccess,
            RetryReason::TeslaUnavailable,
            RetryReason::RefreshBlocked,
            RetryReason::RefreshRequestRejected,
            RetryReason::TokenEndpointMisrouted,
            RetryReason::ClientCheck,
            RetryReason::ShortTokenLifetime,
            RetryReason::ClockSkew,
            RetryReason::RefreshNotRenewed,
            RetryReason::StoreUnavailable,
        ]
        .map(RetryReason::code),
        [
            "network",
            "sensitive_access",
            "tesla_unavailable",
            "refresh_blocked",
            "refresh_request_rejected",
            "token_endpoint_misrouted",
            "client_check",
            "short_token_lifetime",
            "clock_skew",
            "refresh_not_renewed",
            "store_unavailable",
        ]
    );
    // Every code fits the column of the status row: lower-case ASCII words of
    // 1 to 64 bytes.
    for code in [
        ConfigurationReason::RegionConflict.code(),
        ConfigurationReason::RegionRequired.code(),
        ConfigurationReason::IssuerNotAllowed.code(),
        ConfigurationReason::TokenUrlInvalid.code(),
        ConfigurationReason::TokenUrlNotAllowed.code(),
    ] {
        assert!((1..=64).contains(&code.len()));
        assert!(code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
    }
}

#[test]
fn t69_selection_and_construction_expose_no_secret() {
    let token_url = "https://auth.provider.example.test/secret-route/token";
    let access = jg();
    let payload_segment = access.split('.').nth(1).unwrap().to_owned();
    let secrets = [
        access.as_str(),
        payload_segment.as_str(),
        "refresh-0",
        "refresh-1",
        "IDTOKEN-SECRET-9",
        "secret-route",
    ];

    let config = provider(Some(token_url), false);
    let auth = load(&access, "refresh-0", &config).unwrap();
    let mut rendered = vec![
        format!("{auth:?}"),
        format!("{config:?}"),
        format!("{:?}", auth.endpoint_source()),
    ];
    let hub_text = format!(
        "data_dir = '/var/lib/teslatlas'\nbind = '127.0.0.1:8080'\n[collector.legacy_auth]\nenabled = true\ntoken_url = '{token_url}'\n"
    );
    let (hub, _) = HubConfig::from_exact_bytes(hub_text.as_bytes()).unwrap();
    rendered.push(format!("{hub:?}"));
    rendered.push(format!("{:?}", hub.collector.legacy_auth));
    // Every error the selection can return, shown and debugged.
    for config in [
        tesla(None),
        tesla(Some(StreamRegion::China)),
        provider(None, false),
        provider(Some("http://x.example.test/secret-route"), false),
    ] {
        for token in [access.as_str(), OP, "bad token"] {
            if let Err(error) = load(token, "refresh-0", &config) {
                rendered.push(format!("{error} {error:?}"));
                if let Some(state) = error.load_failure_state() {
                    rendered.push(format!("{state:?}"));
                }
            }
        }
    }
    for text in rendered {
        for secret in secrets {
            assert!(!text.contains(secret), "{secret} appeared in {text}");
        }
    }
    // The debug form of the auth still says where it sends requests, by host.
    assert!(format!("{auth:?}").contains("auth.provider.example.test"));
    assert!(format!("{auth:?}").contains("ProviderConfig"));
}
