// SPDX-License-Identifier: AGPL-3.0-only

//! Discovery (`GET /.well-known/teslatlas-hub`) against the committed
//! `hub-http-v1@1.2.0` profile: the profile it names, the exact capability
//! tokens of each router configuration, their order, and the schema's required
//! members. Real SQLite stores, the real routers, synthetic data only.

use std::collections::BTreeSet;

use super::*;

const BASE_TOKENS: [&str; 2] = ["query.vehicles", "query.current"];

/// Base tokens plus the cursor-key set, in profile order.
const CURSOR_KEY_TOKENS: [&str; 7] = [
    "query.vehicles",
    "query.current",
    "query.drives",
    "query.charges",
    "query.charge.samples",
    "query.positions",
    "sync.packs",
];

/// The cursor-key set plus `auth.revoke`, last.
const PAIRED_TOKENS: [&str; 8] = [
    "query.vehicles",
    "query.current",
    "query.drives",
    "query.charges",
    "query.charge.samples",
    "query.positions",
    "sync.packs",
    "auth.revoke",
];

const KEY_BYTE: u8 = 61;

fn profile_document(name: &str) -> serde_json::Value {
    let path = format!(
        "{}/protocol/profiles/hub-http-v1/1.2.0/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = fs::read_to_string(path).expect("committed hub-http-v1@1.2.0 document");
    serde_json::from_str(&text).expect("profile document JSON")
}

fn tokens_of(value: &serde_json::Value) -> Vec<&str> {
    value
        .as_array()
        .expect("token array")
        .iter()
        .map(|token| token.as_str().expect("token string"))
        .collect()
}

/// The three router configurations a Hub runs: the trusted local router without
/// a cursor key, the loopback router with one, and the TLS-facing paired router.
struct Routers {
    _temp: tempfile::TempDir,
    installation_id: Uuid,
    unsigned: Router,
    cursor_keyed: Router,
    paired: Router,
}

impl Routers {
    fn new() -> Self {
        let temp = crate::private_tempdir().expect("temp directory");
        let store = HubStore::initialize(temp.path()).expect("store");
        Self {
            _temp: temp,
            installation_id: store.installation_id().expect("installation identity"),
            unsigned: router(store.clone()),
            cursor_keyed: public_query_router(store.clone(), KEY_BYTE),
            paired: paired_router(store, &CursorKey::from_bytes([KEY_BYTE; 32])),
        }
    }

    /// Every configuration with its name and the exact tokens it advertises.
    fn cases(&self) -> [(&'static str, &Router, &'static [&'static str]); 3] {
        [
            ("router()", &self.unsigned, &BASE_TOKENS[..]),
            (
                "public_query_router()",
                &self.cursor_keyed,
                &CURSOR_KEY_TOKENS[..],
            ),
            ("paired_router()", &self.paired, &PAIRED_TOKENS[..]),
        ]
    }
}

async fn discover(app: &Router) -> (StatusCode, HeaderMap, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/.well-known/teslatlas-hub")
                .body(Body::empty())
                .expect("discovery request"),
        )
        .await
        .expect("discovery response");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("discovery body")
        .to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&body).expect("discovery JSON"),
    )
}

async fn post_without_bearer(app: &Router, uri: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
        .status()
}

#[tokio::test]
async fn each_router_configuration_advertises_its_exact_tokens_and_the_profile() {
    let routers = Routers::new();
    for (name, app, tokens) in routers.cases() {
        let (status, headers, payload) = discover(app).await;
        assert_eq!(status, StatusCode::OK, "{name}");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{name}");
        assert_eq!(tokens_of(&payload["capabilities"]), tokens, "{name}");
        assert_eq!(payload["profile"], "hub-http-v1@1.2.0", "{name}");
        // The Home Assistant invariants stay exactly as they were.
        assert_eq!(
            payload["api_versions"],
            serde_json::json!(["1.0"]),
            "{name}"
        );
        assert_eq!(payload["protocol"], "teslatlas-sync", "{name}");
        assert_eq!(payload["protocol_major"], 1, "{name}");
        assert_eq!(payload["pack_format"], "sqlite-zstd", "{name}");
        assert_eq!(payload["version"], crate::BUILD_VERSION, "{name}");
        assert_eq!(
            payload["sourceUrl"],
            crate::discovery_source_url(),
            "{name}"
        );
        assert_eq!(
            payload["hub_id"],
            routers.installation_id.to_string(),
            "{name}"
        );
    }
}

#[tokio::test]
async fn only_a_hub_with_its_cursor_key_publishes_the_manifest_verifying_key() {
    let routers = Routers::new();
    let (_, _, unsigned) = discover(&routers.unsigned).await;
    assert!(unsigned.get("manifestPublicKey").is_none());
    let expected = ManifestSigning::from_cursor_key(&CursorKey::from_bytes([KEY_BYTE; 32]))
        .verifying_key_hex();
    for app in [&routers.cursor_keyed, &routers.paired] {
        let (_, _, payload) = discover(app).await;
        assert_eq!(payload["manifestPublicKey"], expected);
    }
}

