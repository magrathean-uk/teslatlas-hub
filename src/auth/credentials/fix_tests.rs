// SPDX-License-Identifier: AGPL-3.0-only

//! Tests of the C5 fix round (review findings C5-B1 to C5-S8 and C5T-01 to
//! C5T-13, and the Codex findings on this unit): the manager built in a stopped
//! state, closings that failed and are retried, a forced refresh in
//! `sign_in_required`, a pasted token that lost its padding, a store that could
//! not answer, a refresh that kept its token and an external write that came
//! while it ran, the wiring of the manager's own work between requests, and the
//! manager-level halves of spec tests 16, 27, 49, 65 and 94 that earlier rounds
//! left open.
//!
//! Every test uses the rigs of the refresh and rejection tests: a temporary
//! store, the manual clock and an in-process fake over loopback. Nothing leaves
//! the machine. Tokens, times and rows are the synthetic fixtures of spec
//! section 8.

use super::*;
use crate::{legacy_auth::state::AuthState, owner_api::OwnerApiError};

// ---------------------------------------------------------------------------
// C5-S1: a closing that failed is kept, owed and retried before any new attempt
// ---------------------------------------------------------------------------

/// Plant `count` failures of the next closings (a close or a cancel).
fn fail_closings(manager: &LegacyAuthManager, count: usize) {
    manager
        .persistence
        .hub()
        .expect("a store")
        .close_faults
        .store(count, Ordering::SeqCst);
}

/// Plant `count` store errors in the next reservations.
fn fail_reservations(manager: &LegacyAuthManager, count: usize) {
    manager
        .persistence
        .hub()
        .expect("a store")
        .begin_faults
        .store(count, Ordering::SeqCst);
}

#[tokio::test]
async fn s1_a_close_that_fails_once_is_retried_before_any_new_attempt_and_signs_nobody_out() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend
        .script(vec![Reply::json(503, "{}"), Reply::json(200, SUCCESS)]);
    let mut manager = rig.manager();
    let g0 = manager.generation();
    fail_closings(&manager, 1);
    // The first answer is a 503 (R45 row 13). Its closing fails, so the receipt is
    // still `started`, with its input fenced.
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("503"));
    assert_eq!(error, LegacyAuthError::HttpStatus(503));
    assert_eq!(rig.receipts(), [("started".to_owned(), None, None, true)]);
    assert!(rig.store.has_unresolved_legacy_refresh().unwrap());
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("tesla_unavailable".into()),
            Some(T0 + 60),
            true
        )
    );
    assert_eq!(
        manager
            .auth
            .pending_close
            .map(|pending| matches!(pending, PendingClose::Close(..))),
        Some(true)
    );
    // No new attempt begins while the closing is owed: a forced refresh waits for
    // it and does not read the started receipt as an unresolved attempt.
    rig.at(T0 + 4);
    let error = auth_error(
        manager
            .refresh_now(&rig.client(), SystemTime::now())
            .await
            .expect_err("owed"),
    );
    assert_eq!(
        error,
        LegacyAuthError::AuthUnavailable {
            retry_at: Some(T0 + 5)
        }
    );
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(rig.state().0, "retrying", "nobody was signed out");
    // At +5 s the unit retries the closing by itself, with no request.
    rig.at(T0 + 5);
    manager.service();
    assert_eq!(
        rig.receipts(),
        [("http_error".to_owned(), Some(503), None, false)]
    );
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    assert!(
        rig.store
            .legacy_fenced_inputs_for_test()
            .unwrap()
            .is_empty()
    );
    assert!(manager.auth.pending_close.is_none());
    // At +60 s the same refresh token is presented again, and the pair rotates:
    // the 503 never consumed it.
    rig.at(T0 + 60);
    rig.refresh(&mut manager).await.expect("the retry");
    let posts = rig.backend.token_posts();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[1].refresh.as_deref(), Some("refresh-0"));
    assert_eq!(rig.state().0, "ready");
    assert_ne!(rig.stored_generation(), g0);
    rig.scan(&manager, "");
}

#[tokio::test]
async fn s1_a_cancel_that_fails_after_a_guard_refusal_is_retried_and_is_not_a_sign_in() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    // The guard passes before the attempt is reserved and refuses at the send, so
    // nothing is sent (R17); then the cancel fails once.
    let calls = Arc::new(AtomicUsize::new(0));
    manager.sensitive_access = {
        let calls = Arc::clone(&calls);
        Arc::new(move || {
            if calls.fetch_add(1, Ordering::SeqCst) == 1 {
                Err(CredentialError::SensitiveAccessUnavailable)
            } else {
                Ok(())
            }
        })
    };
    fail_closings(&manager, 1);
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("guard"));
    assert_eq!(error, LegacyAuthError::SensitiveAccessUnavailable);
    assert!(rig.backend.token_posts().is_empty(), "nothing was sent");
    // The receipt is `started` because the cancel failed, yet the state is the
    // retry of R17, and the cancel is owed.
    assert_eq!(rig.receipts(), [("started".to_owned(), None, None, true)]);
    assert_eq!(
        rig.state(),
        ("retrying".into(), Some("sensitive_access".into()))
    );
    assert!(matches!(
        manager.auth.pending_close,
        Some(PendingClose::Cancel(_))
    ));
    rig.at(T0 + 5);
    manager.service();
    assert_eq!(
        rig.receipts(),
        [("cancelled".to_owned(), None, None, false)]
    );
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    // At +60 s the refresh goes out with the same token.
    rig.at(T0 + 60);
    rig.refresh(&mut manager).await.expect("the retry");
    assert_eq!(
        rig.backend.token_posts()[0].refresh.as_deref(),
        Some("refresh-0")
    );
    assert_eq!(rig.state().0, "ready");
}

#[tokio::test]
async fn s1_a_closing_that_an_external_write_resolved_is_done_and_not_owed_for_ever() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![Reply::json(503, "{}")]);
    let mut manager = rig.manager();
    fail_closings(&manager, 1);
    let _ = rig.refresh(&mut manager).await;
    assert!(manager.auth.pending_close.is_some());
    // The owner enters a new login before the retry: the store resolves the
    // started attempt (K4), so the closing finds nothing left to close.
    replace_with_new_login(&rig, &jg_exp(1_800_050_000), "refresh-2");
    rig.at(T0 + 5);
    manager.service();
    assert!(manager.auth.pending_close.is_none());
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
}

#[tokio::test]
async fn s1_a_closing_still_owed_at_shutdown_gets_one_last_try() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![Reply::json(503, "{}")]);
    let mut manager = rig.manager();
    fail_closings(&manager, 1);
    let _ = rig.refresh(&mut manager).await;
    assert!(rig.store.has_unresolved_legacy_refresh().unwrap());
    manager.shutdown().await.expect("an orderly shutdown");
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    assert_eq!(
        rig.receipts(),
        [("http_error".to_owned(), Some(503), None, false)]
    );
}

// ---------------------------------------------------------------------------
// C5-S2: a forced refresh in `sign_in_required` is a wait, not a terminal error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s2_a_forced_refresh_while_sign_in_is_required_is_not_a_terminal_error() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend
        .script(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
    let mut manager = rig.manager();
    let _ = rig.refresh(&mut manager).await;
    assert_eq!(
        rig.state(),
        (
            "sign_in_required".into(),
            Some("refresh_token_rejected".into())
        )
    );
    // The refusal-check refresh (R49) and a forced refresh (R41) are told that
    // none is possible: collection goes on with the usable token and nothing
    // terminal is reported for a state that is not terminal (R56).
    rig.at(T0 + 100);
    let error = manager
        .refresh_now(&rig.client(), SystemTime::now())
        .await
        .expect_err("no refresh possible");
    assert!(matches!(
        error,
        LegacyAuthManagerError::Auth(LegacyAuthError::RefreshNotPossible)
    ));
    assert!(!error.is_terminal_auth_failure());
    assert!(!error.is_sensitive_access_failure());
    let outcome = manager.refusal_check_refresh(&rig.client(), None).await;
    assert!(
        matches!(outcome, RefusalRefresh::RefreshNotPossible),
        "{outcome:?}"
    );
    // The state, the lease and the stop are exactly what they were.
    assert_eq!(
        manager.status_now().state,
        crate::legacy_auth::state::AuthState::SignInRequired
    );
    assert!(!manager.status_now().state.lease_is_terminal());
    assert!(
        manager.stop_view(rig.env.now()).is_none(),
        "requests still go"
    );
    assert!(manager.access_token_for_sensitive_use().is_ok());
    assert_eq!(rig.backend.token_posts().len(), 1);
}

#[test]
fn s2_the_error_of_a_stop_never_calls_a_sign_in_requirement_a_sign_out() {
    use crate::legacy_auth::{authority::stop_error, state::AuthStop};
    for reason in [
        SignOutReason::RefreshTokenRejected,
        SignOutReason::RefreshOutcomeUnknown,
        SignOutReason::RefreshResponseUnusable,
    ] {
        let error = stop_error(AuthStop::SignInRequired(reason));
        assert!(!error.is_terminal_auth_failure(), "{error:?}");
        assert!(!error.is_signed_out(), "{error:?}");
        // The same reasons as a sign-out are terminal.
        assert!(stop_error(AuthStop::SignedOut(reason)).is_terminal_auth_failure());
    }
}

// ---------------------------------------------------------------------------
// C5-S5: a pasted refresh token with padding, and an answer that keeps it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s5_a_padded_refresh_token_that_the_answer_keeps_is_persisted_as_a_changed_generation() {
    // Test 84 (trailing space) with test 52 (no `refresh_token` in the answer).
    let rig = new_rig().await;
    rig.seed(&jg(), "refresh-0 ", Some((T0 + 28_800, T0)));
    rig.backend.script(vec![Reply::json(
        200,
        r#"{"access_token":"AT1","expires_in":28800,"token_type":"Bearer"}"#,
    )]);
    let mut manager = rig.manager();
    let stored = rig.stored_generation();
    let padded =
        OwnerTokens::from_secret_parts(jg(), "refresh-0 ".to_owned()).expect("padded tokens");
    assert_eq!(
        stored,
        crate::teslamate_token::legacy_refresh_credential_generation(&padded),
        "the row is keyed by the stored bytes (R46)"
    );
    rig.refresh(&mut manager).await.expect("the refresh");
    // The POST carried the trimmed token, the answer kept it, and the pair was
    // persisted: the token is not consumed (F6) and the attempt is not left
    // unresolved for the owner to sign in again.
    let posts = rig.backend.token_posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].refresh.as_deref(), Some("refresh-0"));
    assert_eq!(rig.state().0, "ready");
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    assert_eq!(
        rig.stored_pair(),
        ("AT1".into(), "refresh-0".into(), T0 + 28_800, T0 + 21_600)
    );
    let trimmed =
        OwnerTokens::from_secret_parts("AT1".to_owned(), "refresh-0".to_owned()).expect("trimmed");
    let after = rig.stored_generation();
    assert_eq!(
        after,
        crate::teslamate_token::legacy_refresh_credential_generation(&trimmed)
    );
    assert_ne!(
        after, stored,
        "the padding is gone, so the generation changed"
    );
    assert_eq!(manager.generation(), after);
    // The completed binding records the change, so it is the unit's own write and
    // the credential identity does not change.
    assert!(
        rig.store
            .legacy_generation_is_refresh_output(after)
            .unwrap()
    );
    assert_eq!(
        rig.store.owner_wire_credential_identity().unwrap(),
        stored.to_string()
    );
    // The next refresh presents the same token again.
    rig.backend.script(vec![Reply::json(
        200,
        r#"{"access_token":"AT2","expires_in":28800,"token_type":"Bearer"}"#,
    )]);
    rig.at(T0 + 21_600);
    rig.refresh(&mut manager).await.expect("the next refresh");
    assert_eq!(
        rig.backend.token_posts()[1].refresh.as_deref(),
        Some("refresh-0")
    );
    assert_eq!(rig.stored_pair().0, "AT2");
}

