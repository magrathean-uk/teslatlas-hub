// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C4 part P2 (the supervisor's session): SC6 to SC21, SC71 and SC73 to
// SC77, plus tests of the rules the part owns (R6a lock wait, R12 run termination, R13
// Retry-After). Each test runs one or two sessions against an in-process loopback peer
// or the fake Tesla under paused time, with exact expected rows, events and times.
// Synthetic data only; no test touches the network.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use serde_json::json;
use tokio::{
    net::TcpListener,
    sync::{Mutex, Notify, OwnedMutexGuard, mpsc},
    time::{Instant, sleep, timeout},
};

use super::{
    AdmissionEvent, K_MAX_MESSAGE, StreamEvent, StreamPowerGate, StreamSupervisorError,
    session::parse_retry_after, test_peer::*,
};
use crate::credentials::LegacyAuthManager;

/// Lets real I/O (a loopback HTTP exchange with a token issuer) make progress
/// without the paused clock moving: a blocking task holds the runtime's automatic
/// time advance back while it sleeps. Only the tests that use the real issuer
/// need it; everything else runs over in-memory pipes.
async fn real_pause(milliseconds: u64) {
    tokio::task::spawn_blocking(move || std::thread::sleep(Duration::from_millis(milliseconds)))
        .await
        .expect("pause");
}

/// Waits, in real time, until `condition` holds (at most about 10 s).
async fn eventually(mut condition: impl FnMut() -> bool) {
    for _ in 0..2_000 {
        if condition() {
            return;
        }
        real_pause(5).await;
    }
    panic!("the condition never held");
}

/// A peer that reads the subscribe frame, sends each of `frames`, then reads until
/// the client closes.
async fn streaming_peer(frames: Vec<String>) -> Peer {
    Peer::spawn(move |conn| {
        let frames = frames.clone();
        async move {
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            for frame in &frames {
                ws.send(frame).await;
            }
            ws.drain().await;
        }
    })
    .await
}

/// A peer that holds the upgrade of its first connection until `gate` is notified.
/// That connection then reads, answering Pings, until the client ends it; with
/// `subscribes` it first reads the subscribe frame. Every later connection is
/// served at once as a normal stream that reads the subscribe frame.
async fn gated_peer(gate: Arc<Notify>, subscribes: bool) -> Peer {
    Peer::spawn(move |conn| {
        let gate = Arc::clone(&gate);
        async move {
            let first = conn.index == 0;
            if first {
                gate.notified().await;
            }
            let mut ws = conn.ws().await;
            if !first || subscribes {
                ws.subscribe().await;
            }
            ws.drain().await;
        }
    })
    .await
}

/// The frames and Close codes a peer logged, in order, as `frame:...` and `close:...`.
fn wire_labels(peer: &Peer) -> Vec<String> {
    peer.log
        .labels()
        .into_iter()
        .filter(|label| label.starts_with("frame:") || label.starts_with("close:"))
        .collect()
}

const UNSUBSCRIBE: &str = r#"frame:{"msg_type":"data:unsubscribe","tag":"4242"}"#;

fn telemetry_time(event: &StreamEvent) -> i64 {
    match event {
        StreamEvent::Telemetry { update, .. } => update.timestamp_ms,
        other => panic!("expected telemetry, got {other:?}"),
    }
}

/// The run of SC9: one session, `U(T0, V1)`, then the Hub's shutdown.
struct ShutdownWhileStreaming {
    peer: Peer,
    store: crate::db::HubStore,
    gate: Arc<StreamPowerGate>,
    result: Result<(), StreamSupervisorError>,
    gate_confirmed_before: bool,
    events: Vec<StreamEvent>,
    _dir: tempfile::TempDir,
}

async fn shutdown_while_streaming() -> ShutdownWhileStreaming {
    let peer = streaming_peer(vec![u(T0, V1)]).await;
    let gate = Arc::new(StreamPowerGate::default());
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.gate = Some(Arc::clone(&gate));
    let (rig, dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    let events = vec![run.event().await, run.event().await];
    let gate_confirmed_before = gate.is_confirmed();
    let result = run.shut_down().await;
    ShutdownWhileStreaming {
        peer,
        store,
        gate,
        result,
        gate_confirmed_before,
        events,
        _dir: dir,
    }
}

// --------------------------------------------------------------------- SC6, SC7, SC8

#[tokio::test(start_paused = true)]
async fn sc6_the_refresh_comes_before_the_connection_and_the_token_only_in_the_frame() {
    let peer = streaming_peer(Vec::new()).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    peer.log.wait_for("subscribe-received", 1).await;
    let labels = peer.log.labels();
    let position = |wanted: &str| {
        labels
            .iter()
            .position(|label| label == wanted)
            .unwrap_or_else(|| panic!("{wanted} was not logged: {labels:?}"))
    };
    assert!(position("refresh") < position("accept"));
    assert!(position("accept") < position("subscribe-received"));
    // R6 step 5, R7: the token is read after the connect, immediately before the
    // subscribe frame, so one rotated during the connect is the one sent.
    assert!(position("upgrade:/") < position("hook:TokenRead"));
    assert!(position("hook:TokenRead") < position("subscribe-received"));
    // The upgrade request carries no Authorization header and no token in its URL.
    assert_eq!(peer.log.tails("upgrade:"), vec!["/".to_owned()]);
    assert_eq!(peer.log.count("upgrade-authorization"), 0);
    assert_eq!(peer.log.tails("token:"), vec!["tok-1".to_owned()]);
    assert!(
        labels
            .iter()
            .filter(|label| label.starts_with("upgrade"))
            .all(|label| !label.contains("tok-1"))
    );
    // Exactly one refresh call and one subscribe for the session.
    assert_eq!(peer.log.count("refresh"), 1);
    assert_eq!(peer.log.count("subscribe-received"), 1);
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc7_every_session_sends_the_token_the_manager_holds_then() {
    let issuer = Issuer::spawn(Duration::ZERO, "tok-2", "ref-2").await;
    let creds = Creds::current(issuer.url.clone(), "tok-1").held_by(&issuer);
    let manager = Arc::clone(&creds.manager);
    let client = creds.client.clone();
    // Session 1 stays open until the test has rotated the pair, as another holder of the
    // shared manager (the Owner API path) would; later sessions close at once.
    let rotated = Arc::new(Notify::new());
    let peer_rotated = Arc::clone(&rotated);
    let peer = Peer::spawn(move |conn| {
        let rotated = Arc::clone(&peer_rotated);
        async move {
            let index = conn.index;
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            if index == 0 {
                rotated.notified().await;
            }
            ws.close(1000).await;
            ws.drain().await;
        }
    })
    .await;
    let mut run = Rig::for_peer(&peer, creds).start();
    peer.log.wait_for("subscribe-received", 1).await;
    // The rotation is a real HTTP exchange: the paused clock is held still while it
    // runs, so no timer of the credential unit can fire on its account.
    issuer
        .holds
        .held(async {
            manager
                .lock()
                .await
                .refresh_now(&client, SystemTime::now())
                .await
        })
        .await
        .expect("the rotation");
    rotated.notify_one();
    peer.log.wait_for("subscribe-received", 3).await;
    assert_eq!(
        peer.log.tails("token:"),
        vec!["tok-1".to_owned(), "tok-2".to_owned(), "tok-2".to_owned()]
    );
    assert_eq!(issuer.requests.load(Ordering::SeqCst), 1);
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc7_a_token_rotated_while_the_connect_is_held_is_the_one_sent() {
    // R6 step 5, R7: the token is read after the connect, so a pair another holder of
    // the shared manager rotates while the upgrade is still pending is the one in the
    // subscribe frame.
    let issuer = Issuer::spawn(Duration::ZERO, "tok-2", "ref-2").await;
    let creds = Creds::current(issuer.url.clone(), "tok-1").held_by(&issuer);
    let manager = Arc::clone(&creds.manager);
    let client = creds.client.clone();
    let gate = Arc::new(Notify::new());
    let peer = gated_peer(Arc::clone(&gate), true).await;
    let mut run = Rig::for_peer(&peer, creds).start();
    peer.log.wait_for("accept", 1).await;
    issuer
        .holds
        .held(async {
            manager
                .lock()
                .await
                .refresh_now(&client, SystemTime::now())
                .await
        })
        .await
        .expect("the rotation");
    assert!(
        !peer.log.labels().contains(&"hook:TokenRead".to_owned()),
        "no token was read while the connect was held"
    );
    gate.notify_one();
    peer.log.wait_for("subscribe-received", 1).await;
    assert_eq!(peer.log.tails("token:"), vec!["tok-2".to_owned()]);
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc8_a_run_holds_one_socket_at_a_time() {
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        ws.close(1000).await;
        ws.drain().await;
    })
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    for _ in 0..6 {
        assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    }
    assert_eq!(peer.log.count("accept"), 6);
    assert_eq!(peer.max_concurrent(), 1);
    run.shut_down().await.expect("shutdown");
}

/// SC8 against the fake Tesla, which checks the subscribe frame against the
/// column set exactly. Real time and real sockets, so two attempts only (the
/// second after 1 s).
#[tokio::test]
async fn sc8_the_fake_tesla_sees_one_session_at_a_time_and_the_exact_columns() {
    use crate::fake_tesla::{
        AdvanceMode, FIXTURE_VID, FakeTeslaSource, StreamAuditEvent, StreamScript,
    };
    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("fake Tesla");
    fake.set_stream_script(StreamScript::CloseAfterSubscribe);
    let creds = Creds::current(fake.oauth_issuer_url(), "tok-1");
    let mut rig = Rig::new(fake.stream_endpoint().to_owned(), creds);
    rig.tag = FIXTURE_VID;
    let mut run = rig.start();
    for _ in 0..2 {
        let event = run.event_within(Duration::from_secs(20)).await;
        assert_eq!(event, Some(StreamEvent::TransportUnavailable));
    }
    let stats = fake.stream_session_stats();
    assert_eq!(stats.connection_attempts, 2);
    assert_eq!(stats.max_concurrent_sessions, 1);
    let accepted_subscribes = fake
        .audited_stream_events()
        .iter()
        .filter(|event| event.event == StreamAuditEvent::Subscribe && event.accepted)
        .count();
    assert_eq!(accepted_subscribes, 2);
    run.shut_down().await.expect("shutdown");
    fake.shutdown().await;
}

// ---------------------------------------------------------------- SC9, SC15, shutdown

#[tokio::test(start_paused = true)]
async fn sc9_shutdown_while_streaming_unsubscribes_and_closes_1000() {
    let run = shutdown_while_streaming().await;
    assert_eq!(run.events[0], StreamEvent::Healthy);
    assert_eq!(telemetry_time(&run.events[1]), T0);
    assert!(run.gate_confirmed_before);
    assert!(run.result.is_ok());
    // The peer receives exactly the unsubscribe frame, then Close 1000.
    let wire = wire_labels(&run.peer);
    assert_eq!(wire.len(), 3, "{wire:?}");
    assert!(wire[0].contains("data:subscribe_oauth"));
    assert_eq!(wire[1], UNSUBSCRIBE);
    assert_eq!(wire[2], "close:1000");
    // The session row is an orderly shutdown linked to the unsubscribe row, which
    // completed as a success; the gate is not confirmed.
    let sessions = session_rows(&run.store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "orderly_shutdown");
    assert_eq!(
        sessions[0].unsubscribe_receipt_id,
        request_id(&run.store, "stream_unsubscribe")
    );
    assert!(sessions[0].unsubscribe_receipt_id.is_some());
    assert!(!run.gate.is_confirmed());
}

#[tokio::test(start_paused = true)]
async fn sc15_the_rows_of_a_normal_session() {
    let run = shutdown_while_streaming().await;
    let sessions = session_rows(&run.store);
    assert_eq!(sessions.len(), 1);
    let rows = request_rows(&run.store);
    let outcomes: Vec<_> = rows
        .iter()
        .map(|row| (row.operation.as_str(), row.outcome.as_str()))
        .collect();
    assert_eq!(
        outcomes,
        vec![
            ("stream_connect", "success"),
            ("stream_subscribe", "success"),
            ("stream_unsubscribe", "success"),
        ]
    );
    for row in &rows {
        assert_eq!(row.transport, "stream");
        assert_eq!(row.safety_class, "non_wake_endpoint");
        assert_eq!(row.precondition, "not_required");
        assert_eq!(row.correlation_id, sessions[0].correlation_id);
        assert_eq!(row.http_status, None);
        assert_eq!(row.retry_after_seconds, None);
    }
}

#[tokio::test(start_paused = true)]
async fn sc10_shutdown_while_waiting_returns_at_once_and_writes_nothing() {
    let refusing = Peer::refusing();
    let (rig, _dir) = Rig::for_peer(&refusing, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    // The refused connection puts the run in Waiting (1 s at jitter 0).
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    let rows_before = (request_rows(&store).len(), session_rows(&store).len());
    let before = Instant::now();
    run.shut_down().await.expect("shutdown");
    assert_eq!(Instant::now(), before, "no time passed");
    assert_eq!(
        (request_rows(&store).len(), session_rows(&store).len()),
        rows_before
    );
}

#[tokio::test(start_paused = true)]
async fn sc11_shutdown_while_connecting_cancels_the_connect() {
    let peer = Peer::spawn(|conn| async move { conn.hold().await }).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("accept", 1).await;
    sleep(Duration::from_secs(3)).await;
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "cancelled".to_owned())]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "cancelled_before_subscription");
}

