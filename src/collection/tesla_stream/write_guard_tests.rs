// SPDX-License-Identifier: AGPL-3.0-only

// B2 fix round, P3b fix 2 (R-P3bF2-1, R-P3bF2-3): the stream's audit receipts (the
// session row and the connect, subscribe and unsubscribe request rows) are store
// mutations of a supervised run. Each is preceded by the run's write guard, with no
// await between the guard and the mutation; once the guard refuses, nothing is started
// or completed any more and what was started stays unresolved, as after a crash. The
// guard here refuses from its `n`th consultation on, and reads the audit rows at that
// very moment, so the rows at the end of the run can be compared with them. Synthetic
// data only; the peer is an in-memory loopback.

use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicUsize, Ordering},
};

use super::{StreamEvent, StreamSupervisorError, test_peer::*};

/// What the audit rows were when the guard refused first.
type Rows = (Vec<RequestRow>, Vec<SessionRow>);

/// One run of the stream over a peer that sends one data frame: the guard refuses from
/// its `refuses_from`th consultation on (`None`: never). Returns how often the guard was
/// consulted, the rows at the first refusal, the rows at the end and the run's result.
async fn audited_run(
    refuses_from: Option<usize>,
) -> (usize, Option<Rows>, Rows, Result<(), StreamSupervisorError>) {
    let peer = Peer::spawn(|conn| async move {
        let mut ws = conn.ws().await;
        ws.subscribe().await;
        ws.send(&u(T0, V1)).await;
        ws.drain().await;
    })
    .await;
    let (mut rig, _dir) = Rig::for_peer(&peer, Creds::offline("tok-1")).with_store();
    let store = rig.store.clone().expect("store");
    let consulted = Arc::new(AtomicUsize::new(0));
    let first_refusal: Arc<StdMutex<Option<Rows>>> = Arc::default();
    let (counter, slot, observed) = (
        Arc::clone(&consulted),
        Arc::clone(&first_refusal),
        store.clone(),
    );
    rig.write_guard = Some(Arc::new(move || {
        let seen = counter.fetch_add(1, Ordering::SeqCst) + 1;
        if refuses_from.is_some_and(|from| seen >= from) {
            let mut slot = slot.lock().expect("rows at the refusal");
            if slot.is_none() {
                *slot = Some((request_rows(&observed), session_rows(&observed)));
            }
            return true;
        }
        false
    }));
    let mut run = rig.start();
    // The first data frame says the subscription was accepted; the run is then idle
    // until the shutdown. (A refused guard may end the run before it gets there.)
    let mut telemetry = false;
    while let Some(event) = run.event_within(std::time::Duration::from_secs(5)).await {
        if matches!(event, StreamEvent::Telemetry { .. }) {
            telemetry = true;
            break;
        }
    }
    if refuses_from.is_none() {
        assert!(telemetry, "the data frame arrived");
    }
    let result = run.shut_down().await;
    let ended = (request_rows(&store), session_rows(&store));
    let at_refusal = first_refusal.lock().expect("rows at the refusal").clone();
    (consulted.load(Ordering::SeqCst), at_refusal, ended, result)
}

#[tokio::test(start_paused = true)]
async fn p3bf2_no_audit_receipt_is_written_after_the_guard_refuses() {
    // The control: a guard that never refuses lets the whole audit through (connect,
    // subscribe and unsubscribe rows, an orderly session), and says how many times it
    // is consulted: once before each mutation.
    let (consulted, _, complete, result) = audited_run(None).await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        complete
            .0
            .iter()
            .map(|row| (row.operation.as_str(), row.outcome.as_str()))
            .collect::<Vec<_>>(),
        [
            ("stream_connect", "success"),
            ("stream_subscribe", "success"),
            ("stream_unsubscribe", "success"),
        ]
    );
    assert_eq!(complete.1.len(), 1);
    assert_eq!(complete.1[0].outcome, "orderly_shutdown");
    // The session row and, for each of the three requests, its start and its
    // completion, and the session's completion: eight mutations.
    assert_eq!(consulted, 8, "the guard stands before every audit mutation");

    // Refused from each consultation on: nothing is written after the refusal, and the
    // refusal kept something back (what was started stays unresolved).
    for refuses_from in 1..=consulted {
        let (seen, at_refusal, ended, result) = audited_run(Some(refuses_from)).await;
        let at_refusal = at_refusal.unwrap_or_else(|| {
            panic!("refusing from {refuses_from}: the guard never refused ({seen} calls)")
        });
        assert_eq!(
            ended, at_refusal,
            "refusing from consultation {refuses_from}: an audit row was written after the refusal"
        );
        assert_ne!(
            ended, complete,
            "refusing from consultation {refuses_from}: the refusal kept nothing back"
        );
        assert!(
            result.is_err(),
            "refusing from consultation {refuses_from}: the run ends with the failure: {result:?}"
        );
    }
}
