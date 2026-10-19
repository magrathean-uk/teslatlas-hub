// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use serde_json::Value;
use std::time::Duration;
use tokio_tungstenite::connect_async;

#[tokio::test]
async fn oauth_refresh_endpoint_validates_shape_without_retaining_secret() {
    crate::crypto::install_default_provider();
    let source = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("fake Tesla");
    let endpoint = source
        .oauth_issuer_url()
        .join("token")
        .expect("token endpoint");
    let client = reqwest::Client::new();

    let invalid = client
        .post(endpoint.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(
            serde_json::to_vec(&json!({
                "grant_type": "wrong",
                "scope": "openid email offline_access",
                "client_id": "ownerapi",
                "refresh_token": "must-not-be-retained",
            }))
            .expect("invalid request JSON"),
        )
        .send()
        .await
        .expect("invalid refresh request");
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(source.token_refresh_request_count(), 0);

    // C5 D4: the policy headers are required, and so is an exact four-member
    // body.
    let exact_body = json!({
        "grant_type": "refresh_token",
        "scope": "openid email offline_access",
        "client_id": "ownerapi",
        "refresh_token": "must-not-be-retained",
    });
    let without_headers = client
        .post(endpoint.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(&exact_body).expect("body"))
        .send()
        .await
        .expect("request without policy headers");
    assert_eq!(without_headers.status(), StatusCode::BAD_REQUEST);
    let mut with_extra_member = exact_body.clone();
    with_extra_member["extra"] = json!("member");
    let mut extra = client.post(endpoint.clone());
    for (name, value) in crate::legacy_auth::REFRESH_REQUEST_HEADERS {
        extra = extra.header(name, value);
    }
    let extra = extra
        .body(serde_json::to_vec(&with_extra_member).expect("body"))
        .send()
        .await
        .expect("request with an extra member");
    assert_eq!(extra.status(), StatusCode::BAD_REQUEST);
    assert_eq!(source.token_refresh_request_count(), 0);

    let mut valid = client.post(endpoint.clone());
    for (name, value) in crate::legacy_auth::REFRESH_REQUEST_HEADERS {
        valid = valid.header(name, value);
    }
    let valid = valid
        .body(
            serde_json::to_vec(&json!({
                "grant_type": "refresh_token",
                "scope": "openid email offline_access",
                "client_id": "ownerapi",
                "refresh_token": "must-not-be-retained",
            }))
            .expect("valid request JSON"),
        )
        .send()
        .await
        .expect("valid refresh request");
    assert_eq!(valid.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&valid.bytes().await.expect("token response bytes"))
            .expect("token response JSON");
    assert_eq!(
        body["access_token"], FAKE_REFRESHED_ACCESS_TOKEN,
        "fake access token is deterministic"
    );
    assert_eq!(body["refresh_token"], FAKE_REFRESHED_REFRESH_TOKEN);
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["expires_in"], 3_600);
    assert_eq!(source.token_refresh_request_count(), 1);

    let requests = source.audited_requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].path, "/oauth2/v3/token");
    assert_eq!(requests[0].response_status, 400);
    assert_eq!(requests[3].response_status, 200);
    let serialized = serde_json::to_string(&requests).expect("redacted ledger JSON");
    assert!(!serialized.contains("must-not-be-retained"));

    source.shutdown().await;
}

