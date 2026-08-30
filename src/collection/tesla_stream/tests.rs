// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use serde_json::Value;
use tokio::{net::TcpListener, time::timeout};
use tokio_tungstenite::{
    accept_async,
    tungstenite::{
        Message,
        protocol::frame::{
            Frame,
            coding::{Data, OpCode},
        },
    },
};

fn legacy_supervisor(
    vehicle_id: u64,
    stream_vehicle_id: u64,
    endpoint: String,
    events: mpsc::Sender<StreamEvent>,
) -> Result<TeslaStreamSupervisor, StreamSupervisorError> {
    crate::crypto::install_default_provider();
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        Url::parse("http://127.0.0.1:9/").unwrap(),
        "test-access",
        "test-refresh",
    )
    .with_test_schedule(2_000_000_000, 1_900_000_000);
    let manager = LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
    let client = reqwest::Client::builder().build().unwrap();
    TeslaStreamSupervisor::new_legacy_auth_for_test(
        VehicleId::from_test(vehicle_id),
        StreamVehicleId::from_test(stream_vehicle_id),
        Arc::new(Mutex::new(manager)),
        StreamRegion::Global,
        endpoint,
        client,
        events,
    )
}

fn production_legacy_supervisor(
    endpoint: String,
    events: mpsc::Sender<StreamEvent>,
) -> Result<TeslaStreamSupervisor, StreamSupervisorError> {
    crate::crypto::install_default_provider();
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        Url::parse("https://auth.tesla.com/oauth2/v3/token").unwrap(),
        "test-access",
        "test-refresh",
    )
    .with_test_schedule(2_000_000_000, 1_900_000_000);
    let manager = LegacyAuthManager::for_test(auth, Arc::new(|_, _| Ok(())));
    let client = reqwest::Client::builder().https_only(true).build().unwrap();
    TeslaStreamSupervisor::new_legacy_auth_for_test_production_policy(
        VehicleId::from_test(9),
        StreamVehicleId::from_test(9),
        Arc::new(Mutex::new(manager)),
        StreamRegion::Global,
        endpoint,
        client,
        events,
    )
}

#[test]
fn protocol_frames_match_teslamate() {
    let value: Value = serde_json::from_str(&subscribe_frame("9", "fake-token").unwrap()).unwrap();
    assert_eq!(value["msg_type"], "data:subscribe_oauth");
    assert_eq!(value["tag"], "9");
    assert_eq!(value["value"], TESLAMATE_STREAM_FIELDS.join(","));
    let unsubscribe: Value = serde_json::from_str(&unsubscribe_frame("9").unwrap()).unwrap();
    assert_eq!(unsubscribe["msg_type"], "data:unsubscribe");
    assert!(unsubscribe.get("token").is_none());
}

#[test]
fn plaintext_endpoints_require_literal_loopback_ips() {
    for endpoint in ["ws://127.0.0.1:9000/", "ws://[::1]:9000/"] {
        assert!(
            validate_test_endpoint_override(endpoint).is_ok(),
            "{endpoint}"
        );
        assert!(validate_endpoint_override(endpoint).is_err(), "{endpoint}");
        let (events, _) = mpsc::channel(1);
        assert!(matches!(
            production_legacy_supervisor(endpoint.to_owned(), events),
            Err(StreamSupervisorError::InvalidEndpoint(
                StreamError::InvalidEndpoint
            ))
        ));
    }
    for endpoint in [
        "ws://localhost:9000/",
        "ws://stream.example:9000/",
        "ws://192.168.1.2:9000/",
        "ws://user@127.0.0.1:9000/",
        "ws://127.0.0.1:9000/?token=secret",
    ] {
        assert!(
            validate_test_endpoint_override(endpoint).is_err(),
            "{endpoint}"
        );
    }
    assert!(validate_endpoint_override("wss://streaming.vn.teslamotors.com/streaming/").is_ok());
}

#[tokio::test]
async fn saturated_event_queue_backpressures_until_drained_or_shutdown() {
    let (events, mut receiver) = mpsc::channel(1);
    events.try_send(StreamEvent::Healthy).expect("fill queue");
    let supervisor =
        legacy_supervisor(9, 9, "ws://127.0.0.1:9/".to_owned(), events).expect("supervisor");

    let (_stop, mut shutdown) = oneshot::channel();
    let delivery = supervisor.emit_event(StreamEvent::TransportUnavailable, &mut shutdown);
    tokio::pin!(delivery);
    assert!(
        timeout(Duration::from_millis(20), &mut delivery)
            .await
            .is_err(),
        "a full bounded queue must backpressure instead of failing"
    );
    assert!(matches!(receiver.recv().await, Some(StreamEvent::Healthy)));
    assert!(matches!(
        timeout(Duration::from_secs(1), &mut delivery)
            .await
            .expect("delivery after receiver drain"),
        Ok(EventDelivery::Delivered)
    ));
    assert!(matches!(
        receiver.recv().await,
        Some(StreamEvent::TransportUnavailable)
    ));

    let (events, _receiver) = mpsc::channel(1);
    events.try_send(StreamEvent::Healthy).expect("fill queue");
    let supervisor =
        legacy_supervisor(9, 9, "ws://127.0.0.1:9/".to_owned(), events).expect("supervisor");
    let (stop, mut shutdown) = oneshot::channel();
    stop.send(()).expect("shutdown");
    assert!(matches!(
        supervisor
            .emit_event(StreamEvent::TransportUnavailable, &mut shutdown)
            .await,
        Ok(EventDelivery::Shutdown)
    ));
}

