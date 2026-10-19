// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C4 part P4 (behaviour across sessions and runs): SC40, SC50, SC51, SC53
// to SC58, SC64, SC79, SC80 and SC87 to SC89, plus tests of the rules the part owns that
// no SC test names (R37 stable-session reset and its tie rule, R42 start jitter, R43 the
// endpoint is never rewritten by the region, R54 unresolved receipts). Each scenario runs
// several sessions, or several runs, against an in-process loopback peer over in-memory
// pipes under paused time, with exact expected events, rows and delays. SC80 and SC87 need
// the credential unit (C5); their stream-side halves run against stand-ins and the C5
// halves run against the credential unit itself. Synthetic data only; no test
// touches the network except loopback.

use std::{
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::json;
use tokio::time::{Instant, sleep};

use super::{
    CHINA_STREAM_ENDPOINT, GLOBAL_STREAM_ENDPOINT, StreamEvent, StreamPowerGate, StreamRegion,
    TeslaStreamSupervisor, parse_data_update, test_peer::*, validate_endpoint_override,
};

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn millis(milliseconds: u64) -> Duration {
    Duration::from_millis(milliseconds)
}

// ---------------------------------------------------------------- scenario scripts

/// How a scripted session ends.
#[derive(Clone)]
enum End {
    /// The server sends a Close frame with this code.
    Close(u16),
    /// The server sends a `control:hello` with this `code`.
    Hello(i64),
    /// The server sends a `data:error` with this `error_type`.
    Error(&'static str),
    /// The server sends this frame as given.
    Raw(String),
    /// The server keeps the session open and answers pings until the client leaves.
    Hold,
}

/// What the peer does with one connection, by the connection's order.
#[derive(Clone)]
enum Plan {
    /// Completes the upgrade, reads the subscribe frame, sends each frame after the
    /// given number of seconds, waits `end_after` more seconds, then ends the session.
    Session {
        frames: Vec<(u64, String)>,
        end_after: u64,
        end: End,
    },
    /// Answers the upgrade with this HTTP status instead of 101.
    Refuse(u16),
}

impl Plan {
    /// A session that ends at once, before any data.
    fn ends(end: End) -> Self {
        Self::Session {
            frames: Vec::new(),
            end_after: 0,
            end,
        }
    }

    fn data(frames: Vec<(u64, String)>, end_after: u64, end: End) -> Self {
        Self::Session {
            frames,
            end_after,
            end,
        }
    }

    fn hold() -> Self {
        Self::ends(End::Hold)
    }
}

/// Waits `wait` while still reading, so the client's pings are answered.
async fn idle(ws: &mut Ws, wait: Duration) {
    if wait.is_zero() {
        return;
    }
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

async fn play(conn: Conn, plan: Plan) {
    let index = conn.index;
    match plan {
        Plan::Refuse(status) => {
            conn.log.mark(format!("end:{index}"));
            conn.refuse_upgrade(status, None).await;
        }
        Plan::Session {
            frames,
            end_after,
            end,
        } => {
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            for (wait, frame) in &frames {
                idle(&mut ws, secs(*wait)).await;
                ws.send(frame).await;
            }
            idle(&mut ws, secs(end_after)).await;
            match end {
                End::Hold => {}
                End::Close(code) => {
                    ws.log.mark(format!("end:{index}"));
                    ws.close(code).await;
                }
                End::Hello(code) => {
                    ws.log.mark(format!("end:{index}"));
                    ws.send(&hello_frame(Some(code), None)).await;
                }
                End::Error(token) => {
                    ws.log.mark(format!("end:{index}"));
                    ws.send(&error_frame(token, "text")).await;
                }
                End::Raw(frame) => {
                    ws.log.mark(format!("end:{index}"));
                    ws.send(&frame).await;
                }
            }
            ws.drain().await;
        }
    }
}

/// A peer that plays `plans` by connection order; every later connection is held open.
async fn plan_peer(plans: Vec<Plan>) -> Peer {
    Peer::spawn(move |conn| {
        let plan = plans.get(conn.index).cloned().unwrap_or_else(Plan::hold);
        play(conn, plan)
    })
    .await
}

/// The wait between the end of each scripted session and the next attempt's start (the
/// peer's accept), in connection order; sessions the peer did not end itself (held) have
/// none.
fn delays(peer: &Peer) -> Vec<Duration> {
    let entries = peer.log.entries();
    let accepts = peer.log.times("accept");
    entries
        .iter()
        .filter_map(|(at, label)| {
            let index: usize = label.strip_prefix("end:")?.parse().ok()?;
            accepts.get(index + 1).map(|accept| *accept - *at)
        })
        .collect()
}

/// R38: no two attempts of a run start less than 1 s apart.
fn assert_min_spacing(peer: &Peer) {
    for pair in peer.log.times("accept").windows(2) {
        assert!(
            pair[1] - pair[0] >= secs(1),
            "attempts {:?} apart",
            pair[1] - pair[0]
        );
    }
}

/// The client's frames as the peer received them: subscribe and unsubscribe frames by
/// kind, and every Close with its code.
fn wire_kinds(peer: &Peer) -> Vec<String> {
    peer.log
        .labels()
        .into_iter()
        .filter_map(|label| {
            if let Some(frame) = label.strip_prefix("frame:") {
                if frame.contains("data:subscribe_oauth") {
                    Some("subscribe".to_owned())
                } else if frame.contains("data:unsubscribe") {
                    Some("unsubscribe".to_owned())
                } else {
                    None
                }
            } else if label.starts_with("close:") {
                Some(label)
            } else {
                None
            }
        })
        .collect()
}

fn kind(event: &StreamEvent) -> &'static str {
    match event {
        StreamEvent::Healthy => "Healthy",
        StreamEvent::Telemetry { .. } => "Telemetry",
        StreamEvent::VehicleOffline => "VehicleOffline",
        StreamEvent::AuthRejected => "AuthRejected",
        StreamEvent::TransportUnavailable => "TransportUnavailable",
        StreamEvent::ProtocolViolation => "ProtocolViolation",
    }
}

/// The next `n` events, by kind.
async fn kinds(run: &mut Running, n: usize) -> Vec<&'static str> {
    let mut seen = Vec::new();
    for _ in 0..n {
        seen.push(kind(&run.event().await));
    }
    seen
}

async fn expect_silence(run: &mut Running, wait: Duration) {
    if let Some(event) = run.event_within(wait).await {
        panic!("no event was expected within {wait:?}, got {event:?}");
    }
}

fn telemetry_time(event: &StreamEvent) -> i64 {
    match event {
        StreamEvent::Telemetry { update, .. } => update.timestamp_ms,
        other => panic!("expected telemetry, got {other:?}"),
    }
}

const UNSUBSCRIBE: &str = "unsubscribe";
const SUBSCRIBE: &str = "subscribe";

// ----------------------------------------------------------------------------- SC40

#[tokio::test(start_paused = true)]
async fn sc40_a_hello_503_is_an_outage_and_two_hello_401s_make_auth_rejected() {
    // Hello code 503 is Busy with no retry-after: an outage event, and the Transport
    // delay of 1 s at u = 0.
    let peer = plan_peer(vec![Plan::ends(End::Hello(503)), Plan::hold()]).await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    peer.log.wait_for("accept", 2).await;
    assert_eq!(delays(&peer), vec![secs(1)]);
    run.shut_down().await.expect("shutdown");

    // Hello code 401 in two consecutive sessions: no event after the first, and the
    // second reports the rejection (R46). Each is a fresh admission.
    let peer = plan_peer(vec![
        Plan::ends(End::Hello(401)),
        Plan::ends(End::Hello(401)),
        Plan::hold(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    expect_silence(&mut run, secs(2)).await;
    assert_eq!(peer.log.count("accept"), 1, "the retry has not started yet");
    assert_eq!(run.event().await, StreamEvent::AuthRejected);
    peer.log.wait_for("accept", 3).await;
    assert_eq!(delays(&peer), vec![millis(2_500), secs(150)]);
    assert_eq!(peer.log.count("refresh"), 3, "one refresh call per attempt");
    run.shut_down().await.expect("shutdown");
}

// ----------------------------------------------------------------------------- SC50

#[tokio::test(start_paused = true)]
async fn sc50_the_vehicle_path_waits_15_30_60_120_and_data_starts_it_over() {
    let vehicle = || Plan::ends(End::Error("vehicle_test_a"));
    let peer = plan_peer(vec![
        vehicle(),
        vehicle(),
        vehicle(),
        vehicle(),
        vehicle(),
        // Data arrives, then a vehicle error: the first of a new series.
        Plan::data(vec![(0, u(T0, V1))], 0, End::Error("vehicle_test_a")),
        Plan::hold(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    // No Healthy before data: a vehicle error is not a proof of the subscription.
    assert_eq!(
        kinds(&mut run, 8).await,
        [
            "VehicleOffline",
            "VehicleOffline",
            "VehicleOffline",
            "VehicleOffline",
            "VehicleOffline",
            "Healthy",
            "Telemetry",
            "VehicleOffline"
        ]
    );
    peer.log.wait_for("accept", 7).await;
    // Nominal 15, 30, 60, 120, 120 s at u = 0 (half): 7.5, 15, 30, 60, 60 s; after data
    // the counter is 0, so the next vehicle end waits 7.5 s again.
    assert_eq!(
        delays(&peer),
        vec![
            millis(7_500),
            secs(15),
            secs(30),
            secs(60),
            secs(60),
            millis(7_500)
        ]
    );
    // Every vehicle session sent the unsubscribe (R9): six sessions ended by the Hub.
    let wire = wire_kinds(&peer);
    assert_eq!(
        wire.iter().filter(|k| *k == UNSUBSCRIBE).count(),
        6,
        "{wire:?}"
    );
    assert_eq!(
        wire.iter().filter(|k| *k == SUBSCRIBE).count(),
        7,
        "{wire:?}"
    );
    assert_min_spacing(&peer);
    run.shut_down().await.expect("shutdown");
}

// ----------------------------------------------------------------------------- SC51

#[tokio::test(start_paused = true)]
async fn sc51_the_refusal_path_reports_the_second_refusal_and_data_clears_it() {
    let peer = plan_peer(vec![
        Plan::ends(End::Hello(401)),
        Plan::ends(End::Hello(401)),
        Plan::ends(End::Hello(401)),
        // Data, then a refusal: the series starts over, so no event, and the
        // subscription that existed is released.
        Plan::data(vec![(0, u(T0, V1))], 0, End::Hello(401)),
        Plan::hold(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    // Session 1: one refusal is evidence only. Session 2 and 3: the rejection.
    expect_silence(&mut run, secs(2)).await;
    assert_eq!(run.event().await, StreamEvent::AuthRejected);
    assert_eq!(run.event().await, StreamEvent::AuthRejected);
    // Session 4: Healthy and Telemetry, then no event for the refusal.
    assert_eq!(kinds(&mut run, 2).await, ["Healthy", "Telemetry"]);
    expect_silence(&mut run, secs(2)).await;
    peer.log.wait_for("subscribe-received", 5).await;
    assert_eq!(
        delays(&peer),
        vec![millis(2_500), secs(150), secs(150), millis(2_500)]
    );
    // One refresh call per attempt (R40), each followed by its own subscribe.
    assert_eq!(peer.log.count("refresh"), 5);
    // No unsubscribe followed a refusal that came before data (R9); the one after data
    // was sent.
    assert_eq!(
        wire_kinds(&peer),
        [
            SUBSCRIBE,
            "close:1000",
            SUBSCRIBE,
            "close:1000",
            SUBSCRIBE,
            "close:1000",
            SUBSCRIBE,
            UNSUBSCRIBE,
            "close:1000",
            SUBSCRIBE,
        ]
    );
    assert_min_spacing(&peer);
    run.shut_down().await.expect("shutdown");
}

// ------------------------------------------------------------------ SC53, SC54, SC55

#[tokio::test(start_paused = true)]
async fn sc53_a_session_stable_for_60_seconds_resets_the_transport_back_off() {
    let close = || Plan::ends(End::Close(1000));
    let peer = plan_peer(vec![
        close(),
        close(),
        close(),
        // Streams 30 s, then the server closes it: not yet stable, the counter goes on.
        Plan::data(vec![(0, u(T0, V1))], 30, End::Close(1000)),
        // Streams 61 s with data every 30 s (so the moving silence limit of 60 s does
        // not end it), then closes: stable at 60 s, so the end counts as the first.
        Plan::data(
            vec![
                (0, u(T0 + 1_000, V1)),
                (30, u(T0 + 31_000, V1)),
                (30, u(T0 + 61_000, V1)),
            ],
            1,
            End::Close(1000),
        ),
        Plan::hold(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    peer.log.wait_for("accept", 6).await;
    // Three quick failures: 1, 2, 4 s. Counter 3, then a 30 s session ends: counter 4,
    // nominal 16 s, so 8 s. A 61 s session resets at 60 s, so its end is counter 1: 1 s.
    assert_eq!(
        delays(&peer),
        vec![secs(1), secs(2), secs(4), secs(8), secs(1)]
    );
    assert_min_spacing(&peer);
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc54_a_vehicle_end_restarts_the_transport_back_off() {
    let close = || Plan::ends(End::Close(1000));
    let peer = plan_peer(vec![
        close(),
        close(),
        close(),
        Plan::ends(End::Error("vehicle_test_a")),
        close(),
        Plan::hold(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    peer.log.wait_for("accept", 6).await;
    // 1, 2, 4 s for the socket failures; a vehicle end waits 7.5 s and zeroes the
    // transport counter, so the next socket failure waits 1 s again.
    assert_eq!(
        delays(&peer),
        vec![secs(1), secs(2), secs(4), millis(7_500), secs(1)]
    );
    assert_min_spacing(&peer);
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc55_attempts_are_never_less_than_a_second_apart_whatever_the_classes() {
    // Every class in turn, at the smallest jitter draw: a socket failure, a refusal, a
    // vehicle end, an upgrade refusal with an unknown status, a busy hello, a Close
    // 1013 and a plain Close.
    let peer = plan_peer(vec![
        Plan::ends(End::Close(1000)),
        Plan::ends(End::Hello(401)),
        Plan::ends(End::Error("vehicle_test_a")),
        Plan::Refuse(404),
        Plan::ends(End::Hello(503)),
        Plan::ends(End::Close(1013)),
        Plan::ends(End::Close(1000)),
        Plan::hold(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    peer.log.wait_for("accept", 8).await;
    // Transport 1 s; refusal 2.5 s; vehicle 7.5 s; 404 is Transport after a vehicle
    // end (counter 1) 1 s; busy hello 2 s; Close 1013 (busy) 4 s; Close 8 s.
    assert_eq!(
        delays(&peer),
        vec![
            secs(1),
            millis(2_500),
            millis(7_500),
            secs(1),
            secs(2),
            secs(4),
            secs(8)
        ]
    );
    assert_min_spacing(&peer);
    let smallest = delays(&peer).into_iter().min().expect("delays");
    assert_eq!(smallest, secs(1), "R38: the smallest delay is 1 s");
    run.shut_down().await.expect("shutdown");
}

// ----------------------------------------------------------------------------- SC56

#[tokio::test(start_paused = true)]
async fn sc56_a_token_expiring_mid_stream_changes_nothing_until_the_next_attempt() {
    let issuer = Issuer::spawn(Duration::ZERO, "tok-2", "ref-2").await;
    let creds = Creds::current(issuer.url.clone(), "tok-1").held_by(&issuer);
    let clock = TestClock::new(W);
    let peer_clock = Arc::clone(&clock);
    let peer = Peer::spawn(move |conn| {
        let clock = Arc::clone(&peer_clock);
        async move {
            let index = conn.index;
            let mut ws = conn.ws().await;
            ws.subscribe().await;
            if index == 0 {
                // A frame every 5 s for 120 s, the wall clock following the samples.
                for step in 1..=24_i64 {
                    idle(&mut ws, secs(5)).await;
                    clock.set(W + step * 5_000);
                    ws.send(&u(T0 + step * 5_000, V1)).await;
                }
                ws.log.mark("end:0");
                ws.send(&error_frame("something_new", "the token is not valid"))
                    .await;
            } else {
                ws.send(&u(T0 + 130_000, V1)).await;
            }
            ws.drain().await;
        }
    })
    .await;
    let mut rig = Rig::for_peer(&peer, creds);
    rig.clock = Arc::clone(&clock);
    let mut run = rig.start();
    // The refresh falls due 30 s into the session; the token would expire at 60 s.
    sleep(secs(30)).await;
    run.creds.make_due(issuer.url.clone(), "tok-1").await;
    // One subscribe and 24 samples, in order, with no refresh meanwhile.
    assert_eq!(kind(&run.event().await), "Healthy");
    for step in 1..=24_i64 {
        assert_eq!(telemetry_time(&run.event().await), T0 + step * 5_000);
    }
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(peer.log.count("subscribe-received"), 1);
    assert_eq!(
        issuer.requests.load(Ordering::SeqCst),
        0,
        "no token request"
    );
    assert_eq!(
        peer.log.count("refresh"),
        1,
        "only the first attempt's call"
    );
    // The next attempt (1 s later) calls refresh, which now sends the one request, and
    // the subscribe carries the new token.
    peer.log.wait_for("subscribe-received", 2).await;
    let end = peer.log.times("end:0")[0];
    assert_eq!(peer.log.times("refresh")[1] - end, secs(1));
    assert_eq!(issuer.requests.load(Ordering::SeqCst), 1);
    assert_eq!(peer.log.count("refresh"), 2);
    assert_eq!(
        peer.log.tails("token:"),
        vec!["tok-1".to_owned(), "tok-2".to_owned()]
    );
    // Data proves the session; no rejection was reported at any point.
    assert_eq!(kinds(&mut run, 2).await, ["Healthy", "Telemetry"]);
    expect_silence(&mut run, secs(1)).await;
    run.shut_down().await.expect("shutdown");
}

// ----------------------------------------------------------------------------- SC57

#[tokio::test(start_paused = true)]
async fn sc57_a_china_manager_refused_every_time_keeps_to_its_one_endpoint() {
    let peer = plan_peer(vec![
        Plan::ends(End::Hello(401)),
        Plan::ends(End::Hello(401)),
        Plan::ends(End::Hello(401)),
        Plan::hold(),
    ])
    .await;
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.region = StreamRegion::China;
    let mut run = rig.start();
    expect_silence(&mut run, secs(2)).await;
    assert_eq!(run.event().await, StreamEvent::AuthRejected);
    assert_eq!(run.event().await, StreamEvent::AuthRejected);
    peer.log.wait_for("host:", 4).await;
    // The same policy as any region (R43): refusal 2.5 s, then 150 s twice. Every
    // attempt reached the one configured host, and no other endpoint was tried (R2).
    assert_eq!(delays(&peer), vec![millis(2_500), secs(150), secs(150)]);
    assert_eq!(peer.log.tails("host:"), vec!["127.0.0.1:9".to_owned(); 4]);
    assert_eq!(peer.log.count("accept"), 4);
    assert_eq!(peer.max_concurrent(), 1);
    run.shut_down().await.expect("shutdown");
}

#[tokio::test]
async fn r1_the_region_never_rewrites_the_endpoint() {
    // The supervisor keeps the endpoint it was built with, whatever the region says
    // (the region is only for logs, R1), and neither Tesla host replaces the other.
    for (region, endpoint, host) in [
        (
            StreamRegion::Global,
            CHINA_STREAM_ENDPOINT,
            "streaming.vn.cloud.tesla.cn",
        ),
        (
            StreamRegion::China,
            GLOBAL_STREAM_ENDPOINT,
            "streaming.vn.teslamotors.com",
        ),
        (
            StreamRegion::China,
            CHINA_STREAM_ENDPOINT,
            "streaming.vn.cloud.tesla.cn",
        ),
        (
            StreamRegion::Global,
            GLOBAL_STREAM_ENDPOINT,
            "streaming.vn.teslamotors.com",
        ),
    ] {
        let (events, _receiver) = tokio::sync::mpsc::channel(1);
        let creds = Creds::offline("tok-1");
        let Ok(supervisor) = TeslaStreamSupervisor::new_legacy_auth_for_test_production_policy(
            crate::owner_api::VehicleId::from_test(9),
            crate::owner_api::StreamVehicleId::from_test(4242),
            Arc::clone(&creds.manager),
            region,
            endpoint.to_owned(),
            creds.client.clone(),
            events,
        ) else {
            panic!("a Tesla endpoint is a valid wss endpoint");
        };
        assert_eq!(supervisor.endpoint, endpoint);
        assert_eq!(supervisor.endpoint_host, host);
        assert_eq!(supervisor.region, region);
    }
}

// ----------------------------------------------------------------------------- SC58

#[tokio::test(start_paused = true)]
async fn sc58_a_new_run_starts_with_fresh_counters_jitter_and_correlation_id() {
    let peer = plan_peer(vec![
        // Run 1: two socket failures (its counter is 2), then data, then shutdown.
        Plan::ends(End::Close(1000)),
        Plan::ends(End::Close(1000)),
        Plan::data(vec![(0, u(T0, V1))], 0, End::Hold),
        // Run 2: a socket failure, then the same frame again.
        Plan::ends(End::Close(1000)),
        Plan::data(vec![(0, u(T0, V1))], 0, End::Hold),
    ])
    .await;
    let gate = Arc::new(StreamPowerGate::default());
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.gate = Some(Arc::clone(&gate));
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(
        kinds(&mut run, 4).await,
        [
            "TransportUnavailable",
            "TransportUnavailable",
            "Healthy",
            "Telemetry"
        ]
    );
    assert!(gate.is_confirmed());
    run.shut_down().await.expect("shutdown");
    assert!(!gate.is_confirmed(), "shutdown revokes the gate");

    // Run 2 for the same vehicle, on the same store and gate.
    let mut second = Rig::for_peer(&peer, Creds::offline("tok-1"));
    second.gate = Some(Arc::clone(&gate));
    second.store = Some(store.clone());
    let started = Instant::now();
    let mut run = second.start();
    peer.log.wait_for("accept", 4).await;
    assert_eq!(
        peer.log.times("accept")[3] - started,
        Duration::ZERO,
        "the first attempt waits u * 1 s, here 0"
    );
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert!(!gate.is_confirmed());
    // The counter started at 0: one failure waits 1 s (run 1's counter of 2 would give
    // 4 s).
    peer.log.wait_for("accept", 5).await;
    assert_eq!(delays(&peer), vec![secs(1), secs(2), secs(1)]);
    // The frame run 1 delivered is delivered again: the duplicate memory is empty.
    assert_eq!(run.event().await, StreamEvent::Healthy);
    assert_eq!(telemetry_time(&run.event().await), T0);
    assert!(gate.is_confirmed());
    run.shut_down().await.expect("shutdown");

    // The runs have their own correlation ids; every row of a run carries its own.
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 5);
    let first_id = &sessions[0].correlation_id;
    let second_id = &sessions[3].correlation_id;
    assert_ne!(first_id, second_id);
    assert!(
        sessions[..3]
            .iter()
            .all(|row| &row.correlation_id == first_id)
    );
    assert!(
        sessions[3..]
            .iter()
            .all(|row| &row.correlation_id == second_id)
    );
}

#[tokio::test(start_paused = true)]
async fn r42_the_first_attempt_of_a_run_waits_u_times_one_second() {
    for (draw, expected) in [
        (0.0, Duration::ZERO),
        (0.5, millis(500)),
        (0.999, millis(999)),
    ] {
        let peer = plan_peer(vec![Plan::hold()]).await;
        let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
        rig.jitter = vec![draw];
        let started = Instant::now();
        let mut run = rig.start();
        peer.log.wait_for("accept", 1).await;
        assert_eq!(
            peer.log.times("accept")[0] - started,
            expected,
            "u = {draw}"
        );
        run.shut_down().await.expect("shutdown");
    }
}

// ----------------------------------------------------------------------------- SC64

/// Installs, once per process, the bridge from the `log` crate to `tracing` that the
/// binary's `tracing-subscriber` installs, at the widest level the `log` crate's
/// static cap allows. Records of the WebSocket and TLS libraries then reach the
/// thread's capture as they reach the Hub's log, so a test sees whatever those
/// libraries write.
fn install_log_bridge() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        tracing_log::LogTracer::init().expect("no other logger is installed in the tests");
        log::set_max_level(log::LevelFilter::Trace);
    });
}

/// A `tracing` subscriber that records everything at any level for as long as the
/// capture lives. The tests run on one thread, so the thread-local default reaches the
/// supervisor's tasks.
struct LogCapture {
    sink: Arc<StdMutex<Vec<u8>>>,
    _guard: tracing::subscriber::DefaultGuard,
    /// A second registered dispatcher. While only one dispatcher is registered,
    /// `tracing` computes a call site's interest from the default of whichever thread
    /// first reaches it, so a call site first reached by another test's thread (which
    /// has no subscriber) is cached as disabled for this capture too, and events are
    /// silently missing. With two registered dispatchers the interest is merged over
    /// all of them, and the thread-local default decides at each event.
    _anchor: tracing::Dispatch,
}

impl LogCapture {
    fn start() -> Self {
        install_log_bridge();
        #[derive(Clone)]
        struct Sink(Arc<StdMutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("sink").extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Arc::new(StdMutex::new(Vec::new()));
        let writer = Sink(Arc::clone(&sink));
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let anchor = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let guard = tracing::subscriber::set_default(subscriber);
        tracing::callsite::rebuild_interest_cache();
        Self {
            sink,
            _guard: guard,
            _anchor: anchor,
        }
    }

    fn text(&self) -> String {
        String::from_utf8(self.sink.lock().expect("sink").clone()).expect("UTF-8 log")
    }
}

/// Every cell of every row of `table`, as text.
fn dump_table(store: &crate::db::HubStore, table: &str) -> String {
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(&format!("SELECT * FROM {table}"))
        .expect("statement");
    let columns = statement.column_count();
    let rows = statement
        .query_map([], |row| {
            (0..columns)
                .map(|column| row.get_ref(column).map(|value| format!("{value:?}")))
                .collect::<Result<Vec<_>, _>>()
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    rows.into_iter()
        .map(|row| row.join(" | "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A `data:update` frame for another stream vehicle id than the tests' default.
fn update_for(tag: &str, time: i64, row: &str) -> String {
    json!({"msg_type": "data:update", "tag": tag, "value": format!("{time},{row}")}).to_string()
}

#[tokio::test(start_paused = true)]
async fn sc64_logs_audit_rows_and_errors_never_carry_secrets_or_the_override_path() {
    const TOKEN: &str = "tok-SECRET-1";
    const BIG_TAG: u64 = 987_654_321_123;
    const OVERRIDE: &str = "ws://127.0.0.1:9/k/abc?key=zzz";
    let logs = LogCapture::start();
    let row_with_latitude = V1.replacen("10.5", "10.123456", 1);
    let tag = BIG_TAG.to_string();
    let error_for_tag = json!({
        "msg_type": "data:error",
        "tag": tag,
        "error_type": "something_new",
        "value": format!("client says {TOKEN}"),
    })
    .to_string();
    // A server that echoes the token in the `error_type` of an error frame and of a
    // hello: it passes no grammar a logger may trust, so it is logged as `other`.
    let echoed_error = json!({
        "msg_type": "data:error",
        "tag": tag,
        "error_type": TOKEN,
        "value": format!("client says {TOKEN}"),
    })
    .to_string();
    let echoed_hello = json!({
        "msg_type": "control:hello",
        "code": 500,
        "error_type": TOKEN,
    })
    .to_string();
    let peer = plan_peer(vec![
        // A full frame, then the server closes (SC22 style).
        Plan::data(
            vec![(0, update_for(&tag, T0, &row_with_latitude))],
            0,
            End::Close(1000),
        ),
        // Two refusals (SC51 style): the second reports the rejection.
        Plan::ends(End::Hello(401)),
        Plan::ends(End::Hello(401)),
        // An unknown error whose text echoes the token (SC52 style).
        Plan::ends(End::Raw(error_for_tag)),
        // The token echoed as the error type, in an error frame and in a hello.
        Plan::ends(End::Raw(echoed_error)),
        Plan::ends(End::Raw(echoed_hello)),
        Plan::hold(),
    ])
    .await;
    let mut rig = Rig::for_peer(&peer, Creds::offline(TOKEN));
    rig.endpoint = OVERRIDE.to_owned();
    rig.tag = BIG_TAG;
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(
        kinds(&mut run, 7).await,
        [
            "Healthy",
            "Telemetry",
            "TransportUnavailable",
            "AuthRejected",
            "TransportUnavailable",
            "TransportUnavailable",
            "TransportUnavailable"
        ]
    );
    peer.log.wait_for("subscribe-received", 5).await;
    run.shut_down().await.expect("shutdown");

    // A connect that fails on an endpoint with a secret path: a TLS handshake that
    // never completes (the peer is silent) until the 10 s connect limit.
    let silent = Peer::spawn(|conn| async move { conn.hold().await }).await;
    let mut rig = Rig::for_peer(&silent, Creds::offline(TOKEN));
    rig.endpoint = "wss://127.0.0.1:9/k/abc?key=zzz".to_owned();
    rig.tag = BIG_TAG;
    let (rig, _dir2) = rig.with_store();
    let silent_store = rig.store.clone().expect("store");
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    run.shut_down().await.expect("shutdown");

    let output = logs.text();
    // The WebSocket library renders a frame's payload as hex when it logs a write
    // (`writing frame`), so the token is also looked for in both cases of hex, as
    // well as the library's own phrases for a frame and an upgrade request.
    let token_hex: String = TOKEN.bytes().map(|byte| format!("{byte:02x}")).collect();
    let token_hex_upper = token_hex.to_uppercase();
    for secret in [
        TOKEN,
        token_hex.as_str(),
        token_hex_upper.as_str(),
        "987654321123",
        "10.123456",
        "/k/abc",
        "key=zzz",
        "client says",
        "1760000000000",
        "writing frame",
        "Sending frame",
        "Received message",
        "Request: ",
    ] {
        assert!(
            !output.contains(secret),
            "the log carries {secret:?}:\n{output}"
        );
    }
    assert!(
        output.contains("127.0.0.1"),
        "the host is logged:\n{output}"
    );
    assert!(
        output.contains("something_new"),
        "the token is logged:\n{output}"
    );
    // The token echoed as an error type is logged as `other`, never as itself.
    assert!(
        output.contains("error_type=\"other\"") || output.contains("error_type=other"),
        "an error type outside the grammar is logged as `other`:\n{output}"
    );
    for table in ["outbound_request_receipts", "stream_session_receipts"] {
        for text in [dump_table(&store, table), dump_table(&silent_store, table)] {
            assert!(
                !text.contains("/k/abc") && !text.contains("key=zzz") && !text.contains(TOKEN),
                "{table}: {text}"
            );
        }
    }

    // Errors never repeat the URL either: a rejected override and a supervisor built
    // with a plaintext endpoint under the production rule.
    for rejected in [
        "ws://stream.example/k/abc?key=zzz",
        "wss://user:pw@stream.example/k/abc?key=zzz",
    ] {
        let error = validate_endpoint_override(rejected).expect_err("rejected");
        let (events, _receiver) = tokio::sync::mpsc::channel(1);
        let creds = Creds::offline(TOKEN);
        let Err(built) = TeslaStreamSupervisor::new_legacy_auth_for_test_production_policy(
            crate::owner_api::VehicleId::from_test(9),
            crate::owner_api::StreamVehicleId::from_test(BIG_TAG),
            Arc::clone(&creds.manager),
            StreamRegion::Global,
            rejected.to_owned(),
            creds.client.clone(),
            events,
        ) else {
            panic!("a plaintext or credentialed URL is refused");
        };
        for text in [
            format!("{error}"),
            format!("{error:?}"),
            format!("{built}"),
            format!("{built:?}"),
        ] {
            assert!(
                !text.contains("/k/abc") && !text.contains("key=zzz") && !text.contains("pw@"),
                "{text}"
            );
        }
    }
}

/// R24 and R52: a server that reflects the subscribed token into the gear text or into
/// the `error_type` of an error frame. The grammar of both fields admits a short token
/// of lower-case letters, digits and underscores, so only a comparison with the
/// session's own token can keep it out: the gear is dropped as an invalid field before
/// the frame is emitted (and so before it is stored), and the error type is logged as
/// `other`.
#[tokio::test(start_paused = true)]
async fn r24_a_token_reflected_in_the_gear_or_the_error_type_is_never_emitted_or_logged() {
    const TOKEN: &str = "tok_secret_1";
    let logs = LogCapture::start();
    let reflected_gear = V1.replacen(",D,", &format!(",{TOKEN},"), 1);
    let embedded_gear = V1.replacen(",D,", &format!(",x {TOKEN} y,"), 1);
    let peer = plan_peer(vec![
        Plan::data(
            vec![
                (0, u(T0, &reflected_gear)),
                (0, u(T0 + 1_000, &embedded_gear)),
                (0, u(T0 + 2_000, V1)),
            ],
            0,
            End::Error(TOKEN),
        ),
        Plan::hold(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline(TOKEN)).start();
    let mut gears = Vec::new();
    loop {
        match run.event().await {
            StreamEvent::Healthy => {}
            StreamEvent::Telemetry { update, .. } => gears.push(update.shift_state.clone()),
            StreamEvent::TransportUnavailable => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(gears, [None, None, Some("D".to_owned())]);
    assert_eq!(run.session_counts()[0].fields_invalid, 2);
    run.shut_down().await.expect("shutdown");
    let output = logs.text();
    assert!(
        !output.contains(TOKEN),
        "the log carries the token:\n{output}"
    );
    assert!(
        output.contains("error_type=\"other\"") || output.contains("error_type=other"),
        "the reflected error type is logged as `other`:\n{output}"
    );
}

/// R24: the gear and substring comparison keeps a four-byte minimum, because a shorter
/// one could remove a genuine gear letter. A session subscribed with the token `D` keeps
/// the gear `D` of a valid update; accepted 1, no invalid field.
#[tokio::test(start_paused = true)]
async fn r24_a_gear_letter_is_kept_when_it_is_the_subscribed_token() {
    let peer = plan_peer(vec![Plan::data(vec![(0, u(T0, V1))], 0, End::Hold)]).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("D")).with_store();
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::Healthy);
    match run.event().await {
        StreamEvent::Telemetry { update, .. } => {
            assert_eq!(update.shift_state.as_deref(), Some("D"));
        }
        other => panic!("expected telemetry, got {other:?}"),
    }
    run.shut_down().await.expect("shutdown");
    let counts = run.session_counts();
    assert_eq!(counts[0].accepted, 1);
    assert_eq!(counts[0].fields_invalid, 0);
}

/// R52: an `error_type` that is exactly the session's stream tag is logged as `other`, at
/// the `data:error` site and at the non-success hello site, whatever the token is. The
/// class is read from the original value: a hello 401 with that `error_type` is still
/// `Unknown`, so it makes an outage event and no refusal.
#[tokio::test(start_paused = true)]
async fn r52_a_reflected_stream_tag_is_not_logged_as_an_error_type() {
    let logs = LogCapture::start();
    let hello = json!({"msg_type": "control:hello", "code": 401, "error_type": TAG}).to_string();
    let peer = plan_peer(vec![
        Plan::ends(End::Error(TAG)),
        Plan::ends(End::Raw(hello)),
        Plan::hold(),
    ])
    .await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let mut run = rig.start();
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    assert_eq!(run.event().await, StreamEvent::TransportUnavailable);
    peer.log.wait_for("accept", 3).await;
    run.shut_down().await.expect("shutdown");
    let output = logs.text();
    // Not a search for the bare digits: a timestamp may contain them.
    assert!(
        !output.contains(&format!("error_type={TAG}"))
            && !output.contains(&format!("error_type=\"{TAG}\"")),
        "the log carries the stream tag as an error type:\n{output}"
    );
    for site in ["stream error frame", "stream hello was not a success"] {
        let line = output
            .lines()
            .find(|line| line.contains(site))
            .unwrap_or_else(|| panic!("no `{site}` record:\n{output}"));
        assert!(
            line.contains("error_type=\"other\"") || line.contains("error_type=other"),
            "{line}"
        );
    }
}

/// R52: the WebSocket library logs every frame it reads and writes at trace level
/// through the `log` crate (the subscribe frame with the token, every data frame with
/// its values). With the `log` bridge installed and the capture at TRACE, such a record
/// must not exist: the crate's static level compiles it out, so no environment variable
/// can bring it back. A record at info still passes, which shows the bridge works.
#[test]
fn r52_library_trace_and_debug_records_are_compiled_out_whatever_the_environment_says() {
    assert!(log::STATIC_MAX_LEVEL <= log::LevelFilter::Info);
    let logs = LogCapture::start();
    log::set_max_level(log::LevelFilter::Trace);
    log::trace!(target: "tungstenite::protocol", "Sending frame: tok-SECRET-1");
    log::trace!(target: "tungstenite::protocol", "Received message 10.123456");
    log::debug!(target: "tokio_tungstenite", "debug tok-SECRET-1");
    log::info!(target: "tungstenite", "info-probe");
    let output = logs.text();
    assert!(
        output.contains("info-probe"),
        "a record under the cap reaches the capture through the bridge:\n{output}"
    );
    for secret in [
        "tok-SECRET-1",
        "10.123456",
        "Sending frame",
        "Received message",
    ] {
        assert!(!output.contains(secret), "{secret:?} is logged:\n{output}");
    }
}

// ----------------------------------------------------------------------------- SC79

#[tokio::test(start_paused = true)]
async fn sc79_an_upgrade_refusal_on_an_override_endpoint_is_a_refusal_and_two_are_reported() {
    let peer = plan_peer(vec![Plan::Refuse(401), Plan::Refuse(403), Plan::hold()]).await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    // Session 1: no event, the retry follows after 2.5 s. Session 2 (403): the report.
    expect_silence(&mut run, secs(2)).await;
    assert_eq!(peer.log.count("accept"), 1);
    assert_eq!(run.event().await, StreamEvent::AuthRejected);
    peer.log.wait_for("accept", 3).await;
    assert_eq!(delays(&peer), vec![millis(2_500), secs(150)]);
    let rows = request_rows(&store);
    assert_eq!(rows[0].operation, "stream_connect");
    assert_eq!(rows[0].outcome, "authentication_rejected");
    assert_eq!(rows[0].http_status, Some(401));
    assert_eq!(rows[1].operation, "stream_connect");
    assert_eq!(rows[1].outcome, "authentication_rejected");
    assert_eq!(rows[1].http_status, Some(403));
    let sessions = session_rows(&store);
    assert_eq!(sessions[0].outcome, "failed");
    assert_eq!(sessions[1].outcome, "failed");
    // No subscribe request was begun for either refused upgrade.
    assert!(
        rows[..2]
            .iter()
            .all(|row| row.operation == "stream_connect")
    );
    run.shut_down().await.expect("shutdown");
}

// ----------------------------------------------------------------------- SC80, SC87

#[tokio::test(start_paused = true)]
async fn sc80_stand_in_a_terminal_credential_error_opens_no_socket_writes_no_rows_and_sends_no_event()
 {
    // Stand-in for the region conflict (C5 R5 `RegionConflict`), which reaches this unit
    // as a terminal credential error before any network use (R1, R6a): the manager test
    // double returns a terminal error. It is re-run against C5's manager when C5 lands
    // (see the ignored test below).
    let peer = plan_peer(vec![Plan::hold()]).await;
    let terminal = Arc::new(AtomicBool::new(true));
    let mut rig = Rig::for_peer(&peer, Creds::offline("tok-1"));
    rig.region = StreamRegion::China;
    rig.terminal = Some(terminal);
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    sleep(secs(10 * 60)).await;
    assert_eq!(peer.log.count("accept"), 0, "no socket was opened");
    assert!(request_rows(&store).is_empty());
    assert!(session_rows(&store).is_empty());
    assert!(run.events.try_recv().is_err(), "no event was sent");
    run.shut_down().await.expect("shutdown");
}

/// SC80 against the credential unit (C5 R5, R56, C4 R57 b): the stored pair's
/// issuer is the global one and `stream_region` is `china`, so the unit holds a
/// `region_conflict` configuration error. It reaches the stream as a terminal
/// credential error before any network use: no socket, no rows, no event.
#[tokio::test(start_paused = true)]
async fn sc80_region_conflict_with_the_real_credential_unit() {
    use crate::legacy_auth::{AuthEndpointConfig, AuthMode, ConfigurationReason, LegacyAuth};

    let global = {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        format!(
            "{}.{}.c2ln",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#),
            URL_SAFE_NO_PAD.encode(
                json!({"iss": "https://auth.tesla.com/oauth2/v3", "exp": 1_800_028_800_i64})
                    .to_string()
            )
        )
    };
    let conflict = LegacyAuth::from_persisted_state(
        global.as_str(),
        "refresh-0",
        0,
        0,
        &AuthEndpointConfig {
            mode: AuthMode::Tesla,
            region: Some(StreamRegion::China),
            token_url: None,
            allow_loopback_http: false,
        },
    )
    .expect_err("the configured region conflicts with the token's issuer");
    assert_eq!(
        conflict.configuration_reason(),
        Some(ConfigurationReason::RegionConflict)
    );
    // The manager's answer to a request is that error, and it is terminal.
    let manager = crate::credentials::LegacyAuthManager::for_test_with_configuration_error(
        LegacyAuth::for_test(
            url::Url::parse("http://127.0.0.1:9/oauth2/v3").expect("issuer"),
            "tok-1",
            "refresh-0",
        ),
        ConfigurationReason::RegionConflict,
    );
    assert!(
        manager
            .access_token_for_sensitive_use()
            .expect_err("a configuration error")
            .is_terminal_auth_failure()
    );
    let peer = plan_peer(vec![Plan::hold()]).await;
    let mut rig = Rig::for_peer(&peer, Creds::from_manager(manager));
    rig.region = StreamRegion::China;
    let (rig, _dir) = rig.with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    sleep(secs(10 * 60)).await;
    assert_eq!(peer.log.count("accept"), 0, "no socket was opened");
    assert!(request_rows(&store).is_empty());
    assert!(session_rows(&store).is_empty());
    assert!(run.events.try_recv().is_err(), "no event was sent");
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn sc87_stand_in_failing_refreshes_do_not_change_the_attempt_schedule_or_report_a_rejection()
{
    // Stream side of SC87. The issuer answers 503 to every token request and the peer
    // closes every session at once. The unit calls `refresh_if_due` once per attempt
    // (R40), a failed refresh does not stop the stream (R6 step 2), the attempts follow
    // R34 (1, 2, 4, ... 128 s, then 150 s at u = 0), and nothing reports a rejection.
    // The spacing of the token requests at the issuer (at least 60 s, C5 R16, R19, R31)
    // is C5's and is asserted by the ignored test below.
    let issuer = Issuer::spawn_with_status(Duration::ZERO, "tok-2", "ref-2", 503).await;
    let creds = Creds::due(issuer.url.clone(), "tok-1").held_by(&issuer);
    let peer = plan_peer(vec![Plan::ends(End::Close(1000)); 60]).await;
    let started = Instant::now();
    let mut run = Rig::for_peer(&peer, creds).start();
    // The first attempt's refresh is a real HTTP exchange with the loopback issuer; let
    // it finish before the paused clock is moved (the later attempts' refreshes are
    // deferred by the manager's own gate and do no I/O).
    peer.log.wait_for("accept", 1).await;
    sleep(secs(30 * 60)).await;
    assert!(Instant::now() - started >= secs(30 * 60));
    let delays = delays(&peer);
    // Counters 1 to 8 give 1, 2, 4, ... 128 s; from counter 9 the nominal 300 s gives
    // 150 s.
    let schedule = [1, 2, 4, 8, 16, 32, 64, 128].map(secs);
    assert!(delays.len() > schedule.len() + 3, "{delays:?}");
    assert_eq!(delays[..schedule.len()], schedule);
    assert!(delays[schedule.len()..].iter().all(|d| *d == secs(150)));
    assert_eq!(
        peer.log.count("refresh"),
        peer.log.count("accept"),
        "one refresh call per attempt"
    );
    let mut seen = Vec::new();
    while let Ok(event) = run.events.try_recv() {
        seen.push(kind(&event));
    }
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|k| *k == "TransportUnavailable"),
        "{seen:?}"
    );
    run.shut_down().await.expect("shutdown");
}

/// SC87 against the credential unit (C5 R16, R19, R31, R57 a): a managed pair whose
/// refresh is due, an issuer that answers 503 to every token request, a peer that
/// closes every session at once, thirty minutes of paused time. The unit runs on the
/// runtime's clock, so its retry schedule moves with the supervisor's waits. The
/// stream attempts follow R34 and nothing is reported as a rejection; the token
/// requests at the issuer are spaced by the unit's own retry schedule (R31: 60 s,
/// then 120 s, then 240 s, then 300 s), never closer than 60 s.
#[tokio::test(start_paused = true)]
async fn sc87_refresh_failure_pacing_with_the_real_credential_unit() {
    let creds = Creds::due_on_the_runtime_clock(
        url::Url::parse("http://127.0.0.1:9/oauth2/v3").expect("issuer"),
        "tok-1",
    );
    let manager = Arc::clone(&creds.manager);
    let peer = plan_peer(vec![Plan::ends(End::Close(1000)); 80]).await;
    let started = Instant::now();
    let mut run = Rig::for_peer(&peer, creds).start();
    peer.log.wait_for("accept", 1).await;
    sleep(secs(30 * 60)).await;
    assert!(Instant::now() - started >= secs(30 * 60));
    let delays = delays(&peer);
    let schedule = [1, 2, 4, 8, 16, 32, 64, 128].map(secs);
    assert!(delays.len() > schedule.len() + 3, "{delays:?}");
    assert_eq!(delays[..schedule.len()], schedule);
    assert!(delays[schedule.len()..].iter().all(|d| *d == secs(150)));
    // The token requests: the first at once, then spaced by the unit's schedule.
    let requests = manager.lock().await.injected_request_times();
    assert!(requests.len() >= 5, "{requests:?}");
    assert_eq!(requests[0] - started, Duration::ZERO);
    let gaps: Vec<Duration> = requests.windows(2).map(|pair| pair[1] - pair[0]).collect();
    for gap in &gaps {
        assert!(*gap >= secs(60), "{gaps:?}");
    }
    // The delays are the schedule of R31 (60, 120, 240, then the cap of 300 s), each
    // noticed at the next attempt of the stream, which comes at most 150 s later
    // (R34): never early, and never more than that late.
    for (gap, nominal) in gaps.iter().zip([60, 120, 240, 300, 300, 300]) {
        assert!(
            *gap >= secs(nominal) && *gap <= secs(nominal + 150),
            "{gaps:?}"
        );
    }
    let mut seen = Vec::new();
    while let Ok(event) = run.events.try_recv() {
        seen.push(kind(&event));
    }
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|k| *k == "TransportUnavailable"),
        "no rejection is reported: {seen:?}"
    );
    run.shut_down().await.expect("shutdown");
}

// ----------------------------------------------------------------------------- SC88

#[test]
fn sc88_a_frame_delivered_again_after_a_restart_creates_no_stored_observation() {
    use crate::db::{
        HubStore, ObservationInput, SourceDescriptor, StreamObservationResult, VehicleDescriptor,
    };
    let directory = crate::private_tempdir().expect("temporary directory");
    let store = HubStore::initialize(directory.path()).expect("store");
    let source = store
        .register_source(
            &SourceDescriptor::new("owner_api_compat", "local_installation_v1"),
            1_000,
        )
        .expect("source");
    let vehicle = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, TAG), 1_001)
        .expect("vehicle");
    let observation = |frame: &str| {
        let update = parse_data_update(frame).expect("a valid frame");
        ObservationInput {
            source_id: source.source_id,
            vehicle_id: vehicle.vehicle_id,
            observed_at_ms: update.timestamp_ms,
            payload: crate::lifecycle::stream_observation_payload(&update),
        }
    };
    let delivered = observation(&u(T0, V1));
    let first = store
        .accept_stream_observation_and_lifecycle(&delivered, W, 1)
        .expect("first delivery");
    let StreamObservationResult::Committed {
        observation_id: first_id,
    } = first
    else {
        panic!("the first delivery is committed, got {first:?}");
    };
    // What storage holds for the vehicle: the rows of both observation tables, and the
    // highest observation id either has used.
    let held = |store: &HubStore| -> (i64, i64, i64, i64) {
        let connection = store.open().expect("connection");
        let one = |sql: &str| -> i64 {
            connection
                .query_row(
                    sql,
                    rusqlite::params![vehicle.vehicle_id.to_string()],
                    |row| row.get(0),
                )
                .expect("count")
        };
        (
            one("SELECT COUNT(*) FROM raw_observations WHERE vehicle_id = ?1"),
            one("SELECT COUNT(*) FROM current_observations WHERE vehicle_id = ?1"),
            one(
                "SELECT COALESCE(MAX(observation_id), 0) FROM current_observations
                  WHERE vehicle_id = ?1",
            ),
            one(
                "SELECT COALESCE(MAX(observed_at_ms), 0) FROM current_observations WHERE vehicle_id = ?1",
            ),
        )
    };
    let before = held(&store);
    // The current observation is the one just stored. (Processed raw rows are pruned
    // once the lifecycle has consumed them, so `raw_observations` need not hold it.)
    assert_eq!((before.1, before.2, before.3), (1, first_id, T0));
    drop(store);

    // A restarted Hub opens the same storage; the same frame arrives again.
    let restarted = HubStore::open_existing(directory.path()).expect("reopened store");
    assert_eq!(held(&restarted), before);
    let again = restarted
        .accept_stream_observation_and_lifecycle(&delivered, W + 60_000, 1)
        .expect("second delivery");
    assert_eq!(again, StreamObservationResult::IgnoredDuplicate);
    assert_eq!(held(&restarted), before, "no new stored observation");
    assert_eq!(
        restarted
            .stream_watermark(vehicle.vehicle_id)
            .expect("watermark"),
        Some(T0)
    );

    // The persisted watermark, not the per-run memory, does this: an older sample is
    // ignored too, and a newer one is committed under a later observation id.
    let older = observation(&u(T0 - 1_000, V1));
    assert_eq!(
        restarted
            .accept_stream_observation_and_lifecycle(&older, W + 61_000, 1)
            .expect("older delivery"),
        StreamObservationResult::IgnoredDuplicate
    );
    assert_eq!(held(&restarted), before);
    let newer = observation(&u(T0 + 1_000, V1));
    let StreamObservationResult::Committed {
        observation_id: newer_id,
    } = restarted
        .accept_stream_observation_and_lifecycle(&newer, W + 62_000, 1)
        .expect("newer delivery")
    else {
        panic!("a newer sample is committed");
    };
    assert!(newer_id > first_id);
    assert_eq!(held(&restarted).3, T0 + 1_000);
}

// ----------------------------------------------------------------------------- SC89

#[tokio::test]
async fn sc89_ten_sequential_runs_hold_one_socket_at_a_time_with_their_own_correlation_ids() {
    use crate::fake_tesla::{
        AdvanceMode, FIXTURE_VID, FakeTeslaSource, StreamAuditEvent, StreamScript,
    };
    let fake = FakeTeslaSource::spawn_canonical(AdvanceMode::Manual)
        .await
        .expect("fake Tesla");
    fake.set_stream_script(StreamScript::Normal);
    let directory = crate::private_tempdir().expect("temporary directory");
    let store = crate::db::HubStore::initialize(directory.path()).expect("store");
    let subscribes = || {
        fake.audited_stream_events()
            .iter()
            .filter(|event| event.event == StreamAuditEvent::Subscribe && event.accepted)
            .count()
    };
    for run_number in 1..=10_usize {
        let creds = Creds::current(fake.oauth_issuer_url(), "tok-1");
        let mut rig = Rig::new(fake.stream_endpoint().to_owned(), creds);
        rig.tag = FIXTURE_VID;
        rig.store = Some(store.clone());
        let mut run = rig.start();
        // Wait, in real time, until the fake has recorded this run's subscribe.
        let mut waited = 0;
        while subscribes() < run_number {
            tokio::time::sleep(Duration::from_millis(10)).await;
            waited += 1;
            assert!(waited < 2_000, "run {run_number}: no subscribe within 20 s");
        }
        run.shut_down().await.expect("shutdown");
    }
    let stats = fake.stream_session_stats();
    assert_eq!(stats.max_concurrent_sessions, 1);
    assert_eq!(stats.connection_attempts, 10);
    let sessions = session_rows(&store);
    assert_eq!(sessions.len(), 10);
    let mut ids: Vec<_> = sessions
        .iter()
        .map(|row| row.correlation_id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 10, "every run has its own correlation id");
    fake.shutdown().await;
}

// ------------------------------------------------------------------ R37, R54 extras

#[tokio::test(start_paused = true)]
async fn r37_a_session_the_silence_rule_ends_at_exactly_60_seconds_keeps_its_back_off() {
    // Session 1 ends at once (counter 1). Session 2 delivers one moving frame and then
    // only answers pings, so data silence ends it 60 s after that frame, at the same
    // instant its stable timer is due. The end wins the tie: that minute did not work,
    // so the counter goes on to 2 and the next attempt waits 2 s (nominal 4 s at u = 0).
    // A stable reset that came first would make it counter 1, and 1 s.
    let peer = plan_peer(vec![
        Plan::ends(End::Close(1000)),
        Plan::data(vec![(0, u(T0, V1))], 0, End::Hold),
        Plan::hold(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    assert_eq!(
        kinds(&mut run, 4).await,
        [
            "TransportUnavailable",
            "Healthy",
            "Telemetry",
            "TransportUnavailable"
        ]
    );
    let ended = Instant::now();
    let frame_at = peer.log.times("accept")[1];
    assert_eq!(ended - frame_at, secs(60));
    peer.log.wait_for("accept", 3).await;
    assert_eq!(peer.log.times("accept")[2] - ended, secs(2));
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r37_a_duplicate_first_frame_also_starts_the_stable_timer() {
    // Session 1 delivers a frame and the server closes (counter 1, wait 1 s). Session 2
    // opens with the same frame again, a duplicate that counts as its first accepted
    // frame (R26, R37), then a new frame at 30 s, and the server closes at 61 s: stable
    // at 60 s, so the end is counter 1 again and the wait is 1 s (counter 2 would give
    // 2 s).
    let peer = plan_peer(vec![
        Plan::data(vec![(0, u(T0, V1))], 0, End::Close(1000)),
        Plan::data(
            vec![(0, u(T0, V1)), (30, u(T0 + 30_000, V1))],
            31,
            End::Close(1000),
        ),
        Plan::hold(),
    ])
    .await;
    let mut run = Rig::for_peer(&peer, Creds::offline("tok-1")).start();
    peer.log.wait_for("accept", 3).await;
    assert_eq!(delays(&peer), vec![secs(1), secs(1)]);
    // The duplicate gave Healthy but no Telemetry (session 2): Healthy, Telemetry,
    // outage, Healthy, Telemetry (the new frame), outage.
    assert_eq!(
        kinds(&mut run, 6).await,
        [
            "Healthy",
            "Telemetry",
            "TransportUnavailable",
            "Healthy",
            "Telemetry",
            "TransportUnavailable"
        ]
    );
    run.shut_down().await.expect("shutdown");
}

#[tokio::test(start_paused = true)]
async fn r54_the_rows_of_a_run_that_was_cut_off_stay_started_and_are_never_updated() {
    // Run 1 is aborted (a killed process) after its subscribe was written: its session
    // and subscribe rows stay `started`. Run 2 on the same storage has its own
    // correlation id and never touches them.
    let peer = plan_peer(vec![
        Plan::hold(),
        Plan::data(vec![(0, u(T0, V1))], 0, End::Hold),
    ])
    .await;
    let (rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let mut run = rig.start();
    peer.log.wait_for("subscribe-received", 1).await;
    run.task.take().expect("task").abort();
    sleep(secs(1)).await;
    let before = (request_rows(&store), session_rows(&store));
    assert_eq!(
        before
            .0
            .iter()
            .map(|row| (row.operation.as_str(), row.outcome.as_str()))
            .collect::<Vec<_>>(),
        [
            ("stream_connect", "success"),
            ("stream_subscribe", "started")
        ]
    );
    assert_eq!(before.1.len(), 1);
    assert_eq!(before.1[0].outcome, "started");

    let mut second = Rig::for_peer(&peer, Creds::offline("tok-1"));
    second.store = Some(store.clone());
    let mut run = second.start();
    assert_eq!(kinds(&mut run, 2).await, ["Healthy", "Telemetry"]);
    run.shut_down().await.expect("shutdown");

    // Run 1's rows are exactly as they were; run 2's rows are its own.
    let rows = request_rows(&store);
    let sessions = session_rows(&store);
    assert_eq!(rows[..2], before.0[..]);
    assert_eq!(sessions[0], before.1[0]);
    assert_eq!(sessions.len(), 2);
    assert_ne!(sessions[1].correlation_id, sessions[0].correlation_id);
    assert_eq!(sessions[1].outcome, "orderly_shutdown");
    assert!(
        rows[2..]
            .iter()
            .all(|row| row.correlation_id == sessions[1].correlation_id)
    );
    // The audit summary reports the unclean end.
    let summary = store.stream_audit_summary_since(0).expect("summary");
    assert_eq!(summary.unresolved_sessions, 1);
    assert_eq!(summary.unresolved_attempts, 1);
}