#[test]
fn stream_controls_reject_duplicate_keys() {
    let fields = crate::tesla_stream::STREAM_COLUMNS.join(",");
    let duplicate_subscribes = [
        format!(
            r#"{{"msg_type":"wrong","msg_type":"data:subscribe_oauth","token":"stream-evidence-secret","value":"{fields}","tag":"{FIXTURE_VID_TAG}"}}"#
        ),
        format!(
            r#"{{"msg_type":"data:subscribe_oauth","token":"wrong-stream-evidence-secret","token":"stream-evidence-secret","value":"{fields}","tag":"{FIXTURE_VID_TAG}"}}"#
        ),
        format!(
            r#"{{"msg_type":"data:subscribe_oauth","token":"stream-evidence-secret","token":"wrong-stream-evidence-secret","value":"{fields}","tag":"{FIXTURE_VID_TAG}"}}"#
        ),
        format!(
            r#"{{"msg_type":"data:subscribe_oauth","token":"stream-evidence-secret","value":"wrong","value":"{fields}","tag":"{FIXTURE_VID_TAG}"}}"#
        ),
        format!(
            r#"{{"msg_type":"data:subscribe_oauth","token":"stream-evidence-secret","value":"{fields}","tag":"wrong","tag":"{FIXTURE_VID_TAG}"}}"#
        ),
    ];
    for frame in duplicate_subscribes {
        assert_eq!(exact_subscribe_fields(&frame), Err("malformed_subscribe"));
    }

    for frame in [
        format!(
            r#"{{"msg_type":"wrong","msg_type":"data:unsubscribe","tag":"{FIXTURE_VID_TAG}"}}"#
        ),
        format!(r#"{{"msg_type":"data:unsubscribe","tag":"wrong","tag":"{FIXTURE_VID_TAG}"}}"#),
    ] {
        assert_eq!(exact_unsubscribe_tag(&frame), Err("malformed_unsubscribe"));
    }
}

#[tokio::test]
async fn auto_discovery_records_asleep_offline_online_once_in_scenario_ledger() {
    crate::crypto::install_default_provider();
    let evidence = tempfile::tempdir().expect("evidence");
    let source = FakeTeslaSource::spawn(AdvanceMode::AutoOnDiscovery, Some(evidence.path()))
        .await
        .expect("spawn");
    let products = format!("{}api/1/products", source.http_base_url());
    let client = reqwest::Client::new();

    for expected_state in ["asleep", "offline", "online"] {
        let body = client
            .get(&products)
            .send()
            .await
            .expect("products request")
            .text()
            .await
            .expect("products body");
        let response: Value = serde_json::from_str(&body).expect("products JSON");
        assert_eq!(response["response"][0]["state"], expected_state);
    }

    let events = std::fs::read_to_string(evidence.path().join("scenario-ledger.jsonl"))
        .expect("scenario ledger")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("scenario event"))
        .collect::<Vec<_>>();
    assert_eq!(
        events,
        vec![
            json!({"event": "initial", "step": "asleep_discovery"}),
            json!({"event": "auto_transition", "from": "asleep_discovery", "to": "offline_discovery"}),
            json!({"event": "auto_transition", "from": "offline_discovery", "to": "online_idle"}),
        ]
    );
    source.shutdown().await;
}

#[test]
fn odometer_offset_is_validated_and_applied_to_owner_and_stream_payloads() {
    assert_eq!(parse_odometer_offset_miles("10.5").unwrap(), 10.5);
    assert!(parse_odometer_offset_miles("-0.1").is_err());
    assert!(parse_odometer_offset_miles("NaN").is_err());
    assert!(parse_odometer_offset_miles("infinity").is_err());

    let payload: Value = serde_json::from_str(&vehicle_data_body(
        ScenarioStep::DrivePositions,
        2,
        1_800_000_000_000,
        10.0,
    ))
    .expect("Owner API payload");
    assert_eq!(payload["response"]["drive_state"]["odometer"], 12_355.2);
    assert_eq!(payload["response"]["vehicle_state"]["odometer"], 12_355.2);
    assert_eq!(
        owner_odometer_miles(ScenarioStep::DrivePositions, 2, 10.0),
        12_355.2
    );
}

#[test]
fn owner_odometer_helper_matches_every_vehicle_data_payload() {
    let offset_miles = 10.0;
    for step in ScenarioStep::ALL {
        for substep in 0..vehicle_data_budget(step) {
            let payload: Value = serde_json::from_str(&vehicle_data_body(
                step,
                substep,
                1_800_000_000_000,
                offset_miles,
            ))
            .expect("Owner API payload");
            let expected = owner_odometer_miles(step, substep, offset_miles);
            assert_eq!(
                payload["response"]["drive_state"]["odometer"], expected,
                "drive_state odometer must match the stream endpoint for {step:?}/{substep}"
            );
            assert_eq!(
                payload["response"]["vehicle_state"]["odometer"], expected,
                "vehicle_state odometer must match the stream endpoint for {step:?}/{substep}"
            );
        }
    }
}

