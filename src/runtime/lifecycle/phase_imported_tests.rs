// SPDX-License-Identifier: AGPL-3.0-only

//! Part P imported-state spec tests (R14, ST80), split from `phase_tests.rs`
//! to keep each source file under the repository line limit.

use super::*;

fn imported_online_state() -> OpenSessionState {
    let mut session = TeslaMateOpenSession {
        car_id: 1,
        ..Default::default()
    };
    session.state = Some(TeslaMateState {
        id: 5,
        car_id: 1,
        state: "online".to_owned(),
        start_date_ms: T0 - HOUR,
        end_date_ms: None,
    });
    session.watermarks.states.max_timestamp_ms = Some(T0 - MIN);
    seed_imported_open_session_state(uuid::Uuid::from_u128(7), &session, None).expect("seed")
}

#[test]
fn st80_imported_state_interval_continues_when_the_text_matches() {
    let seeded = imported_online_state();
    assert_eq!(seeded.imported_state_watermark_ms, Some(T0 - MIN));
    assert_eq!(seeded.next_state_id, 6);
    let result = step(
        seeded,
        &v(
            1,
            T0,
            json!({"vehicle_state": {"timestamp": T0, "odometer": 1.0}}),
        ),
    );
    assert!(result.delta.states.is_empty());
    assert_eq!(
        result.state.open_state.as_ref().map(|open| open.id),
        Some(5)
    );
    assert_eq!(result.state.last_reach.as_deref(), Some("online"));
}

#[test]
fn st80_imported_state_interval_closes_with_its_imported_id() {
    let result = step(imported_online_state(), &d(1, T0, "asleep"));
    assert_eq!(
        result.delta.states,
        vec![
            ProjectionState {
                id: 5,
                car_id: 1,
                state: "online".to_owned(),
                start_date_ms: T0 - HOUR,
                end_date_ms: Some(T0),
            },
            ProjectionState {
                id: 6,
                car_id: 1,
                state: "asleep".to_owned(),
                start_date_ms: T0,
                end_date_ms: None,
            },
        ]
    );
}

#[test]
fn st80_a_reach_at_or_before_the_imported_watermark_is_stale() {
    let seeded = imported_online_state();
    let result = step(seeded.clone(), &d(1, T0 - MIN, "asleep"));
    assert!(result.delta.states.is_empty());
    assert_eq!(result.state.last_reach, None);
    let result = step(seeded, &d(1, T0 - MIN + 1, "asleep"));
    assert_eq!(closed_states(&result.delta).len(), 1);
}

#[test]
fn st81_a_stream_wake_between_two_asleep_discoveries() {
    let result = all(
        OpenSessionState::new(),
        &[
            d(1, T0, "asleep"),
            frame(2, T0 + MIN, "0,1000.0,80,25,180,10.0,20.0,,,200,210,181"),
            d(3, T0 + 5 * MIN, "asleep"),
        ],
    );
    assert_eq!(
        closed_states(&result.delta),
        vec![
            (1, "asleep".to_owned(), T0, T0 + MIN),
            (2, "online".to_owned(), T0 + MIN, T0 + 5 * MIN),
        ]
    );
    assert_eq!(
        result
            .state
            .open_state
            .map(|open| (open.id, open.state, open.start_date_ms)),
        Some((3, "asleep".to_owned(), T0 + 5 * MIN))
    );
}

#[test]
fn st81_clock_skew_delays_a_reach_change_by_one_discovery() {
    // A frame at Tesla time +5m30s, then an asleep discovery at Hub time +5m.
    let mut state = step(OpenSessionState::new(), &d(1, T0, "asleep")).state;
    state = step(
        state,
        &frame(
            2,
            T0 + 5 * MIN + 30_000,
            "0,1000.0,80,25,180,10.0,20.0,,,200,210,181",
        ),
    )
    .state;
    assert_eq!(state.last_reach.as_deref(), Some("online"));
    let early = step(state, &d(3, T0 + 5 * MIN, "asleep"));
    assert_eq!(early.state.last_reach.as_deref(), Some("online"), "ignored");
    // The next discovery still differs from last_reach, so it is delivered.
    let later = step(early.state, &d(4, T0 + 6 * MIN, "asleep"));
    assert_eq!(later.state.last_reach.as_deref(), Some("asleep"));
    assert_eq!(later.state.phase, VehiclePhase::Asleep);
}