#[tokio::test(start_paused = true)]
async fn sc12_a_close_is_bounded_when_the_server_never_answers() {
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.reply_to_close = false;
        ws.subscribe().await;
        ws.drain().await;
        // Keep the socket open: the server never sends its Close.
        std::future::pending::<()>().await;
    })
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    peer.log.wait_for("subscribe-received", 1).await;
    let signalled = Instant::now();
    run.shut_down().await.expect("shutdown");
    assert!(Instant::now() - signalled <= Duration::from_secs(4));
    let wire = wire_labels(&peer);
    assert_eq!(wire[1], UNSUBSCRIBE);
    assert_eq!(wire[2], "close:1000");
}

// ----------------------------------------------------------------- SC13, SC14, R12

#[tokio::test(start_paused = true)]
async fn sc13_a_lost_credential_authority_ends_the_run_before_any_network_use() {
    let peer = streaming_peer(Vec::new()).await;
    let authority = Arc::new(AtomicBool::new(false));
    let creds = Creds::with_authority(
        url::Url::parse("http://127.0.0.1:9/").expect("URL"),
        "tok-1",
        authority,
    );
    let (rig, _dir) = Rig::for_peer(&peer, creds).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    let result = run.finished(Duration::from_secs(5)).await;
    assert!(matches!(
        result,
        Err(StreamSupervisorError::CredentialAuthorityUnavailable)
    ));
    assert_eq!(peer.log.count("accept"), 0);
    assert!(request_rows(&store).is_empty());
    assert!(session_rows(&store).is_empty());
}

#[tokio::test(start_paused = true)]
async fn sc14_a_failed_audit_write_sends_nothing_and_ends_the_run() {
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.drain().await;
    })
    .await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    store
        .open()
        .expect("connection")
        .execute_batch(
            "CREATE TRIGGER fail_subscribe BEFORE INSERT ON outbound_request_receipts
             WHEN NEW.operation = 'stream_subscribe'
             BEGIN SELECT RAISE(ABORT, 'test'); END;",
        )
        .expect("trigger");
    let mut run = rig.start();
    let result = run.finished(Duration::from_secs(10)).await;
    assert!(
        matches!(result, Err(StreamSupervisorError::Audit(_))),
        "{result:?}"
    );
    // No subscribe frame reached the peer; the connect row is a success and the
    // session row `failed`.
    peer.log.wait_for("eof", 1).await;
    assert_eq!(peer.log.count("frame:"), 0);
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "success".to_owned())]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
}

#[tokio::test(start_paused = true)]
async fn r12_a_closed_event_receiver_ends_the_run_after_a_close() {
    let peer = streaming_peer(vec![u(T0, V1)]).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    // The receiver is dropped before the first frame arrives.
    drop(std::mem::replace(&mut run.events, mpsc::channel(1).1));
    let result = run.finished(Duration::from_secs(10)).await;
    assert!(
        matches!(result, Err(StreamSupervisorError::EventReceiverClosed)),
        "{result:?}"
    );
    let wire = wire_labels(&peer);
    assert_eq!(wire[1], UNSUBSCRIBE);
    assert_eq!(wire[2], "close:1000");
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
    assert_eq!(sessions[0].unsubscribe_receipt_id, None);
}

// ------------------------------------------------------------ SC16, SC17, SC18

#[tokio::test(start_paused = true)]
async fn sc16_a_refused_session_sends_no_unsubscribe() {
    let peer = streaming_peer(vec![hello_frame(Some(401), None)]).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("close:", 1).await;
    // One refusal is only evidence (R46): no event.
    assert!(run.event_within(Duration::from_secs(1)).await.is_none());
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        request_outcomes(&store),
        vec![
            ("stream_connect".to_owned(), "success".to_owned()),
            (
                "stream_subscribe".to_owned(),
                "authentication_rejected".to_owned()
            ),
        ]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
    // Only the subscribe frame and the Close reached the peer: a refusal before any
    // data gets no unsubscribe (R9).
    let wire = wire_labels(&peer);
    assert_eq!(wire.len(), 2, "{wire:?}");
    assert_eq!(wire[1], "close:1000");
}

#[tokio::test(start_paused = true)]
async fn sc17_a_vehicle_session_unsubscribes_and_ends_as_transport_ended() {
    let peer = streaming_peer(vec![error_frame("vehicle_test_a", "the car is asleep")]).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::VehicleOffline);
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        request_outcomes(&store),
        vec![
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "success".to_owned()),
            ("stream_unsubscribe".to_owned(), "success".to_owned()),
        ]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "transport_ended");
    assert_eq!(sessions[0].unsubscribe_receipt_id, None);
    assert_eq!(wire_labels(&peer)[1], UNSUBSCRIBE);
}

#[tokio::test(start_paused = true)]
async fn sc18_a_refused_connection_is_a_transport_error_without_a_subscribe_row() {
    let refusing = Peer::refusing();
    let (rig, _dir) = Rig::for_peer(&refusing, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "transport_error".to_owned())]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
}