#[test]
fn parked_stream_odometer_is_monotonic_before_its_owner_close_with_offset() {
    let offset_miles = 10.0;
    let last_drive_owner = owner_odometer_miles(ScenarioStep::DrivePositions, 3, offset_miles);
    let parked_owner = owner_odometer_miles(ScenarioStep::Parked, 0, offset_miles);
    let samples = (0..8)
        .map(|index| stream_odometer_miles(last_drive_owner, parked_owner, index, 8))
        .collect::<Vec<_>>();

    assert_eq!(last_drive_owner, 12_355.3);
    assert_eq!(parked_owner, 12_355.4);
    assert!(
        samples
            .iter()
            .all(|sample| *sample > last_drive_owner && *sample < parked_owner),
        "every Parked stream sample must remain inside the Owner closing interval"
    );
    assert!(
        samples.windows(2).all(|pair| pair[0] < pair[1]),
        "Parked stream must never regress before the Owner closing sample"
    );

    let unoffset_samples = (0..8)
        .map(|index| {
            stream_odometer_miles(
                owner_odometer_miles(ScenarioStep::DrivePositions, 3, 0.0),
                owner_odometer_miles(ScenarioStep::Parked, 0, 0.0),
                index,
                8,
            )
        })
        .collect::<Vec<_>>();
    assert!(
        samples
            .iter()
            .zip(unoffset_samples)
            .all(|(offset, unoffset)| (*offset - unoffset - offset_miles).abs() < 1e-9),
        "the configured odometer offset must be preserved for every stream sample"
    );
}

#[test]
fn stream_telemetry_interpolates_strictly_between_owner_samples() {
    let base = 1_800_000_000_000_i64;
    for (last, step, substep) in [
        (None, ScenarioStep::OnlineIdle, 0),
        (
            Some(vehicle_data_timestamp_ms(ScenarioStep::OnlineIdle, 0, base)),
            ScenarioStep::DrivePositions,
            0,
        ),
        (
            Some(vehicle_data_timestamp_ms(
                ScenarioStep::DrivePositions,
                0,
                base,
            )),
            ScenarioStep::DrivePositions,
            1,
        ),
        (
            Some(vehicle_data_timestamp_ms(ScenarioStep::Parked, 0, base)),
            ScenarioStep::ChargeSamples,
            0,
        ),
        (
            Some(vehicle_data_timestamp_ms(
                ScenarioStep::ChargeSamples,
                0,
                base,
            )),
            ScenarioStep::ChargeSamples,
            1,
        ),
    ] {
        let upper = vehicle_data_timestamp_ms(step, substep, base);
        let lower = last.unwrap_or(base);
        let timestamps = stream_telemetry_timestamps(last, upper, base);
        assert_eq!(timestamps.len(), 8);
        assert!(
            timestamps
                .iter()
                .all(|timestamp| { *timestamp > lower && *timestamp < upper })
        );
        assert!(timestamps.windows(2).all(|pair| pair[0] < pair[1]));
    }
    let unchanged = vehicle_data_timestamp_ms(ScenarioStep::UnchangedNoOp, 0, base);
    assert!(
        stream_telemetry_timestamps(Some(unchanged), unchanged, base).is_empty(),
        "there is no valid stream interval inside an exact no-op"
    );
}

#[tokio::test]
async fn stream_ledger_requires_exact_fixture_subscription_and_records_shutdown() {
    let evidence = tempfile::tempdir().expect("evidence");
    let source = FakeTeslaSource::spawn(AdvanceMode::Manual, Some(evidence.path()))
        .await
        .expect("spawn");
    let (mut socket, _) = connect_async(source.stream_endpoint())
        .await
        .expect("connect");
    let subscribe = serde_json::json!({
        "msg_type": "data:subscribe_oauth",
        "token": "not-retained",
        "value": crate::tesla_stream::STREAM_COLUMNS.join(","),
        "tag": FIXTURE_VID_TAG,
    });
    socket
        .send(Message::Text(subscribe.to_string().into()))
        .await
        .expect("subscribe");
    let hello = socket.next().await.expect("hello").expect("frame");
    assert!(matches!(hello, Message::Text(text) if text.contains("control:hello")));
    socket
        .send(Message::Text(
            serde_json::json!({"msg_type": "data:unsubscribe", "tag": FIXTURE_VID_TAG})
                .to_string()
                .into(),
        ))
        .await
        .expect("unsubscribe");
    socket.close(None).await.expect("close");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if source
                .audited_stream_events()
                .iter()
                .any(|event| event.event == StreamAuditEvent::Unsubscribe)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("unsubscribe recorded");
    let events = source.audited_stream_events();
    let expected_fields = crate::tesla_stream::STREAM_COLUMNS.join(",");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event == StreamAuditEvent::Connect)
            .count(),
        1
    );
    assert!(events.iter().any(|event| {
        event.event == StreamAuditEvent::Subscribe
            && event.accepted
            && event.tag.as_deref() == Some(FIXTURE_VID_TAG)
            && event.fields.as_deref() == Some(expected_fields.as_str())
    }));
    assert!(
        events
            .iter()
            .any(|event| event.event == StreamAuditEvent::Unsubscribe)
    );
    assert!(evidence.path().join("stream-ledger.json").is_file());
    source.shutdown().await;
}