#[tokio::test]
async fn s5_an_ambiguous_write_of_a_padded_pair_that_kept_its_token_is_not_written_twice() {
    // The write commits and reports an error (R24, R62). The successor's generation
    // differs from its input's (the padding is gone), so the row holding it proves the
    // write: the retry must not write again, which the closed receipt would refuse for
    // ever.
    let rig = new_rig().await;
    rig.seed(&jg(), "refresh-0 ", Some((T0 + 28_800, T0)));
    rig.backend.script(vec![Reply::json(
        200,
        r#"{"access_token":"AT1","expires_in":28800,"token_type":"Bearer"}"#,
    )]);
    let mut manager = rig.manager();
    plant(&manager, &[PersistFault::Ambiguous]);
    rig.refresh(&mut manager).await.expect("the attempt");
    assert_eq!(row_of(&rig).0, "persistence_pending");
    assert_eq!(rig.stored_pair().0, "AT1", "the write had committed");
    rig.at(T0 + 5);
    manager.service();
    assert_eq!(
        row_of(&rig).0,
        "ready",
        "the retry recognised its own write"
    );
    assert_eq!(manager.write_log.lock().unwrap().as_slice(), [T0]);
    assert_eq!(manager.generation(), rig.stored_generation());
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
}

// ---------------------------------------------------------------------------
// C5-S6: a store that cannot answer is a wait, never a lost authority
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s6_a_store_that_cannot_reserve_the_attempt_is_a_wait_and_does_not_fence_the_run() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    fail_reservations(&manager, 1);
    let error = rig.refresh(&mut manager).await.expect_err("the store");
    // Neither of the two predicates the collector's fences read.
    assert!(!error.is_sensitive_access_failure(), "{error:?}");
    assert!(!error.is_terminal_auth_failure(), "{error:?}");
    assert_eq!(
        auth_error(error),
        LegacyAuthError::AuthUnavailable {
            retry_at: Some(T0 + 60)
        }
    );
    assert_eq!(
        row_of(&rig),
        (
            "retrying".into(),
            Some("store_unavailable".into()),
            Some(T0 + 60),
            true
        )
    );
    assert!(rig.backend.token_posts().is_empty(), "nothing was sent");
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    // The request goes on with the token that is still usable (R30), and the
    // wrapper does not stop at the swallowed wait.
    rig.at(T0 + 1);
    rig.list(&mut manager).await.expect("products");
    assert_eq!(
        rig.backend.products()[0].authorization.as_deref(),
        Some(format!("Bearer {}", jg()).as_str())
    );
    // At +60 s the refresh is sent.
    rig.at(T0 + 60);
    rig.refresh(&mut manager).await.expect("the retry");
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(rig.state().0, "ready");
}

#[tokio::test]
async fn s6_a_fault_of_the_store_while_the_stored_pair_is_adopted_is_a_wait_too() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    // The owner enters a new login, and its ciphertext cannot be opened (a damaged
    // row): adopting the pair fails, once when the unit looks before it sends and
    // once more when the store refuses the reservation of the old generation.
    replace_with_new_login(&rig, &jg_exp(1_800_050_000), "refresh-2");
    rig.store
        .open()
        .unwrap()
        .execute(
            "UPDATE teslamate_legacy_tokens SET access = x'00', refresh = x'00'",
            [],
        )
        .unwrap();
    let error = rig.refresh(&mut manager).await.expect_err("not adopted");
    assert!(!error.is_sensitive_access_failure(), "{error:?}");
    assert!(!error.is_terminal_auth_failure(), "{error:?}");
    assert_eq!(
        auth_error(error),
        LegacyAuthError::AuthUnavailable {
            retry_at: Some(T0 + 60)
        }
    );
    assert_eq!(
        rig.state(),
        ("retrying".into(), Some("store_unavailable".into()))
    );
    assert!(rig.backend.token_posts().is_empty(), "nothing was sent");
}

// ---------------------------------------------------------------------------
// Codex R03: a refresh that kept its token and an external write that came first
// ---------------------------------------------------------------------------

#[tokio::test]
async fn r03_an_external_write_during_a_non_rotating_exchange_is_never_overwritten() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![
        Reply::json(
            200,
            r#"{"access_token":"AT1","expires_in":28800,"token_type":"Bearer"}"#,
        )
        .after(Duration::from_millis(400)),
    ]);
    let manager = Arc::new(tokio::sync::Mutex::new(rig.manager()));
    let client = rig.client();
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(&rig, 1).await;
    // The owner enters a new login while the request is in flight. The store
    // resolves the started attempt (K4).
    let external = jg_exp(1_800_050_000);
    replace_with_new_login(&rig, &external, "refresh-2");
    caller
        .await
        .expect("task")
        .expect("no failure to the caller");
    let mut guard = manager.lock().await;
    rig.at(T0 + 5);
    rig.refresh(&mut guard).await.expect("adopts at the retry");
    // The older answer never reached the store: the owner's pair is what it was.
    assert_eq!(rig.stored_pair().0, external);
    assert_eq!(rig.stored_pair().1, "refresh-2");
    assert_eq!(guard.refresh_token(), "refresh-2");
    assert_eq!(guard.generation(), rig.stored_generation());
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    assert_eq!(guard.auth.stop, None);
    assert_ne!(rig.state().0, "signed_out");
}

// ---------------------------------------------------------------------------
// Codex R07: what the unit owes between requests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn r07_a_pending_successor_is_written_by_the_units_own_service_without_a_request() {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    plant(&manager, &[PersistFault::Fail]);
    rig.refresh(&mut manager).await.expect("the attempt");
    assert_eq!(row_of(&rig).0, "persistence_pending");
    assert_eq!(rig.stored_pair().0, jg(), "not written yet");
    // The token in use is still usable, so no request waits on the write. Five
    // seconds later the store is back, and the unit writes by itself.
    rig.at(T0 + 4);
    manager.service();
    assert_eq!(rig.stored_pair().0, jg(), "not due before five seconds");
    rig.at(T0 + 5);
    manager.service();
    assert_eq!(rig.stored_pair().0, "AT1");
    assert_eq!(row_of(&rig).0, "ready");
    assert_eq!(manager.write_log.lock().unwrap().as_slice(), [T0, T0 + 5]);
    assert!(rig.backend.products().is_empty(), "no request was made");
    assert_eq!(rig.backend.token_posts().len(), 1);
}

// ---------------------------------------------------------------------------
// C5T-02: spec test 16 at the manager (R9)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t16_a_successor_minted_by_another_issuer_is_never_persisted() {
    let rig = new_rig().await;
    rig.seed_due();
    let body = serde_json::json!({
        "access_token": jc(),
        "refresh_token": "refresh-1",
        "expires_in": 28_800,
        "token_type": "Bearer",
    })
    .to_string();
    rig.backend.script(vec![Reply::json(200, &body)]);
    let mut manager = rig.manager();
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("unusable"));
    assert_eq!(error, LegacyAuthError::InvalidResponse);
    assert_eq!(
        rig.state(),
        (
            "sign_in_required".into(),
            Some("refresh_response_unusable".into())
        )
    );
    assert_eq!(rig.receipts(), [("started".to_owned(), None, None, true)]);
    assert!(
        manager.write_log.lock().unwrap().is_empty(),
        "no persist call"
    );
    assert_eq!(
        rig.stored_pair().0,
        jg(),
        "the stored pair is still the global one"
    );
}

#[tokio::test]
async fn t16_under_a_test_override_the_successor_must_name_the_issuer_the_test_gave() {
    let rig = new_rig().await;
    // A refreshed row due long ago (the manager's own clock is the real one).
    rig.seed("AT0", "refresh-0", Some((1_700_100_000, 1_700_000_000)));
    let issuer = Url::parse(&format!("{}oauth2/v3", rig.base)).unwrap();
    let body = serde_json::json!({
        "access_token": jwt_for_test(serde_json::json!({
            "iss": "https://other.example.test/oauth2/v3",
            "exp": 4_000_000_000_i64,
        })),
        "refresh_token": "refresh-1",
        "expires_in": 28_800,
    })
    .to_string();
    rig.backend.script(vec![Reply::json(200, &body)]);
    let mut manager = LegacyAuthManager::from_hub_teslamate_store_with_issuer(
        rig.store.clone(),
        rig.directory.path(),
        issuer,
    )
    .expect("a manager over the given issuer");
    let error = auth_error(rig.refresh(&mut manager).await.expect_err("unusable"));
    assert_eq!(error, LegacyAuthError::InvalidResponse);
    assert_eq!(rig.receipts(), [("started".to_owned(), None, None, true)]);
    assert_eq!(rig.stored_pair().0, "AT0");
}

// ---------------------------------------------------------------------------
// C5T-03: R45 row 5 at the manager (the guard fails between the answer and the write)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn c5t03_a_guard_that_fails_between_the_answer_and_the_write_leaves_a_pending_write_and_a_retry()
 {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    // The guard is asked three times: before the attempt, before the send (R17) and
    // before the write (R45 row 5). It refuses the third only.
    let calls = Arc::new(AtomicUsize::new(0));
    manager.sensitive_access = {
        let calls = Arc::clone(&calls);
        Arc::new(move || {
            if calls.fetch_add(1, Ordering::SeqCst) == 2 {
                Err(CredentialError::SensitiveAccessUnavailable)
            } else {
                Ok(())
            }
        })
    };
    rig.refresh(&mut manager)
        .await
        .expect("a pending write is not a failure of the caller");
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(
        row_of(&rig),
        ("persistence_pending".into(), None, None, true)
    );
    // The rotation succeeded and is not recorded yet: the receipt stays started.
    assert_eq!(rig.receipts(), [("started".to_owned(), None, None, true)]);
    assert_eq!(rig.stored_pair().0, jg());
    assert!(
        manager.write_log.lock().unwrap().is_empty(),
        "no write was tried"
    );
    // Collection goes on with the old token while it is usable.
    rig.at(T0 + 1);
    rig.list(&mut manager).await.expect("products");
    assert_eq!(
        rig.backend.products()[0].authorization.as_deref(),
        Some(format!("Bearer {}", jg()).as_str())
    );
    // Five seconds on, with the guard back, the write is retried and succeeds, and
    // the next request carries the new token.
    rig.at(T0 + 5);
    manager.service();
    assert_eq!(rig.stored_pair().0, "AT1");
    assert_eq!(
        rig.receipts(),
        [("success".to_owned(), Some(200), None, true)]
    );
    assert_eq!(manager.write_log.lock().unwrap().as_slice(), [T0 + 5]);
    rig.list(&mut manager).await.expect("products");
    assert_eq!(
        rig.backend
            .products()
            .last()
            .unwrap()
            .authorization
            .as_deref(),
        Some("Bearer AT1")
    );
    assert_eq!(rig.backend.token_posts().len(), 1, "no second request");
    assert_eq!(row_of(&rig).0, "ready");
}

// ---------------------------------------------------------------------------
// C5T-05: R15 and R15a, a caller that joins a running flight
// ---------------------------------------------------------------------------

#[tokio::test]
async fn c5t05_a_caller_that_arrives_while_a_dropped_callers_exchange_runs_waits_for_it() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![
        Reply::json(200, SUCCESS).after(Duration::from_millis(400)),
    ]);
    let manager = Arc::new(tokio::sync::Mutex::new(rig.manager()));
    let client = rig.client();
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(&rig, 1).await;
    caller.abort();
    let _ = caller.await;
    // The exchange is still running (its answer is 400 ms away). The next caller
    // joins it: it waits for the answer and then carries the new token. It is not
    // refused, and it does not start a second exchange.
    assert!(manager.lock().await.flight.is_running());
    let mut guard = manager.lock().await;
    rig.at(T0 + 1);
    rig.list(&mut guard)
        .await
        .expect("the joined caller is served");
    assert_eq!(
        rig.backend.kinds(),
        ["token", "products"],
        "after the answer"
    );
    assert_eq!(
        rig.backend.products()[0].authorization.as_deref(),
        Some("Bearer AT1")
    );
    assert_eq!(rig.backend.token_posts().len(), 1, "exactly one exchange");
}

// ---------------------------------------------------------------------------
// C5T-08: R58 at the manager, and the clock the receipts are dated on
// ---------------------------------------------------------------------------