// ------------------------------------------------------------------ SC19, SC73

/// Two attempts against a peer that answers the upgrade with `status`: the rows, the
/// time between the two attempts and the summary's transport-error count.
async fn upgrade_refusal_run(
    status: u16,
    retry_after: Option<&'static str>,
) -> (Vec<RequestRow>, Duration, u64) {
    let peer = Peer::spawn(move |conn| async move {
        conn.refuse_upgrade(status, retry_after).await;
    })
    .await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    let times = peer.log.times("refused:");
    let gap = times[1] - times[0];
    run.shut_down().await.expect("shutdown");
    let summary = store.stream_audit_summary_since(0).expect("summary");
    (request_rows(&store), gap, summary.transport_errors)
}

#[tokio::test(start_paused = true)]
async fn sc19_upgrade_answers_set_the_status_the_retry_after_and_the_next_attempt() {
    let (rows, gap, errors) = upgrade_refusal_run(503, Some("600")).await;
    assert_eq!(rows[0].operation, "stream_connect");
    assert_eq!(rows[0].outcome, "transport_error");
    assert_eq!(rows[0].http_status, Some(503));
    assert_eq!(rows[0].retry_after_seconds, Some(600));
    assert_eq!(gap, Duration::from_secs(600));
    assert_eq!(errors, 2);

    // A Retry-After above the cap is recorded as sent and honoured only up to 3,600 s.
    let (rows, gap, errors) = upgrade_refusal_run(503, Some("7200")).await;
    assert_eq!(rows[0].retry_after_seconds, Some(7200));
    assert_eq!(gap, Duration::from_secs(3_600));
    assert_eq!(errors, 2);

    // Any other status is a transport error on the 1 s schedule and has no Retry-After.
    let (rows, gap, errors) = upgrade_refusal_run(404, None).await;
    assert_eq!(rows[0].outcome, "transport_error");
    assert_eq!(rows[0].http_status, Some(404));
    assert_eq!(rows[0].retry_after_seconds, None);
    assert_eq!(gap, Duration::from_secs(1));
    assert_eq!(errors, 2);
    // No subscribe or session-subscribe row follows a refused upgrade.
    assert!(rows.iter().all(|row| row.operation == "stream_connect"));
}

#[tokio::test(start_paused = true)]
async fn r13_an_upgrade_status_outside_the_http_range_is_a_transport_error_without_a_status() {
    // The WebSocket library accepts any three-digit status from a peer, but the audit
    // column takes only 100 to 599. Such an answer is still a failed upgrade of class
    // `Transport`: its row records no status and the run goes on (R12, R13), where a
    // recorded 999 would have ended the run with an audit error.
    for status in [999, 700, 600] {
        let (rows, gap, errors) = upgrade_refusal_run(status, None).await;
        assert_eq!(rows[0].operation, "stream_connect", "{status}");
        assert_eq!(rows[0].outcome, "transport_error", "{status}");
        assert_eq!(rows[0].http_status, None, "{status}");
        assert_eq!(rows[0].retry_after_seconds, None, "{status}");
        assert_eq!(gap, Duration::from_secs(1), "{status}: the 1 s schedule");
        assert_eq!(errors, 2, "{status}: the run went on to a second attempt");
    }
    // The edges of the range are recorded as they are.
    for status in [100, 599] {
        let (rows, _, _) = upgrade_refusal_run(status, None).await;
        assert_eq!(rows[0].http_status, Some(i64::from(status)), "{status}");
    }
}

/// The date of `W + 600 s` in the preferred HTTP-date form (RFC 9110 section 5.6.7).
const DATE_PLUS_600: &str = "Thu, 09 Oct 2025 09:03:25 GMT";
/// The date of `W - 3,600 s`.
const DATE_MINUS_3600: &str = "Thu, 09 Oct 2025 07:53:25 GMT";

#[tokio::test(start_paused = true)]
async fn sc73_retry_after_as_an_http_date_is_read_against_the_wall_clock() {
    let (rows, gap, _) = upgrade_refusal_run(503, Some(DATE_PLUS_600)).await;
    assert_eq!(rows[0].retry_after_seconds, Some(600));
    assert_eq!(gap, Duration::from_secs(600));
    // A date in the past floors at 0, so the schedule's own 1 s applies.
    let (rows, gap, _) = upgrade_refusal_run(503, Some(DATE_MINUS_3600)).await;
    assert_eq!(rows[0].retry_after_seconds, Some(0));
    assert_eq!(gap, Duration::from_secs(1));
}

#[test]
fn r13_retry_after_values_are_read_as_seconds_or_a_date() {
    assert_eq!(parse_retry_after("120", W), Some(120));
    assert_eq!(parse_retry_after(" 0 ", W), Some(0));
    assert_eq!(parse_retry_after(DATE_PLUS_600, W), Some(600));
    assert_eq!(parse_retry_after(DATE_MINUS_3600, W), Some(0));
    // The date has second resolution: 599.5 s ahead floors to 599.
    assert_eq!(parse_retry_after(DATE_PLUS_600, W + 500), Some(599));
    // A value too large for the audit column saturates.
    assert_eq!(
        parse_retry_after("99999999999999999999999", W),
        Some(i64::MAX as u64)
    );
    // Anything else is absent: empty, negative, fractional, the obsolete date forms.
    for text in [
        "",
        "-5",
        "1.5",
        "soon",
        "Thursday, 09-Oct-25 09:03:25 GMT",
        "Thu Oct  9 09:03:25 2025",
        "Thu, 09 Oct 2025 09:03:25 UTC",
        "Thu, 32 Oct 2025 09:03:25 GMT",
        "Thu, 09 Oct 2025 25:03:25 GMT",
        "Thu, 09 Foo 2025 09:03:25 GMT",
    ] {
        assert_eq!(parse_retry_after(text, W), None, "{text:?}");
    }
}

// ------------------------------------------------------------------ SC20, SC21

#[tokio::test(start_paused = true)]
async fn sc20_the_connect_limit_is_ten_seconds_and_sc21_the_manager_stays_free() {
    let peer = Peer::spawn(|conn| async move { conn.hold().await }).await;
    let creds = Creds::offline("tok-1");
    let manager = Arc::clone(&creds.manager);
    let (rig, _dir) = Rig::for_peer(&peer, creds).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("accept", 1).await;
    let accepted = peer.log.times("accept")[0];
    // SC21: during the stalled upgrade another task locks the manager at once.
    let locked = timeout(Duration::from_millis(10), manager.lock()).await;
    assert!(
        locked.is_ok(),
        "the manager lock was free during the upgrade"
    );
    drop(locked);
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - accepted, Duration::from_secs(10));
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "transport_error".to_owned())]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
}

// ------------------------------------------------------------------------- SC71

/// Real time and a real TLS socket: the certificate check is the system under test.
#[tokio::test]
async fn sc71_an_untrusted_certificate_is_a_transport_error_and_no_upgrade_is_sent() {
    use axum::Router;
    use rcgen::{CertifiedKey, generate_simple_self_signed};

    crate::crypto::install_default_provider();
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["127.0.0.1".to_owned()]).expect("certificate");
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
        cert.pem().into_bytes(),
        signing_key.serialize_pem().into_bytes(),
    )
    .await
    .expect("TLS config");
    let requests = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&requests);
    let router = Router::new().fallback(move || {
        let counted = Arc::clone(&counted);
        async move {
            counted.fetch_add(1, Ordering::SeqCst);
            "unexpected"
        }
    });
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener.into_std().expect("std listener"), tls)
            .expect("TLS server")
            .serve(router.into_make_service())
            .await
            .expect("serve");
    });
    let (rig, _dir) = Rig::new(format!("wss://{address}/"), Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(
        run.event_within(Duration::from_secs(20)).await,
        Some(StreamEvent::TransportUnavailable)
    );
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "transport_error".to_owned())]
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "no WebSocket upgrade reached the listener"
    );
    server.abort();
}

// ------------------------------------------------------------ SC74, SC75, SC76, SC77

#[tokio::test(start_paused = true)]
async fn sc74_shutdown_while_admitting_waits_for_no_lock() {
    let gate = Arc::new(Notify::new());
    let peer_gate = Arc::clone(&gate);
    let peer = Peer::spawn(move |conn| {
        let gate = Arc::clone(&peer_gate);
        async move {
            // The upgrade completes only when the test has taken the manager lock.
            gate.notified().await;
            let mut ws = conn.ws().await;
            ws.drain().await;
        }
    })
    .await;
    let creds = Creds::offline("tok-1");
    let manager = Arc::clone(&creds.manager);
    let (rig, _dir) = Rig::for_peer(&peer, creds).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("accept", 1).await;
    let holder = Arc::clone(&manager).lock_owned().await;
    gate.notify_one();
    peer.log.wait_for("upgrade:", 1).await;
    sleep(Duration::from_secs(2)).await;
    run.shut_down().await.expect("shutdown within 5 s");
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "success".to_owned())]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "cancelled_before_subscription");
    // The other holder still holds the lock, and releases it normally.
    assert!(manager.try_lock().is_err());
    drop(holder);
    assert!(manager.try_lock().is_ok());
}