#[tokio::test]
async fn stream_rejects_wrong_tag_without_retaining_token() {
    let evidence = tempfile::tempdir().expect("evidence");
    let source = FakeTeslaSource::spawn(AdvanceMode::Manual, Some(evidence.path()))
        .await
        .expect("spawn");
    let (mut socket, _) = connect_async(source.stream_endpoint())
        .await
        .expect("connect");
    socket
        .send(Message::Text(
            serde_json::json!({
                "msg_type": "data:subscribe_oauth",
                "token": "must-not-appear-in-ledger",
                "value": crate::tesla_stream::STREAM_COLUMNS.join(","),
                "tag": "wrong-fixture",
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("send malformed subscribe");
    let response = socket.next().await.expect("close response").expect("frame");
    assert!(matches!(response, Message::Close(_)));
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if source
                .audited_stream_events()
                .iter()
                .any(|event| event.event == StreamAuditEvent::Rejected)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("rejection recorded");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if source.stream_session_stats().active_sessions == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("rejected session reaped");
    let serialized =
        std::fs::read_to_string(evidence.path().join("stream-ledger.json")).expect("ledger");
    assert!(!serialized.contains("must-not-appear-in-ledger"));
    assert!(
        source
            .audited_stream_events()
            .iter()
            .any(|event| event.event == StreamAuditEvent::Rejected && !event.accepted)
    );
    assert_eq!(
        source.stream_session_stats(),
        StreamSessionStats {
            connection_attempts: 1,
            accepted_connections: 1,
            rejected_connections: 0,
            active_sessions: 0,
            max_concurrent_sessions: 1,
        },
        "a rejected protocol frame must not relabel an accepted handshake"
    );
    source.shutdown().await;
}

#[tokio::test]
async fn completed_handshake_classification_rechecks_an_outage_before_admission() {
    let source = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("spawn");
    source.set_stream_available(false);
    assert!(
        !classify_completed_stream_handshake(&source.state, 99),
        "an outage between wire handshake and classification must not admit a session"
    );
    assert_eq!(
        source.stream_session_stats(),
        StreamSessionStats {
            connection_attempts: 1,
            accepted_connections: 0,
            rejected_connections: 1,
            active_sessions: 0,
            max_concurrent_sessions: 0,
        }
    );
    assert_eq!(
        source
            .audited_stream_events()
            .iter()
            .map(|event| event.event)
            .collect::<Vec<_>>(),
        vec![StreamAuditEvent::Connect, StreamAuditEvent::Rejected]
    );
    source.shutdown().await;
}

#[tokio::test]
async fn vehicle_data_fault_is_a_logged_503_without_muting_discovery_or_safety_routes() {
    let source = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("spawn");
    source.set_step(ScenarioStep::OnlineIdle);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("loopback client");
    let products = format!("{}api/1/products", source.http_base_url());
    let vehicle_data = format!(
        "{}api/1/vehicles/{FIXTURE_EID}/vehicle_data",
        source.http_base_url()
    );

    source.set_owner_vehicle_data_available(false);
    assert!(!source.owner_vehicle_data_available());
    assert_eq!(
        client
            .get(&products)
            .send()
            .await
            .expect("products")
            .status(),
        reqwest::StatusCode::OK,
        "discovery remains available during a vehicle_data-only fault"
    );
    assert_eq!(
        client
            .get(&vehicle_data)
            .send()
            .await
            .expect("vehicle_data fault")
            .status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    for path in [
        "/redirect-capture",
        &format!("/api/1/vehicles/{FIXTURE_EID}/wake_up"),
        &format!("/api/1/vehicles/{FIXTURE_EID}/command/auto_conditioning_start"),
    ] {
        let response = client
            .post(format!(
                "{}{}",
                source.http_base_url(),
                path.trim_start_matches('/')
            ))
            .send()
            .await
            .expect("safety route");
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    }
    let ledger = source.audited_requests();
    let fault = ledger
        .iter()
        .find(|request| request.path.ends_with("/vehicle_data"))
        .expect("fault entry");
    assert!(
        !fault.rejected,
        "503 is a controlled service fault, not unsafe input"
    );
    assert_eq!(
        fault.reject_reason.as_deref(),
        Some("owner_vehicle_data_unavailable")
    );
    assert_eq!(
        fault.response_status,
        StatusCode::SERVICE_UNAVAILABLE.as_u16()
    );
    assert_eq!(
        source.rejected_count(),
        3,
        "only fallback safety routes reject"
    );

    source.set_owner_vehicle_data_available(true);
    assert_eq!(
        client
            .get(&vehicle_data)
            .send()
            .await
            .expect("vehicle_data restored")
            .status(),
        reqwest::StatusCode::OK
    );
    source.shutdown().await;
}

#[tokio::test]
async fn stream_fault_closes_sessions_rejects_handshakes_and_recovers_without_overlap() {
    let source = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("spawn");
    let subscribe = || {
        Message::Text(
            serde_json::json!({
                "msg_type": "data:subscribe_oauth",
                "token": "not-retained",
                "value": crate::tesla_stream::STREAM_COLUMNS.join(","),
                "tag": FIXTURE_VID_TAG,
            })
            .to_string()
            .into(),
        )
    };

    let (mut first, _) = connect_async(source.stream_endpoint())
        .await
        .expect("first connect");
    first.send(subscribe()).await.expect("first subscribe");
    assert!(matches!(
        first.next().await.expect("first hello").expect("first frame"),
        Message::Text(text) if text.contains("control:hello")
    ));
    assert_eq!(source.stream_session_stats().active_sessions, 1);

    source.set_stream_available(false);
    assert!(!source.stream_available());
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match first.next().await {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .expect("outage closes first session");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if source.stream_session_stats().active_sessions == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("first session reaped");

    assert!(
        connect_async(source.stream_endpoint()).await.is_err(),
        "outage rejects a new WebSocket handshake"
    );
    source.set_stream_available(true);
    let (mut recovered, _) = connect_async(source.stream_endpoint())
        .await
        .expect("recovered connect");
    recovered
        .send(subscribe())
        .await
        .expect("recovered subscribe");
    assert!(matches!(
        recovered
            .next()
            .await
            .expect("recovered hello")
            .expect("recovered frame"),
        Message::Text(text) if text.contains("control:hello")
    ));
    recovered
        .send(Message::Text(
            serde_json::json!({"msg_type": "data:unsubscribe", "tag": FIXTURE_VID_TAG})
                .to_string()
                .into(),
        ))
        .await
        .expect("recovered unsubscribe");

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if source.stream_session_stats().active_sessions == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("recovered session reaped");
    let stats = source.stream_session_stats();
    assert_eq!(stats.connection_attempts, 3);
    assert_eq!(stats.accepted_connections, 2);
    assert_eq!(stats.rejected_connections, 1);
    assert_eq!(stats.max_concurrent_sessions, 1);
    let events = source.audited_stream_events();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event == StreamAuditEvent::Connect)
            .map(|event| event.accepted)
            .collect::<Vec<_>>(),
        vec![true, false, true]
    );
    assert!(events.iter().any(|event| {
        event.event == StreamAuditEvent::Disconnect
            && event.reject_reason.as_deref() == Some("stream_unavailable")
            && event.active_session_count == 0
    }));
    assert!(
        events
            .iter()
            .all(|event| event.max_concurrent_session_count == 1)
    );
    source.shutdown().await;
}

#[tokio::test]
async fn concurrent_stream_sessions_keep_evidence_ordered_and_snapshots_exact() {
    async fn subscribe_then_unsubscribe(
        endpoint: String,
        both_subscribed: std::sync::Arc<tokio::sync::Barrier>,
    ) {
        let (mut socket, _) = connect_async(endpoint).await.expect("connect");
        socket
            .send(Message::Text(
                serde_json::json!({
                    "msg_type": "data:subscribe_oauth",
                    "token": "not-retained",
                    "value": crate::tesla_stream::STREAM_COLUMNS.join(","),
                    "tag": FIXTURE_VID_TAG,
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("subscribe");
        assert!(matches!(
            socket.next().await.expect("hello").expect("frame"),
            Message::Text(text) if text.contains("control:hello")
        ));
        both_subscribed.wait().await;
        socket
            .send(Message::Text(
                serde_json::json!({"msg_type": "data:unsubscribe", "tag": FIXTURE_VID_TAG})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("unsubscribe");
    }

    let evidence = tempfile::tempdir().expect("evidence");
    let source = FakeTeslaSource::spawn(AdvanceMode::Manual, Some(evidence.path()))
        .await
        .expect("spawn");
    let both_subscribed = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let ((), ()) = tokio::join!(
        subscribe_then_unsubscribe(source.stream_endpoint().to_owned(), both_subscribed.clone()),
        subscribe_then_unsubscribe(source.stream_endpoint().to_owned(), both_subscribed.clone()),
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if source.stream_session_stats().active_sessions == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("both sessions reaped");
    source.write_ledger_snapshot().expect("manual snapshot");

    let stats = source.stream_session_stats();
    assert_eq!(
        stats,
        StreamSessionStats {
            connection_attempts: 2,
            accepted_connections: 2,
            rejected_connections: 0,
            active_sessions: 0,
            max_concurrent_sessions: 2,
        }
    );
    assert_eq!(
        stats.connection_attempts,
        stats.accepted_connections + stats.rejected_connections
    );
    let events = source.audited_stream_events();
    let jsonl =
        std::fs::read_to_string(evidence.path().join("stream-ledger.jsonl")).expect("stream JSONL");
    let jsonl_events = jsonl
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("complete JSONL event"))
        .collect::<Vec<_>>();
    let expected_events = events
        .iter()
        .map(|event| serde_json::to_value(event).expect("event JSON"))
        .collect::<Vec<_>>();
    assert_eq!(
        jsonl_events, expected_events,
        "no reordered or torn JSONL events"
    );

    let snapshot: Value = serde_json::from_slice(
        &std::fs::read(evidence.path().join("stream-ledger.json")).expect("stream snapshot"),
    )
    .expect("valid stream snapshot JSON");
    assert_eq!(snapshot["events"], serde_json::to_value(events).unwrap());
    assert_eq!(
        snapshot["streamSessionStats"],
        serde_json::to_value(stats).unwrap()
    );
    source.shutdown().await;
}

#[tokio::test]
async fn source_shutdown_joins_active_stream_sessions_before_final_ledger() {
    let evidence = tempfile::tempdir().expect("evidence");
    let source = FakeTeslaSource::spawn(AdvanceMode::Manual, Some(evidence.path()))
        .await
        .expect("spawn");
    let (mut socket, _) = connect_async(source.stream_endpoint())
        .await
        .expect("connect");
    socket
        .send(Message::Text(
            serde_json::json!({
                "msg_type": "data:subscribe_oauth",
                "token": "not-retained",
                "value": crate::tesla_stream::STREAM_COLUMNS.join(","),
                "tag": FIXTURE_VID_TAG,
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("subscribe");
    assert!(matches!(
        socket.next().await.expect("hello").expect("frame"),
        Message::Text(text) if text.contains("control:hello")
    ));

    tokio::time::timeout(Duration::from_secs(1), source.shutdown())
        .await
        .expect("source joins listener and session tasks");
    let ledger: Value = serde_json::from_slice(
        &std::fs::read(evidence.path().join("stream-ledger.json")).expect("final ledger"),
    )
    .expect("ledger json");
    assert_eq!(ledger["streamSessionStats"]["active_sessions"], 0);
    assert_eq!(ledger["streamSessionStats"]["connection_attempts"], 1);
    assert_eq!(ledger["streamSessionStats"]["accepted_connections"], 1);
    assert!(
        ledger["events"]
            .as_array()
            .expect("events")
            .iter()
            .any(|event| {
                event["event"] == "disconnect"
                    && event["reject_reason"] == "stream_unavailable"
                    && event["active_session_count"] == 0
            })
    );
}

// ---- C4 P1: the fake follows the Hub's column set and offers scripted answers --------

/// A subscribe frame for the fixture vehicle with this `value`.
fn subscribe_text(value: &str) -> String {
    serde_json::json!({
        "msg_type": "data:subscribe_oauth",
        "token": "synthetic-token",
        "value": value,
        "tag": FIXTURE_VID_TAG,
    })
    .to_string()
}

#[test]
fn sc66_the_fake_accepts_only_the_hub_column_list_in_its_order() {
    let columns = crate::tesla_stream::STREAM_COLUMNS;
    let accepted = exact_subscribe_fields(&subscribe_text(&columns.join(",")));
    assert_eq!(
        accepted,
        Ok((FIXTURE_VID_TAG.to_owned(), columns.join(",")))
    );
    // The same names in any other order are refused.
    let mut reversed = columns;
    reversed.reverse();
    assert_eq!(
        exact_subscribe_fields(&subscribe_text(&reversed.join(","))),
        Err("unexpected_subscribe_fields")
    );
    // So is the 12-name list that includes `est_heading`.
    let mut with_heading = columns.to_vec();
    with_heading.push("est_heading");
    assert_eq!(
        exact_subscribe_fields(&subscribe_text(&with_heading.join(","))),
        Err("unexpected_subscribe_fields")
    );
    // And a list with one name missing.
    assert_eq!(
        exact_subscribe_fields(&subscribe_text(&columns[1..].join(","))),
        Err("unexpected_subscribe_fields")
    );
}

#[test]
fn the_fake_value_has_the_time_then_eleven_values_in_column_order() {
    let value = stream_update_value(1_700_000_000_123, 20, 12_345.5, 64, 47.5, 19.05, -11, "P");
    let elements: Vec<&str> = value.split(',').collect();
    assert_eq!(elements.len(), 12);
    assert_eq!(elements[0], "1700000000123");
    // power is element 7 (column 6) and speed the last element.
    assert_eq!(elements[7], "-11");
    assert_eq!(elements[9], "P");
    assert_eq!(elements[11], "20");
    let update = crate::tesla_stream::parse_data_update(&format!(
        r#"{{"msg_type":"data:update","tag":"9","value":"{value}"}}"#
    ))
    .expect("the Hub parser accepts the fake's value");
    assert_eq!(update.timestamp_ms, 1_700_000_000_123);
    assert_eq!(update.est_lat, Some(47.5));
    assert_eq!(update.est_lng, Some(19.05));
    assert_eq!(update.odometer, Some(12_345.5));
    assert_eq!(update.power, Some(-11));
    assert_eq!(update.shift_state.as_deref(), Some("P"));
    assert_eq!(update.soc, Some(64.0));
    assert_eq!(update.speed, Some(20.0));
    assert_eq!(update.est_heading, None);
    assert!(update.elevation.is_some() && update.heading.is_some());
    assert!(update.range.is_some() && update.est_range.is_some());
}

/// Connects to the fake's stream, subscribes with the Hub's own frame and returns
/// the socket.
async fn subscribed_socket(
    source: &FakeTeslaSource,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let (mut socket, _) = connect_async(source.stream_endpoint())
        .await
        .expect("connect");
    let frame = crate::tesla_stream::subscribe_frame(FIXTURE_VID_TAG, "synthetic-token")
        .expect("subscribe frame");
    socket
        .send(Message::Text(frame.into()))
        .await
        .expect("subscribe");
    socket
}

async fn next_text(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("a message in time")
        .expect("not closed")
        .expect("a frame");
    let Message::Text(text) = message else {
        panic!("not text: {message:?}");
    };
    serde_json::from_str(&text).expect("JSON")
}

#[tokio::test]
async fn the_normal_script_sends_a_hello_then_frames_the_hub_parser_accepts() {
    let source = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("fake Tesla");
    // The scenario places stream frames after the last Owner API sample; give it one
    // a minute before its start so the first connection has an interval to fill.
    {
        let mut inner = source.state.inner.lock().expect("fake lock");
        inner.last_owner_data_ts_ms = Some(inner.base_ts_ms - 60_000);
    }
    let mut socket = subscribed_socket(&source).await;
    let hello = next_text(&mut socket).await;
    assert_eq!(hello["msg_type"], "control:hello");
    assert_eq!(hello["connection_timeout"], 0);
    // Every data frame parses with the Hub's rules and carries its own sample time.
    let mut seen = 0;
    while let Ok(Some(Ok(Message::Text(text)))) =
        tokio::time::timeout(Duration::from_millis(300), socket.next()).await
    {
        let envelope: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(envelope["msg_type"], "data:update");
        let update = crate::tesla_stream::parse_data_update(&text).expect("a valid frame");
        assert_eq!(update.tag, FIXTURE_VID.to_string());
        assert_eq!(Some(update.timestamp_ms), envelope["timestamp"].as_i64());
        assert!(update.power.is_some());
        seen += 1;
    }
    assert!(seen > 0, "the scenario sent no frame");
    drop(socket);
    source.shutdown().await;
}

#[tokio::test]
async fn scripted_hello_and_error_frames() {
    let source = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("fake Tesla");

    source.set_stream_script(StreamScript::Hello {
        code: Some(401),
        connection_timeout: Some(30_000),
    });
    let mut socket = subscribed_socket(&source).await;
    let hello = next_text(&mut socket).await;
    assert_eq!(hello["code"], 401);
    assert_eq!(hello["connection_timeout"], 30_000);
    drop(socket);

    source.set_stream_script(StreamScript::Hello {
        code: None,
        connection_timeout: None,
    });
    let mut socket = subscribed_socket(&source).await;
    let hello = next_text(&mut socket).await;
    assert_eq!(hello, serde_json::json!({"msg_type": "control:hello"}));
    drop(socket);

    source.set_stream_script(StreamScript::ErrorFrame {
        error_type: "vehicle_test_a".to_owned(),
        value: "token invalid".to_owned(),
    });
    let mut socket = subscribed_socket(&source).await;
    assert_eq!(next_text(&mut socket).await["msg_type"], "control:hello");
    let error = next_text(&mut socket).await;
    assert_eq!(error["msg_type"], "data:error");
    assert_eq!(error["error_type"], "vehicle_test_a");
    assert_eq!(error["value"], "token invalid");
    assert_eq!(error["tag"], FIXTURE_VID_TAG);
    // No telemetry follows an error script.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), socket.next())
            .await
            .is_err()
    );
    drop(socket);
    source.shutdown().await;
}

#[tokio::test]
async fn scripted_close_and_silence() {
    let source = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("fake Tesla");

    source.set_stream_script(StreamScript::CloseAfterSubscribe);
    let mut socket = subscribed_socket(&source).await;
    let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .expect("a close in time");
    assert!(matches!(
        message,
        Some(Ok(Message::Close(_))) | None | Some(Err(_))
    ));
    drop(socket);

    // A silent session sends nothing, not even a hello, and never answers a Ping.
    source.set_stream_script(StreamScript::Silent);
    let mut socket = subscribed_socket(&source).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), socket.next())
            .await
            .is_err(),
        "a silent session sent a message"
    );
    drop(socket);
    source.shutdown().await;
}

#[tokio::test]
async fn scripted_upgrade_answers_carry_status_and_retry_after() {
    let source = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("fake Tesla");
    for (status, retry_after) in [
        (401_u16, None),
        (403, None),
        (404, None),
        (503, Some("600")),
        (503, Some("Wed, 21 Oct 2026 07:28:00 GMT")),
    ] {
        source.set_stream_script(StreamScript::UpgradeStatus {
            status,
            retry_after: retry_after.map(str::to_owned),
        });
        let error = connect_async(source.stream_endpoint())
            .await
            .expect_err("the upgrade is refused");
        let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
            panic!("not an HTTP answer: {error:?}");
        };
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok()),
            retry_after
        );
    }
    // The canonical script connects again.
    source.set_stream_script(StreamScript::Normal);
    let mut socket = subscribed_socket(&source).await;
    assert_eq!(next_text(&mut socket).await["msg_type"], "control:hello");
    drop(socket);
    source.shutdown().await;
}