#[tokio::test]
async fn supervisor_exits_when_shutdown_hits_a_saturated_event_queue() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        let _ = timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"msg_type":"control:hello","code":200}"#.into(),
            ))
            .await
            .unwrap();
        socket.send(Message::Text(r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#.into())).await.unwrap();
        let _ = timeout(Duration::from_secs(1), socket.next()).await;
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "shutdown must not reconnect"
        );
    });
    let (events, _receiver) = mpsc::channel(1);
    let gate = Arc::new(StreamPowerGate::default());
    let supervisor = legacy_supervisor(9, 9, endpoint, events)
        .unwrap()
        .with_power_gate(Arc::clone(&gate))
        .with_policy(SupervisorPolicy {
            connect_timeout: Duration::from_millis(100),
            silence_timeout: Duration::from_secs(1),
            backoff_initial: Duration::from_millis(5),
            remote_backoff_cap: Duration::from_millis(10),
            connect_backoff_cap: Duration::from_millis(10),
        });
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    timeout(Duration::from_secs(1), async {
        while !gate.is_confirmed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop.send(()).unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!gate.is_confirmed(), "supervisor exit revokes power gate");
    timeout(Duration::from_secs(2), server)
        .await
        .unwrap()
        .unwrap();
}

#[test]
fn websocket_limits_and_jitter_are_bounded() {
    let config = stream_socket_config();
    assert_eq!(config.max_frame_size, Some(STREAM_MAX_FRAME_BYTES));
    assert_eq!(config.max_message_size, Some(STREAM_MAX_MESSAGE_BYTES));
    for _ in 0..8 {
        let delay = equal_jitter(Duration::from_millis(100));
        assert!((Duration::from_millis(50)..=Duration::from_millis(100)).contains(&delay));
    }
}