/// A due refresh whose issuer answers after `delay`: the pair it rotates to.
async fn rotating_creds(delay: Duration) -> (Issuer, Creds) {
    let issuer = Issuer::spawn(delay, "rot-access", "rot-refresh").await;
    let creds = Creds::due(issuer.url.clone(), "tok-1").held_by(&issuer);
    (issuer, creds)
}

/// Waits for the real HTTP exchange to finish, then checks that the rotated pair
/// was persisted exactly once and is the manager's current one.
async fn assert_rotation_persisted_once(creds: &Creds) {
    eventually(|| !creds.persisted.lock().expect("persist lock").is_empty()).await;
    real_pause(50).await;
    let persisted = creds.persisted.lock().expect("persist lock").clone();
    assert_eq!(
        persisted,
        vec![("rot-access".to_owned(), "rot-refresh".to_owned())]
    );
    assert_eq!(creds.manager.lock().await.access_token(), "rot-access");
}

#[tokio::test(start_paused = true)]
async fn sc75_a_refresh_survives_shutdown_and_abort() {
    // Shutdown one second after the refresh started.
    let peer = streaming_peer(Vec::new()).await;
    let (issuer, creds) = rotating_creds(Duration::from_secs(3)).await;
    let mut run = Rig::for_peer(&peer, creds).start();
    issuer.log.wait_for("token-request", 1).await;
    sleep(Duration::from_secs(1)).await;
    run.shut_down().await.expect("shutdown within 5 s");
    assert_eq!(peer.log.count("accept"), 0, "no connection was made");
    sleep(Duration::from_secs(10)).await;
    assert_eq!(issuer.requests.load(Ordering::SeqCst), 1);
    assert_rotation_persisted_once(&run.creds).await;

    // The same with the run's task aborted, as C3's runtime drop does.
    let peer = streaming_peer(Vec::new()).await;
    let (issuer, creds) = rotating_creds(Duration::from_secs(3)).await;
    let mut run = Rig::for_peer(&peer, creds).start();
    issuer.log.wait_for("token-request", 1).await;
    sleep(Duration::from_secs(1)).await;
    run.task.take().expect("task").abort();
    sleep(Duration::from_secs(10)).await;
    assert_eq!(peer.log.count("accept"), 0, "no connection was made");
    assert_eq!(issuer.requests.load(Ordering::SeqCst), 1);
    assert_rotation_persisted_once(&run.creds).await;
}

#[tokio::test(start_paused = true)]
async fn sc76_a_slow_refresh_is_awaited_without_an_outage() {
    let peer = streaming_peer(Vec::new()).await;
    let (_issuer, creds) = rotating_creds(Duration::from_secs(45)).await;
    let mut run = Rig::for_peer(&peer, creds).start();
    peer.log.wait_for("subscribe-received", 1).await;
    // The subscribe carries the rotated token, and nothing was reported meanwhile.
    assert_eq!(peer.log.tails("token:"), vec!["rot-access".to_owned()]);
    assert!(run.events.try_recv().is_err());
    let refreshes = run
        .hook_events()
        .into_iter()
        .filter(|event| *event == AdmissionEvent::RefreshStarted)
        .count();
    assert_eq!(refreshes, 1);
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc77_a_terminal_credential_stops_connections_and_is_rechecked_every_150_seconds() {
    let peer = streaming_peer(Vec::new()).await;
    let terminal = Arc::new(AtomicBool::new(true));
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.terminal = Some(terminal);
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    sleep(Duration::from_secs(20 * 60 - 5)).await;
    // The Checking step (lock, refresh call) ran at 0, 150, ..., 1,050 s: every
    // jittered(300 s, 0), and the token of step 5 was never read.
    let checks = peer.log.times("hook:LockAcquired");
    assert_eq!(checks.len(), 8);
    for pair in checks.windows(2) {
        assert_eq!(pair[1] - pair[0], Duration::from_secs(150));
    }
    assert!(!run.hook_events().contains(&AdmissionEvent::TokenRead));
    assert_eq!(peer.log.count("accept"), 0);
    assert!(request_rows(&store).is_empty());
    assert!(session_rows(&store).is_empty());
    assert!(run.events.try_recv().is_err(), "no event is sent");
    run.shut_down().await.expect("shutdown");
}

/// SC77 against the credential unit (C5 R43, R56, C4 R57 b): an account that is
/// signed out answers every refresh and every token read with a terminal error, so
/// the stream opens no connection, writes no rows and sends no event, and looks
/// again every 150 s.
#[tokio::test(start_paused = true)]
async fn sc77_a_signed_out_account_stops_connections_with_the_real_credential_unit() {
    use crate::legacy_auth::{LegacyAuth, SignOutReason, state::AuthStop};

    let peer = streaming_peer(Vec::new()).await;
    let mut auth = LegacyAuth::for_test(
        url::Url::parse("http://127.0.0.1:9/oauth2/v3").expect("issuer"),
        "tok-1",
        "refresh-0",
    );
    auth.set_stop(AuthStop::SignedOut(SignOutReason::AccessTokenRejected));
    let manager =
        crate::credentials::LegacyAuthManager::for_test_with_active_pair(auth).expect("a manager");
    let (rig, _dir) = Rig::for_peer(&peer, Creds::from_manager(manager)).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    sleep(Duration::from_secs(20 * 60 - 5)).await;
    let checks = peer.log.times("hook:LockAcquired");
    assert_eq!(checks.len(), 8);
    for pair in checks.windows(2) {
        assert_eq!(pair[1] - pair[0], Duration::from_secs(150));
    }
    assert!(!run.hook_events().contains(&AdmissionEvent::TokenRead));
    assert_eq!(peer.log.count("accept"), 0);
    assert!(request_rows(&store).is_empty());
    assert!(session_rows(&store).is_empty());
    assert!(run.events.try_recv().is_err(), "no event is sent");
    run.shut_down().await.expect("shutdown");
}

/// C4 R57 (b) (B2 clarification 3), initial row 8 of C5's outcome table: a 400 or
/// 401 `invalid_grant` or `login_required` to the refresh initially sets
/// `sign_in_required(refresh_token_rejected)`, which is not terminal. Token requests
/// stop, and the stream is admitted with the current, usable access token (no second
/// token request). Once the access token has expired the same answer is a sign-out,
/// and the stream opens no connection.
#[tokio::test(start_paused = true)]
async fn cs_p3_row8_stream_admission_matches_c5_state() {
    use crate::legacy_auth::{SignOutReason, state::AuthStop};

    // The two answers of row 8, with their bodies scripted explicitly: a status
    // and an empty object would be no row 8 at all.
    for (status, body) in [
        (400_u16, r#"{"error":"invalid_grant"}"#),
        (401, r#"{"error":"login_required"}"#),
    ] {
        let issuer =
            Issuer::spawn_with_error_body(Duration::ZERO, "tok-2", "ref-2", status, body).await;
        // A due pair whose access token is still usable, in the real credential unit
        // over a temporary Hub store (the receipt, the fence and the status row are
        // the store's).
        let StoredCreds {
            creds,
            store,
            _directory,
        } = Creds::over_a_store(&issuer.url, "tok-1", schedule_due());
        let creds = creds.held_by(&issuer);
        let manager = Arc::clone(&creds.manager);
        let peer = streaming_peer(Vec::new()).await;
        let mut rig = Rig::for_peer(&peer, creds);
        rig.store = Some(store.clone());
        let mut run = rig.start();
        // The refresh is rejected, the unit is signed in again (not out), and the
        // subscription goes out with the current access token.
        peer.log.wait_for("subscribe-received", 1).await;
        assert_eq!(
            peer.log.tails("token:")[0],
            "tok-1",
            "{status}: the current access token"
        );
        assert_eq!(
            issuer.requests.load(Ordering::SeqCst),
            1,
            "{status}: the one refresh"
        );
        {
            let status_now = manager.lock().await.status_now();
            assert_eq!(
                status_now.stop(),
                Some(AuthStop::SignInRequired(
                    SignOutReason::RefreshTokenRejected
                )),
                "{status}: sign-in, not sign-out"
            );
        }
        // The attempt is closed as an authentication rejection with its status, the
        // dead input stays fenced, nothing is unresolved, and the status row says
        // sign-in required.
        let closed = [(
            "authentication_rejected".to_owned(),
            Some(i64::from(status)),
            None,
            true,
        )];
        assert_eq!(
            store.legacy_token_receipts_for_test().unwrap(),
            closed,
            "{status}"
        );
        let fences = store.legacy_fenced_inputs_for_test().unwrap();
        assert_eq!(fences.len(), 1, "{status}: the dead input is fenced");
        assert!(!store.has_unresolved_legacy_refresh().unwrap(), "{status}");
        assert_eq!(
            store
                .load_collector_auth_row()
                .unwrap()
                .map(|row| row.state)
                .as_deref(),
            Some("sign_in_required"),
            "{status}"
        );
        // Later sessions keep reading the current token, and no second token request
        // is ever made.
        peer.log.wait_for("subscribe-received", 3).await;
        assert!(
            peer.log
                .tails("token:")
                .iter()
                .all(|token| token == "tok-1"),
            "{status}"
        );
        assert_eq!(
            issuer.requests.load(Ordering::SeqCst),
            1,
            "{status}: no second POST"
        );
        assert_eq!(
            store.legacy_token_receipts_for_test().unwrap(),
            closed,
            "{status}"
        );
        assert_eq!(
            store.legacy_fenced_inputs_for_test().unwrap(),
            fences,
            "{status}"
        );
        run.shut_down().await.expect("shutdown");
    }

    // The control: the access token is already past its usability when the same
    // answer arrives, so the unit is signed out with the same reason, and the stream
    // subscribes with no token and sends no event.
    let issuer = Issuer::spawn_with_error_body(
        Duration::ZERO,
        "tok-2",
        "ref-2",
        400,
        r#"{"error":"invalid_grant"}"#,
    )
    .await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let expired = i64::try_from(now).expect("epoch") - 100;
    // An access token whose own `exp` has passed: the pair is not usable.
    let access = {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        format!(
            "{}.{}.c2ln",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#),
            URL_SAFE_NO_PAD.encode(json!({"exp": expired}).to_string())
        )
    };
    let StoredCreds {
        creds,
        store,
        _directory,
    } = Creds::over_a_store(&issuer.url, &access, schedule_due());
    {
        let manager = creds.manager.lock().await;
        assert!(
            manager
                .access_token_for_sensitive_use()
                .is_err_and(|error| !error.is_sensitive_access_failure()),
            "the access token is past its usability"
        );
    }
    let creds = creds.held_by(&issuer);
    let manager = Arc::clone(&creds.manager);
    let peer = streaming_peer(Vec::new()).await;
    let mut rig = Rig::for_peer(&peer, creds);
    rig.store = Some(store.clone());
    let mut run = rig.start();
    sleep(Duration::from_secs(10 * 60)).await;
    assert_eq!(
        peer.log.count("subscribe-received"),
        0,
        "no subscription is sent"
    );
    assert!(
        peer.log.tails("token:").is_empty(),
        "no token is read for a session"
    );
    assert!(session_rows(&store).is_empty());
    assert!(run.events.try_recv().is_err(), "no event is sent");
    assert_eq!(issuer.requests.load(Ordering::SeqCst), 1, "no second POST");
    assert_eq!(peer.log.count("accept"), 0, "no connection is opened");
    assert_eq!(
        manager.lock().await.status_now().stop(),
        Some(AuthStop::SignedOut(SignOutReason::RefreshTokenRejected)),
        "signed out with the same reason"
    );
    assert_eq!(
        store.legacy_token_receipts_for_test().unwrap(),
        [("authentication_rejected".to_owned(), Some(400), None, true)],
        "the one refresh is closed as an authentication rejection"
    );
    assert_eq!(store.legacy_fenced_inputs_for_test().unwrap().len(), 1);
    assert_eq!(
        store
            .load_collector_auth_row()
            .unwrap()
            .map(|row| row.state)
            .as_deref(),
        Some("signed_out")
    );
    run.shut_down().await.expect("shutdown");
}

/// A wait is not a stop (C5 R56, C4 R57 b): `AuthUnavailable` is neither terminal
/// nor a loss of authority, so the attempt is an ordinary transport outage.
#[test]
fn an_auth_unavailable_error_is_not_terminal_and_a_sign_out_is() {
    use crate::{
        credentials::LegacyAuthManagerError,
        legacy_auth::{LegacyAuthError, RefusalReason, SignOutReason},
    };
    let wait = LegacyAuthManagerError::Auth(LegacyAuthError::AuthUnavailable { retry_at: None });
    assert!(!wait.is_terminal_auth_failure());
    assert!(!wait.is_sensitive_access_failure());
    for terminal in [
        LegacyAuthError::SignedOut {
            reason: SignOutReason::RefreshOutcomeUnknown,
        },
        LegacyAuthError::AccessTokenRejected,
        LegacyAuthError::OwnerApiRefused {
            reason: RefusalReason::Client,
        },
        LegacyAuthError::RegionConflict,
        LegacyAuthError::TokenUrlNotAllowed,
    ] {
        assert!(LegacyAuthManagerError::Auth(terminal).is_terminal_auth_failure());
    }
    // `sign_in_required` collects while the token is usable, so it is no stop.
    assert!(
        !LegacyAuthManagerError::Auth(LegacyAuthError::RefreshTokenRejected)
            .is_terminal_auth_failure()
    );
    assert!(!LegacyAuthManagerError::ObserverRefreshDisabled.is_terminal_auth_failure());
}

#[tokio::test(start_paused = true)]
async fn r6a_a_lock_wait_of_ninety_seconds_is_a_transport_outage_without_rows() {
    let peer = streaming_peer(Vec::new()).await;
    let creds = Creds::offline("tok-1");
    let holder = Arc::clone(&creds.manager).lock_owned().await;
    let (rig, _dir) = Rig::for_peer(&peer, creds).with_store();
    let store = rig.store.clone().expect("store");
    let started = Instant::now();
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - started, Duration::from_secs(90));
    assert_eq!(peer.log.count("accept"), 0);
    assert!(request_rows(&store).is_empty());
    assert!(session_rows(&store).is_empty());
    drop(holder);
    // With the lock free the next attempt (after 1 s) connects.
    peer.log.wait_for("subscribe-received", 1).await;
    run.shut_down().await.expect("shutdown");
}

// ---------------------------------------------- R6a step 5 (Admitting) and R29

fn offline_issuer_url() -> url::Url {
    url::Url::parse("http://127.0.0.1:9/").expect("URL")
}

#[tokio::test(start_paused = true)]
async fn r6a_a_lost_authority_in_admitting_drops_the_socket_and_ends_the_run() {
    let gate = Arc::new(Notify::new());
    let peer = gated_peer(Arc::clone(&gate), false).await;
    let authority = Arc::new(AtomicBool::new(true));
    let creds = Creds::with_authority(offline_issuer_url(), "tok-1", Arc::clone(&authority));
    let (rig, _dir) = Rig::for_peer(&peer, creds).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    // Step 2 has passed (the connection is made); the authority is lost while the
    // upgrade is held, so step 5 finds it gone.
    peer.log.wait_for("accept", 1).await;
    authority.store(false, Ordering::SeqCst);
    gate.notify_one();
    let result = run.finished(Duration::from_secs(5)).await;
    assert!(
        matches!(
            result,
            Err(StreamSupervisorError::CredentialAuthorityUnavailable)
        ),
        "{result:?}"
    );
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "success".to_owned())]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
    // Table 8.1: the socket is dropped, not closed, and nothing was subscribed.
    peer.log.wait_for("eof", 1).await;
    assert!(wire_labels(&peer).is_empty(), "{:?}", wire_labels(&peer));
    assert_eq!(peer.log.count("accept"), 1);
    assert!(run.events.try_recv().is_err(), "no event is sent");
}