/// A synthetic token receipt that began `seconds_ago` before the manager's clock,
/// with the outcome `success`.
fn seed_token_receipt(rig: &Rig, seconds_ago: i64) {
    let started_ms = (rig.env.now() - seconds_ago) * 1_000;
    rig.store
        .open()
        .unwrap()
        .execute(
            "INSERT INTO outbound_request_receipts(
                correlation_id, started_at_ms, completed_at_ms, duration_ms, vehicle_tesla_id,
                transport, operation, safety_class, precondition, outcome, http_status)
             VALUES (?1, ?2, ?3, 10, NULL, 'legacy_auth', 'token_refresh',
                     'non_wake_endpoint', 'not_required', 'success', 200)",
            rusqlite::params![
                uuid::Uuid::new_v4().to_string(),
                started_ms,
                started_ms + 10
            ],
        )
        .unwrap();
}

#[tokio::test]
async fn t65_a_token_receipt_twenty_seconds_old_holds_the_first_request_back_for_forty() {
    let rig = new_rig().await;
    rig.seed_due();
    seed_token_receipt(&rig, 20);
    let mut manager = rig.manager();
    // The first attempt is 40 s after the start (R58); a forced refresh sits out
    // the interval when it is no longer than a minute (R49).
    rig.refresh(&mut manager).await.expect("held back");
    assert!(rig.backend.token_posts().is_empty());
    rig.at(T0 + 39);
    rig.refresh(&mut manager).await.expect("still held");
    assert!(rig.backend.token_posts().is_empty());
    rig.at(T0 + 40);
    rig.refresh(&mut manager).await.expect("the first attempt");
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(rig.state().0, "ready");
}

#[tokio::test]
async fn t65_a_forced_refresh_waits_out_the_restart_floor_and_a_receipt_sixty_one_seconds_old_holds_nothing()
 {
    let rig = new_rig().await;
    rig.seed_due();
    seed_token_receipt(&rig, 20);
    let mut manager = rig.manager();
    manager
        .refresh_now(&rig.client(), SystemTime::now())
        .await
        .expect("forced");
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(rig.env.now(), T0 + 40, "the manual clock moved by the wait");

    let rig = new_rig().await;
    rig.seed_due();
    seed_token_receipt(&rig, 61);
    let mut manager = rig.manager();
    rig.refresh(&mut manager).await.expect("immediate");
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(rig.env.now(), T0);
}

// ---------------------------------------------------------------------------
// C5T-06: spec test 27, the P4 clause (R40 step 2): a remedy under way
// ---------------------------------------------------------------------------

/// The consecutive count and the requests the fake saw after a 401 that must not
/// count.
fn assert_not_counted(bed: &Bed, fuse: &LegacyAuthFuse, error: OwnerApiAuthError, retry: i64) {
    assert_eq!(unavailable_at(error), Some(retry));
    assert_eq!(fuse.consecutive(), 0);
    assert_eq!(bed.stored_consecutive(), 0);
}

#[tokio::test]
async fn c5t06_a_401_while_the_startup_refresh_is_retrying_is_not_counted() {
    let bed = new_bed().await;
    bed.rig.seed(&jg(), "refresh-0", None);
    bed.fake.script_tokens(vec![Reply::json(503, "{}")]);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    // t0: the startup refresh fails with a 503 (a retry is pending), and the list
    // goes out with the stored token, which is usable.
    bed.list(&mut manager, &mut fuse).await.expect("products");
    assert_eq!(bed.fake.posts().len(), 1);
    // t0 + 10: the token is rejected while the retry is pending.
    bed.at(T0 + 10);
    bed.fake.reject(&jg());
    let error = bed.list(&mut manager, &mut fuse).await.expect_err("401");
    assert_not_counted(&bed, &fuse, error, T0 + 60);
    assert_eq!(bed.fake.posts().len(), 1, "no extra token request");
    assert_eq!(
        bed.fake.arrivals("products").len(),
        2,
        "the two lists, and no confirmation read"
    );
}

#[tokio::test]
async fn c5t06_a_401_while_only_the_startup_refresh_is_owed_is_not_counted() {
    let bed = new_bed().await;
    bed.rig.seed(&jg(), "refresh-0", None);
    let mut manager = bed.manager();
    // The refresh that an imported pair owes is held back by the restart floor, so
    // it is pending and nothing else is.
    manager.auth_for_test().token_requests_not_before = T0 + 1_000;
    assert!(manager.auth_for_test().startup_refresh_pending);
    let mut fuse = LegacyAuthFuse::default();
    bed.fake.reject(&jg());
    let error = bed.list(&mut manager, &mut fuse).await.expect_err("401");
    assert!(matches!(
        legacy_error(error),
        LegacyAuthError::AuthUnavailable { .. }
    ));
    assert_eq!(fuse.consecutive(), 0);
    assert_eq!(bed.stored_consecutive(), 0);
    assert!(bed.fake.posts().is_empty());
    assert_eq!(
        bed.fake.arrivals("products").len(),
        1,
        "no confirmation read"
    );
}

#[tokio::test]
async fn c5t06_a_401_while_a_successor_waits_for_its_write_is_not_counted() {
    let bed = new_bed().await;
    bed.rig.seed(&jg(), "refresh-0", Some((T0 + 28_800, T0)));
    let mut manager = bed.manager();
    plant(&manager, &[PersistFault::Fail; 4]);
    // The refresh succeeds and its write fails: the successor is pending.
    bed.rig.refresh(&mut manager).await.expect("the attempt");
    assert!(manager.auth_for_test().pending_successor.is_some());
    let mut fuse = LegacyAuthFuse::default();
    bed.at(T0 + 1);
    bed.fake.reject(&jg());
    let error = bed.list(&mut manager, &mut fuse).await.expect_err("401");
    assert_not_counted(&bed, &fuse, error, T0 + 5);
    assert_eq!(bed.fake.posts().len(), 1);
    assert_eq!(
        bed.fake.arrivals("products").len(),
        1,
        "no confirmation read"
    );
}

// ---------------------------------------------------------------------------
// C5T-07, C5-S4: the confirmation read's outcomes at request level
// ---------------------------------------------------------------------------

#[tokio::test]
async fn c5t07_a_cars_401_confirmed_by_a_401_is_counted_forces_one_refresh_and_is_sent_once_more() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    *bed.fake.0.watch.lock().unwrap() = Some(bed.rig.store.clone());
    bed.fake.reject(&jg());
    bed.probe(&mut manager, &mut fuse, 1)
        .await
        .expect("the resend carries the new token");
    // One confirmation read (a 401), one token request, and the car's request sent
    // once more with the new pair.
    let products = bed.fake.arrivals("products");
    assert_eq!(products.len(), 1);
    assert_eq!(products[0].bearer.as_deref(), Some(jg().as_str()));
    assert_eq!(bed.fake.posts().len(), 1);
    let probes = bed.fake.arrivals("probe");
    assert_eq!(probes.len(), 2, "sent once more, and no more");
    assert_eq!(probes[0].bearer.as_deref(), Some(jg().as_str()));
    assert_eq!(probes[1].bearer.as_deref(), Some("AT1"));
    // The count was 1 when the resend arrived (R41, R42: its 2xx then resets it).
    assert_eq!(bed.fake.consecutive_when("probe", 1), Some(1));
    assert!(
        probes[1].at >= bed.fake.posts()[0].at,
        "the resend followed the refresh"
    );
}

#[tokio::test]
async fn c5t07_an_inconclusive_confirmation_fails_the_request_and_changes_nothing() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    bed.fake.0.probe_rejected.lock().unwrap().insert(1);
    bed.fake.script_products(&[503]);
    let error = bed
        .probe(&mut manager, &mut fuse, 1)
        .await
        .expect_err("inconclusive");
    assert_eq!(unavailable_at(error), Some(T0 + 60));
    assert_eq!(fuse.consecutive(), 0);
    assert_eq!(bed.stored_consecutive(), 0);
    assert!(bed.fake.posts().is_empty(), "no token request");
    assert_eq!(bed.fake.arrivals("products").len(), 1);
    // The answer of the read is kept for the collector's own classification.
    assert_eq!(
        fuse.take_confirmation_errors(),
        vec![OwnerApiError::HttpStatus(503)]
    );
    // A second car's 401 inside the 60 s spacing sends no read and fails the same.
    bed.fake.0.probe_rejected.lock().unwrap().insert(2);
    bed.at(T0 + 30);
    let error = bed
        .probe(&mut manager, &mut fuse, 2)
        .await
        .expect_err("inside the spacing");
    assert_eq!(unavailable_at(error), Some(T0 + 60));
    assert_eq!(bed.fake.arrivals("products").len(), 1, "no second read");
    assert!(
        fuse.take_confirmation_errors().is_empty(),
        "nothing was sent"
    );
    assert_eq!(fuse.consecutive(), 0);
}

#[tokio::test]
async fn s4_a_429_on_the_confirmation_read_reaches_the_collector_with_its_retry_after() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    bed.fake.0.probe_rejected.lock().unwrap().insert(1);
    bed.fake.script_products(&[429]);
    bed.fake
        .0
        .products_headers
        .lock()
        .unwrap()
        .push(("retry-after".to_owned(), "120".to_owned()));
    let error = bed
        .probe(&mut manager, &mut fuse, 1)
        .await
        .expect_err("rate limited");
    assert_eq!(unavailable_at(error), Some(T0 + 60));
    assert_eq!(
        fuse.take_confirmation_errors(),
        vec![OwnerApiError::RateLimited {
            retry_after_seconds: 120
        }]
    );
    assert_eq!(fuse.consecutive(), 0);
    assert!(bed.fake.posts().is_empty());
}

#[tokio::test]
async fn s4_a_403_that_names_the_fleet_api_on_the_confirmation_read_reaches_the_collector() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    bed.fake.0.probe_rejected.lock().unwrap().insert(1);
    bed.fake
        .0
        .products_script
        .lock()
        .unwrap()
        .push_back((403, "Please use the Fleet API".to_owned()));
    let error = bed
        .probe(&mut manager, &mut fuse, 1)
        .await
        .expect_err("refused");
    assert_eq!(unavailable_at(error), Some(T0 + 60));
    assert_eq!(
        fuse.take_confirmation_errors(),
        vec![OwnerApiError::Forbidden {
            fleet_api_named: true
        }]
    );
    assert_eq!(
        fuse.consecutive(),
        0,
        "a refusal is the collector's, not evidence here"
    );
}

#[tokio::test]
async fn s4_the_answer_of_a_stream_triggered_confirmation_is_kept_too() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    let mut manager = bed.manager();
    let mut fuse = LegacyAuthFuse::default();
    bed.fake.script_products(&[429]);
    bed.api()
        .confirm_stream_rejection(&mut manager, &mut fuse)
        .await
        .expect("a stream confirmation is not an error of the run");
    assert_eq!(
        fuse.take_confirmation_errors(),
        vec![OwnerApiError::RateLimited {
            retry_after_seconds: 0
        }]
    );
    assert_eq!(fuse.take_confirmation_count(), 1);
}

// ---------------------------------------------------------------------------
// C5-S3: R8a, a car's 401 after the pause is confirmed before the account is blamed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn s3_a_cars_401_after_the_pause_is_confirmed_by_the_list_and_signs_nobody_out() {
    let bed = new_bed().await;
    let mut manager = access_only(&bed, "provider-token-0001");
    let mut fuse = LegacyAuthFuse::default();
    // The list is rejected: the pause starts.
    bed.fake.reject("provider-token-0001");
    let error = bed.list(&mut manager, &mut fuse).await.expect_err("401");
    assert_eq!(unavailable_at(error), Some(T0 + 300));
    // After the pause the first request is one car's, and Tesla refuses that car's
    // access while the list works.
    bed.fake.accept_all();
    bed.fake.0.probe_rejected.lock().unwrap().insert(1);
    bed.at(T0 + 300);
    let error = bed
        .probe(&mut manager, &mut fuse, 1)
        .await
        .expect_err("the car's own refusal");
    assert_eq!(legacy_error(error), LegacyAuthError::VehicleAccessRejected);
    assert_eq!(manager.status_now().state, AuthState::Ready);
    assert_eq!(fuse.consecutive(), 0, "the confirming 2xx resets the fuse");
    assert_eq!(
        bed.fake.arrivals("products").len(),
        2,
        "the 401, then the one read"
    );
    // The same first request when the list is refused as well: the token is dead.
    let bed = new_bed().await;
    let mut manager = access_only(&bed, "provider-token-0001");
    let mut fuse = LegacyAuthFuse::default();
    bed.fake.reject("provider-token-0001");
    let _ = bed.list(&mut manager, &mut fuse).await.expect_err("401");
    bed.at(T0 + 300);
    let error = bed
        .probe(&mut manager, &mut fuse, 1)
        .await
        .expect_err("the account refuses the token");
    assert_eq!(legacy_error(error), LegacyAuthError::ProviderTokenRejected);
    let status = manager.status_now();
    assert_eq!(
        (status.state, status.reason.map(|reason| reason.code())),
        (AuthState::SignedOut, Some("provider_token_rejected"))
    );
    assert!(bed.fake.posts().is_empty(), "no refresh path");
}