#[test]
fn only_valid_protocol_health_resets_backoff() {
    let initial = Duration::from_millis(5);
    let mut remote = Backoff::new(initial, Duration::from_millis(20));
    let mut connect = Backoff::new(initial, Duration::from_millis(20));
    let _ = remote.next();
    let _ = connect.next();
    assert_eq!(remote.current, Duration::from_millis(10));
    assert_eq!(connect.current, Duration::from_millis(10));

    assert!(!apply_health_backoff_reset(None, &mut remote, &mut connect));
    for decoded in [
        decode_message("9", Message::Text("not-json".into())),
        decode_message("9", Message::Ping(Vec::new().into())),
    ] {
        assert!(decoded.is_none());
        assert!(!apply_health_backoff_reset(
            decoded.as_ref(),
            &mut remote,
            &mut connect
        ));
    }
    let telemetry = StreamEvent::Telemetry {
        update: Box::new(
            parse_data_update(r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#)
                .unwrap(),
        ),
        queued_at: Instant::now(),
    };
    for event in [
        StreamEvent::Healthy,
        StreamEvent::VehicleOffline,
        StreamEvent::AuthRejected,
        StreamEvent::TransportUnavailable,
        StreamEvent::ProtocolViolation,
    ] {
        assert!(!apply_health_backoff_reset(
            Some(&event),
            &mut remote,
            &mut connect
        ));
    }
    assert_eq!(remote.current, Duration::from_millis(10));
    assert_eq!(connect.current, Duration::from_millis(10));
    assert!(apply_health_backoff_reset(
        Some(&telemetry),
        &mut remote,
        &mut connect
    ));
    assert_eq!(remote.current, initial);
    assert_eq!(connect.current, initial);

    let error = validate_endpoint_override("ws://127.0.0.1/?access-secret=1")
        .expect_err("production plaintext endpoint rejected");
    assert!(!format!("{error} {error:?}").contains("access-secret"));
}

#[tokio::test]
async fn loopback_socket_closes_1009_after_an_oversized_single_frame() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        let _subscribe = timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"msg_type":"control:hello","code":200}"#.into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Frame(Frame::message(
                vec![b'x'; STREAM_MAX_FRAME_BYTES + 1],
                OpCode::Data(Data::Text),
                true,
            )))
            .await
            .unwrap();
        let close = timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("client must close the oversized-frame connection")
            .expect("client close frame must be valid");
        assert!(matches!(
            close,
            Ok(Message::Close(Some(frame))) if frame.code == CloseCode::Size
        ));
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "an oversize violation must not reconnect"
        );
    });
    let (events, mut received) = mpsc::channel(8);
    let supervisor = legacy_supervisor(9, 9, endpoint, events)
        .unwrap()
        .with_audit_store(store.clone());
    let (_stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    assert_eq!(
        timeout(Duration::from_secs(1), received.recv())
            .await
            .unwrap(),
        Some(StreamEvent::ProtocolViolation)
    );
    assert!(matches!(
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap(),
        Err(StreamSupervisorError::ProtocolViolation)
    ));
    while let Ok(event) = received.try_recv() {
        assert!(!matches!(event, StreamEvent::Telemetry { .. }));
    }
    timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
    let session_outcome: String = store
        .open()
        .expect("receipt catalogue")
        .query_row(
            "SELECT outcome FROM stream_session_receipts ORDER BY id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("failed stream session receipt");
    assert_eq!(session_outcome, "failed");
}

#[tokio::test]
async fn loopback_socket_closes_1009_after_an_oversized_fragmented_message() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        let _subscribe = timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"msg_type":"control:hello","code":200}"#.into(),
            ))
            .await
            .unwrap();
        let fragment = vec![b'x'; STREAM_MAX_FRAME_BYTES - 1];
        socket
            .send(Message::Frame(Frame::message(
                fragment.clone(),
                OpCode::Data(Data::Text),
                false,
            )))
            .await
            .unwrap();
        socket
            .send(Message::Frame(Frame::message(
                fragment.clone(),
                OpCode::Data(Data::Continue),
                false,
            )))
            .await
            .unwrap();
        socket
            .send(Message::Frame(Frame::message(
                fragment.clone(),
                OpCode::Data(Data::Continue),
                false,
            )))
            .await
            .unwrap();
        socket
            .send(Message::Frame(Frame::message(
                fragment.clone(),
                OpCode::Data(Data::Continue),
                false,
            )))
            .await
            .unwrap();
        socket
            .send(Message::Frame(Frame::message(
                fragment,
                OpCode::Data(Data::Continue),
                true,
            )))
            .await
            .unwrap();
        let close = timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("client must close the oversized-message connection")
            .expect("client close frame must be valid");
        assert!(matches!(
            close,
            Ok(Message::Close(Some(frame))) if frame.code == CloseCode::Size
        ));
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "an oversize violation must not reconnect"
        );
    });
    let (events, mut received) = mpsc::channel(8);
    let supervisor = legacy_supervisor(9, 9, endpoint, events).unwrap();
    let (_stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    assert_eq!(
        timeout(Duration::from_secs(1), received.recv())
            .await
            .unwrap(),
        Some(StreamEvent::ProtocolViolation)
    );
    assert!(matches!(
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap(),
        Err(StreamSupervisorError::ProtocolViolation)
    ));
    while let Ok(event) = received.try_recv() {
        assert!(!matches!(event, StreamEvent::Telemetry { .. }));
    }
    timeout(Duration::from_secs(1), server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn local_mock_receives_subscribe_and_unsubscribe() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(tcp).await.unwrap();
        let first = ws.next().await.unwrap().unwrap();
        let Message::Text(first) = first else {
            panic!("stream subscribe must be text")
        };
        let subscribe: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(subscribe["msg_type"], "data:subscribe_oauth");
        assert_eq!(subscribe["tag"], "42");
        let _ = ws
            .send(Message::Text(
                r#"{"msg_type":"control:hello","connection_timeout":0}"#.into(),
            ))
            .await;
        ws.send(Message::Text(
            r#"{"msg_type":"data:update","tag":"42","value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#.into(),
        ))
        .await
        .unwrap();
        let second = ws.next().await.unwrap().unwrap();
        assert!(matches!(second,Message::Text(ref text) if text.contains("data:unsubscribe")));
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "normal zero-timeout hello must not reconnect"
        );
    });
    let (events, mut received) = mpsc::channel(4);
    let supervisor = legacy_supervisor(9, 42, endpoint, events)
        .unwrap()
        .with_audit_store(store.clone());
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    assert!(matches!(
        timeout(Duration::from_secs(1), received.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Telemetry { .. })
    ));
    stop.send(()).unwrap();
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();

    let connection = store.open().expect("receipt catalogue");
    let receipts: Vec<(String, String)> = connection
        .prepare(
            "SELECT operation, outcome
               FROM outbound_request_receipts
              ORDER BY id",
        )
        .expect("outbound receipt query")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("outbound receipt rows")
        .collect::<Result<_, _>>()
        .expect("outbound receipt collection");
    assert_eq!(
        receipts,
        vec![
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "success".to_owned()),
            ("stream_unsubscribe".to_owned(), "success".to_owned()),
        ]
    );
    let session: (String, Option<i64>) = connection
        .query_row(
            "SELECT outcome, unsubscribe_receipt_id
               FROM stream_session_receipts
              ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("stream session receipt");
    assert_eq!(session.0, "orderly_shutdown");
    assert_eq!(session.1, Some(3));
}