#[tokio::test(start_paused = true)]
async fn r6a_a_terminal_credential_in_admitting_closes_the_socket_and_is_rechecked_later() {
    let gate = Arc::new(Notify::new());
    let peer = gated_peer(Arc::clone(&gate), false).await;
    let terminal = Arc::new(AtomicBool::new(false));
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.terminal = Some(Arc::clone(&terminal));
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("accept", 1).await;
    terminal.store(true, Ordering::SeqCst);
    gate.notify_one();
    peer.log.wait_for("close:", 1).await;
    let ended = Instant::now();
    // Table 8.1: the socket is closed (Close 1000), no subscribe is sent, and no event
    // is reported: C3 learns of the stop from the credential unit itself.
    assert_eq!(wire_labels(&peer), vec!["close:1000".to_owned()]);
    assert!(run.event_within(Duration::from_secs(140)).await.is_none());
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "success".to_owned())]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
    // The next check (step 2) follows after jittered(300 s, 0) = 150 s, finds the
    // stop again and opens no connection.
    sleep(Duration::from_secs(20)).await;
    let checks = peer.log.times("hook:LockAcquired");
    assert_eq!(checks.len(), 2);
    assert_eq!(checks[1] - ended, Duration::from_secs(150));
    assert_eq!(peer.log.count("accept"), 1);
    assert!(run.events.try_recv().is_err(), "no event is sent");
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r6a_a_lock_wait_of_ninety_seconds_in_admitting_closes_the_socket_and_is_an_outage() {
    let gate = Arc::new(Notify::new());
    let peer = gated_peer(Arc::clone(&gate), false).await;
    let creds = Creds::offline("tok-1");
    let manager = Arc::clone(&creds.manager);
    let (rig, _dir) = Rig::for_peer(&peer, creds).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    // Step 2 has passed; another holder takes the lock before the upgrade completes.
    peer.log.wait_for("accept", 1).await;
    let holder = Arc::clone(&manager).lock_owned().await;
    gate.notify_one();
    peer.log.wait_for("upgrade:", 1).await;
    let opened = peer.log.times("upgrade:")[0];
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - opened, Duration::from_secs(90));
    // Table 8.1: Close 1000 and no subscribe; the connect succeeded, the session failed.
    peer.log.wait_for("close:", 1).await;
    assert_eq!(wire_labels(&peer), vec!["close:1000".to_owned()]);
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "success".to_owned())]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
    // With the lock free the next attempt (after 1 s) connects and subscribes.
    drop(holder);
    peer.log.wait_for("subscribe-received", 1).await;
    assert_eq!(peer.log.count("accept"), 2);
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r29_pings_run_while_the_attempt_waits_in_admitting() {
    let gate = Arc::new(Notify::new());
    let peer = gated_peer(Arc::clone(&gate), true).await;
    let creds = Creds::offline("tok-1");
    let manager = Arc::clone(&creds.manager);
    let mut run = Rig::for_peer(&peer, creds).start();
    peer.log.wait_for("accept", 1).await;
    let holder = Arc::clone(&manager).lock_owned().await;
    gate.notify_one();
    peer.log.wait_for("upgrade:", 1).await;
    let opened = peer.log.times("upgrade:")[0];
    // Fifty seconds in Admitting, longer than three ping intervals: the Pings of R29
    // run from the moment the socket opens, and the peer's Pongs keep it alive.
    sleep(Duration::from_secs(50)).await;
    assert!(
        !peer.log.labels().contains(&"subscribe-received".to_owned()),
        "still waiting for the lock"
    );
    drop(holder);
    peer.log.wait_for("subscribe-received", 1).await;
    let pings: Vec<Duration> = peer
        .log
        .times("ping")
        .into_iter()
        .map(|at| at - opened)
        .collect();
    assert_eq!(
        pings[..3],
        [
            Duration::from_secs(15),
            Duration::from_secs(30),
            Duration::from_secs(45)
        ],
        "{pings:?}"
    );
    // The session went on normally once the lock was free, with the token read after
    // the wait.
    let labels = peer.log.labels();
    assert!(labels.contains(&"hook:TokenRead".to_owned()));
    assert_eq!(
        peer.log.count("accept"),
        1,
        "one socket, never declared dead"
    );
    assert!(run.events.try_recv().is_err(), "no event: the socket lived");
    // R30: data silence is counted from the subscribe write, not from the opening of
    // the socket: a parked car's 300 s limit ends the session 300 s after the subscribe
    // (the Pongs kept the socket alive meanwhile), although the socket is 350 s old.
    let subscribed = peer.log.times("subscribe-received")[0];
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - subscribed, Duration::from_secs(300));
    run.shut_down().await.expect("shutdown");
}