#[test]
fn the_capability_function_answers_all_four_key_and_authentication_combinations() {
    use super::super::discovery::advertised_capabilities;

    assert_eq!(advertised_capabilities(false, false), BASE_TOKENS);
    assert_eq!(advertised_capabilities(true, false), CURSOR_KEY_TOKENS);
    assert_eq!(advertised_capabilities(true, true), PAIRED_TOKENS);
    // Paired-device authentication without a cursor key serves revoke but no
    // history or pack routes.
    assert_eq!(
        advertised_capabilities(false, true),
        ["query.vehicles", "query.current", "auth.revoke"]
    );
}

#[test]
fn advertised_tokens_follow_the_committed_profile_and_its_order() {
    use super::super::discovery::advertised_capabilities;

    let profile = profile_document("profile.json");
    assert_eq!(profile["profile_id"], HUB_HTTP_PROFILE_ID);
    let rules = &profile["capability_advertisement"];
    let always = tokens_of(&rules["always"]);
    let with_cursor_key = tokens_of(&rules["with_cursor_key"]);
    let with_authentication = tokens_of(&rules["with_paired_device_authentication"]);
    let enrichment = tokens_of(&rules["with_enrichment"]);
    let ordered = tokens_of(&profile["capabilities"]);

    for (has_cursor_key, authentication) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        // The profile's own order, filtered by what is advertised. Enrichment
        // needs attributions, which this Hub does not emit, so it never is.
        let expected: Vec<&str> = ordered
            .iter()
            .copied()
            .filter(|token| {
                always.contains(token)
                    || (has_cursor_key && with_cursor_key.contains(token))
                    || (authentication && with_authentication.contains(token))
            })
            .collect();
        let advertised = advertised_capabilities(has_cursor_key, authentication);
        assert_eq!(
            advertised, expected,
            "cursor key {has_cursor_key}, authentication {authentication}"
        );
        assert!(
            enrichment.iter().all(|token| !advertised.contains(token)),
            "enrichment is never advertised"
        );
    }
    assert_eq!(enrichment, ["query.enrichment"]);
}

#[tokio::test]
async fn discovery_carries_every_member_the_schema_requires_and_no_other() {
    let schema = profile_document("discovery.schema.json");
    let required = tokens_of(&schema["required"]);
    assert!(required.contains(&"profile"));
    let members = schema["properties"].as_object().expect("schema properties");
    let capabilities_schema = &members["capabilities"];
    let max_items = capabilities_schema["maxItems"].as_u64().expect("maxItems");
    let pattern = &members["profile"]["pattern"];
    assert_eq!(pattern, "^hub-http-v1@1\\.[0-9]+\\.[0-9]+$");
    let profile_id = profile_document("profile.json")["profile_id"]
        .as_str()
        .expect("profile id")
        .to_owned();

    let routers = Routers::new();
    for (name, app, _) in routers.cases() {
        let (_, _, payload) = discover(app).await;
        let object = payload.as_object().expect("discovery object");
        for member in &required {
            assert!(object.contains_key(*member), "{name} lacks {member}");
        }
        for member in object.keys() {
            assert!(members.contains_key(member), "{name} adds {member}");
        }
        // The schema's constants.
        for constant in ["api_versions", "protocol", "protocol_major", "pack_format"] {
            assert_eq!(
                payload[constant], members[constant]["const"],
                "{name} {constant}"
            );
        }
        // The capability grammar: at most 16 unique tokens of [a-z0-9.]{1,64},
        // including the two every Hub must advertise.
        let tokens = tokens_of(&payload["capabilities"]);
        assert!(tokens.len() as u64 <= max_items, "{name}");
        let unique: BTreeSet<_> = tokens.iter().collect();
        assert_eq!(unique.len(), tokens.len(), "{name}");
        for token in &tokens {
            assert!(
                (1..=64).contains(&token.len())
                    && token.bytes().all(|byte| byte.is_ascii_digit()
                        || byte.is_ascii_lowercase()
                        || byte == b'.'),
                "{name} token {token}"
            );
        }
        for required_token in ["query.vehicles", "query.current"] {
            assert!(tokens.contains(&required_token), "{name}");
        }
        // The profile pattern, and the committed profile's identity.
        assert_eq!(payload["profile"], profile_id, "{name}");
        assert!(profile_id.starts_with("hub-http-v1@1."), "{name}");
    }
}

#[tokio::test]
async fn auth_revoke_is_advertised_exactly_where_the_revoke_route_is_served() {
    let routers = Routers::new();
    for (name, app, tokens) in routers.cases() {
        let advertised = tokens.contains(&"auth.revoke");
        // A trusted local router (no paired-device authentication) answers the
        // route 404; a paired router asks for the bearer first.
        let expected = if advertised {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(
            post_without_bearer(app, "/v1/device/revoke").await,
            expected,
            "{name}"
        );
    }
    assert!(!CURSOR_KEY_TOKENS.contains(&"auth.revoke"));
}
