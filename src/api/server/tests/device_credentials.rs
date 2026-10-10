// SPDX-License-Identifier: AGPL-3.0-only

//! Device credentials over HTTP: the claim's device-name refusal and lifetime
//! cap, rotation (its grace retry and the answer at the maximum) and
//! `POST /v1/device/revoke`. Real SQLite, the real routers, synthetic data.
//! A nested module of the grant tests: it reuses their fixtures.

use super::*;

const CRED_DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// A paired router (device authentication and claims on) whose claims cap a
/// device's lifetime at `device_max_lifetime_ms`, or at the default.
fn credential_router(store: &HubStore, device_max_lifetime_ms: Option<i64>) -> Router {
    let key = grant_key();
    let state = AppState::new(
        store.clone(),
        false,
        true,
        true,
        Some(ManifestSigning::from_cursor_key(&key)),
        Some(key),
        None,
        None,
    );
    router_for_state(
        match device_max_lifetime_ms {
            Some(span) => state.with_device_max_lifetime_ms(span),
            None => state,
        },
        CorsPolicy::default(),
    )
}

fn credential_invitation(store: &HubStore) -> crate::db::PairingInvitation {
    let now_ms = current_epoch_ms().expect("clock");
    store
        .create_pairing("credentials over HTTP", now_ms - 1, i64::MAX)
        .expect("invitation")
}

async fn claim(
    app: &Router,
    pairing_id: impl std::fmt::Display,
    secret: &str,
    device_name: &str,
) -> Response {
    send(
        app,
        "POST",
        &format!("/v1/pairings/{pairing_id}/claim"),
        None,
        Some(serde_json::json!({"secret": secret, "device_name": device_name})),
    )
    .await
}

async fn claim_ok(app: &Router, store: &HubStore) -> serde_json::Value {
    let invitation = credential_invitation(store);
    let response = claim(
        app,
        invitation.pairing_id,
        invitation.secret(),
        "http phone",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let claimed = response_json(response).await;
    // A claim answers with exactly the device, its bearer and the expiry: the
    // `reason` member belongs to rotation alone.
    assert_eq!(
        member_names(&claimed),
        BTreeSet::from(["access_token", "device_id", "expires_at_ms"].map(String::from))
    );
    claimed
}

async fn rotate(app: &Router, bearer: &str) -> Response {
    send(app, "POST", "/v1/device/rotate", Some(bearer), None).await
}

async fn revoke(app: &Router, bearer: Option<&str>, body: Option<Vec<u8>>) -> Response {
    let mut request = Request::builder().method("POST").uri("/v1/device/revoke");
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let body = match body {
        Some(bytes) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(bytes)
        }
        None => Body::empty(),
    };
    app.clone()
        .oneshot(request.body(body).expect("request"))
        .await
        .expect("in-memory response")
}

/// Whether the device's grace row holds a sealed successor.
fn grace_is_sealed(store: &HubStore, device_id: &str) -> bool {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT sealed_successor IS NOT NULL FROM paired_device_token_grace
              WHERE device_id = ?1",
            [device_id],
            |row| row.get(0),
        )
        .expect("grace row")
}

/// Every device row and grace row, to prove a request changed nothing.
fn credential_rows(store: &HubStore) -> Vec<String> {
    let connection = store.open().expect("catalogue");
    let mut rows: Vec<String> = connection
        .prepare(
            "SELECT device_id || ' ' || hex(token_sha256) || ' ' || expires_at_ms || ' ' ||
                    COALESCE(revoked_at_ms, 'live') || ' ' ||
                    COALESCE(last_authenticated_at_ms, 'never') || ' ' ||
                    COALESCE(max_expires_at_ms, 'none')
               FROM paired_devices ORDER BY device_id",
        )
        .expect("devices")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("device rows")
        .map(|row| row.expect("row"))
        .collect();
    rows.extend(
        connection
            .prepare(
                "SELECT device_id || ' ' || hex(token_sha256) || ' ' || valid_until_ms || ' ' ||
                        COALESCE(hex(sealed_successor), 'unsealed')
                   FROM paired_device_token_grace ORDER BY device_id",
            )
            .expect("graces")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("grace rows")
            .map(|row| row.expect("row")),
    );
    rows
}