/// A peer with a 16-byte pipe that completes the upgrade (after `gate`, if any) and
/// then reads nothing: the client's subscribe frame cannot be written.
async fn stalled_peer(gate: Option<Arc<Notify>>) -> Peer {
    Peer::spawn_with_pipe(16, move |conn| {
        let gate = gate.clone();
        async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            let _ws = conn.ws().await;
            std::future::pending::<()>().await;
        }
    })
    .await
}

#[tokio::test(start_paused = true)]
async fn r32_a_subscribe_write_that_stalls_ends_at_ten_seconds_or_the_dead_socket_deadline() {
    // The peer reads nothing, so the subscribe frame cannot be flushed. With the lock
    // free the write starts at once and ends at the 10 s write limit (R32); a Ping
    // cannot be written to a peer that is not reading either, so a stalled write is
    // the dead-socket case (R29).
    let peer = stalled_peer(None).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("upgrade:", 1).await;
    let opened = peer.log.times("upgrade:")[0];
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - opened, Duration::from_secs(10));
    assert_eq!(
        request_outcomes(&store),
        vec![
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "transport_error".to_owned()),
        ]
    );
    run.shut_down().await.expect("shutdown");

    // After 40 s in Admitting the write starts with only 5 s left to the dead-socket
    // deadline (45 s after the opening), which then ends it: not at 40 + 10 = 50 s.
    let gate = Arc::new(Notify::new());
    let peer = stalled_peer(Some(Arc::clone(&gate))).await;
    let creds = Creds::offline("tok-1");
    let manager = Arc::clone(&creds.manager);
    let (rig, _dir) = Rig::for_peer(&peer, creds).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("accept", 1).await;
    let holder = Arc::clone(&manager).lock_owned().await;
    gate.notify_one();
    peer.log.wait_for("upgrade:", 1).await;
    let opened = peer.log.times("upgrade:")[0];
    sleep(Duration::from_secs(40)).await;
    drop(holder);
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - opened, Duration::from_secs(45));
    assert_eq!(
        request_outcomes(&store),
        vec![
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "transport_error".to_owned()),
        ]
    );
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r31_a_server_close_while_admitting_ends_the_session_at_once() {
    // The peer sends Close 1013 as soon as the upgrade completes, while another holder
    // keeps the manager lock: the end is the session's, not the 90 s lock wait's. No
    // subscribe is sent, and the session row is `failed`.
    let gate = Arc::new(Notify::new());
    let peer_gate = Arc::clone(&gate);
    let peer = Peer::spawn(move |conn| {
        let gate = Arc::clone(&peer_gate);
        async move {
            if conn.index == 0 {
                gate.notified().await;
                let mut ws = conn.ws().await;
                ws.close(1013).await;
                ws.drain().await;
            } else {
                let mut ws = conn.ws().await;
                ws.subscribe().await;
                ws.drain().await;
            }
        }
    })
    .await;
    let creds = Creds::offline("tok-1");
    let manager = Arc::clone(&creds.manager);
    let (rig, _dir) = Rig::for_peer(&peer, creds).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("accept", 1).await;
    let holder = Arc::clone(&manager).lock_owned().await;
    gate.notify_one();
    peer.log.wait_for("upgrade:", 1).await;
    let opened = peer.log.times("upgrade:")[0];
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now(), opened, "no wait for the lock");
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "success".to_owned())]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
    drop(holder);
    peer.log.wait_for("subscribe-received", 1).await;
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r29_a_silent_socket_is_dead_after_three_intervals_even_while_admitting() {
    // The peer completes the upgrade and then neither reads nor answers, while another
    // holder keeps the manager lock: the Pings go unanswered, and 45 s after the socket
    // opened (three intervals) the session ends as a transport failure, without a
    // subscribe row (R29, table 8.1).
    let gate = Arc::new(Notify::new());
    let peer_gate = Arc::clone(&gate);
    let peer = Peer::spawn(move |conn| {
        let gate = Arc::clone(&peer_gate);
        async move {
            if conn.index == 0 {
                gate.notified().await;
                let _ws = conn.ws().await;
                std::future::pending::<()>().await;
            } else {
                let mut ws = conn.ws().await;
                ws.subscribe().await;
                ws.drain().await;
            }
        }
    })
    .await;
    let creds = Creds::offline("tok-1");
    let manager = Arc::clone(&creds.manager);
    let (rig, _dir) = Rig::for_peer(&peer, creds).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("accept", 1).await;
    let holder = Arc::clone(&manager).lock_owned().await;
    gate.notify_one();
    peer.log.wait_for("upgrade:", 1).await;
    let opened = peer.log.times("upgrade:")[0];
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - opened, Duration::from_secs(45));
    assert_eq!(
        request_outcomes(&store),
        vec![("stream_connect".to_owned(), "success".to_owned())]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
    drop(holder);
    peer.log.wait_for("subscribe-received", 1).await;
    run.shut_down().await.expect("shutdown");
}

/// One run in which the first connection sends `hello` (code 200, a 30 s idle limit) as
/// soon as its upgrade completes, while another holder keeps the manager lock, and the
/// ping times that followed are read against the hello.
async fn pings_after_a_hello_before_the_subscribe(binary: bool) -> Vec<Duration> {
    let gate = Arc::new(Notify::new());
    let peer_gate = Arc::clone(&gate);
    let peer = Peer::spawn(move |conn| {
        let gate = Arc::clone(&peer_gate);
        async move {
            gate.notified().await;
            let mut ws = conn.ws().await;
            let hello = hello_frame(Some(200), Some(30_000));
            if binary {
                ws.send_binary(hello.as_bytes()).await;
            } else {
                ws.send(&hello).await;
            }
            ws.log.mark("hello-sent");
            ws.subscribe().await;
            ws.drain().await;
        }
    })
    .await;
    let creds = Creds::offline("tok-1");
    let manager = Arc::clone(&creds.manager);
    let mut run = Rig::for_peer(&peer, creds).start();
    peer.log.wait_for("accept", 1).await;
    let holder = Arc::clone(&manager).lock_owned().await;
    gate.notify_one();
    peer.log.wait_for("hello-sent", 1).await;
    let hello_at = peer.log.times("hello-sent")[0];
    sleep(Duration::from_secs(25)).await;
    drop(holder);
    peer.log.wait_for("subscribe-received", 1).await;
    let pings = peer
        .log
        .times("ping")
        .into_iter()
        .map(|at| at - hello_at)
        .collect();
    run.shut_down().await.expect("shutdown");
    pings
}