// ---------------------------------------------------------------------------
// C5T-10: the refusal row of C3 is read from the store and compared with the
// configured mode, and not with a mode the test puts in the view
// ---------------------------------------------------------------------------

#[tokio::test]
async fn c5t10_a_refusal_row_in_the_store_is_restored_only_for_the_configured_mode() {
    let bed = new_bed().await;
    seed_quiet(&bed);
    bed.rig
        .store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO collector_account_state
                (singleton_id, mode, config_fingerprint, state, reason_code, since_ms, updated_at_ms)
             VALUES (1, 'legacy', ?1, 'owner_api_refused', 'owner_api_refused', 1000, 1000)",
            rusqlite::params!["0".repeat(64)],
        )
        .expect("C3 account row");
    // What the collector's start path reads: the row as the store holds it.
    let rows = bed
        .rig
        .store
        .load_collector_state_rows()
        .expect("rows")
        .account
        .expect("the account row");
    let view = || CollectorStartView {
        reset_refusal: false,
        account: Some((
            rows.mode.as_str(),
            rows.state.as_str(),
            rows.reason_code.as_str(),
        )),
    };
    // The configured mode is Tesla (C3's `legacy`): the refusal is restored.
    let mut tesla = bed.manager();
    tesla.apply_collector_start(&view());
    assert_eq!(tesla.status_now().state, AuthState::ApiRefused);
    assert_eq!(bed.rig.row().stopped_mode.as_deref(), Some("legacy"));
    drop(tesla);
    // The configured mode is Provider: the owner changed the mode, so the same
    // stored row is not this account's refusal (R44 (c)). The unit starts over a
    // pair stored for the other mode, so the status row is cleared first.
    let bed = new_bed().await;
    bed.rig.seed("provider-token-0001", "refresh-0", None);
    bed.rig
        .store
        .open()
        .expect("catalogue")
        .execute(
            "INSERT INTO collector_account_state
                (singleton_id, mode, config_fingerprint, state, reason_code, since_ms, updated_at_ms)
             VALUES (1, 'legacy', ?1, 'owner_api_refused', 'owner_api_refused', 1000, 1000)",
            rusqlite::params!["0".repeat(64)],
        )
        .expect("C3 account row");
    let rows = bed
        .rig
        .store
        .load_collector_state_rows()
        .expect("rows")
        .account
        .expect("the account row");
    let mut provider = LegacyAuthManager::from_hub_teslamate_store_for_test(
        bed.rig.store.clone(),
        bed.rig.directory.path(),
        &AuthEndpointConfig {
            mode: AuthMode::Provider,
            region: None,
            token_url: Some(bed.rig.token_url().to_string()),
            allow_loopback_http: true,
        },
        Some(bed.rig.token_url()),
        false,
        bed.rig.env.clone(),
    )
    .expect("a provider unit");
    provider.apply_collector_start(&CollectorStartView {
        reset_refusal: false,
        account: Some((
            rows.mode.as_str(),
            rows.state.as_str(),
            rows.reason_code.as_str(),
        )),
    });
    assert_eq!(provider.status_now().state, AuthState::Ready);
    assert_eq!(bed.rig.row().stopped_mode, None);
}

// ---------------------------------------------------------------------------
// R33a, C5T-13: a shutdown waits for an exchange whose caller was dropped
// ---------------------------------------------------------------------------

#[tokio::test]
async fn r33a_a_shutdown_waits_for_the_exchange_a_dropped_caller_left_and_persists_its_answer() {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![
        Reply::json(200, SUCCESS).after(Duration::from_millis(400)),
    ]);
    let manager = Arc::new(tokio::sync::Mutex::new(rig.manager()));
    let client = rig.client();
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(&rig, 1).await;
    caller.abort();
    let _ = caller.await;
    // The exchange is 400 ms from its answer. The shutdown does not return before
    // the answer is persisted, and no new request starts.
    assert_eq!(rig.stored_pair().0, jg());
    manager
        .lock()
        .await
        .shutdown()
        .await
        .expect("an orderly shutdown");
    assert_eq!(
        rig.stored_pair().0,
        "AT1",
        "persisted before the shutdown returned"
    );
    assert_eq!(rig.state().0, "ready");
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
}

#[tokio::test]
async fn r03_a_non_rotating_answer_whose_write_fails_leaves_the_receipt_started_until_it_is_written()
 {
    // R45 row 5, R23: the receipt stays `started` until the successor is persisted,
    // and only then is it a success. The write and the close are one step, so a write
    // that fails closes nothing and releases nothing.
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![Reply::json(
        200,
        r#"{"access_token":"AT1","expires_in":28800,"token_type":"Bearer"}"#,
    )]);
    let mut manager = rig.manager();
    let g0 = manager.generation();
    rig.store
        .open()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER block_the_pair_write BEFORE UPDATE ON teslamate_legacy_tokens
             BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
        )
        .unwrap();
    rig.refresh(&mut manager).await.expect("a pending write");
    assert_eq!(row_of(&rig).0, "persistence_pending");
    assert_eq!(rig.receipts(), [("started".to_owned(), None, None, true)]);
    assert_eq!(rig.store.legacy_fenced_inputs_for_test().unwrap(), [g0]);
    assert_eq!(rig.stored_pair().0, jg());
    // The store is back: the retry writes the pair and closes the receipt together.
    rig.store
        .open()
        .unwrap()
        .execute_batch("DROP TRIGGER block_the_pair_write")
        .unwrap();
    rig.at(T0 + 5);
    manager.service();
    assert_eq!(rig.stored_pair().0, "AT1");
    assert_eq!(rig.stored_pair().1, "refresh-0");
    assert_eq!(
        rig.receipts(),
        [("success".to_owned(), Some(200), None, false)]
    );
    assert!(
        rig.store
            .legacy_fenced_inputs_for_test()
            .unwrap()
            .is_empty()
    );
    assert_eq!(row_of(&rig).0, "ready");
}

// ---------------------------------------------------------------------------
// B2 P3: stopped construction of an unresolved Legacy refresh (C5 R57 step 3)
// ---------------------------------------------------------------------------

/// Produce an unresolved attempt in one of the ways the outcome table leaves one:
/// row 4 (the request is never answered), row 6 (an answer over the size cap) and
/// the unpersisted row 5 (an answer whose successor could not be written).
async fn c5t11_unresolved_rig(row: &str) -> Rig {
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    match row {
        "row 4" => {
            rig.backend.script(vec![Reply::hang()]);
            let client = rig.short_client(Duration::from_secs(5), Duration::from_millis(300));
            let _ = manager.refresh_if_due(&client, SystemTime::now()).await;
        }
        "row 6" => {
            let mut body = String::from(r#"{"access_token":""#);
            body.push_str(&"a".repeat(65_537 - body.len() - 2));
            body.push_str("\"}");
            rig.backend.script(vec![Reply::json(200, &body)]);
            let _ = rig.refresh(&mut manager).await;
        }
        _ => {
            plant(&manager, &[PersistFault::Fail]);
            let _ = rig.refresh(&mut manager).await;
            assert_eq!(row_of(&rig).0, "persistence_pending");
        }
    }
    drop(manager);
    assert!(rig.store.has_unresolved_legacy_refresh().unwrap(), "{row}");
    rig
}

#[tokio::test]
async fn c5t11_unresolved_refresh_constructs_a_stopped_manager_without_transport() {
    for row in ["row 4", "row 6", "row 5 unpersisted"] {
        let rig = c5t11_unresolved_rig(row).await;
        let fences = rig.store.legacy_fenced_inputs_for_test().unwrap();
        let pair = rig.stored_pair();
        let posts = rig.backend.token_posts().len();
        let products = rig.backend.products().len();
        // A valid, loadable pair and key construct the manager in its restored
        // stopped state: signed out with an unknown outcome. (The helper asserts the
        // state and that no token is handed out.)
        rig.at(T0 + 200);
        let mut stopped = rig.restarted_stopped();
        assert_eq!(
            rig.state(),
            ("signed_out".into(), Some("refresh_outcome_unknown".into())),
            "{row}"
        );
        // It sends nothing and hands nothing out, however it is asked: no token
        // request, no Owner API request, no refresh, no token for a stream.
        for step in 1..=3 {
            rig.at(T0 + 200 + step * 4_000);
            assert!(rig.list(&mut stopped).await.is_err(), "{row}");
            assert!(rig.refresh(&mut stopped).await.is_err(), "{row}");
            assert!(stopped.access_token_for_sensitive_use().is_err(), "{row}");
            assert!(matches!(
                stopped.stop_view(rig.env.now()),
                Some(LegacyAuthError::SignedOut {
                    reason: SignOutReason::RefreshOutcomeUnknown
                })
            ));
        }
        assert_eq!(
            rig.backend.token_posts().len(),
            posts,
            "{row}: no token request"
        );
        assert_eq!(
            rig.backend.products().len(),
            products,
            "{row}: no Owner API request"
        );
        // The fences, the stored pair and the unresolved evidence are unchanged, and
        // the owner's retry cannot replay the input.
        assert_eq!(
            rig.store.legacy_fenced_inputs_for_test().unwrap(),
            fences,
            "{row}"
        );
        assert_eq!(rig.stored_pair(), pair, "{row}");
        assert!(rig.store.has_unresolved_legacy_refresh().unwrap(), "{row}");
        assert!(
            stopped.owner_retry().is_err(),
            "{row}: the retry cannot lift it"
        );
        assert_eq!(rig.backend.token_posts().len(), posts, "{row}");
        // An external credential write is the recovery: a restart then loads the new
        // pair, collects again, and the fenced refresh token is never presented again.
        // (A pair in its refreshed form, not due: the recovered unit then sends no token
        // request at all.)
        let key = crate::teslamate_credentials::load_key(rig.directory.path()).unwrap();
        let new_access = jg_exp(1_800_050_000);
        let tokens =
            OwnerTokens::from_secret_parts(new_access.clone(), "refresh-9".to_owned()).unwrap();
        let (cipher_access, cipher_refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens).unwrap();
        rig.store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    cipher_access,
                    cipher_refresh,
                    T0 + 40_000,
                    T0 + 30_000,
                )
                .unwrap()
                .with_credential_generation(
                    crate::teslamate_token::legacy_refresh_credential_generation(&tokens),
                )
                .unwrap(),
            )
            .expect("an external write");
        assert!(
            !rig.store.has_unresolved_legacy_refresh().unwrap(),
            "{row}: resolved by the write"
        );
        rig.at(T0 + 300);
        let mut recovered = rig.manager();
        rig.list(&mut recovered).await.expect("collects again");
        assert_eq!(
            rig.backend.token_posts().len(),
            posts,
            "{row}: no token request at all"
        );
        assert_eq!(rig.backend.products().len(), products + 1, "{row}");
        assert_eq!(
            rig.backend
                .products()
                .last()
                .unwrap()
                .authorization
                .as_deref(),
            Some(format!("Bearer {new_access}").as_str()),
            "{row}: the new pair is used"
        );
        assert_eq!(rig.state().0, "ready", "{row}");
        // A recovered pair that comes due is refreshed with ITS refresh token and
        // never with the fenced input: a second external write, due now, is
        // refreshed by exactly one token request carrying `refresh-10`.
        let fenced_before = rig.store.legacy_fenced_inputs_for_test().unwrap();
        let posts_before = rig.backend.token_posts().len();
        let due_tokens =
            OwnerTokens::from_secret_parts(jg_exp(1_800_060_000), "refresh-10".to_owned()).unwrap();
        let (due_access, due_refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &due_tokens)
                .unwrap();
        rig.store
            .replace_teslamate_legacy_tokens(
                &crate::db::TeslaMateLegacyTokenStore::refreshed(
                    due_access,
                    due_refresh,
                    T0 + 40_000,
                    T0 + 350,
                )
                .unwrap()
                .with_credential_generation(
                    crate::teslamate_token::legacy_refresh_credential_generation(&due_tokens),
                )
                .unwrap(),
            )
            .expect("a second external write");
        rig.backend.script(vec![Reply::json(200, SUCCESS)]);
        rig.at(T0 + 400);
        let mut due = rig.manager();
        rig.refresh(&mut due)
            .await
            .unwrap_or_else(|error| panic!("{row}: the recovered pair refreshes: {error:?}"));
        let after: Vec<_> = rig
            .backend
            .token_posts()
            .into_iter()
            .skip(posts_before)
            .collect();
        assert_eq!(after.len(), 1, "{row}: exactly one token request");
        assert_eq!(
            after[0].refresh.as_deref(),
            Some("refresh-10"),
            "{row}: the externally written refresh token"
        );
        assert!(
            after
                .iter()
                .all(|post| post.refresh.as_deref() != Some("refresh-0")),
            "{row}: the fenced input is never presented again"
        );
        let fenced_after = rig.store.legacy_fenced_inputs_for_test().unwrap();
        assert!(
            fenced_before
                .iter()
                .all(|fence| fenced_after.contains(fence)),
            "{row}: the old fences stand"
        );
    }
}