fn device_columns(store: &HubStore, device_id: &str) -> (i64, i64, Option<i64>, Option<i64>) {
    store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT created_at_ms, expires_at_ms, max_expires_at_ms, revoked_at_ms
               FROM paired_devices WHERE device_id = ?1",
            [device_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("device row")
}

fn member_names(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("JSON object")
        .keys()
        .cloned()
        .collect()
}

fn is_bearer_challenge(headers: &HeaderMap) -> bool {
    headers.get(header::WWW_AUTHENTICATE) == Some(&HeaderValue::from_static("Bearer"))
}

/// `bearer` is refused with `401`, the bearer challenge and an empty body on
/// every JSON, sync and pack route, rotate and revoke.
async fn assert_refused_everywhere(grants: &Grants, bearer: &str) {
    let routes = vehicle_routes();
    let mut uris: Vec<(String, &str)> = routes
        .iter()
        .map(|route| (route.uri(&grants.x.to_string()), route.method))
        .collect();
    uris.push(("/v1/vehicles".to_owned(), "GET"));
    uris.push((pack_uri(grants.pack_x.digest), "GET"));
    uris.push(("/v1/device/rotate".to_owned(), "POST"));
    uris.push(("/v1/device/revoke".to_owned(), "POST"));
    for (uri, method) in &uris {
        let body = (*method == "POST" && uri.ends_with("changes-since")).then(|| {
            serde_json::json!({
                "base_receipt_id": "0".repeat(64),
                "base_manifest_schema": "2.2",
                "from_sequence": 0,
                "schema_version_range": {"minimum": "2.2", "maximum": "2.2"}
            })
        });
        let refused = send(&grants.app, method, uri, Some(bearer), body).await;
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
        assert!(is_bearer_challenge(refused.headers()), "{method} {uri}");
        assert!(body_bytes(refused).await.is_empty(), "{method} {uri}");
    }
}

// ------------------------------------------------------------------- claim

#[tokio::test]
async fn a_claim_with_a_refused_device_name_is_a_422_and_consumes_nothing() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let app = credential_router(&store, None);
    let invitation = credential_invitation(&store);
    let refused = [
        ("empty", String::new()),
        ("129 bytes", "a".repeat(129)),
        ("65 two-byte characters (130 bytes)", "\u{e9}".repeat(65)),
        ("newline", "line\nbreak".to_owned()),
        ("tab", "tab\there".to_owned()),
        ("NUL", "nul\u{0}byte".to_owned()),
        ("unit separator", "\u{1f}".to_owned()),
        ("DEL", "del\u{7f}".to_owned()),
        ("NEL (U+0085)", "next\u{85}line".to_owned()),
        ("last C1 control (U+009F)", "\u{9f}".to_owned()),
    ];
    for (label, name) in &refused {
        let response = claim(&app, invitation.pairing_id, invitation.secret(), name).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{label}"
        );
        let content_type = response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(
            content_type.starts_with("text/plain"),
            "{label}: {content_type}"
        );
        let body = body_bytes(response).await;
        assert!(
            !body.is_empty() && body.len() <= 65_536,
            "{label}: a bounded, non-empty text body"
        );
        assert!(std::str::from_utf8(&body).is_ok(), "{label}");
        // Nothing was consumed or created.
        assert_eq!(
            device_count(&store),
            0,
            "{label}: a refusal pairs no device"
        );
        assert_eq!(
            challenge_count(&store),
            1,
            "{label}: the invitation is intact"
        );
    }
    // The very same invitation still claims with a valid name.
    let accepted = claim(
        &app,
        invitation.pairing_id,
        invitation.secret(),
        "Valid name",
    )
    .await;
    assert_eq!(accepted.status(), StatusCode::OK);
    assert_eq!(device_count(&store), 1);
    assert_eq!(challenge_count(&store), 0);
}

