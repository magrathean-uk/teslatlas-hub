// SPDX-License-Identifier: AGPL-3.0-only

//! What an attempt means for the in-memory authority (C5 R14 to R16, R19, R23,
//! R24, R27, R29, R29a, R31, R32, R43, R45): the gates, the due rule, the rows
//! of the outcome table as state changes, and the status derived from them.
//! Exact inputs, no store, no network; the same rows end to end are tested with
//! the manager in `auth/credentials/refresh_tests.rs`.

use serde_json::json;
use uuid::Uuid;

use super::super::{
    attempt::{self, BeginFailure, ExchangeKind, ExchangeResult, SuccessorPair},
    authority::{Applied, Block, stop_error},
    schedule::{self, Schedule},
    state::{AuthState, AuthStop, FuseSnapshot, StatusReason, StoppedMode},
    wire::{AcceptedKind, AttemptClass},
};
use super::*;
use super::{T0, jg};

fn tesla() -> AuthEndpointConfig {
    AuthEndpointConfig {
        mode: AuthMode::Tesla,
        region: None,
        token_url: None,
        allow_loopback_http: false,
    }
}

/// The stored pair of the spec fixtures: `(JG, refresh-0)` in refreshed form.
fn stored() -> LegacyAuth {
    LegacyAuth::from_persisted_state(jg(), "refresh-0", 1_800_028_800, 1_800_021_600, &tesla())
        .expect("fixture pair")
}

fn imported() -> LegacyAuth {
    LegacyAuth::from_persisted_state(jg(), "refresh-0", 0, 0, &tesla()).expect("imported pair")
}

fn generation() -> Uuid {
    Uuid::from_bytes([7; 16])
}

fn result(kind: ExchangeKind, finished_at: i64) -> ExchangeResult {
    ExchangeResult {
        kind,
        input: generation(),
        sent_at: Some(finished_at),
        finished_at,
        server_date: None,
        pending_close: None,
    }
}

fn failed(class: AttemptClass, status: Option<u16>, retry_after: Option<i64>) -> ExchangeKind {
    let (error, status) = match class {
        AttemptClass::Unknown => (LegacyAuthError::RotationOutcomeUnknown, None),
        AttemptClass::ConsumedWithoutSuccessor => (LegacyAuthError::InvalidResponse, Some(200)),
        AttemptClass::Dead => (LegacyAuthError::RefreshTokenRejected, status),
        AttemptClass::ClientRefused => (LegacyAuthError::ClientRefused, status),
        AttemptClass::Refused => (
            LegacyAuthError::OwnerApiRefused {
                reason: RefusalReason::OwnerApi,
            },
            status,
        ),
        AttemptClass::NotRenewed => (LegacyAuthError::RefreshNotRenewed, status),
        _ => (LegacyAuthError::HttpStatus(status.unwrap_or(500)), status),
    };
    ExchangeKind::Failed {
        class,
        error,
        status,
        retry_after,
    }
}

fn successor(schedule: Schedule, sent_at: i64) -> Box<SuccessorPair> {
    Box::new(SuccessorPair {
        access_token: Zeroizing::new("AT1".to_owned()),
        refresh_token: Zeroizing::new("refresh-1".to_owned()),
        kind: AcceptedKind::Rotated,
        schedule,
        sent_at,
        input: generation(),
        output: Uuid::from_bytes([8; 16]),
        attempt: attempt::Attempt {
            receipt: None,
            input: generation(),
        },
    })
}

fn applied_error(auth: &mut LegacyAuth, kind: ExchangeKind, at: i64) -> Option<LegacyAuthError> {
    match auth.apply_result(result(kind, at), 0.0) {
        Applied::Done(error) => error,
        Applied::Adopt { .. } => panic!("unexpected adoption"),
    }
}

fn status(auth: &LegacyAuth, now: i64, in_flight: bool) -> (AuthState, Option<StatusReason>, bool) {
    let status = auth.derive_status(
        now,
        generation(),
        StoppedMode::Legacy,
        in_flight,
        FuseSnapshot::default(),
    );
    assert!(status.is_consistent(), "{status:?} breaks R52");
    (status.state, status.reason, status.collecting)
}