#[tokio::test]
async fn c5t11_a_genuine_construction_failure_stays_an_error() {
    // The stopped construction is for a loadable pair and key only: a missing pair,
    // a missing or unusable key, a ciphertext that does not open and a store that
    // cannot answer remain errors, with an unresolved attempt beside them or not.
    type Fault = fn(&Rig);
    let faults: [(&str, Fault); 4] = [
        ("missing pair row", |rig| {
            rig.store
                .open()
                .unwrap()
                .execute("DELETE FROM teslamate_legacy_tokens", [])
                .unwrap();
        }),
        ("missing key", |rig| {
            std::fs::remove_file(crate::teslamate_credentials::key_path(rig.directory.path()))
                .expect("remove the key");
        }),
        ("ciphertext that does not open", |rig| {
            rig.store
                .open()
                .unwrap()
                .execute(
                    "UPDATE teslamate_legacy_tokens SET access = X'0102030405060708090a0b0c0d0e0f1011'",
                    [],
                )
                .unwrap();
        }),
        ("store that cannot answer", |rig| {
            rig.store
                .open()
                .unwrap()
                .execute_batch("ALTER TABLE teslamate_legacy_tokens RENAME TO c5t11_hidden")
                .unwrap();
        }),
    ];
    for (label, fault) in faults {
        let rig = c5t11_unresolved_rig("row 4").await;
        fault(&rig);
        let built = LegacyAuthManager::from_hub_teslamate_store_for_test(
            rig.store.clone(),
            rig.directory.path(),
            &rig.endpoint(None),
            Some(rig.token_url()),
            false,
            rig.env.clone(),
        );
        assert!(
            built.is_err(),
            "{label}: a genuine failure is a construction error"
        );
        assert_eq!(rig.backend.token_posts().len(), 1, "{label}: nothing sent");
    }
}

// ---------------------------------------------------------------------------
// B2 fix round, P3b.4 (CL-14, R-P3b-12): an equal-generation successor is proved
// by the exact row this attempt wrote and its own receipt
// ---------------------------------------------------------------------------

/// The access token an unrotated answer carries: a token the unit can load again.
fn unrotated_access() -> String {
    jg_exp(T0 + 40_000)
}

/// A rig whose due refresh was answered without a new refresh token, so the
/// successor has the generation of its input, and whose write committed and then
/// reported an error: the successor is pending, and the store already holds it.
async fn committed_unrotated_write() -> (Rig, LegacyAuthManager, uuid::Uuid) {
    let rig = new_rig().await;
    rig.seed_due();
    rig.backend.script(vec![
        Reply::json(
            200,
            &format!(
                r#"{{"access_token":"{}","expires_in":28800,"token_type":"Bearer"}}"#,
                unrotated_access()
            ),
        ),
        Reply::json(
            200,
            &format!(
                r#"{{"access_token":"{}","expires_in":28800,"token_type":"Bearer"}}"#,
                jg_exp(T0 + 60_000)
            ),
        ),
    ]);
    let mut manager = rig.manager();
    let input = manager.generation();
    plant(&manager, &[PersistFault::Ambiguous]);
    rig.refresh(&mut manager).await.expect("the attempt");
    assert_eq!(row_of(&rig).0, "persistence_pending");
    assert_eq!(
        rig.stored_pair().0,
        unrotated_access(),
        "the write had committed"
    );
    assert_eq!(
        rig.stored_generation(),
        input,
        "an answer that keeps the refresh token keeps the generation"
    );
    assert_eq!(rig.backend.token_posts().len(), 1);
    (rig, manager, input)
}

/// Store a pair that an external writer put there, bound to `generation` when one
/// is given (the unbound form is what the setup and import paths write).
fn external_replacement(rig: &Rig, access: &str, refresh: &str, schedule: (i64, i64), bound: bool) {
    let key = crate::teslamate_credentials::load_key(rig.directory.path()).expect("key");
    let tokens = OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned())
        .expect("replacement tokens");
    let (access, refresh_bytes) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
            .expect("encrypt");
    let stored = crate::db::TeslaMateLegacyTokenStore::refreshed(
        access,
        refresh_bytes,
        schedule.0,
        schedule.1,
    )
    .expect("replacement row");
    let stored = if bound {
        stored
            .with_credential_generation(
                crate::teslamate_token::legacy_refresh_credential_generation(&tokens),
            )
            .expect("generation")
    } else {
        stored
    };
    rig.store
        .replace_teslamate_legacy_tokens(&stored)
        .expect("the external write is accepted");
}

#[tokio::test]
async fn nc_unpadded_ambiguous_commit_settles_on_its_receipt() {
    let (rig, mut manager, _) = committed_unrotated_write().await;
    // Before the retry is due nothing changes.
    rig.at(T0 + 4);
    manager.service();
    assert_eq!(row_of(&rig).0, "persistence_pending");
    // At +5 seconds the row is the one this attempt wrote and the attempt's own
    // receipt is closed as a success: persistence is done, with no second write.
    rig.at(T0 + 5);
    manager.service();
    assert_eq!(
        row_of(&rig).0,
        "ready",
        "the retry recognised its own write"
    );
    assert_eq!(manager.write_log.lock().unwrap().as_slice(), [T0]);
    assert_eq!(rig.backend.token_posts().len(), 1);
    assert_eq!(
        rig.receipts(),
        [("success".to_owned(), Some(200), None, false)]
    );
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    assert_eq!(manager.access_token(), unrotated_access());
    assert_eq!(manager.generation(), rig.stored_generation());
    // The next renewal is allowed and sends one request.
    rig.at(T0 + 21_600);
    rig.refresh(&mut manager).await.expect("the next renewal");
    assert_eq!(rig.backend.token_posts().len(), 2);
    assert_eq!(rig.stored_pair().0, jg_exp(T0 + 60_000));
}

#[tokio::test]
async fn nc_external_replacement_after_ambiguous_commit_wins() {
    // A pair of another generation, written after a rotating answer's ambiguous
    // commit: the generation alone tells it from the successor.
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    plant(&manager, &[PersistFault::Ambiguous]);
    rig.refresh(&mut manager).await.expect("the attempt");
    assert_eq!(
        rig.stored_pair().1,
        "refresh-1",
        "the rotated write committed"
    );
    let external = jg_exp(T0 + 45_000);
    external_replacement(
        &rig,
        &external,
        "refresh-9",
        (T0 + 40_000, T0 + 30_000),
        false,
    );
    rig.at(T0 + 5);
    manager.service();
    assert_eq!(
        manager.write_log.lock().unwrap().as_slice(),
        [T0],
        "no second write"
    );
    assert_eq!(rig.stored_pair().0, external);
    assert_eq!(
        manager.access_token(),
        external,
        "the external pair is adopted"
    );
    assert!(manager.auth.pending_successor.is_none());

    // Equal generations: an external replacement that kept the refresh token is told
    // from this attempt's row by its image (the access token, the schedule and the
    // ciphertext), however much it looks like it. Each is stored after the
    // unrotated successor's write committed.
    let successor_access = unrotated_access();
    let replacements: [(&str, String, &str, (i64, i64), bool); 4] = [
        (
            "a new access token and schedule, bound",
            jg_exp(T0 + 41_000),
            "refresh-0",
            (T0 + 30_000, T0 + 25_000),
            true,
        ),
        // The successor's own tokens and schedule, encrypted again: only the
        // ciphertext differs.
        (
            "the same tokens and schedule, bound",
            successor_access.clone(),
            "refresh-0",
            (T0 + 28_800, T0 + 21_600),
            true,
        ),
        (
            "a new access token, unbound",
            jg_exp(T0 + 42_000),
            "refresh-0",
            (T0 + 30_000, T0 + 25_000),
            false,
        ),
        (
            "the same access token, a later schedule, bound",
            successor_access,
            "refresh-0",
            (T0 + 50_000, T0 + 40_000),
            true,
        ),
    ];
    for (label, access, refresh, schedule, bound) in replacements {
        let (rig, mut manager, _) = committed_unrotated_write().await;
        external_replacement(&rig, &access, refresh, schedule, bound);
        let replaced = rig.stored_pair();
        rig.at(T0 + 5);
        manager.service();
        assert_eq!(
            manager.write_log.lock().unwrap().as_slice(),
            [T0],
            "{label}: no second write"
        );
        assert_eq!(
            rig.stored_pair(),
            replaced,
            "{label}: the external pair is preserved"
        );
        assert_eq!(manager.access_token(), access, "{label}: and adopted");
        assert!(
            manager.auth.pending_successor.is_none(),
            "{label}: the pending successor is dropped"
        );
        assert_eq!(rig.backend.token_posts().len(), 1, "{label}");
        assert_eq!(row_of(&rig).0, "ready", "{label}");
    }
}

#[tokio::test]
async fn nc_removal_after_ambiguous_commit_is_not_success_proof() {
    let (rig, mut manager, _) = committed_unrotated_write().await;
    rig.store.clear_teslamate_legacy_tokens().expect("sign out");
    rig.at(T0 + 5);
    manager.service();
    // The row is gone: the closed receipt is no proof, nothing is written back, the
    // held successor is not adopted and the write is not reported as done.
    assert!(
        rig.store.load_teslamate_legacy_tokens().unwrap().is_none(),
        "the removed pair is not resurrected"
    );
    assert_ne!(row_of(&rig).0, "ready");
    assert_eq!(row_of(&rig).0, "persistence_pending");
    assert!(
        manager.auth.pending_successor.is_some(),
        "still pending, never done"
    );
    assert_eq!(rig.backend.token_posts().len(), 1);
}

// ---------------------------------------------------------------------------
// B2 fix round, P3b.5: the Legacy shutdown returns the unit's sensitive outcome
// ---------------------------------------------------------------------------

/// Start an exchange that the double holds back, drop its caller as a shutdown that
/// drops the collection future does, and return the manager behind a lock with a
/// switch that denies its sensitive access.
async fn detached_exchange_held_by(
    rig: &Rig,
    reply: Reply,
) -> (
    Arc<tokio::sync::Mutex<LegacyAuthManager>>,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
) {
    rig.backend.script(vec![reply]);
    let mut manager = rig.manager();
    let denied = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&denied);
    manager.sensitive_access = Arc::new(move || {
        if switch.load(Ordering::SeqCst) {
            Err(CredentialError::SensitiveAccessUnavailable)
        } else {
            Ok(())
        }
    });
    let shutdown_flag = manager.shutdown_flag();
    let manager = Arc::new(tokio::sync::Mutex::new(manager));
    let client = rig.client();
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(rig, 1).await;
    caller.abort();
    let _ = caller.await;
    (manager, denied, shutdown_flag)
}