fn device_count(store: &HubStore) -> i64 {
    store
        .open()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM paired_devices", [], |row| row.get(0))
        .unwrap()
}

fn challenge_count(store: &HubStore) -> i64 {
    store
        .open()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM pairing_challenges", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test]
async fn device_names_at_the_limits_are_accepted() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let app = credential_router(&store, None);
    let accepted = [
        "a".repeat(128),
        "\u{e9}".repeat(64),
        "x".to_owned(),
        // Not controls: NBSP (U+00A0) and the line separator (U+2028).
        "\u{a0}".to_owned(),
        "two\u{2028}lines".to_owned(),
        "\u{540d}\u{524d}".to_owned(),
    ];
    for name in &accepted {
        let invitation = credential_invitation(&store);
        let response = claim(&app, invitation.pairing_id, invitation.secret(), name).await;
        assert_eq!(response.status(), StatusCode::OK, "{name:?}");
    }
    assert_eq!(device_count(&store), accepted.len() as i64);
}

#[tokio::test]
async fn the_name_is_judged_before_the_invitation_is_consulted() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let app = credential_router(&store, None);
    let invitation = credential_invitation(&store);
    let bad_name = "a".repeat(129);

    // A refused name is the same answer for a real invitation (right secret),
    // a real invitation with a wrong secret, an unknown id and an id that is
    // no UUID: it tells nothing about whether the invitation exists.
    let real =
        fingerprint(claim(&app, invitation.pairing_id, invitation.secret(), &bad_name).await).await;
    let wrong_secret =
        fingerprint(claim(&app, invitation.pairing_id, &"0".repeat(64), &bad_name).await).await;
    let unknown =
        fingerprint(claim(&app, Uuid::new_v4(), invitation.secret(), &bad_name).await).await;
    let not_a_uuid = fingerprint(claim(&app, "not-a-uuid", "secret", &bad_name).await).await;
    assert_eq!(real.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!real.body.is_empty());
    assert_eq!(wrong_secret, real);
    assert_eq!(unknown, real);
    assert_eq!(not_a_uuid, real);
    assert_eq!(challenge_count(&store), 1);

    // With a good name the invitation is consulted: an unknown id or a wrong
    // secret is the empty 401, a non-UUID id is 401 as before.
    for response in [
        claim(&app, Uuid::new_v4(), invitation.secret(), "Valid").await,
        claim(&app, invitation.pairing_id, &"0".repeat(64), "Valid").await,
        claim(&app, "not-a-uuid", invitation.secret(), "Valid").await,
    ] {
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(body_bytes(response).await.is_empty());
    }

    // Where claims are not served at all the answer is the 404, name or not.
    let local = trusted_local_router(store.clone(), false, Some(grant_key()), None);
    for name in [bad_name.as_str(), "Valid"] {
        let response = claim(&local, invitation.pairing_id, invitation.secret(), name).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{name:.8}");
        assert!(body_bytes(response).await.is_empty());
    }
    assert_eq!(challenge_count(&store), 1);
}