#[tokio::test]
async fn aborted_stream_run_leaves_started_session_evidence() {
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    let (events, _received) = mpsc::channel(4);
    let supervisor = legacy_supervisor(9, 9, "ws://127.0.0.1:9/streaming/".to_owned(), events)
        .unwrap()
        .with_audit_store(store.clone())
        .with_policy(SupervisorPolicy {
            connect_timeout: Duration::from_millis(100),
            silence_timeout: Duration::from_secs(1),
            backoff_initial: Duration::from_millis(5),
            remote_backoff_cap: Duration::from_millis(10),
            connect_backoff_cap: Duration::from_millis(10),
        });
    let (_stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    timeout(Duration::from_secs(1), async {
        loop {
            let started: i64 = store
                .open()
                .expect("receipt catalogue")
                .query_row(
                    "SELECT COUNT(*) FROM stream_session_receipts WHERE outcome = 'started'",
                    [],
                    |row| row.get(0),
                )
                .expect("started session count");
            if started == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("stream session must be durable before abort");
    task.abort();
    assert!(task.await.expect_err("aborted stream task").is_cancelled());

    let session: (String, Option<i64>) = store
        .open()
        .expect("receipt catalogue")
        .query_row(
            "SELECT outcome, unsubscribe_receipt_id
               FROM stream_session_receipts
              ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("stream session receipt");
    assert_eq!(session, ("started".to_owned(), None));
}

#[tokio::test]
async fn tagged_binary_telemetry_marks_subscription_live() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(tcp).await.unwrap();
        let subscribe = ws.next().await.unwrap().unwrap();
        assert!(matches!(subscribe, Message::Text(ref text) if text.contains(r#""tag":"42""#)));
        ws.send(Message::Binary(
            r#"{"msg_type":"data:update","tag":"42","value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#
                .as_bytes()
                .to_vec()
                .into(),
        ))
        .await
        .unwrap();
        let unsubscribe = timeout(Duration::from_secs(1), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(unsubscribe, Message::Text(ref text) if text.contains("data:unsubscribe"))
        );
    });
    let (events, mut received) = mpsc::channel(4);
    let supervisor = legacy_supervisor(9, 42, endpoint, events).unwrap();
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));

    let event = timeout(Duration::from_secs(1), received.recv())
        .await
        .unwrap()
        .unwrap();
    let StreamEvent::Telemetry { update, .. } = event else {
        panic!("tagless binary Tesla telemetry must be delivered")
    };
    assert_eq!(update.tag, "42");
    assert_eq!(update.speed, Some(42));

    stop.send(()).unwrap();
    timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn invalidated_sensitive_guard_blocks_subscribe_after_handshake() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        let frame = timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("client closes denied stream");
        assert!(
            !matches!(frame, Some(Ok(Message::Text(text))) if text.contains("data:subscribe_oauth")),
            "invalidated admission must block the bearer subscribe frame"
        );
    });
    let auth = crate::legacy_auth::LegacyAuth::for_test(
        Url::parse("http://127.0.0.1:9/").unwrap(),
        "test-access",
        "test-refresh",
    )
    .with_test_schedule(2_000_000_000, 1_900_000_000);
    let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let manager = LegacyAuthManager::for_test_with_sensitive_access(
        auth,
        Arc::new(|_, _| Ok(())),
        Arc::new(move || {
            (checks.fetch_add(1, Ordering::AcqRel) < 4)
                .then_some(())
                .ok_or(crate::credentials::CredentialError::SensitiveAccessUnavailable)
        }),
    );
    let (events, _) = mpsc::channel(1);
    crate::crypto::install_default_provider();
    let supervisor = TeslaStreamSupervisor::new_legacy_auth_for_test(
        VehicleId::from_test(9),
        StreamVehicleId::from_test(9),
        Arc::new(Mutex::new(manager)),
        StreamRegion::Global,
        endpoint,
        Client::new(),
        events,
    )
    .unwrap();
    let (_stop, shutdown) = oneshot::channel();

    assert!(matches!(
        supervisor.run(shutdown).await,
        Err(StreamSupervisorError::CredentialAuthorityUnavailable)
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn observer_reconnects_with_access_token_without_refreshing() {
    let data = crate::private_tempdir().expect("data directory");
    let store = crate::db::HubStore::initialize(data.path()).expect("Hub store");
    crate::teslamate_credentials::replace_key(data.path(), b"test-cloak-key").expect("private key");
    let key = crate::teslamate_credentials::load_key(data.path()).expect("load private key");
    let tokens = crate::credentials::OwnerTokens::from_secret_parts(
        "observer-access".to_owned(),
        "observer-refresh".to_owned(),
    )
    .expect("observer tokens");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
            .expect("encrypt observer tokens");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(
                access,
                refresh,
                2_000_000_000,
                1_900_000_000,
            )
            .expect("schedule"),
        )
        .expect("store observer tokens");
    let fake =
        crate::fake_tesla::FakeTeslaSource::spawn_canonical(crate::fake_tesla::AdvanceMode::Manual)
            .await
            .expect("fake Tesla");
    let manager = LegacyAuthManager::from_hub_teslamate_store_observer_with_issuer(
        store,
        data.path(),
        fake.oauth_issuer_url(),
    )
    .expect("observer manager");
    crate::crypto::install_default_provider();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let (reconnected, reconnected_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut first = accept_async(tcp).await.unwrap();
        let first_subscribe = first.next().await.unwrap().unwrap();
        assert!(
            matches!(first_subscribe, Message::Text(ref text) if text.contains("observer-access"))
        );
        drop(first);

        let (tcp, _) = listener.accept().await.unwrap();
        let mut second = accept_async(tcp).await.unwrap();
        let second_subscribe = second.next().await.unwrap().unwrap();
        assert!(
            matches!(second_subscribe, Message::Text(ref text) if text.contains("observer-access"))
        );
        second
            .send(Message::Text(
                r#"{"msg_type":"control:hello","code":200}"#.into(),
            ))
            .await
            .unwrap();
        reconnected.send(()).unwrap();
        let _ = second.next().await;
    });
    let (events, _received) = mpsc::channel(4);
    let supervisor = TeslaStreamSupervisor::new_legacy_auth_for_test(
        VehicleId::from_test(9),
        StreamVehicleId::from_test(9),
        Arc::new(Mutex::new(manager)),
        StreamRegion::Global,
        endpoint,
        Client::new(),
        events,
    )
    .unwrap()
    .with_policy(SupervisorPolicy {
        connect_timeout: Duration::from_millis(100),
        silence_timeout: Duration::from_secs(1),
        backoff_initial: Duration::from_millis(5),
        remote_backoff_cap: Duration::from_millis(10),
        connect_backoff_cap: Duration::from_millis(10),
    });
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    timeout(Duration::from_secs(1), reconnected_rx)
        .await
        .unwrap()
        .unwrap();
    stop.send(()).unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
    assert_eq!(fake.token_refresh_request_count(), 0);
    fake.shutdown().await;
}

#[tokio::test]
async fn vehicle_disconnected_resubscribes_on_same_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        assert!(matches!(
            socket.next().await.unwrap().unwrap(),
            Message::Text(ref text) if text.contains("data:subscribe_oauth")
        ));
        socket
            .send(Message::Text(
                r#"{"msg_type":"control:hello","code":200}"#.into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"msg_type":"data:error","tag":"9","error_type":"vehicle_error","value":"temporary vehicle error"}"#.into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"msg_type":"data:error","tag":"9","error_type":"vehicle_disconnected"}"#.into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Message::Text(ref text) if text.contains("data:subscribe_oauth")
        ));
        socket
            .send(Message::Text(
                r#"{"msg_type":"control:hello","code":200}"#.into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#.into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Message::Text(ref text) if text.contains("data:unsubscribe")
        ));
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "vehicle_disconnected must not open a second socket"
        );
    });
    let (events, mut received) = mpsc::channel(8);
    let supervisor = legacy_supervisor(9, 9, endpoint, events)
        .unwrap()
        .with_policy(SupervisorPolicy {
            connect_timeout: Duration::from_millis(100),
            silence_timeout: Duration::from_secs(1),
            backoff_initial: Duration::from_millis(5),
            remote_backoff_cap: Duration::from_millis(10),
            connect_backoff_cap: Duration::from_millis(10),
        });
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    assert_eq!(
        timeout(Duration::from_secs(1), received.recv())
            .await
            .unwrap(),
        Some(StreamEvent::TransportUnavailable)
    );
    assert_eq!(
        timeout(Duration::from_secs(1), received.recv())
            .await
            .unwrap(),
        Some(StreamEvent::TransportUnavailable)
    );
    assert!(matches!(
        timeout(Duration::from_secs(1), received.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Telemetry { .. })
    ));
    stop.send(()).unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn repeated_vehicle_disconnects_force_a_fresh_socket_then_recover() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut first = accept_async(tcp).await.unwrap();
        assert!(matches!(
            first.next().await.unwrap().unwrap(),
            Message::Text(ref text) if text.contains("data:subscribe_oauth")
        ));
        for disconnect in 1..=VEHICLE_DISCONNECTED_RECONNECT_LIMIT {
            first
                .send(Message::Text(
                    r#"{"msg_type":"data:error","tag":"9","error_type":"vehicle_disconnected"}"#
                        .into(),
                ))
                .await
                .unwrap();
            if disconnect < VEHICLE_DISCONNECTED_RECONNECT_LIMIT {
                assert!(matches!(
                    timeout(Duration::from_secs(1), first.next())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap(),
                    Message::Text(ref text) if text.contains("data:subscribe_oauth")
                ));
            }
        }
        let (tcp, _) = timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("fresh transport timeout")
            .expect("fresh transport");
        let mut second = accept_async(tcp).await.unwrap();
        assert!(matches!(
            second.next().await.unwrap().unwrap(),
            Message::Text(ref text) if text.contains("data:subscribe_oauth")
        ));
        second
            .send(Message::Text(
                r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#.into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_secs(1), second.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            Message::Text(ref text) if text.contains("data:unsubscribe")
        ));
    });
    let (events, mut received) = mpsc::channel(8);
    let supervisor = legacy_supervisor(9, 9, endpoint, events)
        .unwrap()
        .with_policy(SupervisorPolicy {
            connect_timeout: Duration::from_millis(100),
            silence_timeout: Duration::from_secs(1),
            backoff_initial: Duration::from_millis(5),
            remote_backoff_cap: Duration::from_millis(10),
            connect_backoff_cap: Duration::from_millis(10),
        });
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    for _ in 0..VEHICLE_DISCONNECTED_RECONNECT_LIMIT {
        assert_eq!(
            timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap(),
            Some(StreamEvent::TransportUnavailable)
        );
    }
    assert!(matches!(
        timeout(Duration::from_secs(1), received.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Telemetry { .. })
    ));
    stop.send(()).unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn full_event_queue_applies_backpressure_without_reconnect_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut socket = accept_async(tcp).await.unwrap();
        assert!(matches!(
            socket.next().await.unwrap().unwrap(),
            Message::Text(ref text) if text.contains("data:subscribe_oauth")
        ));
        socket
            .send(Message::Text(
                r#"{"msg_type":"control:hello","code":200}"#.into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#.into(),
            ))
            .await
            .unwrap();
        socket
            .send(Message::Text(
                r#"{"msg_type":"data:update","tag":"9","timestamp":1700000001123,"value":"42,12345.7,80,25,180,51.5,-0.1,120,D,200,210,180"}"#.into(),
            ))
            .await
            .unwrap();
    });

    let (events, mut receiver) = mpsc::channel(1);
    let supervisor = legacy_supervisor(9, 9, endpoint, events).unwrap();
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));

    // Capacity one is intentionally filled by the first telemetry frame.
    // The second frame must wait, not terminate the stream or reconnect.
    assert!(
        timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("first telemetry")
            .is_some()
    );
    assert!(
        timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("second telemetry")
            .is_some()
    );
    stop.send(()).expect("stop stream");
    timeout(Duration::from_secs(1), task)
        .await
        .expect("stream shutdown")
        .expect("stream task")
        .expect("orderly shutdown");
    server.await.unwrap();
}
#[tokio::test]
async fn silence_emits_transport_event_and_reconnects_with_bounded_backoff() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let (reconnected, reconnected_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut first = accept_async(tcp).await.unwrap();
        let message = first.next().await.unwrap().unwrap();
        assert!(
            matches!(message, Message::Text(ref text) if text.contains("data:subscribe_oauth"))
        );
        tokio::time::sleep(Duration::from_millis(35)).await;
        drop(first);

        let (tcp, _) = listener.accept().await.unwrap();
        let mut second = accept_async(tcp).await.unwrap();
        let message = second.next().await.unwrap().unwrap();
        assert!(
            matches!(message, Message::Text(ref text) if text.contains("data:subscribe_oauth"))
        );
        reconnected.send(()).unwrap();
        let message = second.next().await;
        assert!(matches!(message, Some(Ok(Message::Close(_))) | None));
    });

    let (events, mut received) = mpsc::channel(4);
    let supervisor = legacy_supervisor(9, 9, endpoint, events)
        .unwrap()
        .with_policy(SupervisorPolicy {
            connect_timeout: Duration::from_millis(100),
            silence_timeout: Duration::from_millis(20),
            backoff_initial: Duration::from_millis(5),
            remote_backoff_cap: Duration::from_millis(10),
            connect_backoff_cap: Duration::from_millis(10),
        });
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    assert_eq!(
        timeout(Duration::from_secs(1), received.recv())
            .await
            .unwrap(),
        Some(StreamEvent::TransportUnavailable)
    );
    timeout(Duration::from_secs(1), reconnected_rx)
        .await
        .unwrap()
        .unwrap();
    stop.send(()).unwrap();
    timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn health_requires_hello_then_recovers_after_timeout() {
    // This test exercises a real TCP/WebSocket reconnect. A 20 ms receive
    // deadline is below the scheduler noise of small or emulated x86
    // builders and can expire after the second server has queued telemetry
    // but before the client task is polled. Keep the production behavior
    // under test while giving both runtimes a bounded scheduling margin.
    let silence_timeout = Duration::from_millis(500);
    let test_timeout = Duration::from_secs(5);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/streaming/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut first = accept_async(tcp).await.unwrap();
        assert!(matches!(
            first.next().await.unwrap().unwrap(),
            Message::Text(ref text) if text.contains("data:subscribe_oauth")
        ));
        first
            .send(Message::Text(
                r#"{"msg_type":"control:hello","connection_timeout":0}"#.into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            timeout(test_timeout, first.next()).await.unwrap(),
            Some(Ok(Message::Close(_))) | None
        ));

        let (tcp, _) = listener.accept().await.unwrap();
        let mut second = accept_async(tcp).await.unwrap();
        assert!(matches!(
            second.next().await.unwrap().unwrap(),
            Message::Text(ref text) if text.contains("data:subscribe_oauth")
        ));
        second
            .send(Message::Text(
                r#"{"msg_type":"control:hello","connection_timeout":0}"#.into(),
            ))
            .await
            .unwrap();
        second
            .send(Message::Text(
                r#"{"msg_type":"data:update","tag":"9","value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#.into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            second.next().await.unwrap().unwrap(),
            Message::Text(ref text) if text.contains("data:unsubscribe")
        ));
    });

    let (events, mut received) = mpsc::channel(8);
    let supervisor = legacy_supervisor(9, 9, endpoint, events)
        .unwrap()
        .with_policy(SupervisorPolicy {
            connect_timeout: Duration::from_millis(100),
            silence_timeout,
            backoff_initial: Duration::from_millis(5),
            remote_backoff_cap: Duration::from_millis(10),
            connect_backoff_cap: Duration::from_millis(10),
        });
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(supervisor.run(shutdown));
    assert_eq!(
        timeout(test_timeout, received.recv()).await.unwrap(),
        Some(StreamEvent::TransportUnavailable)
    );
    let recovered = timeout(test_timeout, received.recv())
        .await
        .expect("telemetry after timeout reconnect");
    assert!(
        matches!(recovered, Some(StreamEvent::Telemetry { .. })),
        "unexpected recovery event: {recovered:?}"
    );
    stop.send(()).unwrap();
    timeout(test_timeout, task).await.unwrap().unwrap().unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn legacy_token_refresh_is_cancelled_by_owner_api_client_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (_tcp, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    let auth = crate::legacy_auth::LegacyAuth::for_test(
        Url::parse(&issuer).unwrap(),
        "old-access",
        "old-refresh",
    );
    let manager =
        crate::credentials::LegacyAuthManager::for_test(auth, std::sync::Arc::new(|_, _| Ok(())));
    crate::crypto::install_default_provider();
    let client = Client::builder()
        .timeout(Duration::from_millis(25))
        .build()
        .unwrap();
    let (events, _) = mpsc::channel(1);
    let supervisor = TeslaStreamSupervisor::new_legacy_auth_for_test(
        crate::owner_api::VehicleId::from_test(9),
        crate::owner_api::StreamVehicleId::from_test(9),
        Arc::new(Mutex::new(manager)),
        StreamRegion::Global,
        "ws://127.0.0.1:1/streaming/".to_owned(),
        client,
        events,
    )
    .unwrap();

    assert!(
        timeout(Duration::from_secs(1), supervisor.access_token())
            .await
            .unwrap()
            .is_err()
    );
    server.await.unwrap();
}