#[tokio::test]
async fn b2_credential_shutdown_returns_a_sensitive_failure_from_a_detached_exchange() {
    let rig = new_rig().await;
    rig.seed_due();
    let gate = Arc::new(tokio::sync::Notify::new());
    let (manager, denied, shutdown_flag) =
        detached_exchange_held_by(&rig, Reply::reset().held_by(&gate)).await;
    // The shutdown waits for the sent exchange. The admission is denied while it
    // waits, and the unknown answer is then released.
    let status_attempts_before = manager.lock().await.status_write_attempts().len();
    let credential_rows_before = (rig.row(), rig.receipts(), rig.stored_pair());
    let shutting_down = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.lock().await.shutdown().await })
    };
    for _ in 0..500 {
        if shutdown_flag.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(shutdown_flag.load(Ordering::Acquire), "the shutdown began");
    assert!(!shutting_down.is_finished(), "and waits for the exchange");
    denied.store(true, Ordering::SeqCst);
    gate.notify_one();
    let outcome = shutting_down.await.expect("the shutdown task");
    assert!(
        matches!(
            outcome,
            Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            ))
        ),
        "the unit's own sensitive outcome, as it was: {outcome:?}"
    );
    assert_eq!(rig.backend.token_posts().len(), 1, "no second request");
    assert!(rig.store.has_unresolved_legacy_refresh().unwrap());
    // After a sensitive failure the shutdown keeps what it learned in memory and
    // journals it, and mutates no store (R-P3bF-2): no status write attempt, not even
    // a failed one, and no change of the account row, a receipt or the pair.
    assert_eq!(
        manager.lock().await.status_write_attempts().len(),
        status_attempts_before,
        "no status write was attempted after the sensitive failure"
    );
    assert_eq!(
        (rig.row(), rig.receipts(), rig.stored_pair()),
        credential_rows_before,
        "the account row, the receipts and the pair are as they were"
    );

    // The same shutdown with the admission intact: an ordinary unknown outcome is the
    // unit's state and no sensitive result; a successful exchange is a success.
    let rig = new_rig().await;
    rig.seed_due();
    let gate = Arc::new(tokio::sync::Notify::new());
    let (manager, _denied, shutdown_flag) =
        detached_exchange_held_by(&rig, Reply::reset().held_by(&gate)).await;
    let shutting_down = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.lock().await.shutdown().await })
    };
    for _ in 0..500 {
        if shutdown_flag.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    gate.notify_one();
    shutting_down
        .await
        .expect("the shutdown task")
        .expect("an unknown outcome that is no loss of the admission is no sensitive result");

    let rig = new_rig().await;
    rig.seed_due();
    let gate = Arc::new(tokio::sync::Notify::new());
    let (manager, _denied, shutdown_flag) =
        detached_exchange_held_by(&rig, Reply::json(200, SUCCESS).held_by(&gate)).await;
    let shutting_down = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.lock().await.shutdown().await })
    };
    for _ in 0..500 {
        if shutdown_flag.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    gate.notify_one();
    shutting_down
        .await
        .expect("the shutdown task")
        .expect("the allowed exchange is a success");
    assert_eq!(rig.stored_pair().0, "AT1", "and its successor is stored");
    assert_eq!(rig.backend.token_posts().len(), 1);
}

// ---------------------------------------------------------------------------
// B2 fix round, third P3 round (R-P3F3-1, R-P3F3-2): the status over a refusal the
// unit holds for the start to decide on
// ---------------------------------------------------------------------------

/// A refusal of another mode in the store, as an earlier run of that mode left it.
fn refusal_of_another_mode(rig: &Rig) {
    rig.store
        .write_collector_auth_row(
            &crate::db::CollectorAuthRow {
                state: "api_refused".to_owned(),
                reason: Some("owner_api".to_owned()),
                collecting: false,
                retry_at: None,
                stopped_generation: None,
                stopped_mode: Some("provider".to_owned()),
                fuse_consecutive: 0,
                fuse_generation: None,
                fuse_last_rejection_at: None,
                fuse_pause_until: None,
                fuse_rejected_access_digest: None,
                clock_skew: false,
            },
            T0 * 1_000,
        )
        .expect("the other mode's refusal");
}

#[tokio::test]
async fn p3f3_1_a_due_status_retry_writes_only_the_newest_status_over_a_retained_refusal() {
    // The unit holds a refusal of another mode for the start to decide on, so every
    // status it writes carries that refusal with its own fuse and clock-skew flag. A
    // write of that row fails; at its retry deadline the fuse (or the flag) has
    // changed again, and the store is back. The retry writes the newest row once: no
    // attempt carries a fuse or a flag that has been superseded, and none carries a
    // state other than the refusal.
    for change in ["a fuse counted again", "the clock-skew flag"] {
        let rig = new_rig().await;
        rig.seed_due();
        refusal_of_another_mode(&rig);
        let mut manager = rig.manager();
        assert!(
            manager.retained_refusal.is_some(),
            "{change}: the refusal is retained"
        );
        let generation = manager.generation();
        let fuse_of = |consecutive: u8| FuseSnapshot {
            consecutive,
            generation: Some(generation),
            last_rejection_at: Some(T0),
            pause_until: None,
            rejected_access: None,
        };
        // The first change cannot be written: the store refuses.
        manager.fail_status_writes(true);
        manager.fuse = fuse_of(1);
        rig.at(T0 + 10);
        manager.service();
        let first = manager.status_write_attempts();
        assert_eq!(
            first.len(),
            1,
            "{change}: one attempt, which failed: {first:?}"
        );
        assert_eq!(first[0].fuse_consecutive, 1);
        // At the retry deadline (five seconds on) the news has moved on, and the store
        // is back.
        manager.fail_status_writes(false);
        match change {
            "a fuse counted again" => manager.fuse = fuse_of(2),
            _ => manager.auth.clock_skew = true,
        }
        rig.at(T0 + 15);
        manager.service();
        let attempts = manager.status_write_attempts();
        let late = &attempts[first.len()..];
        assert_eq!(
            late.len(),
            1,
            "{change}: the retry writes the newest status once: {late:?}"
        );
        for row in &attempts {
            assert_eq!(
                (
                    row.state.as_str(),
                    row.reason.as_deref(),
                    row.stopped_mode.as_deref()
                ),
                ("api_refused", Some("owner_api"), Some("provider")),
                "{change}: no row but the retained refusal is written"
            );
        }
        let written = &late[0];
        match change {
            "a fuse counted again" => assert_eq!(written.fuse_consecutive, 2, "{change}"),
            _ => {
                assert!(written.clock_skew, "{change}");
                assert_eq!(written.fuse_consecutive, 1, "{change}");
            }
        }
        assert_eq!(
            rig.row(),
            *written,
            "{change}: the store holds the newest row"
        );
        // The shutdown follows the same order and adds nothing superseded.
        manager.shutdown().await.expect("an orderly shutdown");
        assert_eq!(rig.row(), *written, "{change}: after the shutdown");
        assert!(
            manager
                .status_write_attempts()
                .iter()
                .all(|row| row.state == "api_refused"),
            "{change}"
        );
    }
}

#[tokio::test]
async fn p3f3_2_an_own_stop_ends_the_retention_of_a_refusal_for_good() {
    // A refusal of another mode is retained; the unit then stops by its own news (a
    // dead refresh token), which replaces the row and ends the retention. A new
    // login lifts that stop, and the unit is ready, collects, and writes nothing
    // of the old refusal back: not when serviced, not at the shutdown, not when
    // rebuilt.
    let rig = new_rig().await;
    rig.seed_due();
    refusal_of_another_mode(&rig);
    rig.backend
        .script(vec![Reply::json(400, r#"{"error":"invalid_grant"}"#)]);
    let mut manager = rig.manager();
    assert!(manager.retained_refusal.is_some());
    let _ = rig.refresh(&mut manager).await;
    assert_eq!(
        rig.state(),
        (
            "sign_in_required".into(),
            Some("refresh_token_rejected".into())
        ),
        "the unit's own stop replaced the other mode's row"
    );
    // The owner signs in again.
    rig.seed(&jg(), "refresh-9", Some((T0 + 28_800, T0 + 21_600)));
    rig.at(T0 + 200);
    let _ = rig.refresh(&mut manager).await;
    assert_eq!(rig.state().0, "ready", "the new login lifted the stop");
    rig.list(&mut manager).await.expect("products");
    assert_eq!(rig.backend.products().len(), 1, "collection goes on");
    // Nothing writes the old refusal back.
    for step in 1..=3 {
        rig.at(T0 + 200 + step * 30);
        manager.service();
        assert_eq!(rig.state().0, "ready", "after servicing {step}");
    }
    manager.shutdown().await.expect("an orderly shutdown");
    assert_eq!(rig.state().0, "ready", "after the shutdown");
    assert!(
        manager
            .status_write_attempts()
            .iter()
            .skip(1)
            .all(|row| row.state != "api_refused"),
        "no write of the refusal after the own stop"
    );
    let rebuilt = rig.manager();
    assert_ne!(rig.state().0, "api_refused", "after reconstruction");
    assert!(rebuilt.retained_refusal.is_none());
}

// ---------------------------------------------------------------------------
// B2 fix round, P3b fix 1 (R-P3bF-1, R-P3bF-2): the irreversible loss fence reaches the
// unit's persistence, status and receipt writes
// ---------------------------------------------------------------------------

/// A switch the unit's loss fence reads: set, it never clears.
fn loss_fence_for(manager: &mut LegacyAuthManager) -> Arc<AtomicBool> {
    let lost = Arc::new(AtomicBool::new(false));
    let observer = Arc::clone(&lost);
    manager.share_loss_fence(Arc::new(move || observer.load(Ordering::SeqCst)));
    lost
}

#[tokio::test]
async fn p3bf1_the_loss_fence_stops_every_write_of_the_unit_while_the_admission_passes() {
    // The answer is held at the endpoint, the fence is set (the supervisor observed
    // the loss) and the admission, which passes throughout, cannot help. A valid answer
    // is not persisted; a complete error answer does not close its receipt; no status
    // is written; the unit hands out no token; and its shutdown returns the sensitive
    // failure. The receipt stays started and the pair is the one it was.
    let answers: [(&str, fn(&Arc<tokio::sync::Notify>) -> Reply); 2] = [
        ("a valid rotated answer", |gate| {
            Reply::json(200, SUCCESS).held_by(gate)
        }),
        ("a complete error answer", |gate| {
            Reply::json(503, "{}").held_by(gate)
        }),
    ];
    for (label, reply) in answers {
        let rig = new_rig().await;
        rig.seed_due();
        let gate = Arc::new(tokio::sync::Notify::new());
        rig.backend.script(vec![reply(&gate)]);
        let mut manager = rig.manager();
        let lost = loss_fence_for(&mut manager);
        let client = rig.client();
        let manager = Arc::new(tokio::sync::Mutex::new(manager));
        let caller = {
            let manager = Arc::clone(&manager);
            tokio::spawn(async move {
                manager
                    .lock()
                    .await
                    .refresh_if_due(&client, SystemTime::now())
                    .await
            })
        };
        seen_posts(&rig, 1).await;
        caller.abort();
        let _ = caller.await;
        let before = (rig.row(), rig.receipts(), rig.stored_pair());
        let attempts_before = manager.lock().await.status_write_attempts().len();
        lost.store(true, Ordering::SeqCst);
        gate.notify_one();
        // The detached exchange finishes without writing anything.
        for _ in 0..500 {
            if !manager.lock().await.flight.is_running() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !manager.lock().await.flight.is_running(),
            "{label}: the exchange ended"
        );
        assert_eq!(
            rig.stored_pair(),
            before.2,
            "{label}: the successor was not written"
        );
        assert_eq!(
            rig.receipts(),
            before.1,
            "{label}: the receipt was not closed"
        );
        // The unit applies the outcome in memory and writes no status over it.
        manager.lock().await.service();
        assert_eq!(
            manager.lock().await.status_write_attempts().len(),
            attempts_before,
            "{label}: no status write was attempted after the fence"
        );
        assert!(
            manager
                .lock()
                .await
                .access_token_for_sensitive_use()
                .is_err_and(|error| error.is_sensitive_access_failure()),
            "{label}: no token is handed out after the fence"
        );
        let outcome = manager.lock().await.shutdown().await;
        assert!(
            outcome
                .as_ref()
                .is_err_and(LegacyAuthManagerError::is_sensitive_access_failure),
            "{label}: the shutdown returns the sensitive failure: {outcome:?}"
        );
        assert_eq!(
            manager.lock().await.status_write_attempts().len(),
            attempts_before,
            "{label}: nor did the shutdown"
        );
        assert_eq!(
            (rig.row(), rig.receipts(), rig.stored_pair()),
            before,
            "{label}: the account row, the receipts and the pair are as they were"
        );
        assert!(
            rig.store.has_unresolved_legacy_refresh().unwrap(),
            "{label}"
        );
        assert_eq!(
            rig.backend.token_posts().len(),
            1,
            "{label}: no second request"
        );
    }
}

#[tokio::test]
async fn p3bf1_the_loss_fence_refuses_a_reservation_and_a_cancel() {
    // The unit's ports are store writes too: with the fence set a reservation is
    // refused and a reserved attempt is not cancelled, so its receipt stays started.
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    let lost = loss_fence_for(&mut manager);
    let reserved = manager
        .ports()
        .begin()
        .expect("a reservation before the fence");
    let receipts = rig.receipts();
    lost.store(true, Ordering::SeqCst);
    assert!(
        matches!(manager.ports().begin(), Err(BeginFailure::Store)),
        "no reservation after the fence"
    );
    assert!(
        manager.ports().cancel_unsent(&reserved).is_err(),
        "no cancel after the fence"
    );
    assert_eq!(rig.receipts(), receipts, "the receipt is still started");
}

#[tokio::test]
async fn p3bf1_the_loss_fence_refuses_the_adoption_of_a_stored_pair() {
    // An external writer's pair is not adopted once the fence is set: adopting binds
    // its generation, a store write.
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    let lost = loss_fence_for(&mut manager);
    external_replacement(
        &rig,
        &jg_exp(T0 + 45_000),
        "refresh-9",
        (T0 + 40_000, T0 + 30_000),
        false,
    );
    let stored_before = rig.store.stored_legacy_credential_generation().unwrap();
    assert_eq!(stored_before, crate::db::StoredLegacyGeneration::Unbound);
    lost.store(true, Ordering::SeqCst);
    let adopted = manager.adopt_stored_pair(true);
    assert!(
        adopted
            .as_ref()
            .is_err_and(LegacyAuthManagerError::is_sensitive_access_failure),
        "{adopted:?}"
    );
    assert_eq!(
        rig.store.stored_legacy_credential_generation().unwrap(),
        stored_before,
        "the stored pair was not bound"
    );
}

#[tokio::test]
async fn p3bf2_a_sensitive_outcome_stops_the_shutdown_writes_though_the_admission_is_back() {
    // The denial is met once, by the exchange that ended unknown while it was out; the
    // admission passes at every other check, the shutdown's included. The outcome is
    // sensitive nonetheless, and the shutdown that returns it writes no status, closes
    // no receipt and attempts no final write (R-P3bF-2).
    let rig = new_rig().await;
    rig.seed_due();
    let gate = Arc::new(tokio::sync::Notify::new());
    rig.backend.script(vec![Reply::reset().held_by(&gate)]);
    let mut manager = rig.manager();
    // A denial that is met once: the first check after the switch is set is denied and
    // clears it. While the exchange is held the unit makes no check, so that check is
    // the one the exchange makes when its answer turns out unknown.
    let denial = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&denial);
    manager.sensitive_access = Arc::new(move || {
        if switch.swap(false, Ordering::SeqCst) {
            Err(CredentialError::SensitiveAccessUnavailable)
        } else {
            Ok(())
        }
    });
    let shutdown_flag = manager.shutdown_flag();
    let client = rig.client();
    let manager = Arc::new(tokio::sync::Mutex::new(manager));
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(&rig, 1).await;
    caller.abort();
    let _ = caller.await;
    let attempts_before = manager.lock().await.status_write_attempts().len();
    let before = (rig.row(), rig.receipts(), rig.stored_pair());
    let shutting_down = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.lock().await.shutdown().await })
    };
    for _ in 0..500 {
        if shutdown_flag.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    denial.store(true, Ordering::SeqCst);
    gate.notify_one();
    let outcome = shutting_down.await.expect("the shutdown task");
    assert!(
        matches!(
            outcome,
            Err(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            ))
        ),
        "{outcome:?}"
    );
    assert!(!denial.load(Ordering::SeqCst), "the denial was met, once");
    assert_eq!(
        manager.lock().await.status_write_attempts().len(),
        attempts_before,
        "no status write attempt"
    );
    assert_eq!((rig.row(), rig.receipts(), rig.stored_pair()), before);
}