#[test]
fn the_due_rule_is_the_startup_refresh_a_retry_and_the_schedule() {
    // The refreshed form is due at its next refresh time (test 23).
    let auth = stored();
    assert!(
        !auth
            .refresh_due(UNIX_EPOCH + Duration::from_secs(1_800_021_599))
            .unwrap()
    );
    assert!(
        auth.refresh_due(UNIX_EPOCH + Duration::from_secs(1_800_021_600))
            .unwrap()
    );
    // An imported pair is owed one refresh until one succeeds (R26).
    assert!(imported().is_due(T0));
    // A pair loaded for one use (native setup) is not (R26).
    let one_use = LegacyAuth::from_access_token(jg(), "refresh-0", &tesla()).unwrap();
    assert!(!one_use.is_due(T0));
    // A retry is due once its time is reached; the gate holds it until then.
    let mut retrying = stored();
    retrying.schedule_retry(RetryReason::Network, schedule::RetryPlan::Backoff, T0, 0.0);
    assert!(retrying.is_due(T0));
    assert_eq!(retrying.retry_at, Some(T0 + 60));
}

#[test]
fn t25_a_stored_schedule_that_breaks_the_row_invariants_loads_as_the_imported_form() {
    // expires_at 100 and next_refresh_at 200 cannot be in the table (its CHECK
    // refuses them) and can reach the constructor only from another caller.
    let auth =
        LegacyAuth::from_persisted_state(jg(), "refresh-0", 100, 200, &tesla()).expect("loads");
    assert_eq!(
        auth.schedule_diagnostic(),
        Some(&LegacyAuthError::InvalidPersistedSchedule)
    );
    assert_eq!((auth.expires_at(), auth.next_refresh_at()), (0, 0));
    // A refresh is owed on the first need, as for any imported pair (R26).
    assert!(auth.startup_refresh_pending);
    assert!(auth.is_due(T0));
    // Usability follows the token's own exp (R25): t0 + 28,500.
    assert_eq!(auth.usable_until(), Some(T0 + 28_500));
}

#[test]
fn r16_every_gate_blocks_a_token_request_and_names_when_it_opens() {
    let mut auth = stored();
    assert_eq!(auth.gate(T0, false), None);
    assert_eq!(auth.gate(T0, true), Some(Block::InFlight));

    // R31: a retry pending.
    auth.retry_at = Some(T0 + 60);
    auth.retry_reason = Some(RetryReason::Network);
    assert_eq!(auth.gate(T0, false), Some(Block::Until(T0 + 60)));
    assert_eq!(auth.gate(T0 + 60, false), None);
    auth.clear_retry();

    // R19: 60 s after the last success, in this process.
    auth.last_success_at = Some(T0);
    assert_eq!(auth.gate(T0 + 59, false), Some(Block::Until(T0 + 60)));
    assert_eq!(auth.gate(T0 + 60, false), None);

    // R58: the restart floor.
    auth.last_success_at = None;
    auth.token_requests_not_before = T0 + 40;
    assert_eq!(auth.gate(T0, false), Some(Block::Until(T0 + 40)));

    // The gates combine: a request waits for the last of them.
    auth.last_success_at = Some(T0);
    auth.retry_at = Some(T0 + 90);
    assert_eq!(auth.gate(T0, false), Some(Block::Until(T0 + 90)));
    auth.retry_at = None;
    auth.last_success_at = None;
    auth.token_requests_not_before = 0;

    // R41: the confirmation pause.
    auth.hold_until = Some(T0 + 300);
    assert_eq!(auth.gate(T0 + 299, false), Some(Block::Until(T0 + 300)));
    assert_eq!(auth.gate(T0 + 300, false), None);
    auth.hold_until = None;

    // R38: a clock below the floor.
    assert_eq!(auth.gate(1_500_000_000, false), Some(Block::ClockInvalid));

    // R24: a successor not yet persisted.
    auth.hold_successor(successor(Schedule::after_send(T0, None, "AT1"), T0), T0);
    assert_eq!(auth.gate(T0, false), Some(Block::SuccessorPending));
    auth.pending_successor = None;

    // R43: a stop.
    auth.set_stop(AuthStop::SignInRequired(
        SignOutReason::RefreshTokenRejected,
    ));
    assert_eq!(auth.gate(T0, false), Some(Block::Stopped));
}