#[test]
fn binary_tesla_stream_frames_are_decoded() {
    let hello = r#"{"msg_type":"control:hello","connection_timeout":15}"#;
    assert_eq!(
        decode_message("42", Message::Text(hello.into())),
        Some(StreamEvent::Healthy)
    );
    assert_eq!(
        decode_message("42", Message::Binary(hello.as_bytes().to_vec().into())),
        Some(StreamEvent::Healthy)
    );

    let update = r#"{"msg_type":"data:update","tag":"42","value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#;
    for message in [
        Message::Text(update.into()),
        Message::Binary(update.as_bytes().to_vec().into()),
    ] {
        let Some(StreamEvent::Telemetry { update, .. }) = decode_message("42", message) else {
            panic!("Tesla data:update must decode")
        };
        assert_eq!(update.tag, "42");
        assert_eq!(update.timestamp_ms, 1_700_000_000_123);
    }

    let tagless = r#"{"msg_type":"data:update","value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#;
    assert_eq!(decode_message("42", Message::Text(tagless.into())), None);

    let other_tag = r#"{"msg_type":"data:update","tag":"9","value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#;
    assert_eq!(decode_message("42", Message::Text(other_tag.into())), None);
}

#[test]
fn non_utf8_binary_is_a_protocol_violation_and_control_frames_are_ignored() {
    assert_eq!(
        decode_message("42", Message::Binary(vec![0xff, 0xfe].into())),
        Some(StreamEvent::ProtocolViolation)
    );
    assert_eq!(decode_message("42", Message::Ping(Vec::new().into())), None);
    assert_eq!(decode_message("42", Message::Pong(Vec::new().into())), None);
    assert_eq!(decode_message("42", Message::Close(None)), None);
}

