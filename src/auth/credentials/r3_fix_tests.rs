// SPDX-License-Identifier: AGPL-3.0-only

//! Tests of the B2 round-3 fix round (coordinator rulings of 2026-10-15) for the
//! credential unit. They use the rigs of the refresh and rejection tests. Nothing
//! leaves the machine; tokens, times and rows are the synthetic fixtures of spec
//! section 8.

use super::*;
use crate::db::{CollectionAttentionCode, CollectorAuthReadiness};

// ---------------------------------------------------------------------------
// C5-R3-01: a restart gives back what a counted first rejection owed (R57 step 7)
// ---------------------------------------------------------------------------

/// Date every token receipt on the manual clock: the store dates them by the wall clock,
/// the fixtures run on 2027-01-15, and a test that wants the restart floor of R58 (or the
/// receipts' place relative to a counted rejection) puts them where its own clock is.
fn date_receipts_at(bed: &Bed, at_ms: i64) {
    bed.rig
        .store
        .open()
        .expect("a connection")
        .execute(
            "UPDATE outbound_request_receipts
                SET started_at_ms = ?1, completed_at_ms = ?1 + 5, duration_ms = 5",
            rusqlite::params![at_ms],
        )
        .expect("receipts are dated on the manual clock");
}

#[tokio::test]
async fn c5_r3_01_a_managed_restart_before_the_forced_refresh_still_owes_it() {
    // t0: a refresh succeeds. t0 + 10: the new token is refused, which is the first
    // count and owes a forced refresh at t0 + 60 (R19). The collector stops before
    // that minute is over (shutdown has begun: no token request starts, R33a). The
    // restart at t0 + 20 is inside the minute the receipt of the refresh holds (R58): the
    // owed refresh waits for it and the requests wait with it (B2 apply, R3C-01).
    let bed = new_bed().await;
    bed.rig.seed_due();
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    bed.list(&mut manager, &mut fuse).await.expect("first list");
    assert_eq!(bed.fake.posts().len(), 1);
    date_receipts_at(&bed, T0 * 1_000);
    bed.fake.reject("AT1");
    bed.at(T0 + 10);
    manager.shutdown_flag().store(true, Ordering::Release);
    let _ = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("stopped");
    assert_eq!(
        bed.fake.posts().len(),
        1,
        "no token request after shutdown began"
    );
    assert_eq!(bed.stored_consecutive(), 1);
    drop(manager);

    // Restart at t0 + 20: the pair and the count are the same, the refresh is owed and
    // R58 puts it at t0 + 60. Until then no request carries the rejected token: no
    // repeated 401, and no list that the restored fuse would coalesce.
    bed.at(T0 + 20);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(manager.status_now().fuse.consecutive, 1);
    let products_before = bed.fake.arrivals("products").len();
    for at in [T0 + 20, T0 + 40, T0 + 59] {
        bed.at(at);
        let error = bed
            .list(&mut manager, &mut fuse)
            .await
            .expect_err("held until the owed refresh can run");
        assert_eq!(unavailable_at(error), Some(T0 + 60), "at {at}");
        assert_eq!(
            bed.fake.arrivals("products").len(),
            products_before,
            "at {at}: no list went out with the rejected token"
        );
        assert_eq!(bed.fake.posts().len(), 1, "at {at}: and no token request");
    }
    // At t0 + 60 the next list refreshes (exactly one POST, at the floor) and goes out with
    // the new token: answered 2xx, no repeated 401.
    bed.at(T0 + 60);
    bed.list(&mut manager, &mut fuse).await.expect("accepted");
    let posts = bed.fake.posts();
    assert_eq!(posts.len(), 2, "exactly one POST after the restart");
    assert_eq!(posts[1].at, T0 + 60, "at the floor");
    assert_eq!(
        bed.fake.arrivals("products").len(),
        products_before + 1,
        "one list, answered 2xx: no repeated 401"
    );
    for at in [T0 + 61, T0 + 120, T0 + 600] {
        bed.at(at);
        bed.rig
            .refresh(&mut manager)
            .await
            .expect("nothing more is owed");
    }
    assert_eq!(bed.fake.posts().len(), 2, "and no second one");
    assert_eq!(bed.rig.state().0, "ready");
}