#[test]
fn r8a_an_access_token_only_pair_has_no_refresh_path() {
    let config = AuthEndpointConfig {
        mode: AuthMode::Provider,
        region: None,
        token_url: None,
        allow_loopback_http: false,
    };
    let auth = LegacyAuth::from_persisted_state("opaque-access-0001", "", 0, 0, &config).unwrap();
    assert_eq!(auth.gate(T0, false), Some(Block::NoRefreshPath));
    assert!(!auth.startup_refresh_pending);
}

#[test]
fn r29_the_token_is_available_until_usable_until_and_then_not() {
    let auth = stored();
    // `usable_until` = min(expires_at, exp - 300) = t0 + 28,500.
    assert_eq!(auth.usable_until(), Some(T0 + 28_500));
    assert_eq!(auth.token_available(T0 + 28_499), Ok(()));
    assert_eq!(
        auth.token_available(T0 + 28_500),
        Err(LegacyAuthError::AuthUnavailable { retry_at: None })
    );
    // The retry time is what a waiting request is told.
    let mut waiting = stored();
    waiting.schedule_retry(RetryReason::Network, schedule::RetryPlan::Backoff, T0, 0.0);
    assert_eq!(
        waiting.token_available(T0 + 28_500),
        Err(LegacyAuthError::AuthUnavailable {
            retry_at: Some(T0 + 60)
        })
    );
    // R38: no token for a clock below the floor.
    assert_eq!(
        auth.token_available(1_500_000_000),
        Err(LegacyAuthError::AuthUnavailable {
            retry_at: Some(1_500_000_060)
        })
    );
    // An unknown bound is usable for ever.
    let opaque = LegacyAuth::from_persisted_state("AT1", "refresh-0", 0, 0, &{
        let mut config = tesla();
        config.region = Some(StreamRegion::Global);
        config
    })
    .unwrap();
    assert_eq!(opaque.usable_until(), None);
    assert_eq!(opaque.token_available(T0 + 10_000_000), Ok(()));
}

#[test]
fn r45_a_sign_in_stop_collects_until_the_token_expires_and_then_signs_out() {
    let mut auth = stored();
    auth.set_stop(AuthStop::SignInRequired(
        SignOutReason::RefreshOutcomeUnknown,
    ));
    assert_eq!(auth.token_available(T0 + 7_200), Ok(()));
    assert_eq!(
        status(&auth, T0 + 7_200, false),
        (
            AuthState::SignInRequired,
            Some(StatusReason::SignOut(SignOutReason::RefreshOutcomeUnknown)),
            true
        )
    );
    // Past `usable_until` the same reason is a sign-out and nothing collects.
    assert_eq!(
        auth.effective_stop(T0 + 28_500),
        Some(AuthStop::SignedOut(SignOutReason::RefreshOutcomeUnknown))
    );
    assert_eq!(
        status(&auth, T0 + 28_500, false),
        (
            AuthState::SignedOut,
            Some(StatusReason::SignOut(SignOutReason::RefreshOutcomeUnknown)),
            false
        )
    );
    assert_eq!(
        auth.token_available(T0 + 28_500),
        Err(LegacyAuthError::SignedOut {
            reason: SignOutReason::RefreshOutcomeUnknown
        })
    );
    auth.settle_stop(T0 + 28_500);
    assert_eq!(
        auth.stop,
        Some(AuthStop::SignedOut(SignOutReason::RefreshOutcomeUnknown))
    );
}

