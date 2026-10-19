// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C4 part P3 (the read loop of one session): SC31 to SC36, SC38, SC43 to
// SC45, SC47, SC52, SC59 to SC63 and SC81 to SC86, plus tests of the rules the part owns
// that no SC test names (R29 liveness by any inbound frame, R30 silence clocks, R31 close
// class, the session clocks). Each test runs one or two sessions against an in-process
// loopback peer over an in-memory pipe under paused time, with exact expected events, rows
// and times. Synthetic data only; no test touches the network.

use std::{sync::Arc, time::Duration};

use serde_json::json;
use tokio::{
    sync::Notify,
    time::{Instant, sleep},
};
use tokio_tungstenite::tungstenite::{Message, protocol::frame::coding::CloseCode};

use super::{
    StreamEvent, StreamPowerGate, StreamSupervisorError, StreamUpdate,
    liveness::SessionClocks,
    policy::FailureClass,
    session::{FrameCounts, close_failure_class},
    test_peer::*,
};

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

const UNSUBSCRIBE: &str = r#"frame:{"msg_type":"data:unsubscribe","tag":"4242"}"#;

/// The frame `data:error` of another vehicle.
fn error_frame_for_tag(tag: &str, error_type: &str) -> String {
    json!({"msg_type": "data:error", "tag": tag, "error_type": error_type, "value": "text"})
        .to_string()
}

/// `U` for another vehicle's tag.
fn u_for_tag(tag: &str, time: i64) -> String {
    json!({"msg_type": "data:update", "tag": tag, "value": format!("{time},{V1}")}).to_string()
}

/// V1 with one column replaced; `index` counts columns from 0 in `STREAM_COLUMNS` order.
fn v1_with(index: usize, text: &str) -> String {
    let mut elements: Vec<&str> = V1.split(',').collect();
    elements[index] = text;
    elements.join(",")
}

const SPEED: usize = 10;

/// The record V1 parses to at sample time `time` (spec section 9 notation).
fn v1_update(time: i64) -> StreamUpdate {
    StreamUpdate {
        tag: TAG.to_owned(),
        timestamp_ms: time,
        elevation: Some(25.0),
        est_heading: None,
        est_lat: Some(10.5),
        est_lng: Some(20.25),
        est_range: Some(230.5),
        heading: Some(182.0),
        odometer: Some(12345.6),
        power: Some(15),
        range: Some(241.25),
        shift_state: Some("D".to_owned()),
        soc: Some(80.0),
        speed: Some(30.0),
    }
}

/// The update and the mono instant of a `Telemetry` event.
fn telemetry_parts(event: StreamEvent) -> (StreamUpdate, Instant) {
    match event {
        StreamEvent::Telemetry { update, queued_at } => (*update, queued_at),
        other => panic!("expected telemetry, got {other:?}"),
    }
}

/// Reads the next event and checks it is `Telemetry` with exactly this update.
async fn expect_telemetry(run: &mut Running, expected: StreamUpdate) -> Instant {
    let (update, queued_at) = telemetry_parts(run.event().await);
    assert_eq!(update, expected);
    queued_at
}

async fn expect_no_event_for(run: &mut Running, wait: Duration) {
    if let Some(event) = run.event_within(wait).await {
        panic!("no event was expected within {wait:?}, got {event:?}");
    }
}

/// A peer that reads the subscribe frame, sends each of `frames`, then reads (and so
/// answers pings) until the client closes.
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

/// Waits `wait` on the peer's side of a connection while still reading, so its
/// automatic Pong answers any Ping that arrives meanwhile.
async fn idle(ws: &mut Ws, wait: Duration) {
    let deadline = Instant::now() + wait;
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => return,
            message = ws.message() => {
                if message.is_none() {
                    return;
                }
            }
        }
    }
}

/// The frames and Close codes a peer logged, in order, as `frame:...` and `close:...`.
fn wire_labels(peer: &Peer) -> Vec<String> {
    peer.log
        .labels()
        .into_iter()
        .filter(|label| label.starts_with("frame:") || label.starts_with("close:"))
        .collect()
}

/// How long after `since` the `nth` (1-based) connection attempt was accepted.
async fn accepted_after(peer: &Peer, since: Instant, nth: usize) -> Duration {
    peer.log.wait_for("accept", nth).await;
    peer.log.times("accept")[nth - 1] - since
}

// ------------------------------------------------------------------ SC31 to SC34

#[tokio::test(start_paused = true)]
async fn sc31_a_frame_for_another_tag_is_dropped() {
    let peer = streaming_peer(vec![u_for_tag("4243", T0), u(T0, V1)]).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    expect_no_event_for(&mut run, secs(5)).await;
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc32_a_repeated_sample_is_not_sent_twice() {
    let faster = v1_with(SPEED, "31");
    let peer = streaming_peer(vec![u(T0, V1), u(T0, V1), u(T0, &faster)]).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    let mut expected = v1_update(T0);
    expected.speed = Some(31.0);
    expect_telemetry(&mut run, expected).await;
    expect_no_event_for(&mut run, secs(5)).await;
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc33_an_unknown_message_type_is_ignored() {
    let peer = streaming_peer(vec![
        r#"{"msg_type":"control:other"}"#.to_owned(),
        u(T0, V1),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    expect_no_event_for(&mut run, secs(5)).await;
    run.shut_down().await.expect("shutdown");
    // The ignored message is counted and nothing else is.
    assert_eq!(
        run.session_counts(),
        vec![FrameCounts {
            accepted: 1,
            unknown_messages: 1,
            ..FrameCounts::default()
        }]
    );
}

#[tokio::test(start_paused = true)]
async fn sc34_binary_messages_are_decoded_as_utf8() {
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        ws.send_binary(u(T0, V1).as_bytes()).await;
        ws.drain().await;
    })
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    run.shut_down().await.expect("shutdown");

    // Three binary messages of invalid UTF-8 are three rejected frames.
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        for _ in 0..3 {
            ws.send_binary(&[0xff, 0xfe, 0xfd]).await;
        }
        ws.drain().await;
    })
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::ProtocolViolation);
    run.shut_down().await.expect("shutdown");
}