#[tokio::test]
async fn c5_r3_01_k2_a_completed_unrotated_refresh_is_no_obligation_after_a_restart() {
    // t0: the list is refused (the first count), the forced refresh succeeds and keeps the
    // refresh token (the generation is unchanged), and the resend meets a 503. The
    // collector stops. The fuse of the restart is a count 1 of the current generation, but
    // the refresh it owes has been made: the stored pair no longer holds the token the
    // count rejected, whatever the receipts say. The restart sends no further token
    // request and holds no request back (B2 apply, K2; hub fix pass 2, F2-1, case (c)).
    let bed = new_bed().await;
    bed.rig
        .seed(&jg(), "refresh-0", Some((T0 + 28_800, T0 + 21_600)));
    bed.fake.script_products(&[401, 503]);
    bed.fake.script_tokens(vec![Reply::json(
        200,
        &format!(
            r#"{{"access_token":"{}","expires_in":28800,"token_type":"Bearer"}}"#,
            jg_exp(T0 + 40_000)
        ),
    )]);
    {
        let mut manager = bed.manager_global();
        let mut fuse = LegacyAuthFuse::default();
        let _ = bed.list(&mut manager, &mut fuse).await;
        assert_eq!(bed.fake.posts().len(), 1, "the forced refresh");
        assert_eq!(bed.stored_consecutive(), 1);
        assert!(
            !bed.stored_rejection_is_of_the_stored_token(),
            "the completed refresh replaced the token the count rejected"
        );
    }
    let generation_stored = bed.rig.stored_generation();
    // The receipt of the completed refresh is dated on the manual clock, so the restart
    // floor of R58 (t0 + 61) is still ahead at the restart: a unit that held its requests
    // for it without owing anything would be seen holding them (hub fix pass 3, F2C-02).
    date_receipts_at(&bed, T0 * 1_000 + 500);

    bed.at(T0 + 10);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(
        manager.status_now().fuse.consecutive,
        1,
        "the count is restored"
    );
    assert_eq!(
        manager.generation(),
        generation_stored,
        "the refresh kept the refresh token: the generation is the one counted"
    );
    // Asked at once, later and after the floor: nothing is owed and nothing is held.
    let products_before = bed.fake.arrivals("products").len();
    for at in [T0 + 10, T0 + 61, T0 + 600] {
        bed.at(at);
        bed.rig
            .refresh(&mut manager)
            .await
            .expect("no refresh is owed");
    }
    assert_eq!(bed.fake.posts().len(), 1, "no additional POST");
    bed.at(T0 + 10);
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("the list is not held back");
    assert_eq!(
        bed.fake.arrivals("products").len(),
        products_before + 1,
        "the list went out at once"
    );
    assert_eq!(bed.fake.posts().len(), 1, "still no additional POST");
}

#[tokio::test]
async fn c5_r3_01_f2_1_a_forced_refresh_answered_with_the_presented_pair_still_owes_it_after_a_restart()
 {
    // Hub fix pass 2 (F2-1), case (a): t0, the list is refused (the first count), and the
    // forced refresh is answered with the pair that was presented (C5 R45 row 14: the
    // attempt closes as a success with its input released, and retries in memory). The
    // collector stops. That answer renewed nothing: the rejected access token is still
    // the stored one, so the restart still owes the forced refresh. Requests are held to
    // the restart floor of R58, exactly one POST goes out there, and no 401 comes first.
    let bed = new_bed().await;
    bed.rig
        .seed(&jg(), "refresh-0", Some((T0 + 28_800, T0 + 21_600)));
    bed.fake.reject(&jg());
    bed.fake.script_tokens(vec![Reply::json(
        200,
        &format!(
            r#"{{"access_token":"{}","refresh_token":"refresh-0","expires_in":28800}}"#,
            jg()
        ),
    )]);
    {
        let mut manager = bed.manager_global();
        let mut fuse = LegacyAuthFuse::default();
        let _ = bed
            .list(&mut manager, &mut fuse)
            .await
            .expect_err("refused, and the forced refresh renewed nothing");
        assert_eq!(bed.fake.posts().len(), 1, "the forced refresh");
        assert_eq!(bed.fake.arrivals("products").len(), 1, "one refused list");
        assert_eq!(bed.stored_consecutive(), 1);
        assert!(
            bed.stored_rejection_is_of_the_stored_token(),
            "the answer renewed nothing: the rejected token is still the stored one"
        );
    }
    assert_eq!(
        bed.rig.receipts(),
        [("success".to_owned(), Some(200), None, false)],
        "the receipt of a row-14 answer is a success with its binding released"
    );
    date_receipts_at(&bed, T0 * 1_000);

    bed.at(T0 + 10);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(manager.status_now().fuse.consecutive, 1);
    // While the restored count-1 hold runs, the status is the one the count-2 pause shows
    // (hub fix pass 2, F2-3): the state is awaiting_confirmation and raises the
    // collector_auth_waiting attention.
    assert_eq!(bed.rig.state().0, "awaiting_confirmation");
    assert_eq!(
        bed.rig.store.collector_auth_readiness().unwrap(),
        Some(CollectorAuthReadiness::Attention(
            CollectionAttentionCode::CollectorAuthWaiting,
        ))
    );
    let products_before = bed.fake.arrivals("products").len();
    for at in [T0 + 10, T0 + 30, T0 + 59] {
        bed.at(at);
        let error = bed
            .list(&mut manager, &mut fuse)
            .await
            .expect_err("held until the owed refresh can run");
        assert_eq!(unavailable_at(error), Some(T0 + 60), "at {at}");
        assert_eq!(
            bed.fake.arrivals("products").len(),
            products_before,
            "at {at}: no list went out with the rejected token, so no 401 came back"
        );
        assert_eq!(bed.fake.posts().len(), 1, "at {at}: and no token request");
    }
    bed.at(T0 + 60);
    bed.list(&mut manager, &mut fuse).await.expect("accepted");
    let posts = bed.fake.posts();
    assert_eq!(posts.len(), 2, "exactly one POST after the restart");
    assert_eq!(posts[1].at, T0 + 60, "at the floor");
    assert_eq!(
        bed.fake.arrivals("products").len(),
        products_before + 1,
        "one list with the new token, answered 2xx"
    );
}