#[tokio::test]
async fn p3bf2_a_shutdown_under_a_standing_denial_writes_nothing() {
    // The admission is denied while the answer is held and stays denied: the answer is
    // a valid one, which the guard keeps from the store. The shutdown applies it in
    // memory and writes no status, closes no receipt, makes no final attempt, and
    // returns the failure of the admission (R-P3bF-1, R-P3bF-2).
    let rig = new_rig().await;
    rig.seed_due();
    let gate = Arc::new(tokio::sync::Notify::new());
    let (manager, denied, shutdown_flag) =
        detached_exchange_held_by(&rig, Reply::json(200, SUCCESS).held_by(&gate)).await;
    let attempts_before = manager.lock().await.status_write_attempts().len();
    let before = (rig.row(), rig.receipts(), rig.stored_pair());
    let shutting_down = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move { manager.lock().await.shutdown().await })
    };
    for _ in 0..500 {
        if shutdown_flag.load(Ordering::Acquire) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    denied.store(true, Ordering::SeqCst);
    gate.notify_one();
    let outcome = shutting_down.await.expect("the shutdown task");
    assert!(
        outcome
            .as_ref()
            .is_err_and(LegacyAuthManagerError::is_sensitive_access_failure),
        "{outcome:?}"
    );
    assert_eq!(
        manager.lock().await.status_write_attempts().len(),
        attempts_before,
        "no status write attempt"
    );
    assert_eq!((rig.row(), rig.receipts(), rig.stored_pair()), before);
}

// ---------------------------------------------------------------------------
// B2 fix round, P3b fix 2 (R-P3bF2-1, R-P3bF2-2): a sensitive outcome is latched wherever
// the exchange's result is applied, and the guard's lost admission stops the writes
// ---------------------------------------------------------------------------

/// A detached exchange whose answer is unknown (the connection fails after the status
/// line) and whose admission check is denied once, at the moment the exchange meets its
/// unknown answer; the admission is back at every other check. The caller is gone and
/// the answer is released: the exchange has ended, and nothing has applied its result.
/// Returns the unit, the credential rows, the status write attempts so far and the
/// client.
async fn p3bf2_unknown_exchange_denied_once(
    rig: &Rig,
) -> (
    Arc<tokio::sync::Mutex<LegacyAuthManager>>,
    (
        crate::db::CollectorAuthRow,
        Vec<(String, Option<i64>, Option<i64>, bool)>,
        (String, String, i64, i64),
    ),
    usize,
) {
    let gate = Arc::new(tokio::sync::Notify::new());
    rig.backend.script(vec![Reply::reset().held_by(&gate)]);
    let mut manager = rig.manager();
    let denial = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&denial);
    manager.sensitive_access = Arc::new(move || {
        if switch.swap(false, Ordering::SeqCst) {
            Err(CredentialError::SensitiveAccessUnavailable)
        } else {
            Ok(())
        }
    });
    let client = rig.client();
    let manager = Arc::new(tokio::sync::Mutex::new(manager));
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(rig, 1).await;
    caller.abort();
    let _ = caller.await;
    let before = (rig.row(), rig.receipts(), rig.stored_pair());
    let attempts_before = manager.lock().await.status_write_attempts().len();
    denial.store(true, Ordering::SeqCst);
    gate.notify_one();
    for _ in 0..500 {
        if !manager.lock().await.flight.is_running() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !manager.lock().await.flight.is_running(),
        "the exchange ended"
    );
    assert!(!denial.load(Ordering::SeqCst), "the denial was met, once");
    (manager, before, attempts_before)
}

#[tokio::test]
async fn p3bf2_a_sensitive_outcome_met_by_the_service_or_a_refresh_is_kept_for_the_shutdown() {
    // R-P3bF2-2 (Codex): the first thing to apply the finished exchange's result is not
    // the shutdown but the service between requests, or the next refresh. The outcome is
    // sensitive nonetheless: it latches before any later mutation, so no status is
    // written, no receipt closed and no token handed out, and the shutdown that follows
    // returns the original sensitive result. The admission passes again throughout.
    for consumer in ["the service", "a refresh"] {
        let rig = new_rig().await;
        rig.seed_due();
        let (manager, before, attempts_before) = p3bf2_unknown_exchange_denied_once(&rig).await;
        match consumer {
            "the service" => manager.lock().await.service(),
            _ => {
                let outcome = manager
                    .lock()
                    .await
                    .refresh_if_due(&rig.client(), SystemTime::now())
                    .await;
                assert!(
                    matches!(
                        outcome,
                        Err(LegacyAuthManagerError::Auth(
                            LegacyAuthError::SensitiveRotationOutcomeUnknown
                        ))
                    ),
                    "{consumer}: the refresh answers with the kept outcome: {outcome:?}"
                );
            }
        }
        assert!(
            matches!(
                manager.lock().await.sensitive_outcome(),
                Some(LegacyAuthManagerError::Auth(
                    LegacyAuthError::SensitiveRotationOutcomeUnknown
                ))
            ),
            "{consumer}: the unit kept the outcome"
        );
        assert_eq!(
            manager.lock().await.status_write_attempts().len(),
            attempts_before,
            "{consumer}: no status write was attempted"
        );
        assert_eq!(
            (rig.row(), rig.receipts(), rig.stored_pair()),
            before,
            "{consumer}: the account row, the receipts and the pair are as they were"
        );
        assert!(
            manager
                .lock()
                .await
                .access_token_for_sensitive_use()
                .is_err_and(|error| error.is_sensitive_access_failure()),
            "{consumer}: no token is handed out"
        );
        let outcome = manager.lock().await.shutdown().await;
        assert!(
            matches!(
                outcome,
                Err(LegacyAuthManagerError::Auth(
                    LegacyAuthError::SensitiveRotationOutcomeUnknown
                ))
            ),
            "{consumer}: the shutdown returns the original sensitive result: {outcome:?}"
        );
        assert_eq!(
            manager.lock().await.status_write_attempts().len(),
            attempts_before,
            "{consumer}: nor did the shutdown write a status"
        );
        assert_eq!(
            (rig.row(), rig.receipts(), rig.stored_pair()),
            before,
            "{consumer}: after the shutdown"
        );
        assert_eq!(
            rig.backend.token_posts().len(),
            1,
            "{consumer}: no second request"
        );
    }
}

#[tokio::test]
async fn p3bf3_the_first_sensitive_outcome_is_kept_when_a_second_one_is_applied() {
    // B2 fix round, P3b fix 3 (Opus N2): two sensitive results applied directly. The
    // first (an exchange that ended unknown while the admission was denied) is the unit's
    // outcome; the second (an adoption refused for the lost access) is sensitive too, is
    // returned to its caller as such, and does not replace the first.
    let rig = new_rig().await;
    rig.seed_due();
    let (manager, _before, _attempts) = p3bf2_unknown_exchange_denied_once(&rig).await;
    let mut manager = manager.lock().await;
    manager.service();
    assert!(
        matches!(
            manager.sensitive_outcome(),
            Some(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            ))
        ),
        "the first outcome is kept"
    );
    let second = manager.apply_flight_result(ExchangeResult {
        kind: ExchangeKind::External,
        input: uuid::Uuid::nil(),
        sent_at: None,
        finished_at: 0,
        server_date: None,
        pending_close: None,
    });
    assert!(
        second
            .as_ref()
            .is_some_and(|error| error.is_sensitive_access_failure()
                && !matches!(
                    error,
                    LegacyAuthManagerError::Auth(LegacyAuthError::SensitiveRotationOutcomeUnknown)
                )),
        "the second result is another sensitive outcome: {second:?}"
    );
    assert!(
        matches!(
            manager.sensitive_outcome(),
            Some(LegacyAuthManagerError::Auth(
                LegacyAuthError::SensitiveRotationOutcomeUnknown
            ))
        ),
        "the first outcome is still the one returned: {:?}",
        manager.sensitive_outcome()
    );
}