#[test]
fn the_errors_of_the_stops_are_the_variants_of_the_vocabulary() {
    let signed_out = |reason| stop_error(AuthStop::SignedOut(reason));
    assert_eq!(
        signed_out(SignOutReason::AccessTokenRejected),
        LegacyAuthError::AccessTokenRejected
    );
    assert_eq!(
        signed_out(SignOutReason::ProviderTokenRejected),
        LegacyAuthError::ProviderTokenRejected
    );
    assert_eq!(
        signed_out(SignOutReason::ProviderTokenExpired),
        LegacyAuthError::ProviderTokenExpired
    );
    assert_eq!(
        signed_out(SignOutReason::RefreshTokenRejected),
        LegacyAuthError::SignedOut {
            reason: SignOutReason::RefreshTokenRejected
        }
    );
    assert_eq!(
        stop_error(AuthStop::ApiRefused(RefusalReason::Client)),
        LegacyAuthError::OwnerApiRefused {
            reason: RefusalReason::Client
        }
    );
    // Every one of them stops a request, and the display text carries no secret.
    for reason in [
        SignOutReason::RefreshTokenRejected,
        SignOutReason::RefreshOutcomeUnknown,
        SignOutReason::RefreshResponseUnusable,
        SignOutReason::AccessTokenRejected,
        SignOutReason::StoredPairInvalid,
        SignOutReason::ProviderTokenRejected,
        SignOutReason::ProviderTokenExpired,
    ] {
        let error = stop_error(AuthStop::SignedOut(reason));
        assert!(error.is_terminal_auth_failure(), "{error:?}");
        assert!(!error.to_string().contains("refresh-0"));
    }
}

/// R45: the retryable rows as retry state, with the exact times of R31 and R32.
#[test]
fn r45_the_retryable_rows_set_the_reason_and_the_time_of_the_next_attempt() {
    let cases: [(&str, ExchangeKind, RetryReason, i64); 8] = [
        (
            "row 1, network",
            ExchangeKind::NotSent {
                reason: RetryReason::Network,
            },
            RetryReason::Network,
            60,
        ),
        (
            "row 1, guard",
            ExchangeKind::NotSent {
                reason: RetryReason::SensitiveAccess,
            },
            RetryReason::SensitiveAccess,
            60,
        ),
        (
            "row 9",
            failed(AttemptClass::RequestRejected, Some(400), None),
            RetryReason::RefreshRequestRejected,
            3_600,
        ),
        (
            "row 11",
            failed(AttemptClass::Misrouted, Some(404), None),
            RetryReason::TokenEndpointMisrouted,
            3_600,
        ),
        (
            "row 12",
            failed(AttemptClass::Blocked, Some(403), None),
            RetryReason::RefreshBlocked,
            60,
        ),
        (
            "row 13",
            failed(AttemptClass::Transient, Some(503), None),
            RetryReason::TeslaUnavailable,
            60,
        ),
        (
            "row 14",
            failed(AttemptClass::NotRenewed, Some(200), None),
            RetryReason::RefreshNotRenewed,
            60,
        ),
        (
            "row 7, first answer",
            failed(AttemptClass::ClientRefused, Some(400), None),
            RetryReason::ClientCheck,
            60,
        ),
    ];
    for (name, kind, reason, delay) in cases {
        let mut auth = stored();
        let error = applied_error(&mut auth, kind, T0);
        assert!(error.is_some(), "{name}");
        assert_eq!(auth.retry_at, Some(T0 + delay), "{name}");
        assert_eq!(auth.retry_reason, Some(reason), "{name}");
        assert_eq!(auth.stop, None, "{name}");
        assert_eq!(
            status(&auth, T0, false),
            (AuthState::Retrying, Some(StatusReason::Retry(reason)), true),
            "{name}"
        );
        // Collection goes on while the token is usable (R30) and stops with it.
        assert_eq!(auth.token_available(T0 + 100), Ok(()), "{name}");
        assert!(!status(&auth, T0 + 28_500, false).2, "{name}");
    }
}

#[test]
fn t45_repeated_transient_answers_back_off_to_the_cap() {
    let mut auth = stored();
    let mut at = T0;
    let mut times = vec![at];
    for _ in 0..5 {
        applied_error(
            &mut auth,
            failed(AttemptClass::Transient, Some(503), None),
            at,
        );
        at = auth.retry_at.unwrap();
        times.push(at);
    }
    assert_eq!(
        times,
        [T0, T0 + 60, T0 + 180, T0 + 420, T0 + 720, T0 + 1_020]
    );
}