#[tokio::test(start_paused = true)]
async fn r29_a_hello_before_the_subscribe_sets_the_ping_interval() {
    // A hello that arrives while the attempt waits for the lock applies the server's
    // idle limit at once (R27, R29): 30 s gives a 10 s interval, so the next Ping is
    // 10 s after the hello, in a Text message and in a UTF-8 Binary one alike.
    for binary in [false, true] {
        let pings = pings_after_a_hello_before_the_subscribe(binary).await;
        assert_eq!(
            pings[..2],
            [Duration::from_secs(10), Duration::from_secs(20)],
            "binary {binary}: {pings:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_successful_hello_alone_keeps_the_session_and_data_follows() {
    let peer = streaming_peer(vec![hello_frame(Some(200), Some(30_000)), u(T0, V1)]).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    assert_eq!(telemetry_time(&run.event().await), T0);
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r31_an_upgrade_refusal_on_an_override_endpoint_is_a_refusal_row_and_two_make_an_event() {
    // The endpoint of the rig is an override (not one of Tesla's two), so a 401 or
    // 403 on the upgrade is the provider's statement about its key (R31, R59).
    let peer = Peer::spawn(|conn| async move { conn.refuse_upgrade(401, None).await }).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    // One refusal is only evidence: no event, and the next attempt waits 2.5 s.
    peer.log.wait_for("refused:401", 1).await;
    assert!(run.event_within(Duration::from_secs(2)).await.is_none());
    assert_eq!(run.event().await, StreamEvent::AuthRejected);
    let times = peer.log.times("refused:");
    assert_eq!(times[1] - times[0], Duration::from_millis(2_500));
    run.shut_down().await.expect("shutdown");
    let rows = request_rows(&store);
    assert_eq!(rows[0].operation, "stream_connect");
    assert_eq!(rows[0].outcome, "authentication_rejected");
    assert_eq!(rows[0].http_status, Some(401));
    assert!(rows.iter().all(|row| row.operation == "stream_connect"));
    assert!(
        session_rows(&store)
            .iter()
            .all(|row| row.outcome == "failed")
    );
}

#[tokio::test(start_paused = true)]
async fn r13_a_failed_subscribe_write_is_a_transport_error_and_a_failed_session() {
    // The peer completes the upgrade and drops its end at once, so the subscribe
    // write finds a closed pipe.
    let peer = Peer::spawn(|conn| async move {
        drop(conn.ws().await);
    })
    .await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        request_outcomes(&store),
        vec![
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "transport_error".to_owned()),
        ]
    );
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].outcome, "failed");
}

// ------------------------------------------------------------ Admitting dispatch (R27 to R33)

/// A frame the peer sends to a socket that has no subscription yet, in either encoding.
#[derive(Clone)]
enum Wire {
    Text(String),
    Binary(Vec<u8>),
}

impl Wire {
    /// The same frame once as Text and once as UTF-8 Binary.
    fn both(frame: &str) -> [Self; 2] {
        [
            Self::Text(frame.to_owned()),
            Self::Binary(frame.as_bytes().to_vec()),
        ]
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Binary(_) => "binary",
        }
    }
}

/// What a scripted connection does after its frames.
#[derive(Clone)]
enum Then {
    /// Reads until the client leaves: no subscribe frame is expected.
    Drain,
    /// Reads the subscribe frame, sends these frames, then reads until the client leaves.
    Subscribe(Vec<String>),
}

/// The script of one connection: the frames it sends right after the upgrade, then `then`.
#[derive(Clone)]
struct Script {
    frames: Vec<Wire>,
    then: Then,
}

impl Script {
    fn ends(frames: Vec<Wire>) -> Self {
        Self {
            frames,
            then: Then::Drain,
        }
    }
}

/// A peer whose connection `i` waits for its gate, completes the upgrade, sends
/// `scripts[i].frames`, logs `sent`, then does `scripts[i].then`. A connection past the
/// scripts is served at once as a normal stream.
async fn admitting_peer(gates: &[Arc<Notify>], scripts: Vec<Script>) -> Peer {
    let gates = gates.to_vec();
    Peer::spawn(move |conn| {
        let gate = gates.get(conn.index).cloned();
        let script = scripts.get(conn.index).cloned();
        async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            let mut ws = conn.ws().await;
            let Some(script) = script else {
                ws.subscribe().await;
                ws.drain().await;
                return;
            };
            for frame in &script.frames {
                match frame {
                    Wire::Text(text) => ws.send(text).await,
                    Wire::Binary(bytes) => ws.send_binary(bytes).await,
                }
            }
            ws.log.mark("sent");
            match script.then {
                Then::Drain => {}
                Then::Subscribe(after) => {
                    ws.subscribe().await;
                    for frame in &after {
                        ws.send(frame).await;
                    }
                }
            }
            ws.drain().await;
        }
    })
    .await
}

/// A run against an [`admitting_peer`], with the manager available to hold.
struct HeldRun {
    peer: Peer,
    run: Running,
    store: crate::db::HubStore,
    manager: Arc<Mutex<LegacyAuthManager>>,
    gates: Vec<Arc<Notify>>,
    holder: Option<OwnedMutexGuard<LegacyAuthManager>>,
    _dir: tempfile::TempDir,
}

impl HeldRun {
    async fn start(scripts: Vec<Script>) -> Self {
        let gates: Vec<Arc<Notify>> = scripts.iter().map(|_| Arc::new(Notify::new())).collect();
        let peer = admitting_peer(&gates, scripts).await;
        let creds = Creds::offline("tok-1");
        let manager = Arc::clone(&creds.manager);
        let (rig, dir) = Rig::for_peer(&peer, creds).with_store();
        let store = rig.store.clone().expect("store");
        let run = rig.start();
        Self {
            peer,
            run,
            store,
            manager,
            gates,
            holder: None,
            _dir: dir,
        }
    }

    /// Takes the manager once the attempt `index` is connecting (its credential check has
    /// released the lock), then lets that connection's upgrade complete: the attempt is
    /// then in Admitting with the lock held by the test.
    async fn open_held(&mut self, index: usize) {
        self.peer.log.wait_for("accept", index + 1).await;
        self.holder = Some(Arc::clone(&self.manager).lock_owned().await);
        self.gates[index].notify_one();
        self.peer.log.wait_for("upgrade:", index + 1).await;
    }

    /// Lets the next attempt's credential check take the lock again.
    fn release(&mut self) {
        self.holder = None;
    }

    /// The instant of the `n`th (one-based) log entry that starts with `prefix`.
    fn at(&self, prefix: &str, n: usize) -> Instant {
        self.peer.log.times(prefix)[n - 1]
    }
}

fn connect_only() -> Vec<(String, String)> {
    vec![("stream_connect".to_owned(), "success".to_owned())]
}

fn all_sessions_failed(store: &crate::db::HubStore, count: usize) {
    let sessions = session_rows(store);
    assert_eq!(sessions.len(), count, "{sessions:?}");
    assert!(
        sessions.iter().all(|row| row.outcome == "failed"),
        "{sessions:?}"
    );
}

fn hello_json(members: serde_json::Value) -> String {
    let mut frame = json!({"msg_type": "control:hello"});
    frame
        .as_object_mut()
        .expect("an object")
        .extend(members.as_object().expect("an object").clone());
    frame.to_string()
}

/// The event an immediately ending Admitting attempt makes: it follows the frame at
/// once, so one paused second is the bound, not the rig's two-hour default.
async fn ended_event(held: &mut HeldRun) -> StreamEvent {
    held.run
        .event_within(Duration::from_secs(1))
        .await
        .expect("an event within one paused second of the frame")
}