#[tokio::test]
async fn a_claim_is_capped_at_the_configured_lifetime() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");

    // The configured span is the device's absolute maximum, and the first
    // bearer (30 days) never outlives it.
    let ten_days = credential_router(&store, Some(10 * CRED_DAY_MS));
    let claimed = claim_ok(&ten_days, &store).await;
    let device = claimed["device_id"].as_str().unwrap();
    let (created, expires, maximum, _) = device_columns(&store, device);
    assert_eq!(maximum, Some(created + 10 * CRED_DAY_MS));
    assert_eq!(expires, created + 10 * CRED_DAY_MS);
    assert_eq!(claimed["expires_at_ms"], expires);

    let sixty_days = credential_router(&store, Some(60 * CRED_DAY_MS));
    let claimed = claim_ok(&sixty_days, &store).await;
    let (created, expires, maximum, _) =
        device_columns(&store, claimed["device_id"].as_str().unwrap());
    assert_eq!(maximum, Some(created + 60 * CRED_DAY_MS));
    assert_eq!(expires, created + 30 * CRED_DAY_MS);
    assert_eq!(claimed["expires_at_ms"], expires);

    // Without configuration the default is 180 days, through paired_router too.
    for app in [
        credential_router(&store, None),
        paired_router(store.clone(), &grant_key()),
    ] {
        let claimed = claim_ok(&app, &store).await;
        let (created, expires, maximum, _) =
            device_columns(&store, claimed["device_id"].as_str().unwrap());
        assert_eq!(maximum, Some(created + 180 * CRED_DAY_MS));
        assert_eq!(expires, created + 30 * CRED_DAY_MS);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_tls_router_takes_the_claim_lifetime_from_the_configuration() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    // The router both TLS serve paths build from the loaded configuration.
    let router = |pairing: &str| {
        let source =
            format!("data_dir = '/var/lib/teslatlas-test'\nbind = '127.0.0.1:8443'\n{pairing}");
        let (config, _) = HubConfig::from_exact_bytes(source.as_bytes()).expect("configuration");
        tls_router_for_config(
            store.clone(),
            false,
            &grant_key(),
            Sha256Digest::of_bytes(b"configuration"),
            &config,
        )
    };
    for (table, days) in [
        ("[pairing]\ndevice_max_lifetime_days = 10\n", 10),
        ("[pairing]\ndevice_max_lifetime_days = 365\n", 365),
        ("", 180),
    ] {
        let app = router(table);
        let claimed = claim_ok(&app, &store).await;
        let (created, expires, maximum, _) =
            device_columns(&store, claimed["device_id"].as_str().unwrap());
        assert_eq!(maximum, Some(created + days * CRED_DAY_MS), "{table:?}");
        assert_eq!(expires, created + days.min(30) * CRED_DAY_MS, "{table:?}");
    }
}

// ------------------------------------------------------------------ rotate