#[test]
fn t46_retry_after_counts_on_a_429_and_a_503_only() {
    for (status_code, retry_after, delay) in [
        (429, Some(7_200), 3_600),
        (429, Some(5), 60),
        (429, Some(900), 900),
        (503, Some(900), 900),
        (500, Some(900), 60),
        (408, Some(900), 60),
        (429, None, 60),
    ] {
        let mut auth = stored();
        applied_error(
            &mut auth,
            failed(AttemptClass::Transient, Some(status_code), retry_after),
            T0,
        );
        assert_eq!(
            auth.retry_at,
            Some(T0 + delay),
            "{status_code} {retry_after:?}"
        );
    }
}

#[test]
fn t55_the_second_consecutive_client_refusal_is_a_stop_and_any_other_answer_resets_the_count() {
    let mut auth = stored();
    applied_error(
        &mut auth,
        failed(AttemptClass::ClientRefused, Some(400), None),
        T0,
    );
    assert_eq!(auth.client_refusals, 1);
    let error = applied_error(
        &mut auth,
        failed(AttemptClass::ClientRefused, Some(400), None),
        T0 + 60,
    );
    assert_eq!(
        error,
        Some(LegacyAuthError::OwnerApiRefused {
            reason: RefusalReason::Client
        })
    );
    assert_eq!(auth.stop, Some(AuthStop::ApiRefused(RefusalReason::Client)));
    assert_eq!(auth.retry_at, None, "a stop ends the retry");
    assert_eq!(
        status(&auth, T0 + 60, false),
        (
            AuthState::ApiRefused,
            Some(StatusReason::Refusal(RefusalReason::Client)),
            false
        )
    );

    // A 503 between the two answers returns the count to zero.
    let mut auth = stored();
    applied_error(
        &mut auth,
        failed(AttemptClass::ClientRefused, Some(400), None),
        T0,
    );
    applied_error(
        &mut auth,
        failed(AttemptClass::Transient, Some(503), None),
        T0 + 60,
    );
    assert_eq!(auth.client_refusals, 0);
    applied_error(
        &mut auth,
        failed(AttemptClass::ClientRefused, Some(400), None),
        T0 + 180,
    );
    assert_eq!(auth.client_refusals, 1);
    assert_eq!(auth.stop, None);
}

#[test]
fn r45_the_rows_that_stop_set_the_stop_the_table_names() {
    let cases = [
        (
            "row 8",
            failed(AttemptClass::Dead, Some(400), None),
            AuthStop::SignInRequired(SignOutReason::RefreshTokenRejected),
        ),
        (
            "row 4",
            failed(AttemptClass::ConsumedWithoutSuccessor, Some(200), None),
            AuthStop::SignInRequired(SignOutReason::RefreshResponseUnusable),
        ),
        (
            "row 6",
            failed(AttemptClass::Unknown, None, None),
            AuthStop::SignInRequired(SignOutReason::RefreshOutcomeUnknown),
        ),
        (
            "row 10",
            failed(AttemptClass::Refused, Some(403), None),
            AuthStop::ApiRefused(RefusalReason::OwnerApi),
        ),
    ];
    for (name, kind, stop) in cases {
        let mut auth = stored();
        applied_error(&mut auth, kind, T0);
        assert_eq!(auth.stop, Some(stop), "{name}");
        assert_eq!(auth.retry_at, None, "{name}");
        // No token request follows, in any of them.
        assert_eq!(
            auth.gate(T0 + 100_000, false),
            Some(Block::Stopped),
            "{name}"
        );
    }
}