#[tokio::test(start_paused = true)]
async fn r27_a_hello_401_while_admitting_is_a_refusal_and_two_make_auth_rejected() {
    // Two gated attempts, one answering Text and the other Binary, each with the manager
    // held by another holder: the Close follows the frame at once, the first refusal makes
    // no event and the second consecutive one makes AuthRejected. The connect succeeded
    // and no subscribe was begun, so only the session rows are `failed`.
    let frame = hello_json(json!({"code": 401}));
    let [text, binary] = Wire::both(&frame);
    let mut held = HeldRun::start(vec![Script::ends(vec![text]), Script::ends(vec![binary])]).await;

    held.open_held(0).await;
    held.peer.log.wait_for("close:", 1).await;
    assert_eq!(
        held.at("close:", 1),
        held.at("sent", 1),
        "Close at the mark"
    );
    // The holder is released and reacquired between the attempts: the credential check of
    // the next attempt takes the lock too. The first refusal made no event.
    held.release();
    held.peer.log.wait_for("accept", 2).await;
    assert!(
        held.run.events.try_recv().is_err(),
        "one refusal is no event"
    );
    held.open_held(1).await;
    assert_eq!(ended_event(&mut held).await, StreamEvent::AuthRejected);
    assert_eq!(
        held.at("close:", 2),
        held.at("sent", 2),
        "Close at the mark"
    );
    assert_eq!(Instant::now(), held.at("sent", 2));
    assert_eq!(
        wire_labels(&held.peer),
        vec!["close:1000".to_owned(), "close:1000".to_owned()]
    );
    assert_eq!(
        request_outcomes(&held.store),
        vec![
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_connect".to_owned(), "success".to_owned()),
        ]
    );
    all_sessions_failed(&held.store, 2);

    // With the lock free a later attempt connects and subscribes.
    held.release();
    held.peer.log.wait_for("subscribe-received", 1).await;
    assert_eq!(held.peer.log.count("accept"), 3);
    held.run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r27_busy_and_unknown_hellos_while_admitting_end_at_the_frame() {
    for code in [429, 503, 500] {
        for wire in Wire::both(&hello_json(json!({"code": code}))) {
            let name = wire.name();
            let mut held = HeldRun::start(vec![Script::ends(vec![wire])]).await;
            held.open_held(0).await;
            assert_eq!(
                ended_event(&mut held).await,
                StreamEvent::TransportUnavailable,
                "{code} {name}"
            );
            let sent = held.at("sent", 1);
            assert_eq!(
                Instant::now(),
                sent,
                "{code} {name}: the event follows the frame"
            );
            held.peer.log.wait_for("close:", 1).await;
            assert_eq!(held.at("close:", 1), sent, "{code} {name}");
            // No subscription of any kind, and the receipts of an attempt that never
            // subscribed.
            assert_eq!(
                wire_labels(&held.peer),
                vec!["close:1000".to_owned()],
                "{code} {name}"
            );
            assert_eq!(held.peer.log.count("subscribe-received"), 0);
            assert_eq!(
                request_outcomes(&held.store),
                connect_only(),
                "{code} {name}"
            );
            let sessions = session_rows(&held.store);
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].outcome, "failed", "{code} {name}");
            assert_eq!(sessions[0].unsubscribe_receipt_id, None);
            held.run.shut_down().await.expect("shutdown");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn r28_a_data_error_while_admitting_ends_the_attempt_with_its_class() {
    let error = |members: serde_json::Value| {
        let mut frame = json!({"msg_type": "data:error", "value": "text"});
        frame
            .as_object_mut()
            .expect("an object")
            .extend(members.as_object().expect("an object").clone());
        frame.to_string()
    };
    let cases = [
        // Matching and absent tags, the Vehicle family and anything else.
        (
            error(json!({"tag": TAG, "error_type": "vehicle_unavailable"})),
            StreamEvent::VehicleOffline,
        ),
        (
            error(json!({"error_type": "vehicle_unavailable"})),
            StreamEvent::VehicleOffline,
        ),
        (
            error(json!({"tag": TAG, "error_type": "client_error"})),
            StreamEvent::TransportUnavailable,
        ),
        (
            error(json!({"error_type": "client_error"})),
            StreamEvent::TransportUnavailable,
        ),
        (
            error(json!({"tag": TAG})),
            StreamEvent::TransportUnavailable,
        ),
        // A hello that carries both a refusing code and a Vehicle error is a Vehicle end,
        // never a refusal: the `error_type` string comes first.
        (
            hello_json(json!({"code": 401, "error_type": "vehicle_unavailable"})),
            StreamEvent::VehicleOffline,
        ),
    ];
    for (frame, expected) in cases {
        for wire in Wire::both(&frame) {
            let name = wire.name();
            let mut held = HeldRun::start(vec![Script::ends(vec![wire])]).await;
            held.open_held(0).await;
            assert_eq!(ended_event(&mut held).await, expected, "{frame} {name}");
            let sent = held.at("sent", 1);
            assert_eq!(Instant::now(), sent, "{frame} {name}");
            held.peer.log.wait_for("close:", 1).await;
            assert_eq!(wire_labels(&held.peer), vec!["close:1000".to_owned()]);
            assert_eq!(held.at("close:", 1), sent);
            assert_eq!(held.peer.log.count("subscribe-received"), 0);
            assert_eq!(
                request_outcomes(&held.store),
                connect_only(),
                "{frame} {name}"
            );
            all_sessions_failed(&held.store, 1);
            // No refusal was counted: a second run of the same class would not report
            // AuthRejected, and the events stay those of the class.
            assert!(held.run.events.try_recv().is_err(), "{frame} {name}");
            held.run.shut_down().await.expect("shutdown");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn r28_foreign_errors_and_malformed_frames_while_admitting_are_not_counted() {
    let foreign =
        json!({"msg_type": "data:error", "tag": "9999", "error_type": "vehicle_unavailable"});
    let non_string_tag =
        json!({"msg_type": "data:error", "tag": 4242, "error_type": "vehicle_unavailable"});
    let unknown_type = json!({"msg_type": "control:other", "code": 401});
    let update = u(T0, V1);
    let mut frames = vec![
        Wire::Text(foreign.to_string()),
        Wire::Binary(foreign.to_string().into_bytes()),
        Wire::Text(non_string_tag.to_string()),
        Wire::Text(unknown_type.to_string()),
        Wire::Text("{not json".to_owned()),
        Wire::Text("[1,2]".to_owned()),
        Wire::Binary(vec![0xff, 0xfe, b'{']),
        // Application frames before the subscribe succeeded count for nothing.
        Wire::Text(update.clone()),
        Wire::Binary(update.into_bytes()),
        Wire::Text(hello_json(json!({"code": 200, "connection_timeout": 0}))),
    ];
    // Three malformed frames in a row would end a subscribed session; here they never
    // count towards that.
    frames.extend((0..3).map(|_| Wire::Text("garbage".to_owned())));
    let script = Script {
        frames,
        then: Then::Subscribe(vec![u(T0, V1)]),
    };
    let mut held = HeldRun::start(vec![script]).await;
    held.open_held(0).await;
    held.peer.log.wait_for("sent", 1).await;
    // The manager stays held for one paused second: the same socket is still waiting.
    sleep(Duration::from_secs(1)).await;
    assert_eq!(held.peer.log.count("subscribe-received"), 0);
    assert_eq!(wire_labels(&held.peer), Vec::<String>::new());
    assert!(held.run.events.try_recv().is_err(), "nothing was emitted");
    held.release();
    held.peer.log.wait_for("subscribe-received", 1).await;
    assert_eq!(held.run.event().await, StreamEvent::Healthy);
    assert_eq!(telemetry_time(&held.run.event().await), T0);
    held.run.shut_down().await.expect("shutdown");
    assert_eq!(
        held.peer.log.count("accept"),
        1,
        "the same socket subscribed"
    );
    assert_eq!(
        held.run.session_counts(),
        vec![super::session::FrameCounts {
            accepted: 1,
            ..Default::default()
        }]
    );
}

#[tokio::test(start_paused = true)]
async fn r33_an_oversize_message_while_admitting_is_a_protocol_end() {
    // A hello whose trailing white space makes the whole message exactly one byte over
    // the bound.
    let mut oversize = hello_json(json!({"code": 401}));
    oversize.push_str(&" ".repeat(K_MAX_MESSAGE + 1 - oversize.len()));
    assert_eq!(oversize.len(), K_MAX_MESSAGE + 1);
    for wire in [
        Wire::Text(oversize.clone()),
        Wire::Binary(oversize.clone().into_bytes()),
    ] {
        let name = wire.name();
        let mut held = HeldRun::start(vec![Script::ends(vec![wire])]).await;
        held.open_held(0).await;
        assert_eq!(
            ended_event(&mut held).await,
            StreamEvent::ProtocolViolation,
            "{name}"
        );
        let sent = held.at("sent", 1);
        assert_eq!(Instant::now(), sent, "{name}: the event follows the frame");
        held.peer.log.wait_for("close:", 1).await;
        assert_eq!(held.at("close:", 1), sent, "{name}");
        assert_eq!(
            wire_labels(&held.peer),
            vec!["close:1009".to_owned()],
            "{name}"
        );
        assert_eq!(held.peer.log.count("subscribe-received"), 0);
        assert!(
            held.peer
                .log
                .labels()
                .iter()
                .all(|label| !label.contains("unsubscribe")),
            "{name}: no unsubscribe frame"
        );
        assert_eq!(request_outcomes(&held.store), connect_only(), "{name}");
        all_sessions_failed(&held.store, 1);
        held.run.shut_down().await.expect("shutdown");
    }
}

#[tokio::test(start_paused = true)]
async fn r10_a_classified_admitting_end_waits_at_most_two_seconds_for_close() {
    // The peer sends a hello 500 with no `error_type`, reads the client's Close and then
    // neither answers it nor flushes nor drops the socket: the Hub's handshake gives up
    // after two seconds, within the three-second bound, and the attempt ends as an outage.
    let gate = Arc::new(Notify::new());
    let peer_gate = Arc::clone(&gate);
    let peer = Peer::spawn(move |conn| {
        let gate = Arc::clone(&peer_gate);
        async move {
            if conn.index != 0 {
                let mut ws = conn.ws().await;
                ws.subscribe().await;
                ws.drain().await;
                return;
            }
            gate.notified().await;
            let mut ws = conn.ws().await;
            ws.reply_to_close = false;
            ws.send(&hello_json(json!({"code": 500}))).await;
            ws.log.mark("sent");
            let _ = ws.message().await;
            std::future::pending::<()>().await;
        }
    })
    .await;
    let creds = Creds::offline("tok-1");
    let manager = Arc::clone(&creds.manager);
    let (rig, _dir) = Rig::for_peer(&peer, creds).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("accept", 1).await;
    let holder = Arc::clone(&manager).lock_owned().await;
    gate.notify_one();
    peer.log.wait_for("close:", 1).await;
    let mark = peer.log.times("sent")[0];
    assert_eq!(peer.log.times("close:")[0], mark, "Close 1000 at the mark");
    assert_eq!(
        run.event_within(Duration::from_secs(3)).await,
        Some(StreamEvent::TransportUnavailable),
        "the handshake gives up within the three-second bound"
    );
    let elapsed = Instant::now() - mark;
    assert_eq!(elapsed, Duration::from_secs(2));
    assert!(elapsed < Duration::from_secs(3));
    assert_eq!(wire_labels(&peer), vec!["close:1000".to_owned()]);
    assert_eq!(peer.log.count("subscribe-received"), 0);
    assert_eq!(request_outcomes(&store), connect_only());
    all_sessions_failed(&store, 1);
    drop(holder);
    run.shut_down().await.expect("shutdown");
}