#[tokio::test]
async fn rotation_over_http_retries_through_the_grace_window_and_names_no_reason() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let app = credential_router(&store, None);
    let claimed = claim_ok(&app, &store).await;
    let first = claimed["access_token"].as_str().unwrap().to_owned();
    let device = claimed["device_id"].as_str().unwrap().to_owned();

    let rotated = rotate(&app, &first).await;
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated = response_json(rotated).await;
    let second = rotated["access_token"].as_str().unwrap().to_owned();
    assert_ne!(second, first);
    assert_eq!(rotated["device_id"], device.as_str());
    let ordinary_members: BTreeSet<String> = ["access_token", "device_id", "expires_at_ms"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        member_names(&rotated),
        ordinary_members,
        "an ordinary rotation names no reason"
    );

    // The lost-response retry: the same successor, byte for byte, and no row
    // changes.
    let before = credential_rows(&store);
    for _ in 0..3 {
        let retried = rotate(&app, &first).await;
        assert_eq!(retried.status(), StatusCode::OK);
        let retried = response_json(retried).await;
        assert_eq!(retried, rotated, "the retry is the very same answer");
    }
    assert_eq!(credential_rows(&store), before);

    // Both bearers serve while the window is open.
    for bearer in [&first, &second] {
        assert_eq!(
            send(&app, "GET", "/v1/vehicles", Some(bearer), None)
                .await
                .status(),
            StatusCode::OK
        );
    }
    // T2 -> T3 ends T1's grace: T1 is refused everywhere, T2 retries to T3.
    let third = response_json(rotate(&app, &second).await).await;
    let third_bearer = third["access_token"].as_str().unwrap().to_owned();
    assert_ne!(third_bearer, second);
    for uri in ["/v1/vehicles", "/v1/device/rotate"] {
        let method = if uri == "/v1/vehicles" { "GET" } else { "POST" };
        let refused = send(&app, method, uri, Some(&first), None).await;
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert!(is_bearer_challenge(refused.headers()));
        assert!(body_bytes(refused).await.is_empty());
    }
    assert_eq!(response_json(rotate(&app, &second).await).await, third);

    // No bearer, a malformed one and an unknown one are the same 401.
    let unknown = "0123456789abcdef".repeat(4);
    for bearer in [None, Some("short"), Some(unknown.as_str())] {
        let response = send(&app, "POST", "/v1/device/rotate", bearer, None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(is_bearer_challenge(response.headers()));
    }
    // Without paired-device authentication there is no rotation.
    let local = trusted_local_router(store.clone(), false, Some(grant_key()), None);
    let response = send(&local, "POST", "/v1/device/rotate", Some(&first), None).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_grace_retry_over_http_is_401_after_the_window_or_without_a_sealed_successor() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let app = credential_router(&store, None);
    let claimed = claim_ok(&app, &store).await;
    let first = claimed["access_token"].as_str().unwrap().to_owned();
    let rotated = response_json(rotate(&app, &first).await).await;
    let second = rotated["access_token"].as_str().unwrap().to_owned();
    // Inside the window the retry is the successor, as a control.
    assert_eq!(response_json(rotate(&app, &first).await).await, rotated);

    // After the window (the grace row's deadline has passed): 401, the bearer
    // challenge and no body, on rotate and on a JSON route; the current bearer
    // is unaffected and nothing is written.
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE paired_device_token_grace SET valid_until_ms = 1",
            [],
        )
        .expect("expire the grace window");
    let before = credential_rows(&store);
    for (method, uri) in [("POST", "/v1/device/rotate"), ("GET", "/v1/vehicles")] {
        let refused = send(&app, method, uri, Some(&first), None).await;
        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert!(is_bearer_challenge(refused.headers()), "{uri}");
        assert!(body_bytes(refused).await.is_empty(), "{uri}");
    }
    assert_eq!(credential_rows(&store), before);
    assert_eq!(
        send(&app, "GET", "/v1/vehicles", Some(&second), None)
            .await
            .status(),
        StatusCode::OK
    );

    // A grace row that has no sealed successor (one a Hub before 1.2.0 left
    // behind): inside its window the retry cannot be answered, so 401.
    store
        .open()
        .expect("catalogue")
        .execute(
            "UPDATE paired_device_token_grace
                SET valid_until_ms = 9223372036854775807, sealed_successor = NULL",
            [],
        )
        .expect("an unsealed grace row");
    let before = credential_rows(&store);
    let refused = rotate(&app, &first).await;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    assert!(is_bearer_challenge(refused.headers()));
    assert!(body_bytes(refused).await.is_empty());
    assert_eq!(credential_rows(&store), before, "a refusal changes nothing");
    // The current bearer still rotates.
    assert_eq!(rotate(&app, &second).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn rotation_at_the_maximum_answers_the_contract_example_and_writes_nothing() {
    let example: serde_json::Value = serde_json::from_slice(
        &fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("protocol/profiles/hub-http-v1/1.2.0/examples/rotate-max-lifetime.json"),
        )
        .expect("contract example"),
    )
    .expect("contract example JSON");
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    // A 10-day cap: the claimed device is at its maximum from the start.
    let app = credential_router(&store, Some(10 * CRED_DAY_MS));
    let claimed = claim_ok(&app, &store).await;
    let bearer = claimed["access_token"].as_str().unwrap().to_owned();
    let device = claimed["device_id"].as_str().unwrap().to_owned();
    let (created, _, maximum, _) = device_columns(&store, &device);
    let maximum = maximum.expect("a maximum");
    assert_eq!(maximum, created + 10 * CRED_DAY_MS);

    let before = credential_rows(&store);
    let answered = rotate(&app, &bearer).await;
    assert_eq!(answered.status(), StatusCode::OK);
    let answered = response_json(answered).await;
    assert_eq!(member_names(&answered), member_names(&example));
    assert_eq!(answered["reason"], example["reason"]);
    assert_eq!(answered["reason"], "max_lifetime");
    assert_eq!(
        answered["access_token"],
        bearer.as_str(),
        "the presented bearer, unchanged"
    );
    assert_eq!(answered["device_id"], device.as_str());
    assert_eq!(
        answered["expires_at_ms"], maximum,
        "the expiry is unchanged"
    );
    assert_eq!(
        credential_rows(&store),
        before,
        "nothing was issued or written: no new token, no grace row"
    );
    assert_eq!(before.len(), 1, "one device row and no grace row");
    // Repeated rotations are the same answer, and the bearer keeps serving.
    assert_eq!(response_json(rotate(&app, &bearer).await).await, answered);
    assert_eq!(
        send(&app, "GET", "/v1/vehicles", Some(&bearer), None)
            .await
            .status(),
        StatusCode::OK
    );
}