// ----------------------------------------------------------------------- SC35, SC36

/// A frame of 11 elements, one short of the 12 that a frame must have (R19).
fn short_frame() -> String {
    json!({
        "msg_type": "data:update",
        "tag": TAG,
        "value": format!("{T0},25,10.5,20.25,230.5,182,12345.6,15,241.25,D,80"),
    })
    .to_string()
}

#[tokio::test(start_paused = true)]
async fn sc35_three_rejected_frames_in_a_row_end_the_session() {
    let peer = streaming_peer(vec![short_frame(), short_frame(), short_frame()]).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::ProtocolViolation);
    let ended = Instant::now();
    assert_eq!(accepted_after(&peer, ended, 2).await, secs(1));
    run.shut_down().await.expect("shutdown");
    // The peer receives the unsubscribe: the Hub ended a session that existed.
    assert!(wire_labels(&peer).contains(&UNSUBSCRIBE.to_owned()));
    assert_eq!(
        request_outcomes(&store)[..3],
        [
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "protocol_error".to_owned()),
            ("stream_unsubscribe".to_owned(), "success".to_owned()),
        ]
    );
    assert_eq!(session_rows(&store)[0].outcome, "failed");
}

#[tokio::test(start_paused = true)]
async fn sc35_a_good_frame_between_rejected_ones_resets_the_streak() {
    let peer = streaming_peer(vec![
        short_frame(),
        short_frame(),
        u(T0, V1),
        short_frame(),
        short_frame(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    expect_no_event_for(&mut run, secs(10)).await;
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r25_frames_for_another_tag_count_towards_the_invalid_streak() {
    // R33 lists R25 among the rejected frames: three in a row end the session.
    let peer = streaming_peer(vec![
        u_for_tag("4243", T0),
        u_for_tag("4243", T0),
        u_for_tag("4243", T0),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::ProtocolViolation);
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        run.session_counts()[0],
        FrameCounts {
            rejected: 3,
            ..FrameCounts::default()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn r17_a_message_that_is_not_an_object_with_a_string_msg_type_is_a_rejected_frame() {
    // Not JSON and a JSON array are two rejected frames; the good frame resets the streak;
    // a numeric msg_type, an object without one and a bare string are three more.
    let peer = streaming_peer(vec![
        "not json".to_owned(),
        "[1,2]".to_owned(),
        u(T0, V1),
        r#"{"msg_type":7}"#.to_owned(),
        "{}".to_owned(),
        r#""text""#.to_owned(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    assert_eq!(run.event().await, StreamEvent::ProtocolViolation);
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        run.session_counts()[0],
        FrameCounts {
            accepted: 1,
            rejected: 5,
            ..FrameCounts::default()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn c4r3_n3_a_one_element_array_with_a_string_is_not_a_message_head() {
    // R17: a message is a top-level object with a string `msg_type`. serde also reads
    // a struct from a sequence, so `["control:other"]` used to be an unknown message;
    // it is a rejected frame, and three in a row (here with leading whitespace on one)
    // end the session before the good frame that follows them is read.
    let peer = streaming_peer(vec![
        r#"["control:other"]"#.to_owned(),
        "  \n[\"data:update\"]".to_owned(),
        r#"["control:hello"]"#.to_owned(),
        u(T0, V1),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::ProtocolViolation);
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        run.session_counts()[0],
        FrameCounts {
            rejected: 3,
            ..FrameCounts::default()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn r27_a_hello_member_of_the_wrong_type_counts_as_absent() {
    // A string code and a numeric error_type are absent: the hello is a success.
    let peer = streaming_peer(vec![
        r#"{"msg_type":"control:hello","code":"401","error_type":5}"#.to_owned(),
        u(T0, V1),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    expect_no_event_for(&mut run, secs(5)).await;
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r27_a_hello_with_code_503_is_busy_and_ends_the_session_with_an_unsubscribe() {
    let peer = streaming_peer(vec![hello_frame(Some(503), None)]).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    // Busy with no retry-after is the Transport delay: 2 s at u = 0 gives 1 s.
    assert_eq!(accepted_after(&peer, Instant::now(), 2).await, secs(1));
    run.shut_down().await.expect("shutdown");
    assert!(wire_labels(&peer).contains(&UNSUBSCRIBE.to_owned()));
    assert_eq!(
        request_outcomes(&store)[..3],
        [
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "transport_error".to_owned()),
            ("stream_unsubscribe".to_owned(), "success".to_owned()),
        ]
    );
    assert_eq!(session_rows(&store)[0].outcome, "transport_ended");
}

/// A `data:update` of exactly `total` bytes, padded with an extra member the unit
/// ignores (R17).
fn padded_update(total: usize) -> String {
    let make = |pad: String| {
        json!({
            "msg_type": "data:update",
            "tag": TAG,
            "value": format!("{T0},{V1}"),
            "pad": pad,
        })
        .to_string()
    };
    let fill = total - make(String::new()).len();
    let frame = make("x".repeat(fill));
    assert_eq!(frame.len(), total);
    frame
}

#[tokio::test(start_paused = true)]
async fn sc36_a_message_above_65536_bytes_ends_the_session_with_close_1009() {
    let peer = streaming_peer(vec![padded_update(65_537)]).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::ProtocolViolation);
    run.shut_down().await.expect("shutdown");
    let wire = wire_labels(&peer);
    assert_eq!(
        wire.last().map(String::as_str),
        Some("close:1009"),
        "{wire:?}"
    );
    assert_eq!(
        request_outcomes(&store)[..2],
        [
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "protocol_error".to_owned()),
        ]
    );
    assert_eq!(session_rows(&store)[0].outcome, "failed");
}

#[tokio::test(start_paused = true)]
async fn sc36_a_message_of_exactly_65536_bytes_is_accepted() {
    let peer = streaming_peer(vec![padded_update(65_536)]).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    run.shut_down().await.expect("shutdown");
}

// ------------------------------------------------------------------------ SC38

/// A peer that sends `hellos` at their offsets from the session start, then reads
/// (and answers pings) until the client closes.
async fn hello_peer(hellos: Vec<(u64, i64)>) -> Peer {
    Peer::spawn(move |conn| {
        let hellos = hellos.clone();
        async move {
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            let started = Instant::now();
            for (at, timeout_ms) in hellos {
                idle(&mut ws, (started + secs(at)) - Instant::now()).await;
                ws.send(&hello_frame(Some(200), Some(timeout_ms))).await;
            }
            ws.drain().await;
        }
    })
    .await
}

/// The offsets of the first `n` pings the peer read, from the first accepted
/// connection.
async fn ping_offsets(peer: &Peer, n: usize) -> Vec<Duration> {
    peer.log.wait_for("ping", n).await;
    let accepted = peer.log.times("accept")[0];
    peer.log
        .times("ping")
        .into_iter()
        .take(n)
        .map(|at| at - accepted)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn sc38_pings_follow_the_hello_and_the_default() {
    // Hello with connection_timeout 30000 at +0 s: interval 10 s, pings at +10 and +20.
    let peer = hello_peer(vec![(0, 30_000)]).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(ping_offsets(&peer, 2).await, vec![secs(10), secs(20)]);
    run.shut_down().await.expect("shutdown");

    // No hello: the default 15 s, pings at +15 and +30.
    let peer = streaming_peer(Vec::new()).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(ping_offsets(&peer, 2).await, vec![secs(15), secs(30)]);
    run.shut_down().await.expect("shutdown");

    // A second hello at +12 s with connection_timeout 15000 (interval 5 s) moves the next
    // ping to one new interval after it: pings at +10, +17, +22.
    let peer = hello_peer(vec![(0, 30_000), (12, 15_000)]).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(
        ping_offsets(&peer, 3).await,
        vec![secs(10), secs(17), secs(22)]
    );
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc38_a_hello_without_a_valid_timeout_keeps_the_interval() {
    // A zero timeout, an absent one and a wrong type leave the interval at 15 s (R27).
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        ws.send(&hello_frame(Some(200), Some(0))).await;
        ws.send(&hello_frame(Some(200), None)).await;
        ws.send(r#"{"msg_type":"control:hello","connection_timeout":"30000"}"#)
            .await;
        ws.drain().await;
    })
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(ping_offsets(&peer, 2).await, vec![secs(15), secs(30)]);
    run.shut_down().await.expect("shutdown");
}

// ---------------------------------------------------------------- SC43, SC44, SC45

#[tokio::test(start_paused = true)]
async fn sc43_an_error_for_another_tag_is_ignored() {
    let peer = streaming_peer(vec![
        error_frame_for_tag("4243", "vehicle_test_a"),
        u(T0, V1),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    expect_no_event_for(&mut run, secs(5)).await;
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        run.session_counts(),
        vec![FrameCounts {
            accepted: 1,
            ignored_errors: 1,
            ..FrameCounts::default()
        }]
    );
}

#[tokio::test(start_paused = true)]
async fn sc44_a_dead_socket_is_found_45_seconds_after_the_last_frame() {
    // The peer sends one frame, then neither reads nor writes until the test lets it
    // read what reached it.
    let resume = Arc::new(Notify::new());
    let peer_resume = Arc::clone(&resume);
    let peer = Peer::spawn(move |conn| {
        let resume = Arc::clone(&peer_resume);
        async move {
            let index = conn.index;
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            if index == 0 {
                ws.send(&u(T0, V1)).await;
                resume.notified().await;
            }
            ws.drain().await;
        }
    })
    .await;
    let gate = Arc::new(StreamPowerGate::default());
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.gate = Some(Arc::clone(&gate));
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    let frame_at = Instant::now();
    assert!(gate.is_confirmed());
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - frame_at, secs(45));
    assert!(
        !gate.is_confirmed(),
        "the gate is revoked when a session ends"
    );
    assert_eq!(accepted_after(&peer, Instant::now(), 2).await, secs(1));
    // Let the peer read what the client sent: pings only, no unsubscribe, no Close.
    resume.notify_one();
    peer.log.wait_for("eof", 1).await;
    assert!(
        wire_labels(&peer)
            .iter()
            .all(|label| label.contains("data:subscribe_oauth")),
        "{:?}",
        wire_labels(&peer)
    );
    // The pings at +15 s and +30 s were written; the peer's own Pong to the first fails
    // once the client has dropped the socket, so how many it logs is not the rule here.
    assert!(peer.log.count("ping") >= 1, "{:?}", peer.log.labels());
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        request_outcomes(&store)[..2],
        [
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "success".to_owned()),
        ]
    );
    assert_eq!(session_rows(&store)[0].outcome, "transport_ended");
}

#[tokio::test(start_paused = true)]
async fn sc45_data_silence_after_motion_ends_the_session_after_the_configured_time() {
    let peer = streaming_peer(vec![u(T0, V1)]).await;
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.health = Some(secs(60));
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    let frame_at = Instant::now();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    let ended = Instant::now();
    assert_eq!(ended - frame_at, secs(60));
    assert_eq!(accepted_after(&peer, ended, 2).await, secs(1));
    run.shut_down().await.expect("shutdown");
    // The Hub ended it: the peer received the unsubscribe and Close 1000.
    let wire = wire_labels(&peer);
    assert_eq!(wire[1], UNSUBSCRIBE, "{wire:?}");
    assert_eq!(wire[2], "close:1000", "{wire:?}");
    assert_eq!(
        request_outcomes(&store)[..3],
        [
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "success".to_owned()),
            ("stream_unsubscribe".to_owned(), "success".to_owned()),
        ]
    );
    assert_eq!(session_rows(&store)[0].outcome, "transport_ended");
}

// -------------------------------------------------------------------- SC47, SC52

#[tokio::test(start_paused = true)]
async fn sc47_a_server_close_ends_the_session_and_the_next_attempt_follows_after_1_second() {
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        ws.send(&u(T0, V1)).await;
        ws.close(1000).await;
        ws.drain().await;
    })
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(accepted_after(&peer, Instant::now(), 2).await, secs(1));
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc47_close_1013_is_busy_and_waits_the_transport_delay() {
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        ws.send(&u(T0, V1)).await;
        ws.close(1013).await;
        ws.drain().await;
    })
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    // Busy with no retry-after is the Transport delay: 2 s at u = 0 gives 1 s.
    assert_eq!(accepted_after(&peer, Instant::now(), 2).await, secs(1));
    run.shut_down().await.expect("shutdown");
}

#[test]
fn r31_the_class_of_a_server_close_is_busy_only_for_code_1013() {
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    let frame = |code: u16| CloseFrame {
        code: CloseCode::from(code),
        reason: "".into(),
    };
    assert_eq!(
        close_failure_class(Some(&frame(1013))),
        FailureClass::Busy { retry_after: None }
    );
    for code in [1000, 1001, 1006, 1011, 1012, 1014, 4000] {
        assert_eq!(
            close_failure_class(Some(&frame(code))),
            FailureClass::Transport,
            "code {code}"
        );
    }
    assert_eq!(close_failure_class(None), FailureClass::Transport);
}

#[tokio::test(start_paused = true)]
async fn sc52_an_unknown_error_token_ends_the_session_as_unknown() {
    for token in ["something_new", "client_test_a"] {
        let peer = streaming_peer(vec![error_frame(token, "the text is never the rule")]).await;
        let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
        let store = rig.store.clone().expect("store");
        let mut run = rig.start();
        assert_eq!(
            run.event().await,
            StreamEvent::TransportUnavailable,
            "{token}"
        );
        assert_eq!(accepted_after(&peer, Instant::now(), 2).await, secs(1));
        run.shut_down().await.expect("shutdown");
        assert!(
            wire_labels(&peer).contains(&UNSUBSCRIBE.to_owned()),
            "{token}"
        );
        assert_eq!(
            request_outcomes(&store)[..3],
            [
                ("stream_connect".to_owned(), "success".to_owned()),
                ("stream_subscribe".to_owned(), "protocol_error".to_owned()),
                ("stream_unsubscribe".to_owned(), "success".to_owned()),
            ],
            "{token}"
        );
        assert_eq!(
            session_rows(&store)[0].outcome,
            "transport_ended",
            "{token}"
        );
    }
}

// -------------------------------------------------------------------- SC59, SC60

#[tokio::test(start_paused = true)]
async fn sc59_and_sc60_events_follow_the_facts_in_order_with_their_read_times() {
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        ws.log.mark("sent");
        ws.send(&u(T0, V1)).await;
        idle(&mut ws, secs(1)).await;
        ws.log.mark("sent");
        ws.send(&u(T0 + 1000, V1)).await;
        ws.log.mark("sent");
        ws.send(&error_frame("vehicle_test_a", "asleep")).await;
        ws.drain().await;
    })
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    let first = expect_telemetry(&mut run, v1_update(T0)).await;
    let second = expect_telemetry(&mut run, v1_update(T0 + 1000)).await;
    assert_eq!(run.event().await, StreamEvent::VehicleOffline);
    let sent = peer.log.times("sent");
    assert!(first >= sent[0], "queued_at is not before the send");
    assert!(second >= sent[1]);
    assert!(second >= first, "queued_at is non-decreasing");
    assert_eq!(second - first, secs(1), "queued_at is the time of the read");
    // Nothing more before the next session, which starts after 7.5 s.
    expect_no_event_for(&mut run, secs(5)).await;
    run.shut_down().await.expect("shutdown");
}

// --------------------------------------------------------------------------- SC61

#[tokio::test(start_paused = true)]
async fn sc61_the_gate_follows_the_frames_and_the_end_of_each_session() {
    let go_error = Arc::new(Notify::new());
    let go_first = Arc::new(Notify::new());
    let peer_go = Arc::clone(&go_error);
    let peer_first = Arc::clone(&go_first);
    let peer = Peer::spawn(move |conn| {
        let go = Arc::clone(&peer_go);
        let first = Arc::clone(&peer_first);
        async move {
            let index = conn.index;
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            if index == 0 {
                first.notified().await;
                ws.send(&u(T0, V1)).await;
                go.notified().await;
                ws.send(&error_frame("vehicle_test_a", "asleep")).await;
            } else {
                ws.send(&u(T0 + 1000, V1)).await;
            }
            ws.drain().await;
        }
    })
    .await;
    let gate = Arc::new(StreamPowerGate::default());
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.gate = Some(Arc::clone(&gate));
    let mut run = rig.start();
    peer.log.wait_for("subscribe-received", 1).await;
    assert!(!gate.is_confirmed(), "not confirmed before any frame");
    go_first.notify_one();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    assert!(gate.is_confirmed(), "confirmed after U(T0, V1)");
    go_error.notify_one();
    assert_eq!(run.event().await, StreamEvent::VehicleOffline);
    assert!(!gate.is_confirmed(), "not confirmed after VehicleOffline");
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0 + 1000)).await;
    assert!(
        gate.is_confirmed(),
        "confirmed after the next session's frame"
    );
    run.shut_down().await.expect("shutdown");
    assert!(!gate.is_confirmed(), "not confirmed after shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc61_a_new_run_starts_with_the_gate_revoked() {
    let peer = streaming_peer(Vec::new()).await;
    let gate = Arc::new(StreamPowerGate::default());
    gate.observe(&v1_update(T0), W);
    assert!(gate.is_confirmed_at(W));
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.gate = Some(Arc::clone(&gate));
    let mut run = rig.start();
    peer.log.wait_for("subscribe-received", 1).await;
    assert!(!gate.is_confirmed_at(W));
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc61_a_parked_frame_confirms_only_with_power_of_at_least_1_kw() {
    let resume = Arc::new(Notify::new());
    let peer_resume = Arc::clone(&resume);
    let peer = Peer::spawn(move |conn| {
        let resume = Arc::clone(&peer_resume);
        async move {
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            ws.send(&u(T0, PK)).await;
            resume.notified().await;
            ws.send(&u(T0 + 1000, &PK.replace(",0,241.25,P", ",-7,241.25,P")))
                .await;
            ws.drain().await;
        }
    })
    .await;
    let gate = Arc::new(StreamPowerGate::default());
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.gate = Some(Arc::clone(&gate));
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    run.event().await;
    assert!(!gate.is_confirmed(), "a parked frame confirms nothing");
    resume.notify_one();
    run.event().await;
    assert!(gate.is_confirmed(), "power -7 shows use");
    run.shut_down().await.expect("shutdown");
}

// ----------------------------------------------------------------- SC62, SC63

#[tokio::test(start_paused = true)]
async fn sc62_a_full_queue_ends_the_run_after_10_seconds() {
    let peer = streaming_peer(vec![u(T0, V1), u(T0 + 1000, V1)]).await;
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.capacity = 1;
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let started = Instant::now();
    let mut run = rig.start();
    // The receiver is kept and never drained: Healthy fills the channel, so the
    // Telemetry that follows waits 10 s and then ends the run.
    let result = run.finished(secs(15)).await;
    assert!(
        matches!(result, Err(StreamSupervisorError::EventQueueFull)),
        "{result:?}"
    );
    assert_eq!(Instant::now() - started, secs(10));
    peer.log.wait_for("close:", 1).await;
    let wire = wire_labels(&peer);
    assert!(wire.contains(&UNSUBSCRIBE.to_owned()), "{wire:?}");
    assert_eq!(wire.last().map(String::as_str), Some("close:1000"));
    assert_eq!(session_rows(&store)[0].outcome, "failed");
}

#[tokio::test(start_paused = true)]
async fn sc63_a_closed_receiver_ends_the_run_at_the_first_event() {
    let go = Arc::new(Notify::new());
    let peer_go = Arc::clone(&go);
    let peer = Peer::spawn(move |conn| {
        let go = Arc::clone(&peer_go);
        async move {
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            go.notified().await;
            ws.send(&u(T0, V1)).await;
            ws.drain().await;
        }
    })
    .await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("subscribe-received", 1).await;
    // Dropping the receiver: the first event send, Healthy, fails.
    drop(std::mem::replace(
        &mut run.events,
        tokio::sync::mpsc::channel(1).1,
    ));
    go.notify_one();
    let result = run.finished(secs(5)).await;
    assert!(
        matches!(result, Err(StreamSupervisorError::EventReceiverClosed)),
        "{result:?}"
    );
    peer.log.wait_for("close:", 1).await;
    let wire = wire_labels(&peer);
    assert!(wire.contains(&UNSUBSCRIBE.to_owned()), "{wire:?}");
    assert_eq!(wire.last().map(String::as_str), Some("close:1000"));
    assert_eq!(session_rows(&store)[0].outcome, "failed");
}

// --------------------------------------------------------------------------- SC81

#[tokio::test(start_paused = true)]
async fn sc81_implausible_sample_times_are_rejected_frames() {
    let peer = streaming_peer(vec![
        u("1760000000000000", V1),
        u(W + 86_400_000, V1),
        u(T0, V1),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    expect_no_event_for(&mut run, secs(5)).await;
    run.shut_down().await.expect("shutdown");
    // The session's rejected count is 2, both for an implausible time.
    assert_eq!(
        run.session_counts(),
        vec![FrameCounts {
            accepted: 1,
            rejected: 2,
            implausible_time: 2,
            ..FrameCounts::default()
        }]
    );
}

#[tokio::test(start_paused = true)]
async fn sc81_three_frames_beyond_the_skew_end_the_session_and_one_at_the_skew_is_accepted() {
    let late = W + 61_000;
    let peer = streaming_peer(vec![u(late, V1), u(late, V1), u(late, V1)]).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::ProtocolViolation);
    run.shut_down().await.expect("shutdown");

    let peer = streaming_peer(vec![u(W + 60_000, V1)]).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(W + 60_000)).await;
    run.shut_down().await.expect("shutdown");
}

// --------------------------------------------------------------------------- SC82

#[tokio::test(start_paused = true)]
async fn sc82_a_repeated_frame_as_the_first_frame_of_a_session_still_proves_the_subscription() {
    let go_error = Arc::new(Notify::new());
    let peer_go = Arc::clone(&go_error);
    let peer = Peer::spawn(move |conn| {
        let go = Arc::clone(&peer_go);
        async move {
            let index = conn.index;
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            ws.send(&u(T0, V1)).await;
            if index == 0 {
                ws.close(1000).await;
            } else {
                go.notified().await;
                ws.send(&error_frame("vehicle_test_a", "asleep")).await;
            }
            ws.drain().await;
        }
    })
    .await;
    let gate = Arc::new(StreamPowerGate::default());
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.gate = Some(Arc::clone(&gate));
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    // Session 2: the identical frame is a duplicate, so only Healthy is sent, and the
    // subscribe receipt completes as a success.
    assert_eq!(run.event().await, StreamEvent::Healthy);
    assert!(gate.is_confirmed(), "the duplicate confirmed the gate");
    go_error.notify_one();
    assert_eq!(run.event().await, StreamEvent::VehicleOffline);
    let ended = Instant::now();
    assert_eq!(
        accepted_after(&peer, ended, 3).await,
        Duration::from_millis(7_500)
    );
    run.shut_down().await.expect("shutdown");
    let outcomes = request_outcomes(&store);
    assert_eq!(
        outcomes[..2],
        [
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "success".to_owned()),
        ]
    );
    // Session 2's rows: connect success, subscribe success (from the duplicate),
    // unsubscribe success (the vehicle error).
    assert_eq!(
        outcomes[2..5],
        [
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "success".to_owned()),
            ("stream_unsubscribe".to_owned(), "success".to_owned()),
        ]
    );
}

// --------------------------------------------------------------------------- SC83

#[tokio::test(start_paused = true)]
async fn sc83_a_ping_from_the_server_is_answered_with_a_pong_of_the_same_payload() {
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        let _ = futures_util::SinkExt::send(&mut ws.inner, Message::Ping("abc".into())).await;
        // The next client frame must be the Pong; scheduled Pings come later (+15 s).
        while let Some(Ok(message)) = futures_util::StreamExt::next(&mut ws.inner).await {
            match message {
                Message::Pong(payload) => {
                    ws.log.mark(format!(
                        "pong-payload:{}",
                        String::from_utf8_lossy(&payload)
                    ));
                }
                Message::Ping(_) => ws.log.mark("ping"),
                Message::Text(text) => ws.log.mark(format!("frame:{}", text.as_str())),
                Message::Close(_) => break,
                _ => {}
            }
        }
    })
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    peer.log.wait_for("pong-payload:", 1).await;
    let labels = peer.log.labels();
    let position = |wanted: &str| {
        labels
            .iter()
            .position(|label| label == wanted)
            .unwrap_or_else(|| panic!("{wanted} was not logged: {labels:?}"))
    };
    let (subscribed, pong) = (position("subscribe-received"), position("pong-payload:abc"));
    assert!(subscribed < pong);
    // Between the subscribe and the Pong no other client frame reached the peer.
    assert!(
        labels[subscribed + 1..pong]
            .iter()
            .all(|label| !label.starts_with("frame:") && label != "ping"),
        "{labels:?}"
    );
    expect_no_event_for(&mut run, secs(1)).await;
    run.shut_down().await.expect("shutdown");
}

// --------------------------------------------------------------------------- SC84

#[tokio::test(start_paused = true)]
async fn sc84_the_gate_expires_30_seconds_after_the_frame_on_the_wall_clock() {
    let peer = streaming_peer(vec![u(T0, V1)]).await;
    let gate = Arc::new(StreamPowerGate::default());
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.gate = Some(Arc::clone(&gate));
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    run.clock.set(W + 30_000);
    assert!(gate.is_confirmed());
    run.clock.set(W + 30_001);
    assert!(!gate.is_confirmed());
    // A wall clock set back before the frame reads as not confirmed (fail safe).
    run.clock.set(W - 1);
    assert!(!gate.is_confirmed());
    // The session stayed open throughout: no event, no close.
    expect_no_event_for(&mut run, secs(1)).await;
    assert_eq!(peer.log.count("close:"), 0);
    run.shut_down().await.expect("shutdown");
}

// --------------------------------------------------------------------------- SC85

#[tokio::test(start_paused = true)]
async fn sc85_a_host_sleep_revokes_the_gate_and_pings_at_the_next_tick() {
    let peer = streaming_peer(vec![u(T0, V1)]).await;
    let gate = Arc::new(StreamPowerGate::default());
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.gate = Some(Arc::clone(&gate));
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    let accepted = peer.log.times("accept")[0];
    assert!(gate.is_confirmed_at(W), "confirmed by the frame");
    // 600 s of wall time pass while the monotonic clock advances 1 s.
    sleep(secs(1)).await;
    run.clock.set(W + 600_000);
    assert!(gate.is_confirmed_at(W), "still confirmed before the tick");
    // The next ping tick (+15 s) detects the sleep: the gate is revoked and the Ping is
    // sent at once; the peer answers, so the session continues with no event.
    peer.log.wait_for("ping", 1).await;
    assert_eq!(peer.log.times("ping")[0] - accepted, secs(15));
    assert!(!gate.is_confirmed_at(W), "revoked at the tick");
    expect_no_event_for(&mut run, secs(20)).await;
    assert_eq!(peer.log.count("close:"), 0);
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc85_a_host_sleep_shortens_the_dead_socket_deadline_to_one_interval() {
    let resume = Arc::new(Notify::new());
    let peer_resume = Arc::clone(&resume);
    let peer = Peer::spawn(move |conn| {
        let resume = Arc::clone(&peer_resume);
        async move {
            let index = conn.index;
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            if index == 0 {
                ws.send(&u(T0, V1)).await;
                // Never reads: the Ping at the tick is not answered.
                resume.notified().await;
            }
            ws.drain().await;
        }
    })
    .await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    let frame_at = Instant::now();
    sleep(secs(1)).await;
    run.clock.set(W + 600_000);
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    // Tick at +15 s detects the sleep, the Ping goes out, and the deadline is one
    // interval (15 s) after it: +30 s, not +45 s.
    assert_eq!(Instant::now() - frame_at, secs(30));
    resume.notify_one();
    peer.log.wait_for("eof", 1).await;
    assert!(
        wire_labels(&peer)
            .iter()
            .all(|label| label.contains("data:subscribe_oauth")),
        "no unsubscribe: {:?}",
        wire_labels(&peer)
    );
    assert_eq!(peer.log.count("ping"), 1, "the one Ping of the tick");
    run.shut_down().await.expect("shutdown");
    assert_eq!(session_rows(&store)[0].outcome, "transport_ended");
}

// --------------------------------------------------------------------------- SC86

#[tokio::test(start_paused = true)]
async fn sc86_a_parked_session_waits_300_seconds_for_data() {
    let peer = streaming_peer(vec![u(T0, PK)]).await;
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.health = Some(secs(60));
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    run.event().await;
    let frame_at = Instant::now();
    // Pings are answered, and the 60 s configured limit does not apply to a parked car.
    expect_no_event_for(&mut run, secs(299)).await;
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - frame_at, secs(300));
    peer.log.wait_for("close:", 1).await;
    assert!(wire_labels(&peer).contains(&UNSUBSCRIBE.to_owned()));
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc86_a_session_that_never_receives_a_frame_ends_300_seconds_after_the_subscribe() {
    let peer = streaming_peer(Vec::new()).await;
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.health = Some(secs(60));
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("subscribe-received", 1).await;
    let subscribed = Instant::now();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - subscribed, secs(300));
    run.shut_down().await.expect("shutdown");
    assert_eq!(
        request_outcomes(&store)[..3],
        [
            ("stream_connect".to_owned(), "success".to_owned()),
            ("stream_subscribe".to_owned(), "transport_error".to_owned()),
            ("stream_unsubscribe".to_owned(), "success".to_owned()),
        ]
    );
    assert_eq!(session_rows(&store)[0].outcome, "transport_ended");
}

// ------------------------------------------------- R29 and R30 rules without an SC id

#[tokio::test(start_paused = true)]
async fn r29_any_inbound_frame_resets_the_liveness_clock() {
    // The peer never reads, so it never answers a Ping; it sends a message of an unknown
    // type every 30 s four times. The socket outlives the 45 s of a silent one, and is
    // found dead 45 s after the last of them.
    let resume = Arc::new(Notify::new());
    let peer_resume = Arc::clone(&resume);
    let peer = Peer::spawn(move |conn| {
        let resume = Arc::clone(&peer_resume);
        async move {
            let index = conn.index;
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            if index == 0 {
                for _ in 0..4 {
                    sleep(secs(30)).await;
                    ws.send(r#"{"msg_type":"control:other"}"#).await;
                }
                ws.log.mark("last-message");
                resume.notified().await;
            }
            ws.drain().await;
        }
    })
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    let last = peer.log.times("last-message")[0];
    assert_eq!(Instant::now() - last, secs(45));
    resume.notify_one();
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r30_a_repeated_frame_resets_the_silence_clock_and_a_rejected_one_does_not() {
    // Moving, limit 60 s. Frame at +0, the same frame again at +50 (a duplicate counts as
    // data), a frame for another tag at +100 (rejected: does not count), so the session
    // ends 60 s after the duplicate, at +110.
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        ws.send(&u(T0, V1)).await;
        idle(&mut ws, secs(50)).await;
        ws.send(&u(T0, V1)).await;
        idle(&mut ws, secs(50)).await;
        ws.send(&u_for_tag("4243", T0)).await;
        ws.drain().await;
    })
    .await;
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.health = Some(secs(60));
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    expect_telemetry(&mut run, v1_update(T0)).await;
    let first = Instant::now();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - first, secs(110));
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r30_the_moving_flag_belongs_to_the_run_not_the_session() {
    // Session 1 ends after a moving frame; session 2 gets no frame and is ended after the
    // moving limit (60 s) from its subscribe, not the parked limit (300 s).
    let peer = Peer::spawn(|conn| async move {
        let index = conn.index;
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        if index == 0 {
            ws.send(&u(T0, V1)).await;
            ws.close(1000).await;
        }
        ws.drain().await;
    })
    .await;
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.health = Some(secs(60));
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    run.event().await;
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    peer.log.wait_for("subscribe-received", 2).await;
    let subscribed = Instant::now();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - subscribed, secs(60));
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r30_a_parked_frame_moves_the_run_back_to_the_parked_limit() {
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        ws.send(&u(T0, V1)).await;
        idle(&mut ws, secs(10)).await;
        ws.send(&u(T0 + 10_000, PK)).await;
        ws.drain().await;
    })
    .await;
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.health = Some(secs(60));
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    run.event().await;
    run.event().await;
    let parked_at = Instant::now();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(Instant::now() - parked_at, secs(300));
    run.shut_down().await.expect("shutdown");
}

// ------------------------------------------------------------------ SessionClocks

fn clocks_at(now: Instant) -> SessionClocks {
    SessionClocks::start(now, W)
}

#[tokio::test(start_paused = true)]
async fn the_clocks_start_with_the_default_interval_and_deadlines_from_the_start() {
    let now = Instant::now();
    let clocks = clocks_at(now);
    assert_eq!(clocks.ping_interval(), secs(15));
    assert_eq!(clocks.next_ping(), now + secs(15));
    assert_eq!(clocks.dead_at(), now + secs(45));
    assert_eq!(clocks.silent_at(secs(300)), now + secs(300));
}

#[tokio::test(start_paused = true)]
async fn an_inbound_frame_moves_the_dead_deadline_and_a_data_frame_the_silence_deadline() {
    let now = Instant::now();
    let mut clocks = clocks_at(now);
    clocks.on_inbound(now + secs(10));
    assert_eq!(clocks.dead_at(), now + secs(55));
    assert_eq!(
        clocks.silent_at(secs(60)),
        now + secs(60),
        "not a data frame"
    );
    clocks.on_data(now + secs(20));
    assert_eq!(clocks.silent_at(secs(60)), now + secs(80));
}

#[tokio::test(start_paused = true)]
async fn a_new_interval_reschedules_the_next_ping_and_scales_the_dead_deadline() {
    let now = Instant::now();
    let mut clocks = clocks_at(now);
    clocks.adopt_ping_interval(now + secs(12), secs(5));
    assert_eq!(clocks.ping_interval(), secs(5));
    assert_eq!(clocks.next_ping(), now + secs(17));
    // Three of the new intervals after the last inbound frame (the session start).
    assert_eq!(clocks.dead_at(), now + secs(15));
}

#[tokio::test(start_paused = true)]
async fn the_host_sleep_check_compares_wall_drift_with_the_monotonic_clock() {
    let now = Instant::now();
    let mut clocks = clocks_at(now);
    // A tick on time: 15 s of wall time over 15 s of mono time.
    assert!(!clocks.on_ping_tick(now + secs(15), W + 15_000));
    assert_eq!(clocks.next_ping(), now + secs(30));
    // Exactly 60 s of drift is not above the constant; 60.001 s is.
    assert!(!clocks.on_ping_tick(now + secs(30), W + 15_000 + 15_000 + 60_000));
    clocks.on_inbound(now + secs(40));
    assert!(clocks.on_ping_tick(now + secs(45), W + 90_000 + 15_000 + 60_001));
    // The deadline is one interval after the detecting tick (+60 s), earlier than three
    // intervals after the last inbound frame (+85 s).
    assert_eq!(clocks.dead_at(), now + secs(60));
    // Any inbound frame restores the normal rule.
    clocks.on_inbound(now + secs(50));
    assert_eq!(clocks.dead_at(), now + secs(95));
    // A wall clock stepped back is no sleep.
    assert!(!clocks.on_ping_tick(now + secs(60), W - 1_000_000));
}