#[tokio::test]
async fn p3bf2_a_clean_refusal_before_the_send_is_not_a_sensitive_outcome() {
    // The control: the guard's refusal before anything was sent (R17) is a wait that
    // retries, not a sensitive outcome. The unit's admission check is denied once, at its
    // `n`th look; for every `n` at which that is before the request goes out, the
    // reservation is cancelled, nothing is latched or kept, and once the admission is
    // back the unit refreshes and persists as it always did.
    let mut refused_before_the_send = 0;
    for nth in 1..=4_usize {
        let rig = new_rig().await;
        rig.seed_due();
        rig.backend.script(vec![Reply::json(200, SUCCESS)]);
        let mut manager = rig.manager();
        let looks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&looks);
        manager.sensitive_access = Arc::new(move || {
            if counter.fetch_add(1, Ordering::SeqCst) + 1 == nth {
                Err(CredentialError::SensitiveAccessUnavailable)
            } else {
                Ok(())
            }
        });
        let first = rig.refresh(&mut manager).await;
        if rig.backend.token_posts().is_empty() {
            refused_before_the_send += 1;
            assert!(first.is_err(), "look {nth}: {first:?}");
            assert!(
                manager.sensitive_outcome().is_none(),
                "look {nth}: a refusal before the send is no sensitive outcome"
            );
            // The admission is back and the retry is due: the unit refreshes and persists.
            rig.at(T0 + 600);
            rig.refresh(&mut manager)
                .await
                .unwrap_or_else(|error| panic!("look {nth}: the retry: {error:?}"));
            assert_eq!(rig.backend.token_posts().len(), 1, "look {nth}");
            assert_eq!(rig.stored_pair().1, "refresh-1", "look {nth}");
            assert!(manager.sensitive_outcome().is_none(), "look {nth}");
        }
    }
    assert!(
        refused_before_the_send >= 1,
        "a denial before the send was met"
    );
}

/// A guard as the supervised run makes it: it refuses once its fence is set, whatever
/// the admission says now, and otherwise when the admission is denied, setting the fence
/// as it does (`RunWriteGuard::refused`). Returns the observer and the two switches.
fn guard_like_observer(manager: &mut LegacyAuthManager) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
    let fence = Arc::new(AtomicBool::new(false));
    let denied = Arc::new(AtomicBool::new(false));
    let (fence_seen, denied_seen) = (Arc::clone(&fence), Arc::clone(&denied));
    manager.share_loss_fence(Arc::new(move || {
        if fence_seen.load(Ordering::SeqCst) {
            return true;
        }
        if denied_seen.load(Ordering::SeqCst) {
            fence_seen.store(true, Ordering::SeqCst);
            return true;
        }
        false
    }));
    (fence, denied)
}

#[tokio::test]
async fn p3bf2_a_lost_admission_with_the_fence_clear_stops_the_closing_the_status_and_the_adoption()
{
    // R-P3bF2-1 (Opus F1): the unit's gate is the run's guard, which revalidates the
    // admission as well as reading the fence. The fence is clear and the unit's own
    // access check passes (the unit does not rely on it): only the guard says the
    // admission is gone. A complete error answer does not close its receipt, no status
    // is written, and an external pair is not adopted (its generation is not bound).
    let rig = new_rig().await;
    rig.seed_due();
    let gate = Arc::new(tokio::sync::Notify::new());
    rig.backend
        .script(vec![Reply::json(503, "{}").held_by(&gate)]);
    let mut manager = rig.manager();
    let (fence, denied) = guard_like_observer(&mut manager);
    let client = rig.client();
    let manager = Arc::new(tokio::sync::Mutex::new(manager));
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(&rig, 1).await;
    caller.abort();
    let _ = caller.await;
    let before = (rig.row(), rig.receipts(), rig.stored_pair());
    let attempts_before = manager.lock().await.status_write_attempts().len();
    assert!(!fence.load(Ordering::SeqCst));
    denied.store(true, Ordering::SeqCst);
    gate.notify_one();
    for _ in 0..500 {
        if !manager.lock().await.flight.is_running() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !manager.lock().await.flight.is_running(),
        "the exchange ended"
    );
    manager.lock().await.service();
    assert!(
        fence.load(Ordering::SeqCst),
        "the guard met the lost admission and set its fence"
    );
    assert_eq!(
        manager.lock().await.status_write_attempts().len(),
        attempts_before,
        "no status write was attempted"
    );
    assert_eq!(
        (rig.row(), rig.receipts(), rig.stored_pair()),
        before,
        "the receipt is still started, the row and the pair are as they were"
    );
    assert!(rig.store.has_unresolved_legacy_refresh().unwrap());
    assert_eq!(rig.backend.token_posts().len(), 1, "no second request");

    // The adoption of an external pair, on a unit whose guard says the same.
    let rig = new_rig().await;
    rig.seed_due();
    let mut manager = rig.manager();
    let (fence, denied) = guard_like_observer(&mut manager);
    external_replacement(
        &rig,
        &jg_exp(T0 + 45_000),
        "refresh-9",
        (T0 + 40_000, T0 + 30_000),
        false,
    );
    let stored_before = rig.store.stored_legacy_credential_generation().unwrap();
    assert_eq!(stored_before, crate::db::StoredLegacyGeneration::Unbound);
    denied.store(true, Ordering::SeqCst);
    let adopted = manager.adopt_stored_pair(true);
    assert!(
        adopted
            .as_ref()
            .is_err_and(LegacyAuthManagerError::is_sensitive_access_failure),
        "{adopted:?}"
    );
    assert!(fence.load(Ordering::SeqCst));
    assert_eq!(
        rig.store.stored_legacy_credential_generation().unwrap(),
        stored_before,
        "the stored pair was not bound"
    );
}

// ---------------------------------------------------------------------------
// B2 fix round, P3b fix 1 (R-P3bF-3, R-P3bF-7): the write witness and the receipt proof
// ---------------------------------------------------------------------------

/// Make the receipts unreadable: the proof's snapshot and the closing of a receipt
/// both fail until they are restored.
fn hide_the_receipts(rig: &Rig) {
    rig.store
        .open()
        .expect("catalogue")
        .execute_batch("ALTER TABLE outbound_request_receipts RENAME TO p3bf1_hidden_receipts")
        .expect("hide the receipts");
}

fn restore_the_receipts(rig: &Rig) {
    rig.store
        .open()
        .expect("catalogue")
        .execute_batch("ALTER TABLE p3bf1_hidden_receipts RENAME TO outbound_request_receipts")
        .expect("restore the receipts");
}

#[tokio::test]
async fn nc_a_failed_retry_keeps_the_witness_so_a_recovered_reread_recognises_the_write() {
    // The write committed and reported an error. The next snapshot cannot be read
    // (R-P3bF-7 b: the read-error fallback leaves the write to be tried again), the
    // retried write is refused by the store, and a later reread recovers. The row is
    // still the one this attempt wrote, so it is its own write: the fuse and the state
    // the unit holds are not taken for an external credential write's reset.
    let (rig, mut manager, _) = committed_unrotated_write().await;
    let generation = manager.generation();
    let fuse = FuseSnapshot {
        consecutive: 1,
        generation: Some(generation),
        last_rejection_at: Some(T0),
        pause_until: None,
        rejected_access: None,
    };
    manager.fuse = fuse;
    let stored = rig.stored_pair();
    let attempt = manager
        .auth
        .pending_successor
        .as_ref()
        .expect("the pending successor")
        .attempt;
    hide_the_receipts(&rig);
    assert_eq!(
        manager.ports().reread(&attempt, generation),
        PersistReread::StillInput,
        "a snapshot that cannot be read leaves the write to be tried again"
    );
    // The retry writes again, and the store refuses it.
    rig.at(T0 + 5);
    manager.service();
    assert_eq!(
        manager.write_log.lock().unwrap().as_slice(),
        [T0, T0 + 5],
        "the write was tried again"
    );
    assert_eq!(
        rig.stored_pair(),
        stored,
        "and refused: the row is unchanged"
    );
    assert!(manager.auth.pending_successor.is_some(), "still pending");
    assert_eq!(manager.fuse, fuse);
    // The receipts are readable again: the next retry finds its own write.
    restore_the_receipts(&rig);
    rig.at(T0 + 600);
    manager.service();
    assert_eq!(
        manager.write_log.lock().unwrap().as_slice(),
        [T0, T0 + 5],
        "no third write"
    );
    assert_eq!(rig.stored_pair(), stored);
    assert!(
        manager.auth.pending_successor.is_none(),
        "done on its own receipt"
    );
    assert_eq!(row_of(&rig).0, "ready");
    assert_eq!(manager.fuse, fuse, "the fuse is not reset");
    assert!(!manager.fuse_reset_pending, "no external write was assumed");
    assert_eq!(
        rig.receipts(),
        [("success".to_owned(), Some(200), None, false)]
    );
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
}

#[tokio::test]
async fn nc_another_attempts_success_receipt_is_not_this_attempts_proof() {
    // Attempt A wrote an equal-generation successor and keeps its witness. Attempt B,
    // a later refresh, is refused its write by the guard and never reaches the store,
    // so the witness stays A's. When the guard passes again B's retry must not take A's
    // row and A's closed receipt for its own proof: it writes, and its own receipt
    // closes.
    let rig = new_rig().await;
    rig.seed_due();
    let gate = Arc::new(tokio::sync::Notify::new());
    rig.backend.script(vec![
        Reply::json(
            200,
            &format!(
                r#"{{"access_token":"{}","expires_in":28800,"token_type":"Bearer"}}"#,
                unrotated_access()
            ),
        ),
        Reply::json(
            200,
            &format!(
                r#"{{"access_token":"{}","expires_in":28800,"token_type":"Bearer"}}"#,
                jg_exp(T0 + 60_000)
            ),
        )
        .held_by(&gate),
    ]);
    let mut manager = rig.manager();
    let denied = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&denied);
    manager.sensitive_access = Arc::new(move || {
        if switch.load(Ordering::SeqCst) {
            Err(CredentialError::SensitiveAccessUnavailable)
        } else {
            Ok(())
        }
    });
    // Attempt A: written and closed.
    rig.refresh(&mut manager).await.expect("attempt A");
    assert_eq!(rig.stored_pair().0, unrotated_access());
    assert_eq!(manager.write_log.lock().unwrap().as_slice(), [T0]);
    // Attempt B: its answer is held, the guard is then denied, the answer released.
    rig.at(T0 + 21_600);
    let client = rig.client();
    let manager = Arc::new(tokio::sync::Mutex::new(manager));
    let caller = {
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            manager
                .lock()
                .await
                .refresh_if_due(&client, SystemTime::now())
                .await
        })
    };
    seen_posts(&rig, 2).await;
    denied.store(true, Ordering::SeqCst);
    gate.notify_one();
    let _ = caller.await;
    assert_eq!(
        manager.lock().await.write_log.lock().unwrap().as_slice(),
        [T0],
        "B's write was refused by the guard before the store"
    );
    assert!(manager.lock().await.auth.pending_successor.is_some());
    denied.store(false, Ordering::SeqCst);
    rig.at(T0 + 21_605);
    manager.lock().await.service();
    assert_eq!(
        manager.lock().await.write_log.lock().unwrap().as_slice(),
        [T0, T0 + 21_605],
        "B's own write was made: A's row and receipt are no proof of it"
    );
    assert_eq!(rig.stored_pair().0, jg_exp(T0 + 60_000));
    assert_eq!(
        rig.receipts(),
        [
            ("success".to_owned(), Some(200), None, false),
            ("success".to_owned(), Some(200), None, false)
        ],
        "B's receipt closed as a success"
    );
    assert!(!rig.store.has_unresolved_legacy_refresh().unwrap());
    assert!(manager.lock().await.auth.pending_successor.is_none());
}