// ------------------------------------------------------------------ revoke

#[tokio::test]
async fn revoke_is_204_and_then_the_bearer_is_401_on_json_sync_and_pack_routes() {
    let grants = Grants::new();
    let (device, bearer) = grants.device(&[grants.x]);
    let (other_device, other_bearer) = grants.device(&[grants.x]);
    let get = |uri: String, bearer: Option<String>| {
        let app = grants.app.clone();
        async move { send(&app, "GET", &uri, bearer.as_deref(), None).await }
    };
    let routes = vehicle_routes();
    // Before: every route answers its own status.
    for route in &routes {
        assert_eq!(
            grants
                .send(route, &grants.x.to_string(), Some(&bearer))
                .await
                .status(),
            route.own_status,
            "{} before",
            route.label
        );
    }
    assert_eq!(
        get("/v1/vehicles".into(), Some(bearer.clone()))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        get(pack_uri(grants.pack_x.digest), Some(bearer.clone()))
            .await
            .status(),
        StatusCode::OK
    );

    let revoked = revoke(&grants.app, Some(&bearer), None).await;
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert_eq!(revoked.headers()[header::CACHE_CONTROL], "no-store");
    assert!(!revoked.headers().contains_key(header::WWW_AUTHENTICATE));
    assert!(body_bytes(revoked).await.is_empty());
    let (_, _, _, revoked_at) = device_columns(&grants.store, &device.to_string());
    assert!(
        revoked_at.is_some(),
        "the device is revoked in the catalogue"
    );

    // After: 401 with the bearer challenge and an empty body everywhere.
    assert_refused_everywhere(&grants, &bearer).await;
    // A repeated call is the ordinary 401: clients treat it as already revoked.
    let again = revoke(&grants.app, Some(&bearer), None).await;
    assert_eq!(again.status(), StatusCode::UNAUTHORIZED);
    assert!(is_bearer_challenge(again.headers()));
    assert!(body_bytes(again).await.is_empty());

    // Another device is untouched.
    assert_eq!(
        get("/v1/vehicles".into(), Some(other_bearer))
            .await
            .status(),
        StatusCode::OK
    );
    let (_, _, _, other_revoked) = device_columns(&grants.store, &other_device.to_string());
    assert!(other_revoked.is_none());
}