#[test]
fn a_begin_that_found_the_row_changed_asks_for_the_stored_pair_not_a_sign_out() {
    for (failure, external) in [
        (BeginFailure::ExternalWrite, true),
        (BeginFailure::OwnOutput, false),
    ] {
        let mut auth = stored();
        let applied = auth.apply_result(result(ExchangeKind::RowChanged(failure), T0), 0.0);
        assert!(matches!(applied, Applied::Adopt { external: e } if e == external));
        assert_eq!(auth.stop, None);
    }
    let mut auth = stored();
    assert!(matches!(
        auth.apply_result(result(ExchangeKind::External, T0), 0.0),
        Applied::Adopt { external: true }
    ));
    // An unresolved attempt is a sign-out (R57 step 3); a store that cannot
    // answer is a retry.
    let mut auth = stored();
    let error = applied_error(
        &mut auth,
        ExchangeKind::RowChanged(BeginFailure::Unresolved),
        T0,
    );
    assert_eq!(
        error,
        Some(LegacyAuthError::SignedOut {
            reason: SignOutReason::RefreshOutcomeUnknown
        })
    );
    assert_eq!(
        auth.stop,
        Some(AuthStop::SignedOut(SignOutReason::RefreshOutcomeUnknown))
    );
    // C5-S6: a store that cannot answer is a wait with its own reason, and the
    // error is the wait every request understands. It is not a loss of the
    // sensitive access and it is not terminal, so the collector's fence stays down.
    let mut auth = stored();
    let error = applied_error(&mut auth, ExchangeKind::RowChanged(BeginFailure::Store), T0);
    assert_eq!(auth.retry_reason, Some(RetryReason::StoreUnavailable));
    assert_eq!(auth.retry_at, Some(T0 + 60));
    assert_eq!(
        error,
        Some(LegacyAuthError::AuthUnavailable {
            retry_at: Some(T0 + 60)
        })
    );
    assert!(!error.expect("an error").is_terminal_auth_failure());
    assert_eq!(auth.stop, None);
}

#[test]
fn r23_a_persisted_successor_becomes_the_pair_and_clears_every_retry_field() {
    let mut auth = stored();
    auth.client_refusals = 1;
    auth.retry_step = 2;
    auth.retry_at = Some(T0 + 10);
    auth.retry_reason = Some(RetryReason::Network);
    let schedule = Schedule::after_send(T0, Some(&json!(28_800)), "AT1");
    applied_error(
        &mut auth,
        ExchangeKind::Persisted(successor(schedule, T0)),
        T0,
    );
    assert_eq!(auth.access_token(), "AT1");
    assert_eq!(auth.refresh_token(), "refresh-1");
    assert_eq!(auth.expires_at(), 1_800_028_800);
    assert_eq!(auth.next_refresh_at(), 1_800_021_600);
    assert_eq!(auth.usable_until(), Some(1_800_028_800));
    assert_eq!(auth.last_success_at, Some(T0));
    assert_eq!(
        (
            auth.retry_at,
            auth.retry_reason,
            auth.retry_step,
            auth.client_refusals
        ),
        (None, None, 0, 0)
    );
    assert!(!auth.startup_refresh_pending);
    assert_eq!(status(&auth, T0, false).0, AuthState::Ready);
    // R19: nothing within 60 s of the send time.
    assert_eq!(auth.gate(T0 + 30, false), Some(Block::Until(T0 + 60)));
}

#[test]
fn r29a_a_short_successor_is_retrying_with_its_reason_and_the_step_is_kept() {
    let mut auth = imported();
    let short = Schedule::after_send(T0, Some(&json!(30)), "AT1");
    applied_error(&mut auth, ExchangeKind::Persisted(successor(short, T0)), T0);
    assert_eq!(auth.retry_reason, Some(RetryReason::ShortTokenLifetime));
    assert_eq!(auth.retry_at, Some(T0 + 60));
    assert!(!auth.startup_refresh_pending);
    // A second short successor waits longer, so a provider that keeps issuing
    // them is rotated at most every 300 s (R29a).
    let short = Schedule::after_send(T0 + 60, Some(&json!(30)), "AT2");
    applied_error(
        &mut auth,
        ExchangeKind::Persisted(successor(short, T0 + 60)),
        T0 + 60,
    );
    assert_eq!(auth.retry_at, Some(T0 + 60 + 120));
    // With the clock known to be wrong the reason says so (R55).
    let mut skewed = imported();
    skewed.clock_skew = true;
    let short = Schedule::after_send(T0, Some(&json!(30)), "AT1");
    applied_error(
        &mut skewed,
        ExchangeKind::Persisted(successor(short, T0)),
        T0,
    );
    assert_eq!(skewed.retry_reason, Some(RetryReason::ClockSkew));
}