#[tokio::test]
async fn c5_r3_01_f2_1_a_refresh_in_the_second_of_the_rejection_does_not_satisfy_it() {
    // Hub fix pass 2 (F2-1), case (b): a refresh that kept the refresh token completes at
    // t0 + 100 ms, and the new access token is refused at t0 + 800 ms, in the same second.
    // The collector stops before the forced refresh that rejection owes. The refresh
    // came first, so it cannot have satisfied the rejection that followed it: the
    // restart restores the obligation (the stored token is the rejected one; the
    // receipts' second-granularity clocks decide nothing).
    let bed = new_bed().await;
    bed.rig.seed_due();
    let renewed = jg_exp(T0 + 40_000);
    bed.fake.script_tokens(vec![Reply::json(
        200,
        &format!(r#"{{"access_token":"{renewed}","expires_in":28800,"token_type":"Bearer"}}"#),
    )]);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    bed.list(&mut manager, &mut fuse).await.expect("first list");
    assert_eq!(bed.fake.posts().len(), 1, "the refresh that kept the token");
    date_receipts_at(&bed, T0 * 1_000 + 100);
    bed.fake.reject(&renewed);
    manager.shutdown_flag().store(true, Ordering::Release);
    let _ = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("refused, and shutdown has begun");
    assert_eq!(
        bed.fake.posts().len(),
        1,
        "no token request after shutdown began"
    );
    assert_eq!(bed.stored_consecutive(), 1);
    drop(manager);

    bed.at(T0 + 20);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    let products_before = bed.fake.arrivals("products").len();
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("held until the owed refresh can run");
    let floor = unavailable_at(error).expect("the owed refresh has a time");
    assert!(floor > T0 + 20, "the floor of R58 is still ahead");
    assert_eq!(
        bed.fake.arrivals("products").len(),
        products_before,
        "no list went out with the rejected token"
    );
    bed.at(floor);
    bed.list(&mut manager, &mut fuse).await.expect("accepted");
    let posts = bed.fake.posts();
    assert_eq!(posts.len(), 2, "exactly one POST after the restart");
    assert_eq!(posts[1].at, floor, "at the floor");
}

#[tokio::test]
async fn c5_r3_01_an_observer_restart_after_a_counted_rejection_waits_again() {
    // A counted rejection makes the observer wait (R28). The status says so, and the
    // restart must not turn it into ready: collection stays held until the refresh
    // owner's next pair arrives.
    let bed = new_bed().await;
    bed.rig.seed(&jg(), "refresh-0", None);
    let mut manager = bed.observer();
    let mut fuse = LegacyAuthFuse::default();
    bed.at(T0 + 100);
    bed.fake.reject(&jg());
    let _ = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("rejected");
    assert_eq!(
        bed.rig.state(),
        ("observer_waiting".into(), Some("token_rejected".into()))
    );
    drop(manager);

    // Restart inside the window: before any further Owner API request the status
    // and the readiness say the same.
    bed.at(T0 + 200);
    let requests = bed.fake.all().len();
    let manager = bed.observer();
    assert_eq!(
        bed.rig.state(),
        ("observer_waiting".into(), Some("token_rejected".into()))
    );
    assert_eq!(
        bed.rig.store.collector_auth_readiness().unwrap(),
        Some(CollectorAuthReadiness::Attention(
            CollectionAttentionCode::CollectorAuthWaiting,
        ))
    );
    assert!(!bed.rig.row().collecting);
    assert_eq!(
        bed.fake.all().len(),
        requests,
        "no request was needed to know"
    );
    assert!(manager.access_token_for_sensitive_use().is_err());
}

// ---------------------------------------------------------------------------
// Hub fix pass 3: the obligation is derived from the rejected access token's digest
// ---------------------------------------------------------------------------

/// An unrotated token answer: a new access token, no refresh token (C5 R45 row 3).
fn unrotated_reply(access: &str) -> Reply {
    Reply::json(
        200,
        &format!(r#"{{"access_token":"{access}","expires_in":28800,"token_type":"Bearer"}}"#),
    )
}

#[tokio::test]
async fn c5_r3_01_f3_a_rejection_counted_by_an_observer_is_owed_to_a_managed_restart() {
    // Opus F2C-01: a managed Serve refreshed at t0, and an observer run on the same store
    // counted a rejection of that refresh's token at t0 + 10. A managed restart at t0 + 20,
    // inside the fuse window, still presents the rejected token unless it owes the forced
    // refresh: the first list would carry it and its 401 would be coalesced behind an
    // exchange that does not exist. The restart holds its requests to the floor of R58
    // (t0 + 60), sends exactly one token request there, and no 401 comes first.
    let bed = new_bed().await;
    bed.rig.seed_due();
    {
        let mut manager = bed.manager_global();
        let mut fuse = LegacyAuthFuse::default();
        bed.list(&mut manager, &mut fuse).await.expect("first list");
    }
    assert_eq!(bed.fake.posts().len(), 1);
    date_receipts_at(&bed, T0 * 1_000);
    bed.fake.reject("AT1");
    bed.at(T0 + 10);
    {
        let mut observer = observer_global(&bed);
        let mut fuse = LegacyAuthFuse::default();
        let _ = bed
            .list(&mut observer, &mut fuse)
            .await
            .expect_err("rejected");
        assert_eq!(
            bed.rig.state(),
            ("observer_waiting".into(), Some("token_rejected".into()))
        );
        assert_eq!(bed.stored_consecutive(), 1);
        assert!(
            bed.stored_rejection_is_of_the_stored_token(),
            "the observer's count keeps the digest of the token it rejected"
        );
    }

    bed.at(T0 + 20);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    let products_before = bed.fake.arrivals("products").len();
    for at in [T0 + 20, T0 + 40, T0 + 59] {
        bed.at(at);
        let error = bed
            .list(&mut manager, &mut fuse)
            .await
            .expect_err("held until the owed refresh can run");
        assert_eq!(unavailable_at(error), Some(T0 + 60), "at {at}");
        assert_eq!(
            bed.fake.arrivals("products").len(),
            products_before,
            "at {at}: no list went out with the rejected token, so no 401 came back"
        );
        assert_eq!(bed.fake.posts().len(), 1, "at {at}: and no token request");
    }
    bed.at(T0 + 60);
    bed.list(&mut manager, &mut fuse).await.expect("accepted");
    let posts = bed.fake.posts();
    assert_eq!(posts.len(), 2, "exactly one POST after the restart");
    assert_eq!(posts[1].at, T0 + 60, "at the floor");
    assert_eq!(
        bed.fake.arrivals("products").len(),
        products_before + 1,
        "one list with the new token, answered 2xx"
    );
}

#[tokio::test]
async fn c5_r3_01_f3_a_successor_written_by_the_shutdown_retry_leaves_nothing_owed() {
    // Codex 1: the forced refresh of a count 1 returns a new access token and keeps the
    // refresh token; the first write of that successor fails, so the request sees an
    // unavailable account and the collector shuts down; the shutdown's one more try
    // writes it. The stored pair then holds a token that was not rejected: the restart
    // owes nothing, holds nothing and sends no further token request.
    let bed = new_bed().await;
    bed.rig
        .seed(&jg(), "refresh-0", Some((T0 + 28_800, T0 + 21_600)));
    bed.fake.script_products(&[401]);
    let renewed = jg_exp(T0 + 40_000);
    bed.fake.script_tokens(vec![unrotated_reply(&renewed)]);
    {
        let mut manager = bed.manager_global();
        plant(&manager, &[PersistFault::Fail]);
        let mut fuse = LegacyAuthFuse::default();
        let _ = bed
            .list(&mut manager, &mut fuse)
            .await
            .expect_err("refused, and the successor is not written yet");
        assert_eq!(bed.fake.posts().len(), 1, "the forced refresh");
        assert_eq!(bed.rig.stored_pair().0, jg(), "the successor is pending");
        manager.shutdown().await.expect("an orderly shutdown");
        assert_eq!(bed.rig.stored_pair().0, renewed, "the shutdown wrote it");
        assert_eq!(bed.stored_consecutive(), 1);
    }

    bed.at(T0 + 10);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(manager.status_now().fuse.consecutive, 1);
    for at in [T0 + 10, T0 + 61, T0 + 600] {
        bed.at(at);
        bed.rig
            .refresh(&mut manager)
            .await
            .expect("no refresh is owed");
    }
    assert_eq!(bed.fake.posts().len(), 1, "no additional POST");
    bed.at(T0 + 10);
    let products_before = bed.fake.arrivals("products").len();
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("the list is not held back");
    assert_eq!(
        bed.fake.arrivals("products").len(),
        products_before + 1,
        "the list went out at once"
    );
    assert_eq!(bed.fake.posts().len(), 1, "still no additional POST");
}

#[tokio::test]
async fn c5_r3_01_f3_a_status_row_captured_before_the_completion_cannot_bring_the_obligation_back()
{
    // Codex 2: a count 1 is stored while the collector shuts down before the forced
    // refresh (t0 + 5). The restart (t0 + 10) refreshes, keeping the refresh token, and the
    // pair is stored. A status row computed before that completion (the unit was
    // `refreshing`) is written after it, and the process crashes before the flight's
    // result is applied. Nothing in the row says a flag: it carries the digest of the token
    // that was rejected, which the stored pair no longer holds, so the next restart owes
    // nothing.
    let bed = new_bed().await;
    bed.rig
        .seed(&jg(), "refresh-0", Some((T0 + 28_800, T0 + 21_600)));
    bed.fake.script_products(&[401]);
    bed.at(T0 + 5);
    let mut stale = {
        let mut manager = bed.manager_global();
        manager.shutdown_flag().store(true, Ordering::Release);
        let mut fuse = LegacyAuthFuse::default();
        let _ = bed
            .list(&mut manager, &mut fuse)
            .await
            .expect_err("refused, and shutdown has begun");
        assert_eq!(bed.fake.posts().len(), 0, "no token request");
        assert_eq!(bed.stored_consecutive(), 1);
        bed.rig.row()
    };
    stale.state = "refreshing".to_owned();

    bed.at(T0 + 10);
    let renewed = jg_exp(T0 + 40_000);
    bed.fake.script_products(&[503]);
    bed.fake.script_tokens(vec![unrotated_reply(&renewed)]);
    {
        let mut manager = bed.manager_global();
        let mut fuse = LegacyAuthFuse::default();
        let _ = bed.list(&mut manager, &mut fuse).await;
        assert_eq!(bed.fake.posts().len(), 1, "the owed refresh went out");
        assert_eq!(bed.rig.stored_pair().0, renewed, "and was stored");
        bed.rig
            .store
            .write_collector_auth_row(&stale, (T0 + 11) * 1_000)
            .expect("the stale status lands after the completion");
    }

    bed.at(T0 + 20);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(manager.status_now().fuse.consecutive, 1);
    for at in [T0 + 20, T0 + 61, T0 + 600] {
        bed.at(at);
        bed.rig
            .refresh(&mut manager)
            .await
            .expect("no refresh is owed");
    }
    assert_eq!(bed.fake.posts().len(), 1, "no additional POST");
    bed.at(T0 + 20);
    let products_before = bed.fake.arrivals("products").len();
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("the list is not held back");
    assert_eq!(bed.fake.arrivals("products").len(), products_before + 1);
    assert_eq!(bed.fake.posts().len(), 1, "still no additional POST");
}

/// An observer over the stored pair with the region configured, for opaque stored tokens.
fn observer_global(bed: &Bed) -> LegacyAuthManager {
    LegacyAuthManager::from_hub_teslamate_store_for_test(
        bed.rig.store.clone(),
        bed.rig.directory.path(),
        &bed.rig
            .endpoint(Some(crate::tesla_stream::StreamRegion::Global)),
        Some(bed.rig.token_url()),
        true,
        bed.rig.env.clone(),
    )
    .expect("observer")
}

/// A bed at the end of a count 2: the list met a 401 at t0 (the first count) and the
/// forced refresh rotated the pair (AT1, refresh-1) one second later; the resend was
/// refused too, which is the second count, and the pause runs until t0 + 301. The
/// manager is returned so a test may carry on with it.
async fn count_two_bed() -> (Bed, LegacyAuthManager) {
    let bed = new_bed().await;
    seed_quiet(&bed);
    bed.fake.reject(&jg());
    bed.fake.reject("AT1");
    bed.fake.0.advance_after_token.store(1, Ordering::SeqCst);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    let error = bed
        .list(&mut manager, &mut fuse)
        .await
        .expect_err("refused twice");
    assert_eq!(unavailable_at(error), Some(T0 + 301));
    assert_eq!(bed.fake.posts().len(), 1);
    let row = bed.rig.row();
    assert_eq!(
        (
            row.state.as_str(),
            row.fuse_consecutive,
            row.fuse_pause_until
        ),
        ("awaiting_confirmation", 2, Some(T0 + 301))
    );
    (bed, manager)
}

#[tokio::test]
async fn c5_r3_01_f3_a_restart_while_the_count_two_token_is_still_the_stored_one_owes_the_refresh()
{
    // Control for Codex 3: the pause is over (t0 + 310) and the rejected token is still
    // the stored one: the forced refresh is owed, and goes out once.
    let (bed, manager) = count_two_bed().await;
    drop(manager);
    bed.fake.0.advance_after_token.store(0, Ordering::SeqCst);
    bed.at(T0 + 310);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    bed.list(&mut manager, &mut fuse).await.expect("accepted");
    let posts = bed.fake.posts();
    assert_eq!(posts.len(), 2, "exactly one POST after the restart");
    assert_eq!(posts[1].at, T0 + 310);
}

#[tokio::test]
async fn c5_r3_01_f3_a_restart_right_after_a_rotated_count_two_refresh_owes_nothing() {
    // Codex 3: the forced refresh after the pause rotated the pair (refresh-2). The process
    // ends before the fuse is synchronised, so the stored row still holds the count 2 and
    // its elapsed pause. The restart owes nothing: no token request, no hold.
    let (bed, mut manager) = count_two_bed().await;
    bed.fake.0.advance_after_token.store(0, Ordering::SeqCst);
    bed.at(T0 + 301);
    bed.rig
        .refresh(&mut manager)
        .await
        .expect("the forced refresh");
    assert_eq!(bed.fake.posts().len(), 2);
    assert_eq!(bed.rig.stored_pair().1, "refresh-2", "rotated");
    assert_eq!(
        bed.rig.row().fuse_pause_until,
        Some(T0 + 301),
        "not synchronised"
    );
    drop(manager);

    bed.at(T0 + 310);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    bed.rig
        .refresh(&mut manager)
        .await
        .expect("no refresh is owed");
    let products_before = bed.fake.arrivals("products").len();
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("the list is not held back");
    assert_eq!(bed.fake.arrivals("products").len(), products_before + 1);
    assert_eq!(bed.fake.posts().len(), 2, "no additional POST");
}

#[tokio::test]
async fn c5_r3_01_f3_a_restart_right_after_an_unrotated_count_two_refresh_owes_nothing() {
    // The same with a refresh that kept the refresh token: the generation is the one
    // counted, and only the stored access token tells that the rejected one was replaced.
    let (bed, mut manager) = count_two_bed().await;
    bed.fake.0.advance_after_token.store(0, Ordering::SeqCst);
    let renewed = jg_exp(T0 + 40_000);
    bed.fake.script_tokens(vec![unrotated_reply(&renewed)]);
    bed.at(T0 + 301);
    bed.rig
        .refresh(&mut manager)
        .await
        .expect("the forced refresh");
    assert_eq!(bed.fake.posts().len(), 2);
    assert_eq!(bed.rig.stored_pair().0, renewed);
    assert_eq!(bed.rig.stored_pair().1, "refresh-1", "not rotated");
    assert_eq!(
        bed.rig.row().fuse_pause_until,
        Some(T0 + 301),
        "not synchronised"
    );
    drop(manager);

    bed.at(T0 + 310);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    bed.rig
        .refresh(&mut manager)
        .await
        .expect("no refresh is owed");
    let products_before = bed.fake.arrivals("products").len();
    bed.list(&mut manager, &mut fuse)
        .await
        .expect("the list is not held back");
    assert_eq!(bed.fake.arrivals("products").len(), products_before + 1);
    assert_eq!(bed.fake.posts().len(), 2, "no additional POST");
}

#[test]
fn c5_r3_01_f3_the_fuse_keeps_the_digest_of_the_rejected_token_for_every_counted_non_stop() {
    use crate::legacy_auth::HolderKind;
    let g0 = gen_n(1);
    let g1 = gen_n(2);
    let g2 = gen_n(3);
    let digest = gen_n(0xacce55);
    let other = gen_n(0xacce56);
    let managed = |generation: uuid::Uuid, token: uuid::Uuid| RejectionContext {
        access_digest: token,
        ..context(RequestScope::Account, generation, generation)
    };

    // Managed: counts 1 and 2 keep the digest of the token they rejected; the stop of count
    // 3 keeps none (nothing is owed after it).
    let mut fuse = LegacyAuthFuse::default();
    assert_eq!(fuse.snapshot().rejected_access, None);
    assert_eq!(
        fuse.record_rejection(T0, &managed(g0, digest)),
        RejectionAction::ForceRefreshThenRetry
    );
    assert_eq!(fuse.snapshot().rejected_access, Some(digest));
    assert!(matches!(
        fuse.record_rejection(T0 + 1, &managed(g1, other)),
        RejectionAction::PauseThenRefresh { .. }
    ));
    assert_eq!(fuse.snapshot().rejected_access, Some(other));
    assert!(matches!(
        fuse.record_rejection(T0 + 2, &managed(g2, digest)),
        RejectionAction::Stop(_)
    ));
    assert_eq!(fuse.snapshot().rejected_access, None);

    // An observer's count and an access-token-only provider's count 1 keep it as well.
    let mut fuse = LegacyAuthFuse::default();
    let observer = RejectionContext {
        holder: HolderKind::Observer,
        refresh_possible: false,
        ..managed(g0, digest)
    };
    assert_eq!(
        fuse.record_rejection(T0, &observer),
        RejectionAction::ObserverWait
    );
    assert_eq!(fuse.snapshot().rejected_access, Some(digest));
    let mut fuse = LegacyAuthFuse::default();
    let only = RejectionContext {
        holder: HolderKind::AccessTokenOnly,
        refresh_possible: false,
        ..managed(g0, digest)
    };
    assert!(matches!(
        fuse.record_rejection(T0, &only),
        RejectionAction::PauseThenConfirm { .. }
    ));
    assert_eq!(fuse.snapshot().rejected_access, Some(digest));

    // A 2xx of the current generation and a reset clear it with the count; a snapshot
    // carries it; its debug text shows presence only.
    let mut fuse = LegacyAuthFuse::default();
    fuse.record_rejection(T0, &managed(g0, digest));
    let restored = LegacyAuthFuse::from_snapshot(fuse.snapshot());
    assert_eq!(restored.snapshot().rejected_access, Some(digest));
    let shown = format!("{fuse:?} {:?} {:?}", fuse.snapshot(), managed(g0, digest));
    assert!(!shown.contains(&digest.to_string()), "{shown}");
    assert!(shown.contains("has_rejected_access: true"), "{shown}");
    fuse.record_success(g0, RequestScope::Account);
    assert_eq!(fuse.snapshot().rejected_access, None);
    fuse.record_rejection(T0, &managed(g0, digest));
    fuse.reset();
    assert_eq!(fuse.snapshot().rejected_access, None);
}

#[test]
fn c5_r3_01_f3_the_obligation_needs_the_window_the_generation_and_the_same_token() {
    use crate::legacy_auth::state::{FUSE_WINDOW_SECONDS, fuse_owes_forced_refresh};
    let (current, token, other) = (gen_n(1), gen_n(0xacce55), gen_n(0xacce56));
    let fuse = FuseSnapshot {
        consecutive: 1,
        generation: Some(current),
        last_rejection_at: Some(T0),
        pause_until: None,
        rejected_access: Some(token),
    };
    assert!(fuse_owes_forced_refresh(
        &fuse,
        current,
        token,
        T0 + 10,
        true
    ));
    assert!(
        fuse_owes_forced_refresh(&fuse, current, token, T0 + FUSE_WINDOW_SECONDS, true),
        "the window is open up to its last second"
    );
    // The stored pair holds another token: the rejected one was replaced.
    assert!(!fuse_owes_forced_refresh(
        &fuse,
        current,
        other,
        T0 + 10,
        true
    ));
    // The window has passed (a rejection counted more than an hour ago counts again).
    assert!(!fuse_owes_forced_refresh(
        &fuse,
        current,
        token,
        T0 + FUSE_WINDOW_SECONDS + 1,
        true
    ));
    // With an invalid clock the window cannot be judged and the fuse is kept whole.
    assert!(fuse_owes_forced_refresh(
        &fuse,
        current,
        token,
        T0 + FUSE_WINDOW_SECONDS + 1,
        false
    ));
    // Another generation (a rotation), no digest, no count.
    assert!(!fuse_owes_forced_refresh(
        &fuse,
        gen_n(2),
        token,
        T0 + 10,
        true
    ));
    let without = FuseSnapshot {
        rejected_access: None,
        ..fuse
    };
    assert!(!fuse_owes_forced_refresh(
        &without,
        current,
        token,
        T0 + 10,
        true
    ));
    assert!(!fuse_owes_forced_refresh(
        &FuseSnapshot::default(),
        current,
        token,
        T0 + 10,
        true
    ));
}

#[tokio::test]
async fn c5_r3_01_f3_a_row_migrated_without_a_digest_is_filled_from_the_stored_pair() {
    // A count-1 row stored before schema step 72 has no digest (the migration cannot read
    // the encrypted pair). The first start fills it from the stored pair: the row still
    // owes its forced refresh, exactly one token request goes out, and the row is written
    // with the digest at once.
    let bed = new_bed().await;
    bed.rig
        .seed(&jg(), "refresh-0", Some((T0 + 28_800, T0 + 21_600)));
    bed.fake.script_products(&[401]);
    {
        let mut manager = bed.manager_global();
        manager.shutdown_flag().store(true, Ordering::Release);
        let mut fuse = LegacyAuthFuse::default();
        let _ = bed
            .list(&mut manager, &mut fuse)
            .await
            .expect_err("refused, and shutdown has begun");
        assert_eq!(bed.stored_consecutive(), 1);
    }
    bed.rig
        .store
        .open()
        .expect("a connection")
        .execute(
            "UPDATE collector_auth_state SET fuse_rejected_access_digest = NULL",
            [],
        )
        .expect("the row as the migration leaves it");
    assert!(!bed.stored_rejection_is_of_the_stored_token());

    bed.at(T0 + 20);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    assert!(
        bed.stored_rejection_is_of_the_stored_token(),
        "filled and written before any request"
    );
    bed.list(&mut manager, &mut fuse).await.expect("accepted");
    assert_eq!(bed.fake.posts().len(), 1, "the owed refresh went out once");
}

// ---------------------------------------------------------------------------
// Hub fix pass 4: the digest is of the canonical (trimmed) access token, and the fill is durable
// ---------------------------------------------------------------------------

/// Store `access` followed by pasted whitespace in place of the stored access token
/// (R46 trims it at load; the stored ciphertext keeps it), the refresh token and the
/// schedule being those of the refreshed form.
fn pad_stored_access(bed: &Bed, access: &str, refresh: &str) {
    bed.rig.seed(
        &format!("{access}  "),
        refresh,
        Some((T0 + 28_800, T0 + 21_600)),
    );
}

#[tokio::test]
async fn c5_r3_01_f4_a_padded_stored_token_counted_by_an_observer_is_owed_to_a_managed_restart() {
    // Codex 1 / Opus F3C-01: a rejection counted while the stored access token carries
    // pasted whitespace is a rejection of the trimmed token, and a managed restart that
    // hashes the raw stored text would never find it. Same rig as the unpadded observer
    // test: held to the floor of R58, one POST there, no 401 first.
    let bed = new_bed().await;
    bed.rig.seed_due();
    {
        let mut manager = bed.manager_global();
        let mut fuse = LegacyAuthFuse::default();
        bed.list(&mut manager, &mut fuse).await.expect("first list");
    }
    assert_eq!(bed.fake.posts().len(), 1);
    date_receipts_at(&bed, T0 * 1_000);
    pad_stored_access(&bed, "AT1", "refresh-1");
    bed.fake.reject("AT1");
    bed.at(T0 + 10);
    {
        let mut observer = observer_global(&bed);
        let mut fuse = LegacyAuthFuse::default();
        let _ = bed
            .list(&mut observer, &mut fuse)
            .await
            .expect_err("rejected");
        assert_eq!(bed.stored_consecutive(), 1);
        assert_eq!(
            bed.rig
                .store
                .load_collector_auth_row()
                .expect("row")
                .and_then(|row| row.fuse_rejected_access_digest),
            Some(crate::teslamate_token::legacy_access_token_digest("AT1")),
            "the count keeps the digest of the trimmed token it rejected"
        );
    }

    bed.at(T0 + 20);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    let products_before = bed.fake.arrivals("products").len();
    for at in [T0 + 20, T0 + 40, T0 + 59] {
        bed.at(at);
        let error = bed
            .list(&mut manager, &mut fuse)
            .await
            .expect_err("held until the owed refresh can run");
        assert_eq!(unavailable_at(error), Some(T0 + 60), "at {at}");
        assert_eq!(
            bed.fake.arrivals("products").len(),
            products_before,
            "at {at}: no list went out with the rejected token"
        );
        assert_eq!(bed.fake.posts().len(), 1, "at {at}: and no token request");
    }
    bed.at(T0 + 60);
    bed.list(&mut manager, &mut fuse).await.expect("accepted");
    let posts = bed.fake.posts();
    assert_eq!(posts.len(), 2, "exactly one POST after the restart");
    assert_eq!(posts[1].at, T0 + 60, "at the floor");
}

#[tokio::test]
async fn c5_r3_01_f4_a_padded_count_two_token_still_stored_owes_the_forced_refresh() {
    // The count-2 pause restores its forced refresh by the same comparison: with the
    // rejected token stored with pasted whitespace, the refresh is still owed once the
    // pause is over (exactly one POST), as it is for the unpadded token.
    let (bed, manager) = count_two_bed().await;
    drop(manager);
    pad_stored_access(&bed, "AT1", "refresh-1");
    bed.fake.0.advance_after_token.store(0, Ordering::SeqCst);
    bed.at(T0 + 310);
    let mut manager = bed.manager_global();
    let mut fuse = LegacyAuthFuse::default();
    bed.list(&mut manager, &mut fuse).await.expect("accepted");
    let posts = bed.fake.posts();
    assert_eq!(posts.len(), 2, "exactly one POST after the restart");
    assert_eq!(posts[1].at, T0 + 310);
}

/// A managed construction over the stored pair that reports its error.
fn try_manager_global(bed: &Bed) -> Result<LegacyAuthManager, LegacyAuthManagerError> {
    LegacyAuthManager::from_hub_teslamate_store_for_test(
        bed.rig.store.clone(),
        bed.rig.directory.path(),
        &bed.rig
            .endpoint(Some(crate::tesla_stream::StreamRegion::Global)),
        Some(bed.rig.token_url()),
        false,
        bed.rig.env.clone(),
    )
}

#[tokio::test]
async fn c5_r3_01_f4_a_failed_fill_write_fails_the_construction_before_any_refresh() {
    // Codex 3: a count-1 row without a digest (as the migration leaves it) is filled by a
    // conditional write, committed at construction. When that write fails the construction
    // fails: no unit exists, so no refresh and no request is made on the unfilled row (a
    // crash after an unrotated completion could otherwise revive the satisfied
    // obligation). A later successful construction fills the row.
    let bed = new_bed().await;
    bed.rig
        .seed(&jg(), "refresh-0", Some((T0 + 28_800, T0 + 21_600)));
    bed.fake.script_products(&[401]);
    {
        let mut manager = bed.manager_global();
        manager.shutdown_flag().store(true, Ordering::Release);
        let mut fuse = LegacyAuthFuse::default();
        let _ = bed
            .list(&mut manager, &mut fuse)
            .await
            .expect_err("refused, and shutdown has begun");
        assert_eq!(bed.stored_consecutive(), 1);
    }
    let connection = bed.rig.store.open().expect("a connection");
    connection
        .execute(
            "UPDATE collector_auth_state SET fuse_rejected_access_digest = NULL",
            [],
        )
        .expect("the row as the migration leaves it");
    connection
        .execute_batch(
            "CREATE TRIGGER b2r3f4_fail_the_fill BEFORE UPDATE ON collector_auth_state
             BEGIN SELECT RAISE(ABORT, 'injected failure of the fill write'); END;",
        )
        .expect("the injected fault");
    let posts_before = bed.fake.posts().len();
    let products_before = bed.fake.arrivals("products").len();

    bed.at(T0 + 20);
    assert!(
        try_manager_global(&bed).is_err(),
        "the construction fails when the fill cannot be written"
    );
    assert!(!bed.stored_rejection_is_of_the_stored_token());
    assert_eq!(bed.fake.posts().len(), posts_before, "no refresh");
    assert_eq!(
        bed.fake.arrivals("products").len(),
        products_before,
        "and no request"
    );

    connection
        .execute_batch("DROP TRIGGER b2r3f4_fail_the_fill;")
        .expect("the fault is lifted");
    let mut manager = try_manager_global(&bed).expect("the construction retries and succeeds");
    assert!(
        bed.stored_rejection_is_of_the_stored_token(),
        "filled by the construction itself"
    );
    let mut fuse = LegacyAuthFuse::default();
    bed.list(&mut manager, &mut fuse).await.expect("accepted");
    assert_eq!(
        bed.fake.posts().len(),
        posts_before + 1,
        "the owed refresh went out once"
    );
}