#[tokio::test]
async fn a_request_body_of_any_size_is_a_400_never_a_413_and_revokes_nothing() {
    let grants = Grants::new();
    let (device, bearer) = grants.device(&[grants.x]);
    let usable = || async {
        send(&grants.app, "GET", "/v1/vehicles", Some(&bearer), None)
            .await
            .status()
    };
    let bodies: Vec<(&str, Vec<u8>)> = vec![
        ("{}", b"{}".to_vec()),
        ("one byte", b"x".to_vec()),
        ("one space", b" ".to_vec()),
        ("4096 bytes", vec![b'x'; 4096]),
        ("4097 bytes", vec![b'x'; 4097]),
        ("100000 bytes", vec![b'x'; 100_000]),
    ];
    for (label, body) in bodies {
        let response = revoke(&grants.app, Some(&bearer), Some(body)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{label}");
        assert!(body_bytes(response).await.is_empty(), "{label}");
        assert_eq!(
            usable().await,
            StatusCode::OK,
            "{label}: the device is still usable"
        );
        let (_, _, _, revoked_at) = device_columns(&grants.store, &device.to_string());
        assert!(revoked_at.is_none(), "{label}: nothing was revoked");
    }
    // The bearer is checked before the body: no or a wrong bearer is the 401
    // whatever the body.
    let unknown = "0123456789abcdef".repeat(4);
    for bearer in [None, Some("short"), Some(unknown.as_str())] {
        let response = revoke(&grants.app, bearer, Some(b"{}".to_vec())).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(is_bearer_challenge(response.headers()));
        assert!(body_bytes(response).await.is_empty());
        let response = revoke(&grants.app, bearer, None).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(is_bearer_challenge(response.headers()));
        assert!(body_bytes(response).await.is_empty());
    }
    // And the empty request still revokes.
    assert_eq!(
        revoke(&grants.app, Some(&bearer), None).await.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(usable().await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoke_with_the_current_or_the_grace_bearer_revokes_the_device() {
    let grants = Grants::new();
    for use_grace in [true, false] {
        let (device, first) = grants.device(&[grants.x]);
        let rotated = response_json(rotate(&grants.app, &first).await).await;
        let second = rotated["access_token"].as_str().unwrap().to_owned();
        for bearer in [&first, &second] {
            assert_eq!(
                send(&grants.app, "GET", "/v1/vehicles", Some(bearer), None)
                    .await
                    .status(),
                StatusCode::OK
            );
        }
        let presented = if use_grace { &first } else { &second };
        assert!(
            grace_is_sealed(&grants.store, &device.to_string()),
            "grace: {use_grace}: the rotation sealed a successor"
        );
        let revoked = revoke(&grants.app, Some(presented), None).await;
        assert_eq!(
            revoked.status(),
            StatusCode::NO_CONTENT,
            "grace: {use_grace}"
        );
        let (_, _, _, revoked_at) = device_columns(&grants.store, &device.to_string());
        assert!(revoked_at.is_some());
        assert!(
            !grace_is_sealed(&grants.store, &device.to_string()),
            "grace: {use_grace}: the revocation cleared the sealed successor"
        );
        // Both bearers of the device are dead, on every route.
        for bearer in [&first, &second] {
            for (method, uri) in [
                ("GET", "/v1/vehicles".to_owned()),
                ("POST", "/v1/device/rotate".to_owned()),
                ("POST", "/v1/device/revoke".to_owned()),
                ("GET", pack_uri(grants.pack_x.digest)),
            ] {
                let refused = send(&grants.app, method, &uri, Some(bearer), None).await;
                assert_eq!(
                    refused.status(),
                    StatusCode::UNAUTHORIZED,
                    "grace: {use_grace} {method} {uri}"
                );
            }
        }
    }
}

#[tokio::test]
async fn revoke_is_a_404_where_paired_device_authentication_is_off() {
    let temp = crate::private_tempdir().expect("temp directory");
    let store = HubStore::initialize(temp.path()).expect("store");
    let paired = credential_router(&store, None);
    let claimed = claim_ok(&paired, &store).await;
    let bearer = claimed["access_token"].as_str().unwrap().to_owned();
    let device = claimed["device_id"].as_str().unwrap().to_owned();
    // The loopback router has no device principal: the route is not served,
    // with or without a (valid) bearer or a body, and nothing is revoked.
    let local = trusted_local_router(store.clone(), false, Some(grant_key()), None);
    let before = credential_rows(&store);
    for (bearer, body) in [
        (None, None),
        (Some(bearer.as_str()), None),
        (Some(bearer.as_str()), Some(b"{}".to_vec())),
        (None, Some(vec![b'x'; 4097])),
    ] {
        let response = revoke(&local, bearer, body).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(body_bytes(response).await.is_empty());
    }
    assert_eq!(credential_rows(&store), before, "no device was touched");
    let (_, _, _, revoked_at) = device_columns(&store, &device);
    assert!(revoked_at.is_none());
    // The same bearer revokes on the paired router.
    assert_eq!(
        revoke(&paired, Some(&bearer), None).await.status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn a_bearer_at_its_absolute_maximum_is_401_on_json_sync_pack_rotate_and_revoke_routes() {
    let grants = Grants::new();
    let (device, bearer) = grants.device(&[grants.x]);
    let (other, other_bearer) = grants.device(&[grants.x]);
    let rotated = response_json(rotate(&grants.app, &bearer).await).await;
    let successor = rotated["access_token"].as_str().unwrap().to_owned();
    // The column's check allows a maximum equal to the creation time, which is
    // already past: the device is at its absolute maximum.
    grants
        .store
        .open()
        .unwrap()
        .execute(
            "UPDATE paired_devices SET max_expires_at_ms = created_at_ms WHERE device_id = ?1",
            [device.to_string()],
        )
        .unwrap();
    // Neither the current bearer nor the grace one (inside its window) serves.
    for refused in [&successor, &bearer] {
        assert_refused_everywhere(&grants, refused).await;
    }
    let (_, _, _, revoked_at) = device_columns(&grants.store, &device.to_string());
    assert!(revoked_at.is_none(), "the refused revoke did nothing");
    // Another device without that state is served, and was never touched.
    assert_eq!(
        send(
            &grants.app,
            "GET",
            "/v1/vehicles",
            Some(&other_bearer),
            None
        )
        .await
        .status(),
        StatusCode::OK
    );
    let (_, _, _, other_revoked) = device_columns(&grants.store, &other.to_string());
    assert!(other_revoked.is_none());
}

#[tokio::test]
async fn a_revocation_answer_is_204_401_for_a_vanished_device_and_503_for_a_store_failure() {
    use super::super::super::device_revoke::revocation_response;
    let revoked = revocation_response(Ok(()));
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert_eq!(revoked.headers()[header::CACHE_CONTROL], "no-store");
    assert!(!revoked.headers().contains_key(header::WWW_AUTHENTICATE));
    assert!(body_bytes(revoked).await.is_empty());
    // The device row vanished between the bearer check and the revocation.
    let gone = revocation_response(Err(crate::db::StoreError::PairingRejected));
    assert_eq!(gone.status(), StatusCode::UNAUTHORIZED);
    assert!(is_bearer_challenge(gone.headers()));
    assert!(body_bytes(gone).await.is_empty());
    // Any other store failure is the empty 503, with nothing of it exposed.
    let failed = revocation_response(Err(crate::db::StoreError::RevokeDevice(
        rusqlite::Error::InvalidQuery,
    )));
    assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(!failed.headers().contains_key(header::WWW_AUTHENTICATE));
    assert!(!failed.headers().contains_key(header::CONTENT_TYPE));
    assert!(body_bytes(failed).await.is_empty());
}

#[tokio::test]
async fn an_admitted_origin_can_post_a_real_revoke_and_read_the_answers() {
    const ORIGIN: &str = "https://client.example";
    let grants = Grants::new();
    let (device, bearer) = grants.device(&[grants.x]);
    let key = grant_key();
    let cors = CorsPolicy {
        http: crate::config::HttpConfig {
            allowed_origins: vec![ORIGIN.into()],
        },
        same_origin: None,
    };
    let app = router_for_state(
        AppState::new(
            grants.store.clone(),
            false,
            true,
            true,
            Some(ManifestSigning::from_cursor_key(&key)),
            Some(key),
            None,
            None,
        ),
        cors,
    );
    let post = |origin: &'static str| {
        let app = app.clone();
        let bearer = bearer.clone();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/device/revoke")
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .header(header::ORIGIN, origin)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("in-memory response")
        }
    };
    // An origin that is not admitted is refused before the handler: the
    // device stays.
    let refused = post("https://elsewhere.example").await;
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    let (_, _, _, revoked_at) = device_columns(&grants.store, &device.to_string());
    assert!(revoked_at.is_none());
    // The admitted origin's real request revokes, and its page can read the
    // answer.
    let revoked = post(ORIGIN).await;
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        revoked.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        ORIGIN
    );
    assert_eq!(revoked.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        !revoked
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
    );
    let (_, _, _, revoked_at) = device_columns(&grants.store, &device.to_string());
    assert!(revoked_at.is_some());
    // The repeat is the 401 a page can read as "already revoked".
    let again = post(ORIGIN).await;
    assert_eq!(again.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(again.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], ORIGIN);
    assert!(body_bytes(again).await.is_empty());
}