#[test]
fn r24_a_pending_successor_keeps_the_old_pair_and_retries_on_the_schedule() {
    let mut auth = stored();
    let schedule = Schedule::after_send(T0, Some(&json!(28_800)), "AT1");
    applied_error(
        &mut auth,
        ExchangeKind::Pending {
            successor: successor(schedule, T0),
            error: LegacyAuthError::Persistence,
        },
        T0,
    );
    assert_eq!(auth.access_token(), jg(), "the previous pair stays in use");
    assert!(auth.pending_successor.is_some());
    assert_eq!(auth.persist_retry_at, Some(T0 + 5));
    assert!(!auth.persist_retry_due(T0 + 4));
    assert!(auth.persist_retry_due(T0 + 5));
    // 5, 10, 20, 40, 80, 160, then 300 for ever.
    let mut at = T0 + 5;
    let mut gaps = Vec::new();
    for _ in 0..8 {
        auth.note_persist_failure(at);
        let next = auth.persist_retry_at.unwrap();
        gaps.push(next - at);
        at = next;
    }
    assert_eq!(gaps, [10, 20, 40, 80, 160, 300, 300, 300]);
    // The status is `persistence_pending`, collecting while the old token lives.
    assert_eq!(
        status(&auth, T0, false),
        (AuthState::PersistencePending, None, true)
    );
    assert!(!status(&auth, T0 + 28_500, false).2);
}

#[test]
fn r52_the_status_follows_the_priority_stop_clock_pending_refreshing_retrying_ready() {
    let mut auth = stored();
    assert_eq!(status(&auth, T0, false), (AuthState::Ready, None, true));
    assert_eq!(status(&auth, T0, true), (AuthState::Refreshing, None, true));
    // R38: the clock beats everything but a stop.
    assert_eq!(
        status(&auth, 1_500_000_000, true),
        (AuthState::ClockInvalid, None, false)
    );
    auth.schedule_retry(RetryReason::Network, schedule::RetryPlan::Backoff, T0, 0.0);
    assert_eq!(status(&auth, T0, false).0, AuthState::Retrying);
    assert_eq!(status(&auth, T0, true).0, AuthState::Refreshing);
    auth.set_stop(AuthStop::ApiRefused(RefusalReason::OwnerApi));
    assert_eq!(status(&auth, 1_500_000_000, true).0, AuthState::ApiRefused);
    // R43: a refusal stores the mode, not the generation.
    let row = auth
        .derive_status(
            T0,
            generation(),
            StoppedMode::Provider,
            false,
            FuseSnapshot::default(),
        )
        .to_row();
    assert_eq!(row.stopped_mode.as_deref(), Some("provider"));
    assert_eq!(row.stopped_generation, None);
}

#[test]
fn r55_the_date_header_sets_and_clears_the_skew_flag() {
    let mut auth = stored();
    let mut answer = result(failed(AttemptClass::Transient, Some(503), None), T0);
    // 2027-01-15T08:10:00Z is 600 s after t0.
    answer.server_date = Some("Fri, 15 Jan 2027 08:10:00 GMT".to_owned());
    let _ = auth.apply_result(answer, 0.0);
    assert!(auth.clock_skew);
    let mut answer = result(failed(AttemptClass::Transient, Some(503), None), T0 + 60);
    answer.server_date = Some("Fri, 15 Jan 2027 08:01:10 GMT".to_owned());
    let _ = auth.apply_result(answer, 0.0);
    assert!(!auth.clock_skew);
}

#[test]
fn r53_nothing_secret_is_shown_by_the_debug_text_of_the_authority_or_a_successor() {
    let mut auth = stored();
    let schedule = Schedule::after_send(T0, Some(&json!(28_800)), "AT1");
    auth.hold_successor(successor(schedule, T0), T0);
    auth.set_stop(AuthStop::SignInRequired(
        SignOutReason::RefreshTokenRejected,
    ));
    let rendered = format!(
        "{auth:?} {:?} {}",
        auth.pending_successor,
        stop_error(AuthStop::SignedOut(SignOutReason::StoredPairInvalid))
    );
    for secret in ["AT1", "refresh-0", "refresh-1", "c2ln"] {
        assert!(!rendered.contains(secret), "{secret} appears in {rendered}");
    }
    assert!(!rendered.contains(&jg()));
}