#[test]
fn accepts_teslamate_control_hello_without_status_code() {
    let event = decode_message(
        "9",
        Message::Text(r#"{"msg_type":"control:hello","connection_timeout":15}"#.into()),
    );

    assert_eq!(event, Some(StreamEvent::Healthy));
}

#[test]
fn teslamate_v4_tagless_data_errors_are_classified() {
    assert_eq!(
        classify_data_error(Some("client_error"), Some("owner_api error: unavailable")),
        DataErrorCategory::OwnerApiError
    );
    assert_eq!(
        classify_data_error(Some("client_error"), None),
        DataErrorCategory::ClientError
    );
    assert_eq!(
        classify_data_error(Some("vehicle_error"), Some("temporary vehicle error")),
        DataErrorCategory::Other
    );
    let disconnected = decode_message(
        "9",
        Message::Text(
            r#"{"msg_type":"data:error","tag":"9","error_type":"vehicle_disconnected"}"#.into(),
        ),
    );
    assert_eq!(disconnected, Some(StreamEvent::TransportUnavailable));

    let offline = decode_message(
        "9",
        Message::Text(
            r#"{"msg_type":"data:error","tag":"9","error_type":"vehicle_error","value":"Vehicle is offline"}"#
                .into(),
        ),
    );
    assert_eq!(offline, Some(StreamEvent::VehicleOffline));

    let tagless_offline = decode_message(
        "9",
        Message::Binary(
            r#"{"msg_type":"data:error","error_type":"vehicle_error","value":"Vehicle is offline"}"#
                .as_bytes()
                .to_vec()
                .into(),
        ),
    );
    assert_eq!(tagless_offline, Some(StreamEvent::VehicleOffline));

    let rejected = decode_message(
        "9",
        Message::Text(
            r#"{"msg_type":"data:error","tag":"9","error_type":"client_error","value":"Can't validate token"}"#
                .into(),
        ),
    );
    assert_eq!(rejected, Some(StreamEvent::AuthRejected));

    // TeslaMate v4.1.1 also receives client_error without a value.
    // It is a reconnectable transport result, not an auth rejection.
    let client_error_without_value = decode_message(
        "9",
        Message::Binary(
            br#"{"msg_type":"data:error","tag":"9","error_type":"client_error"}"#
                .to_vec()
                .into(),
        ),
    );
    assert_eq!(
        client_error_without_value,
        Some(StreamEvent::TransportUnavailable)
    );

    let other_vehicle = decode_message(
        "9",
        Message::Text(
            r#"{"msg_type":"data:error","tag":"10","error_type":"vehicle_disconnected"}"#.into(),
        ),
    );
    assert_eq!(other_vehicle, None);
}

#[test]
fn control_hello_auth_rejection_takes_precedence_over_timeout() {
    let event = decode_message(
        "9",
        Message::Text(r#"{"msg_type":"control:hello","connection_timeout":15,"code":401}"#.into()),
    );

    assert_eq!(event, Some(StreamEvent::AuthRejected));
    assert_eq!(
        decode_message(
            "9",
            Message::Text(r#"{"msg_type":"control:hello","connection_timeout":0}"#.into(),),
        ),
        Some(StreamEvent::Healthy)
    );
}

#[test]
fn parses_teslamate_timestamp_first_stream_values() {
    let update = parse_data_update(
        r#"{"msg_type":"data:update","tag":"9","value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#,
    )
    .unwrap();

    assert_eq!(update.timestamp_ms, 1_700_000_000_123);
    assert_eq!(update.speed, Some(42));
    assert_eq!(update.odometer, Some(12_345.6));
    assert_eq!(update.est_lat, Some(51.5));
    assert_eq!(update.shift_state.as_deref(), Some("D"));
    assert_eq!(update.heading, Some(180));
}

#[test]
fn timestamp_first_stream_values_fail_closed_on_ambiguity_or_bad_time() {
    assert_eq!(
        parse_data_update(
            r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"1700000000123,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#,
        ),
        Err(StreamError::MalformedDataUpdate)
    );
    assert_eq!(
        parse_data_update(
            r#"{"msg_type":"data:update","tag":"9","value":"not-a-time,42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#,
        ),
        Err(StreamError::InvalidTimestamp)
    );
}

#[test]
fn parser_rejects_extra_missing_and_oversized_owned_fields() {
    let frame = |tag: &str, fields: Vec<String>| {
        serde_json::json!({
            "msg_type": "data:update",
            "tag": tag,
            "timestamp": 1_700_000_000_123_i64,
            "value": fields.join(","),
        })
        .to_string()
    };
    let mut boundary = vec![String::new(); TESLAMATE_STREAM_FIELDS.len()];
    boundary[8] = "D".repeat(32);
    let boundary_tag = "t".repeat(128);
    let parsed = parse_data_update(&frame(&boundary_tag, boundary.clone())).unwrap();
    assert_eq!(parsed.tag, boundary_tag);
    assert_eq!(parsed.shift_state.as_deref(), Some(boundary[8].as_str()));

    let mut oversized_field = boundary.clone();
    oversized_field[0] = "1".repeat(65);
    assert_eq!(
        parse_data_update(&frame("9", oversized_field)),
        Err(StreamError::InvalidField)
    );

    let mut oversized_state = boundary.clone();
    oversized_state[8] = "D".repeat(33);
    assert_eq!(
        parse_data_update(&frame("9", oversized_state)),
        Err(StreamError::InvalidField)
    );
    assert_eq!(
        parse_data_update(&frame(&"t".repeat(129), boundary.clone())),
        Err(StreamError::InvalidTag)
    );

    let mut extra = boundary.clone();
    extra.push(String::new());
    assert_eq!(
        parse_data_update(&frame("9", extra)),
        Err(StreamError::MalformedDataUpdate)
    );
    let mut missing = boundary;
    missing.pop();
    assert_eq!(
        parse_data_update(&frame("9", missing)),
        Err(StreamError::MalformedDataUpdate)
    );
}

#[test]
fn parses_nested_stream_values() {
    let update=parse_data_update(r#"{"msg_type":"data:update","tag":"9","timestamp":1700000000123,"value":"42,12345.6,80,25,180,51.5,-0.1,120,D,200,210,180"}"#).unwrap();
    assert_eq!(update.speed, Some(42));
    assert_eq!(update.est_lat, Some(51.5));
}
