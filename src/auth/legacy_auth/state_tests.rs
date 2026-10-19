// SPDX-License-Identifier: AGPL-3.0-only

//! C5 part P2 spec tests for the durable status, the clock floor, skew,
//! restart and the pair row's relation to the unit's own writes (spec tests
//! 64 (a), 65, 66, 67, 68, 72, 85, 87, 94 (c) and (d); the readiness and
//! diagnostics halves of 66, 72 and 89 are in `db/tests/auth_state.rs` and
//! `runtime/diagnostics.rs`). Synthetic data only; every store is a temporary
//! one and nothing here uses the network. The send-side halves of these tests
//! ("no POST", "no Owner API request") belong to parts P3 and P4; here the
//! same fact is the restored status that stops them (`collecting` and the
//! stop's `blocks_requests`) and the floor that gates them.

use std::cell::Cell;

use uuid::Uuid;

use super::super::http_date::parse_http_date;
use super::super::state::*;
use super::{AuthMode, ConfigurationReason, RefusalReason, RetryReason, SignOutReason, T0};
use crate::db::{CollectorAuthRow, HubStore, StoreError, TeslaMateLegacyTokenStore};

/// A synthetic generation: sixteen equal bytes, never derived from a token.
fn gen_id(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

fn test_store() -> (tempfile::TempDir, HubStore) {
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    (temporary, store)
}

/// Store a pair row whose ciphertext is a placeholder and whose generation is
/// `generation`, through the path every external writer uses (K4).
fn put_pair(store: &HubStore, generation: Uuid) {
    let stored = TeslaMateLegacyTokenStore::imported(vec![generation.as_bytes()[0]; 8], vec![7; 8])
        .expect("placeholder pair")
        .with_credential_generation(generation)
        .expect("generation");
    store
        .replace_teslamate_legacy_tokens(&stored)
        .expect("store pair");
}

/// A refreshed-form pair for the output of a refresh (K2).
fn refreshed_pair(generation: Uuid) -> TeslaMateLegacyTokenStore {
    TeslaMateLegacyTokenStore::refreshed(
        vec![generation.as_bytes()[0]; 8],
        vec![8; 8],
        T0 + 28_800,
        T0 + 21_600,
    )
    .expect("refreshed pair")
    .with_credential_generation(generation)
    .expect("generation")
}

/// Insert a token-request receipt with a chosen start time (synthetic rows, so
/// the test controls the time).
fn put_receipt(store: &HubStore, started_ms: i64, outcome: &str, transport: &str, operation: &str) {
    let connection = store.open().expect("connection");
    let (completed, duration, status): (Option<i64>, Option<i64>, Option<i64>) =
        if outcome == "started" {
            (None, None, None)
        } else {
            (Some(started_ms + 40), Some(40), Some(200))
        };
    connection
        .execute(
            "INSERT INTO outbound_request_receipts(
                correlation_id, started_at_ms, completed_at_ms, duration_ms,
                vehicle_tesla_id, transport, operation, safety_class, precondition,
                outcome, http_status)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, 'non_wake_endpoint', 'not_required', ?7, ?8)",
            rusqlite::params![
                Uuid::new_v4().to_string(),
                started_ms,
                completed,
                duration,
                transport,
                operation,
                outcome,
                status
            ],
        )
        .expect("receipt");
}

fn inputs(now: i64, generation: Uuid) -> StartInputs {
    StartInputs {
        now,
        mode: AuthMode::Tesla,
        observer: false,
        pair: PairLoad::Loaded {
            generation,
            usable_until: Some(T0 + 28_500),
        },
        configuration: Ok(()),
        c3_reset_refusal: false,
    }
}

fn facts() -> StartFacts {
    StartFacts {
        unresolved_attempt: false,
        stored: None,
        current_is_refresh_output: false,
        last_receipt_started_at_ms: None,
    }
}

fn fuse(consecutive: u8, generation: Uuid, last: i64, pause: Option<i64>) -> FuseSnapshot {
    FuseSnapshot {
        consecutive,
        generation: Some(generation),
        last_rejection_at: Some(last),
        pause_until: pause,
        rejected_access: None,
    }
}

fn signed_out(reason: SignOutReason, generation: Uuid, fuse: FuseSnapshot) -> AuthStatus {
    AuthStatus::stopped(
        AuthStop::SignedOut(reason),
        generation,
        StoppedMode::Legacy,
        false,
        fuse,
        false,
    )
}

fn refused(reason: RefusalReason, mode: StoppedMode) -> AuthStatus {
    AuthStatus::stopped(
        AuthStop::ApiRefused(reason),
        gen_id(200),
        mode,
        false,
        FuseSnapshot::default(),
        false,
    )
}

fn ready() -> AuthStatus {
    AuthStatus::ready(FuseSnapshot::default(), false)
}

fn with_stored(status: AuthStatus) -> StartFacts {
    StartFacts {
        stored: Some(status.to_row()),
        ..facts()
    }
}

/// An IMF-fixdate for an epoch second, written independently of the parser
/// under test: the civil date from the days count (Hinnant's inverse
/// algorithm), the names from tables.
fn imf_fixdate(epoch: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = epoch.div_euclid(86_400);
    let seconds = epoch.rem_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[usize::try_from(days.rem_euclid(7)).unwrap()],
        day,
        MONTHS[usize::try_from(month - 1).unwrap()],
        year,
        seconds / 3_600,
        seconds % 3_600 / 60,
        seconds % 60
    )
}

// ---------------------------------------------------------------------------
// Vocabulary and the stored shape (R52, section 3.3)
// ---------------------------------------------------------------------------

#[test]
fn every_state_and_reason_code_round_trips_and_follows_the_table_of_r52() {
    for state in AuthState::ALL {
        assert_eq!(AuthState::from_code(state.code()), Some(state));
    }
    assert_eq!(AuthState::from_code("nonsense"), None);
    let retry = [
        "network",
        "sensitive_access",
        "tesla_unavailable",
        "refresh_blocked",
        "refresh_request_rejected",
        "token_endpoint_misrouted",
        "client_check",
        "short_token_lifetime",
        "clock_skew",
        "refresh_not_renewed",
    ];
    for code in retry {
        let reason = StatusReason::parse(AuthState::Retrying, code).expect(code);
        assert_eq!(reason.code(), code);
        assert!(AuthState::Retrying.admits(Some(reason)));
        assert!(!AuthState::Ready.admits(Some(reason)));
    }
    let table: [(AuthState, &[&str]); 7] = [
        (AuthState::AwaitingConfirmation, &["access_token_rejected"]),
        (
            AuthState::ObserverWaiting,
            &["token_rejected", "token_expired", "clock_skew"],
        ),
        (
            AuthState::ConfigurationError,
            &[
                "region_conflict",
                "region_required",
                "issuer_not_allowed",
                "token_url_invalid",
                "token_url_not_allowed",
            ],
        ),
        (
            AuthState::SignInRequired,
            &[
                "refresh_token_rejected",
                "refresh_outcome_unknown",
                "refresh_response_unusable",
            ],
        ),
        (
            AuthState::SignedOut,
            &[
                "refresh_token_rejected",
                "refresh_outcome_unknown",
                "refresh_response_unusable",
                "access_token_rejected",
                "stored_pair_invalid",
                "provider_token_rejected",
                "provider_token_expired",
            ],
        ),
        (AuthState::ApiRefused, &["owner_api", "client"]),
        (AuthState::Ready, &[]),
    ];
    for (state, codes) in table {
        for code in codes {
            let reason = StatusReason::parse(state, code)
                .unwrap_or_else(|| panic!("{} admits {code}", state.code()));
            assert!(state.admits(Some(reason)), "{} {code}", state.code());
        }
    }
    // A sign-in stop does not take the codes that only a sign-out has, and a
    // state that carries no reason takes none.
    assert_eq!(
        StatusReason::parse(AuthState::SignInRequired, "access_token_rejected"),
        None
    );
    assert_eq!(
        StatusReason::parse(AuthState::SignInRequired, "stored_pair_invalid"),
        None
    );
    assert_eq!(
        StatusReason::parse(AuthState::AwaitingConfirmation, "stored_pair_invalid"),
        None
    );
    assert_eq!(StatusReason::parse(AuthState::Ready, "network"), None);
    assert_eq!(StatusReason::parse(AuthState::ApiRefused, "network"), None);
    // The same code means two things under two states.
    assert_eq!(
        StatusReason::parse(AuthState::Retrying, "clock_skew"),
        Some(StatusReason::Retry(RetryReason::ClockSkew))
    );
    assert_eq!(
        StatusReason::parse(AuthState::ObserverWaiting, "clock_skew"),
        Some(StatusReason::Observer(ObserverReason::ClockSkew))
    );
    assert!(AuthState::Ready.admits(None));
    assert!(!AuthState::Retrying.admits(None));
    assert!(!AuthState::SignedOut.admits(None));
}

#[test]
fn the_outcome_table_of_the_wire_part_only_uses_codes_the_status_admits() {
    use super::super::wire::AttemptClass;
    for class in [
        AttemptClass::NotSent,
        AttemptClass::Rotated,
        AttemptClass::NotRotated,
        AttemptClass::ConsumedWithoutSuccessor,
        AttemptClass::Unknown,
        AttemptClass::ClientRefused,
        AttemptClass::Dead,
        AttemptClass::RequestRejected,
        AttemptClass::Refused,
        AttemptClass::Misrouted,
        AttemptClass::Blocked,
        AttemptClass::Transient,
        AttemptClass::NotRenewed,
    ] {
        let code = class.state_code();
        let state = AuthState::from_code(code.state)
            .unwrap_or_else(|| panic!("{:?} gives an unknown state {}", class, code.state));
        let reason = code.reason.map(|reason| {
            StatusReason::parse(state, reason)
                .unwrap_or_else(|| panic!("{:?}: {} does not admit {reason}", class, code.state))
        });
        assert!(state.admits(reason), "{class:?}");
    }
}

#[test]
fn a_status_round_trips_through_its_row_and_the_checks_of_the_table_are_held_in_code() {
    let g = gen_id(1);
    let cases = [
        ready(),
        AuthStatus {
            state: AuthState::Retrying,
            reason: Some(StatusReason::Retry(RetryReason::Network)),
            collecting: true,
            retry_at: Some(T0 + 60),
            ..ready()
        },
        AuthStatus::stopped(
            AuthStop::SignInRequired(SignOutReason::RefreshTokenRejected),
            g,
            StoppedMode::Legacy,
            true,
            FuseSnapshot::default(),
            true,
        ),
        signed_out(SignOutReason::AccessTokenRejected, g, fuse(3, g, T0, None)),
        refused(RefusalReason::Client, StoppedMode::Provider),
    ];
    for status in cases {
        assert!(status.is_consistent(), "{status:?}");
        let row = status.to_row();
        assert_eq!(AuthStatus::from_row(&row).expect("round trip"), status);
    }
    // Rows that break the table's own CHECKs are refused on read, not guessed.
    let mut row = signed_out(
        SignOutReason::AccessTokenRejected,
        g,
        FuseSnapshot::default(),
    )
    .to_row();
    row.stopped_generation = None;
    assert!(matches!(
        AuthStatus::from_row(&row),
        Err(StoreError::InvalidStoredAuthStatus)
    ));
    let mut row = ready().to_row();
    row.state = "frozen".to_owned();
    assert!(matches!(
        AuthStatus::from_row(&row),
        Err(StoreError::InvalidStoredAuthStatus)
    ));
    let mut row = ready().to_row();
    row.reason = Some("network".to_owned());
    assert!(matches!(
        AuthStatus::from_row(&row),
        Err(StoreError::InvalidStoredAuthStatus)
    ));
    let mut row = ready().to_row();
    row.fuse_consecutive = 2;
    assert!(matches!(
        AuthStatus::from_row(&row),
        Err(StoreError::InvalidStoredAuthStatus)
    ));
    let mut row = refused(RefusalReason::OwnerApi, StoppedMode::Legacy).to_row();
    row.stopped_mode = Some("fleet_polling".to_owned());
    assert!(matches!(
        AuthStatus::from_row(&row),
        Err(StoreError::InvalidStoredAuthStatus)
    ));
    // `ready` never stops collecting; a state that cannot collect cannot say it does.
    let mut status = ready();
    status.collecting = false;
    assert!(!status.is_consistent());
    let mut status = refused(RefusalReason::OwnerApi, StoppedMode::Legacy);
    status.collecting = true;
    assert!(!status.is_consistent());
}

#[test]
fn debug_text_of_a_status_shows_codes_and_never_a_generation() {
    let g = gen_id(0xab);
    let status = signed_out(SignOutReason::AccessTokenRejected, g, fuse(3, g, T0, None));
    let text = format!("{status:?} {:?}", status.fuse);
    assert!(text.contains("signed_out") && text.contains("access_token_rejected"));
    assert!(
        !text.contains(&g.to_string()) && !text.contains("abababab"),
        "{text}"
    );
}

#[test]
fn lifting_rules_of_r44_follow_the_table() {
    use AuthStop::*;
    let lifted_by_write = [
        SignedOut(SignOutReason::AccessTokenRejected),
        SignedOut(SignOutReason::ProviderTokenRejected),
        SignedOut(SignOutReason::RefreshTokenRejected),
        SignedOut(SignOutReason::RefreshOutcomeUnknown),
        SignedOut(SignOutReason::RefreshResponseUnusable),
        SignedOut(SignOutReason::ProviderTokenExpired),
        SignedOut(SignOutReason::StoredPairInvalid),
        SignInRequired(SignOutReason::RefreshTokenRejected),
        SignInRequired(SignOutReason::RefreshOutcomeUnknown),
        SignInRequired(SignOutReason::RefreshResponseUnusable),
    ];
    for stop in lifted_by_write {
        assert!(stop.lifted_by_external_write(), "{stop:?}");
        assert!(!stop.lifted_by_mode_change(), "{stop:?}");
    }
    assert!(!ApiRefused(RefusalReason::OwnerApi).lifted_by_external_write());
    assert!(!ApiRefused(RefusalReason::Client).lifted_by_external_write());
    for reason in [RefusalReason::OwnerApi, RefusalReason::Client] {
        assert!(ApiRefused(reason).lifted_by_mode_change());
        assert!(ApiRefused(reason).lifted_by_owner_retry());
    }
    // The owner's retry lifts only a rejected access token or provider token
    // (rows 1 of R44) and a refusal; never a stop whose refresh token is dead,
    // unknown or unusable (R50).
    let retry_lifts: Vec<_> = lifted_by_write
        .into_iter()
        .filter(|stop| stop.lifted_by_owner_retry())
        .collect();
    assert_eq!(
        retry_lifts,
        [
            SignedOut(SignOutReason::AccessTokenRejected),
            SignedOut(SignOutReason::ProviderTokenRejected)
        ]
    );
    // R43: a sign-in stop stops token requests only.
    assert!(!SignInRequired(SignOutReason::RefreshTokenRejected).blocks_requests());
    assert!(SignedOut(SignOutReason::AccessTokenRejected).blocks_requests());
    assert!(ApiRefused(RefusalReason::Client).blocks_requests());
    // R52: the lease is terminal exactly while signed out.
    for state in AuthState::ALL {
        assert_eq!(state.lease_is_terminal(), state == AuthState::SignedOut);
    }
}

#[test]
fn collecting_follows_usability_only_for_the_three_states_that_collect_while_usable() {
    let retrying = AuthStatus {
        state: AuthState::Retrying,
        reason: Some(StatusReason::Retry(RetryReason::Network)),
        retry_at: Some(T0 + 60),
        ..ready()
    };
    assert!(!retrying.with_usability(false).collecting);
    assert!(
        retrying
            .with_usability(false)
            .with_usability(true)
            .collecting
    );
    let pending = AuthStatus {
        state: AuthState::PersistencePending,
        reason: None,
        ..ready()
    };
    assert!(!pending.with_usability(false).collecting);
    let sign_in = AuthStatus::stopped(
        AuthStop::SignInRequired(SignOutReason::RefreshOutcomeUnknown),
        gen_id(1),
        StoppedMode::Legacy,
        true,
        FuseSnapshot::default(),
        false,
    );
    assert!(sign_in.collecting && !sign_in.with_usability(false).collecting);
    // `ready` and the states that never collect are not changed by it.
    assert_eq!(ready().with_usability(false), ready());
    let refusal = refused(RefusalReason::OwnerApi, StoppedMode::Legacy);
    assert_eq!(refusal.with_usability(true), refusal);
    // R45: a sign-in stop whose token passed its expiry is a sign-out.
    let expired = sign_in.with_sign_in_expired();
    assert_eq!(expired.state, AuthState::SignedOut);
    assert_eq!(
        expired.reason,
        Some(StatusReason::SignOut(SignOutReason::RefreshOutcomeUnknown))
    );
    assert_eq!(expired.stopped_generation, Some(gen_id(1)));
    assert!(expired.is_consistent() && !expired.collecting);
    assert_eq!(ready().with_sign_in_expired(), ready());
}

// ---------------------------------------------------------------------------
// R38 clock floor, R54 and R55 skew, the HTTP date reader
// ---------------------------------------------------------------------------

#[test]
fn http_date_reads_the_preferred_form_and_nothing_else() {
    // RFC 9110's own example, and the two fixtures of the spec.
    assert_eq!(
        parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"),
        Some(784_111_777)
    );
    assert_eq!(parse_http_date("Fri, 15 Jan 2027 08:00:00 GMT"), Some(T0));
    assert_eq!(imf_fixdate(T0), "Fri, 15 Jan 2027 08:00:00 GMT");
    // The clock floor of R38 is 2020-01-01T00:00:00Z.
    assert_eq!(
        parse_http_date("Wed, 01 Jan 2020 00:00:00 GMT"),
        Some(CLOCK_FLOOR_EPOCH_SECONDS)
    );
    // A leap day, the last second of a year and the epoch.
    assert_eq!(
        parse_http_date("Thu, 29 Feb 2024 12:00:00 GMT"),
        Some(1_709_208_000)
    );
    assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
    assert_eq!(
        parse_http_date("Fri, 31 Dec 1999 23:59:59 GMT"),
        Some(946_684_799)
    );
    // Every second the independent writer produces is read back unchanged.
    for epoch in [
        0,
        86_399,
        951_782_400,
        1_709_208_000,
        T0,
        4_102_444_800,
        253_402_300_799,
    ] {
        assert_eq!(parse_http_date(&imf_fixdate(epoch)), Some(epoch), "{epoch}");
    }
    let refused = [
        "",
        "Sun, 06 Nov 1994 08:49:37",          // no zone
        "Sun, 06 Nov 1994 08:49:37 UTC",      // not GMT
        "Sun, 06 nov 1994 08:49:37 GMT",      // month case
        "Sun, 06 Nov 1994 08:49:37 gmt",      // zone case
        "Sun 06 Nov 1994 08:49:37 GMT",       // no comma
        "Mon, 06 Nov 1994 08:49:37 GMT",      // weekday does not match
        "Sunday, 06-Nov-94 08:49:37 GMT",     // the obsolete forms are not read
        "Sun Nov  6 08:49:37 1994",           // asctime
        "Sun, 6 Nov 1994 08:49:37 GMT",       // one-digit day
        "Sun, 06 Nov 94 08:49:37 GMT",        // two-digit year
        "Sun, 06 Nov 1994 8:49:37 GMT",       // one-digit hour
        "Sun, 06 Nov 1994 24:00:00 GMT",      // hour 24
        "Sun, 06 Nov 1994 08:60:00 GMT",      // minute 60
        "Sun, 06 Nov 1994 08:49:60 GMT",      // leap second
        "Wed, 30 Feb 2022 08:49:37 GMT",      // impossible day
        "Wed, 29 Feb 2023 08:49:37 GMT",      // not a leap year
        "Wed, 31 Apr 2021 08:49:37 GMT",      // April has 30
        "Tue, 31 Dec 1968 23:59:59 GMT",      // before the epoch
        "Sun, 06 Nov 1994 08:49:37 GMT ",     // trailing space
        " Sun, 06 Nov 1994 08:49:37 GMT",     // leading space
        "Sun,  06 Nov 1994 08:49:37 GMT",     // double space
        "Sun, +6 Nov 1994 08:49:37 GMT",      // a sign is not a digit
        "S\u{fc}n, 06 Nov 1994 08:49:37 GMT", // not ASCII
        "Sun, 06 Nov 1994 08:49:37:00 GMT",   // extra clock field
        "Sun, 06 Nov 1994 08:49:37 GMT extra",
    ];
    for text in refused {
        assert_eq!(parse_http_date(text), None, "{text:?}");
    }
}

#[test]
fn t66_the_clock_floor_is_2020_and_a_clock_below_it_is_clock_invalid() {
    assert!(!clock_is_valid(CLOCK_FLOOR_EPOCH_SECONDS - 1));
    assert!(clock_is_valid(CLOCK_FLOOR_EPOCH_SECONDS));
    assert!(!clock_is_valid(0) && !clock_is_valid(-5));
    assert_eq!(CLOCK_RECHECK_SECONDS, 60);
    let restored = restore_from_facts(&inputs(1_500_000_000, gen_id(1)), &facts()).unwrap();
    assert!(!restored.clock_valid);
    let status = restored.status;
    assert_eq!(
        (status.state, status.reason),
        (AuthState::ClockInvalid, None)
    );
    // R38: nothing collects, so no request is sent and no token is handed out;
    // no schedule is written (the retry fields stay empty); the lease stays
    // active (R52).
    assert!(!status.collecting && status.retry_at.is_none());
    assert!(status.stop().is_none() && !status.state.lease_is_terminal());
    assert!(status.is_consistent());
    // When the clock reads t0 the unit is back to normal operation.
    let sane = restore_from_facts(&inputs(T0, gen_id(1)), &facts()).unwrap();
    assert!(sane.clock_valid);
    assert_eq!(sane.status, ready());
}

#[test]
fn a_stop_is_not_replaced_by_clock_invalid_and_a_stored_fuse_survives_a_bad_clock() {
    let g0 = gen_id(1);
    let kept_fuse = fuse(2, g0, T0, Some(T0 + 300));
    // The table ties `stopped_generation` to the stop states, so a status that
    // said `clock_invalid` would erase the stop; the stop already stops every
    // request (R43), so it is the status that is shown.
    let stored = signed_out(SignOutReason::AccessTokenRejected, g0, kept_fuse);
    let restored = restore_from_facts(&inputs(1_500_000_000, g0), &with_stored(stored)).unwrap();
    assert!(!restored.clock_valid);
    assert_eq!(restored.status, stored);
    // A sign-in stop is kept as it is, and cannot be judged usable.
    let sign_in = AuthStatus::stopped(
        AuthStop::SignInRequired(SignOutReason::RefreshOutcomeUnknown),
        g0,
        StoppedMode::Legacy,
        true,
        kept_fuse,
        false,
    );
    let restored = restore_from_facts(&inputs(1_500_000_000, g0), &with_stored(sign_in)).unwrap();
    assert_eq!(restored.status.state, AuthState::SignInRequired);
    assert!(!restored.status.collecting, "R38: no token is handed out");
    // The fuse and its pause are carried through a clock fault untouched.
    let stored = AuthStatus::ready(kept_fuse, false);
    let restored = restore_from_facts(&inputs(1_500_000_000, g0), &with_stored(stored)).unwrap();
    assert_eq!(restored.status.state, AuthState::ClockInvalid);
    assert_eq!(restored.status.fuse, kept_fuse);
    assert!(!restored.pause_elapsed);
}

#[test]
fn t67_skew_is_read_from_the_date_header_and_changes_only_on_a_valid_one() {
    assert_eq!(SKEW_ALLOWANCE_SECONDS, 300);
    // Token success with `Date` = t0 + 600 received at local t0.
    assert!(skew_after_answer(false, Some(&imf_fixdate(T0 + 600)), T0));
    // A later Owner API answer within 300 s clears it.
    assert!(!skew_after_answer(
        true,
        Some(&imf_fixdate(T0 + 30)),
        T0 + 20
    ));
    // The boundary: 300 s is within the allowance, 301 s is not, either way round.
    assert!(!skew_after_answer(false, Some(&imf_fixdate(T0 + 300)), T0));
    assert!(skew_after_answer(false, Some(&imf_fixdate(T0 + 301)), T0));
    assert!(!skew_after_answer(true, Some(&imf_fixdate(T0 - 300)), T0));
    assert!(skew_after_answer(false, Some(&imf_fixdate(T0 - 301)), T0));
    // Test 82: a local clock two hours ahead of the server.
    assert!(skew_after_answer(
        false,
        Some(&imf_fixdate(T0 + 21_200)),
        T0 + 28_400
    ));
    // No header, or one that is not a valid date, says nothing.
    for current in [false, true] {
        assert_eq!(skew_after_answer(current, None, T0), current);
        assert_eq!(skew_after_answer(current, Some("yesterday"), T0), current);
        assert_eq!(
            skew_after_answer(current, Some("Mon, 15 Jan 2027 08:00:00 GMT"), T0),
            current,
            "a weekday that does not match is not a valid date"
        );
    }
    // The flag is part of the stored status (R52a) and changes only the row's
    // skew column; the extreme epoch values do not overflow.
    assert!(skew_after_answer(false, Some(&imf_fixdate(0)), i64::MAX));
    assert!(skew_after_answer(
        false,
        Some(&imf_fixdate(253_402_300_799)),
        i64::MIN
    ));
    let flagged = AuthStatus::ready(FuseSnapshot::default(), true);
    assert!(flagged.to_row().clock_skew && !ready().to_row().clock_skew);
}

// ---------------------------------------------------------------------------
// R58 restart floor (test 65)
// ---------------------------------------------------------------------------

#[test]
fn t65_the_restart_floor_waits_for_sixty_seconds_after_the_last_receipt() {
    assert_eq!(MIN_REFRESH_INTERVAL_SECONDS, 60);
    let (_temporary, store) = test_store();
    let g0 = gen_id(1);
    put_pair(&store, g0);
    // No receipt: no floor.
    let none = restore_at_start(&store, &inputs(T0, g0)).unwrap();
    assert_eq!(none.token_requests_not_before, 0);
    // The last token receipt (outcome success) started 20 s before start: the
    // first attempt is 40 s after start.
    put_receipt(
        &store,
        (T0 - 20) * 1_000,
        "success",
        "legacy_auth",
        "token_refresh",
    );
    let recent = restore_at_start(&store, &inputs(T0, g0)).unwrap();
    assert_eq!(recent.token_requests_not_before, T0 + 40);
    // A receipt of any other transport or operation is not a token receipt.
    put_receipt(&store, (T0 - 5) * 1_000, "success", "owner_api", "products");
    put_receipt(
        &store,
        (T0 - 5) * 1_000,
        "success",
        "stream",
        "stream_connect",
    );
    let still = restore_at_start(&store, &inputs(T0, g0)).unwrap();
    assert_eq!(still.token_requests_not_before, T0 + 40);
    // The latest wins, whatever its outcome: an unresolved or failed one counts.
    put_receipt(
        &store,
        (T0 - 10) * 1_000,
        "started",
        "legacy_auth",
        "token_refresh",
    );
    let later = restore_at_start(&store, &inputs(T0, g0)).unwrap();
    assert_eq!(later.token_requests_not_before, T0 + 50);
    // 61 s old: the floor has passed, so the first attempt is immediate.
    let (_temporary, store) = test_store();
    put_pair(&store, g0);
    put_receipt(
        &store,
        (T0 - 61) * 1_000,
        "success",
        "legacy_auth",
        "token_refresh",
    );
    let old = restore_at_start(&store, &inputs(T0, g0)).unwrap();
    assert!(old.token_requests_not_before <= T0);
    assert_eq!(old.token_requests_not_before, T0 - 1);
}

#[test]
fn the_restart_floor_rounds_up_and_never_parks_a_refresh_past_a_minute() {
    // 60 s after 20.5 s before `now` is at 39.5 s: rounded up to whole seconds.
    assert_eq!(restart_floor(T0, Some(T0 * 1_000 - 20_500)), T0 + 40);
    assert_eq!(restart_floor(T0, Some(T0 * 1_000 - 60_000)), T0);
    assert_eq!(restart_floor(T0, None), 0);
    // A receipt dated after now means the clock moved back since it: the floor
    // is a minute from now, not a minute after a time that has not come.
    assert_eq!(restart_floor(T0, Some((T0 + 86_400) * 1_000)), T0 + 60);
    assert_eq!(restart_floor(T0, Some(i64::MAX)), T0 + 60);
    // R58: the floor applies to every restore, observer or not, and a restored
    // status leaves `retry_at` empty (R59: a stored retry time is not honoured).
    let restored = restore_from_facts(
        &inputs(T0, gen_id(1)),
        &StartFacts {
            last_receipt_started_at_ms: Some((T0 - 1) * 1_000),
            stored: Some(
                AuthStatus {
                    state: AuthState::Retrying,
                    reason: Some(StatusReason::Retry(RetryReason::Network)),
                    retry_at: Some(T0 + 3_000),
                    ..ready()
                }
                .to_row(),
            ),
            ..facts()
        },
    )
    .unwrap();
    assert_eq!(restored.token_requests_not_before, T0 + 59);
    assert_eq!(restored.status, ready());
}

// ---------------------------------------------------------------------------
// R57 restore (tests 64 (a), 68, 72, 94 (c) and (d))
// ---------------------------------------------------------------------------

#[test]
fn t64a_a_stored_stop_holds_while_its_generation_is_current_and_clears_after_an_external_write() {
    let (_temporary, store) = test_store();
    let g0 = gen_id(1);
    put_pair(&store, g0);
    let stop = signed_out(
        SignOutReason::AccessTokenRejected,
        g0,
        fuse(3, g0, T0, None),
    );
    store
        .write_collector_auth_row(&stop.to_row(), T0 * 1_000)
        .expect("write the stop");
    // Restart one minute later: the stop is restored exactly, the fuse with it
    // (test 35's `stopped_generation` and `fuse_consecutive = 3`), nothing
    // collects and every request stays stopped.
    let restored = restore_at_start(&store, &inputs(T0 + 60, g0)).unwrap();
    assert_eq!(restored.status, stop);
    assert!(!restored.status.collecting);
    assert!(restored.status.stop().unwrap().blocks_requests());
    assert!(restored.status.state.lease_is_terminal());
    // The owner stores a new pair; after a restart the stop is cleared and the
    // fuse starts clean, so requests resume.
    let g9 = gen_id(9);
    put_pair(&store, g9);
    let restarted = restore_at_start(&store, &inputs(T0 + 1_000, g9)).unwrap();
    assert_eq!(restarted.status, ready());
    assert!(restarted.status.collecting && restarted.status.fuse == FuseSnapshot::default());
    // Written through the recorder, the stored row no longer holds a stop.
    let mut recorder = StatusRecorder::new(
        store.clone(),
        restarted.status,
        store.load_collector_auth_row().unwrap(),
    );
    assert_eq!(
        recorder.set(restarted.status, (T0 + 1_000) * 1_000),
        StatusWrite::Written
    );
    let row = store.load_collector_auth_row().unwrap().unwrap();
    assert_eq!(
        (row.state.as_str(), row.stopped_generation),
        ("ready", None)
    );
    assert_eq!(row.fuse_consecutive, 0);
    assert!(recorder.is_synced());
}

#[test]
fn a_stored_stop_of_another_generation_is_cleared_and_a_sign_in_stop_follows_usability() {
    let g0 = gen_id(1);
    let g1 = gen_id(2);
    let stored = signed_out(
        SignOutReason::RefreshTokenRejected,
        g0,
        FuseSnapshot::default(),
    );
    // Credentials changed while stopped (R44a): the stop is cleared.
    let restored = restore_from_facts(&inputs(T0, g1), &with_stored(stored)).unwrap();
    assert_eq!(restored.status, ready());
    // A sign-in stop collects while the token is usable (row 8 of R45, test 81) ...
    let sign_in = AuthStatus::stopped(
        AuthStop::SignInRequired(SignOutReason::RefreshTokenRejected),
        g0,
        StoppedMode::Legacy,
        true,
        FuseSnapshot::default(),
        false,
    );
    let usable = restore_from_facts(&inputs(T0 + 100, g0), &with_stored(sign_in)).unwrap();
    assert_eq!(usable.status, sign_in);
    assert!(usable.status.collecting && !usable.status.stop().unwrap().blocks_requests());
    // ... and is a sign-out once `usable_until` has passed (R57 step 5 (a)).
    let expired = restore_from_facts(&inputs(T0 + 28_500, g0), &with_stored(sign_in)).unwrap();
    assert_eq!(expired.status.state, AuthState::SignedOut);
    assert_eq!(
        expired.status.reason,
        Some(StatusReason::SignOut(SignOutReason::RefreshTokenRejected))
    );
    assert_eq!(expired.status.stopped_generation, Some(g0));
    assert!(!expired.status.collecting && expired.status.state.lease_is_terminal());
    // An unknown bound counts as usable.
    let mut unbounded = inputs(T0 + 99_999, g0);
    unbounded.pair = PairLoad::Loaded {
        generation: g0,
        usable_until: None,
    };
    let restored = restore_from_facts(&unbounded, &with_stored(sign_in)).unwrap();
    assert_eq!(restored.status.state, AuthState::SignInRequired);
    assert!(restored.status.collecting);
}

#[test]
fn t68_an_unresolved_fencing_attempt_at_start_is_a_sign_out_until_an_external_write() {
    let (_temporary, store) = test_store();
    let g0 = gen_id(1);
    put_pair(&store, g0);
    store
        .begin_legacy_refresh(g0)
        .expect("begin an attempt that never finishes");
    assert!(store.has_unresolved_legacy_refresh().unwrap());
    let restored = restore_at_start(&store, &inputs(T0, g0)).unwrap();
    let status = restored.status;
    assert_eq!(
        status.stop(),
        Some(AuthStop::SignedOut(SignOutReason::RefreshOutcomeUnknown))
    );
    assert_eq!(status.stopped_generation, Some(g0));
    // No POST (nothing collects, the stop blocks every request) and the lease
    // is terminal; reading the facts settled nothing.
    assert!(!status.collecting && status.stop().unwrap().blocks_requests());
    assert!(status.state.lease_is_terminal());
    assert!(store.has_unresolved_legacy_refresh().unwrap());
    // Only an external write lifts it (K4 closes the attempt).
    let g9 = gen_id(9);
    put_pair(&store, g9);
    assert!(!store.has_unresolved_legacy_refresh().unwrap());
    assert_eq!(
        restore_at_start(&store, &inputs(T0 + 5, g9))
            .unwrap()
            .status,
        ready()
    );
}

#[test]
fn an_invalid_pair_is_a_stored_pair_invalid_sign_out_and_the_first_step_that_applies_wins() {
    let g0 = gen_id(1);
    let mut input = inputs(T0, g0);
    input.pair = PairLoad::Invalid { generation: g0 };
    // Step 2 stands even when a refusal is stored and an attempt is unresolved.
    let stored = refused(RefusalReason::OwnerApi, StoppedMode::Legacy);
    let restored = restore_from_facts(
        &input,
        &StartFacts {
            unresolved_attempt: true,
            ..with_stored(stored)
        },
    )
    .unwrap();
    assert_eq!(
        restored.status.stop(),
        Some(AuthStop::SignedOut(SignOutReason::StoredPairInvalid))
    );
    assert_eq!(restored.status.stopped_generation, Some(g0));
    assert!(!restored.status.collecting);
    // A valid pair, an unresolved attempt and a configuration error: the stop
    // outranks the configuration error, which is only re-evaluated (R5).
    let mut input = inputs(T0, g0);
    input.configuration = Err(ConfigurationReason::RegionConflict);
    let restored = restore_from_facts(
        &input,
        &StartFacts {
            unresolved_attempt: true,
            ..facts()
        },
    )
    .unwrap();
    assert_eq!(
        restored.status.stop(),
        Some(AuthStop::SignedOut(SignOutReason::RefreshOutcomeUnknown))
    );
    // A stored stop outranks the configuration error too: its columns would
    // otherwise be written over.
    let stored = signed_out(
        SignOutReason::AccessTokenRejected,
        g0,
        FuseSnapshot::default(),
    );
    let restored = restore_from_facts(&input, &with_stored(stored)).unwrap();
    assert_eq!(restored.status, stored);
}

#[test]
fn t72_a_configuration_error_is_not_a_stored_stop() {
    let (_temporary, store) = test_store();
    let g0 = gen_id(1);
    put_pair(&store, g0);
    // Tesla `JG` with configured `china`: `configuration_error(region_conflict)`.
    let mut input = inputs(T0, g0);
    input.configuration = Err(ConfigurationReason::RegionConflict);
    let restored = restore_at_start(&store, &input).unwrap();
    let status = restored.status;
    assert_eq!(
        (status.state, status.reason),
        (
            AuthState::ConfigurationError,
            Some(StatusReason::Configuration(
                ConfigurationReason::RegionConflict
            ))
        )
    );
    // Nothing is sent, the lease stays active and no stop is stored.
    assert!(!status.collecting && status.stop().is_none() && !status.state.lease_is_terminal());
    assert_eq!(status.stopped_generation, None);
    let mut recorder = StatusRecorder::new(store.clone(), status, None);
    assert_eq!(recorder.set(status, T0 * 1_000), StatusWrite::Written);
    let row = store.load_collector_auth_row().unwrap().unwrap();
    assert_eq!(
        (row.state.as_str(), row.reason.as_deref(), row.collecting),
        ("configuration_error", Some("region_conflict"), false)
    );
    assert_eq!((row.stopped_generation, row.stopped_mode), (None, None));
    // Remove the key and restart: the unit is ready, with no owner action.
    let again = restore_at_start(&store, &inputs(T0 + 30, g0)).unwrap();
    assert_eq!(again.status, ready());
    assert_eq!(
        recorder.set(again.status, (T0 + 30) * 1_000),
        StatusWrite::Written
    );
    assert_eq!(
        store.load_collector_auth_row().unwrap().unwrap().state,
        "ready"
    );
    // Every configuration reason shows as configuration_error.
    for reason in [
        ConfigurationReason::RegionRequired,
        ConfigurationReason::IssuerNotAllowed,
        ConfigurationReason::TokenUrlInvalid,
        ConfigurationReason::TokenUrlNotAllowed,
    ] {
        let mut input = inputs(T0, g0);
        input.configuration = Err(reason);
        let status = restore_from_facts(&input, &facts()).unwrap().status;
        assert_eq!(status.reason.map(StatusReason::code), Some(reason.code()));
    }
}

#[test]
fn t94c_a_refusal_the_unit_set_survives_a_restart_and_an_external_write_but_not_a_mode_change() {
    let (_temporary, store) = test_store();
    let g0 = gen_id(1);
    put_pair(&store, g0);
    // Seeded: `api_refused(owner_api)` with `stopped_mode = 'legacy'` and no
    // `stopped_generation`; Tesla mode; no C3 row.
    let seeded = refused(RefusalReason::OwnerApi, StoppedMode::Legacy);
    let row = seeded.to_row();
    assert_eq!(
        (row.stopped_mode.as_deref(), row.stopped_generation),
        (Some("legacy"), None)
    );
    store.write_collector_auth_row(&row, T0 * 1_000).unwrap();
    let restored = restore_at_start(&store, &inputs(T0, g0)).unwrap();
    assert_eq!(restored.status, seeded);
    assert!(!restored.status.collecting && restored.status.stop().unwrap().blocks_requests());
    // Unlike a sign-out, the lease stays active (R52).
    assert!(!restored.status.state.lease_is_terminal());
    // After an external pair write and a restart: still api_refused.
    let g9 = gen_id(9);
    put_pair(&store, g9);
    let after_write = restore_at_start(&store, &inputs(T0 + 500, g9)).unwrap();
    assert_eq!(after_write.status, seeded);
    // With `stopped_mode = 'provider'` and Tesla mode configured: cleared, the
    // row is rewritten with `stopped_mode` NULL, and requests are sent.
    let provider_refusal = refused(RefusalReason::OwnerApi, StoppedMode::Provider);
    store
        .write_collector_auth_row(&provider_refusal.to_row(), (T0 + 600) * 1_000)
        .unwrap();
    let cleared = restore_at_start(&store, &inputs(T0 + 700, g9)).unwrap();
    assert_eq!(cleared.status, ready());
    assert!(cleared.status.collecting);
    let mut recorder = StatusRecorder::new(
        store.clone(),
        cleared.status,
        store.load_collector_auth_row().unwrap(),
    );
    assert_eq!(
        recorder.set(cleared.status, (T0 + 700) * 1_000),
        StatusWrite::Written
    );
    let row = store.load_collector_auth_row().unwrap().unwrap();
    assert_eq!((row.state.as_str(), row.stopped_mode), ("ready", None));
    // The same configured mode in Provider mode keeps a provider refusal.
    let mut provider = inputs(T0 + 800, g9);
    provider.mode = AuthMode::Provider;
    let kept = restore_from_facts(&provider, &with_stored(provider_refusal)).unwrap();
    assert_eq!(kept.status, provider_refusal);
    // The B2 clarification: when C3's start reset its own refusal for an
    // external pair write (the credential identity changed), this unit clears
    // its refusal too; with no C3 row (its own refusal, or a crash before
    // C3's commit) the refusal stays, as above.
    let mut reset = inputs(T0 + 900, g9);
    reset.c3_reset_refusal = true;
    let cleared = restore_from_facts(&reset, &with_stored(seeded)).unwrap();
    assert_eq!(cleared.status, ready());
}

#[test]
fn t94d_a_refusal_hides_an_unresolved_attempt_until_it_lifts() {
    let (_temporary, store) = test_store();
    let g0 = gen_id(1);
    put_pair(&store, g0);
    let seeded = refused(RefusalReason::OwnerApi, StoppedMode::Legacy);
    store
        .write_collector_auth_row(&seeded.to_row(), T0 * 1_000)
        .unwrap();
    store
        .begin_legacy_refresh(g0)
        .expect("an unresolved attempt");
    // Tesla mode, an unresolved attempt and the seeded refusal: the refusal is
    // shown, nothing is sent.
    let restored = restore_at_start(&store, &inputs(T0, g0)).unwrap();
    assert_eq!(restored.status, seeded);
    assert!(!restored.status.collecting);
    // When the refusal lifts (the owner's retry control, C3's reset or a mode
    // change) the same facts give `signed_out(refresh_outcome_unknown)` with
    // the lease terminal, and no POST: a lift re-evaluates from start.
    let mut lifted = inputs(T0 + 60, g0);
    lifted.c3_reset_refusal = true;
    let after = restore_at_start(&store, &lifted).unwrap();
    assert_eq!(
        after.status.stop(),
        Some(AuthStop::SignedOut(SignOutReason::RefreshOutcomeUnknown))
    );
    assert!(after.status.state.lease_is_terminal() && !after.status.collecting);
    // The refusal also replaces a configuration error (step 4).
    let mut config = inputs(T0, g0);
    config.configuration = Err(ConfigurationReason::TokenUrlInvalid);
    let restored = restore_from_facts(&config, &with_stored(seeded)).unwrap();
    assert_eq!(restored.status, seeded);
}

#[test]
fn cs_p3_own_refusal_precedence_and_stale_generation() {
    // B2 P3, R57 step 5 and its precedence: the unit's own refusal outranks an
    // unresolved attempt, a stale stored stop and a configuration error, a stop of
    // another generation is excluded, and what the refusal hid reappears, in the
    // order the steps give, when it lifts.
    let g_old = gen_id(1);
    let g_now = gen_id(2);
    let own = refused(RefusalReason::OwnerApi, StoppedMode::Legacy);
    // A stored sign-out of an earlier generation is stale: credentials changed
    // while it stood (R44a), so it is excluded.
    let stale = signed_out(
        SignOutReason::RefreshTokenRejected,
        g_old,
        FuseSnapshot::default(),
    );
    let restored = restore_from_facts(&inputs(T0, g_now), &with_stored(stale)).unwrap();
    assert_eq!(restored.status.stop(), None, "the stale stop is excluded");
    assert!(restored.status.collecting);
    // The same stop of the current generation holds.
    let held = restore_from_facts(&inputs(T0, g_old), &with_stored(stale)).unwrap();
    assert_eq!(
        held.status.stop(),
        Some(AuthStop::SignedOut(SignOutReason::RefreshTokenRejected))
    );
    // The own refusal, with an unresolved attempt and a configuration error beside
    // it, is what shows; a stale stored sign-out never takes its place.
    let mut hidden = inputs(T0, g_now);
    hidden.configuration = Err(ConfigurationReason::TokenUrlInvalid);
    let facts_with = |stored: AuthStatus| StartFacts {
        unresolved_attempt: true,
        stored: Some(stored.to_row()),
        ..facts()
    };
    for (label, stored) in [("own refusal", own), ("stale stop", stale)] {
        let shown = restore_from_facts(&hidden, &facts_with(stored))
            .unwrap()
            .status;
        match label {
            "own refusal" => assert_eq!(shown, own, "{label}"),
            _ => assert_eq!(
                shown.stop(),
                Some(AuthStop::SignedOut(SignOutReason::RefreshOutcomeUnknown)),
                "{label}: the unresolved attempt, not the stale stop"
            ),
        }
    }
    // The refusal lifts (the owner's retry or C3's reset): the unresolved attempt
    // reappears first; with none, the configuration error reappears; with neither,
    // the unit is ready. A stale stop stays excluded throughout.
    let mut lifted = hidden;
    lifted.c3_reset_refusal = true;
    let both = restore_from_facts(&lifted, &facts_with(own))
        .unwrap()
        .status;
    assert_eq!(
        both.stop(),
        Some(AuthStop::SignedOut(SignOutReason::RefreshOutcomeUnknown)),
        "the unresolved attempt reappears first"
    );
    let configuration_only = StartFacts {
        unresolved_attempt: false,
        stored: Some(own.to_row()),
        ..facts()
    };
    let config = restore_from_facts(&lifted, &configuration_only)
        .unwrap()
        .status;
    assert_eq!(config.state, AuthState::ConfigurationError);
    assert_eq!(
        config.reason.map(StatusReason::code),
        Some(ConfigurationReason::TokenUrlInvalid.code())
    );
    let mut plain = inputs(T0, g_now);
    plain.c3_reset_refusal = true;
    assert_eq!(
        restore_from_facts(&plain, &configuration_only)
            .unwrap()
            .status,
        ready()
    );
    // A refusal of another mode is not this unit's own: it is cleared, and the
    // configuration error it did not hide shows.
    let other_mode = refused(RefusalReason::Client, StoppedMode::Provider);
    let cleared = restore_from_facts(
        &hidden,
        &StartFacts {
            stored: Some(other_mode.to_row()),
            ..facts()
        },
    )
    .unwrap()
    .status;
    assert_eq!(cleared.state, AuthState::ConfigurationError);
}

// ---------------------------------------------------------------------------
// R57 steps 7 and 8: the fuse and its pause
// ---------------------------------------------------------------------------

#[test]
fn the_fuse_is_restored_for_the_generation_it_counted_or_a_pair_this_unit_refreshed() {
    let g0 = gen_id(1);
    let g1 = gen_id(2);
    let counted = AuthStatus::ready(fuse(1, g0, T0, None), false);
    // The generation it counted is current.
    let restored = restore_from_facts(&inputs(T0 + 10, g0), &with_stored(counted)).unwrap();
    assert_eq!(restored.status.fuse, fuse(1, g0, T0, None));
    // A forced refresh produced g1 and the process was killed (test 76, the
    // part this unit owns): at restart the fuse is restored with g0 in it.
    let restored = restore_from_facts(
        &inputs(T0 + 10, g1),
        &StartFacts {
            current_is_refresh_output: true,
            ..with_stored(counted)
        },
    )
    .unwrap();
    assert_eq!(restored.status.fuse, fuse(1, g0, T0, None));
    assert_eq!(restored.status.state, AuthState::Ready);
    // An external pair (neither): a clean fuse (R42).
    let restored = restore_from_facts(&inputs(T0 + 10, g1), &with_stored(counted)).unwrap();
    assert_eq!(restored.status.fuse, FuseSnapshot::default());
    // R41's window: exactly 3,600 s still counts, 3,601 s does not.
    let at_edge = restore_from_facts(&inputs(T0 + 3_600, g0), &with_stored(counted)).unwrap();
    assert_eq!(at_edge.status.fuse.consecutive, 1);
    let past = restore_from_facts(&inputs(T0 + 3_601, g0), &with_stored(counted)).unwrap();
    assert_eq!(past.status.fuse, FuseSnapshot::default());
    // A rejection dated after now (the clock moved back) has not aged.
    let future = restore_from_facts(&inputs(T0 - 500, g0), &with_stored(counted)).unwrap();
    assert_eq!(future.status.fuse.consecutive, 1);
    assert!(rejection_window_open(T0, T0 - 500) && rejection_window_open(T0, T0 + 3_600));
    assert!(!rejection_window_open(T0, T0 + 3_601));
    assert_eq!(FUSE_WINDOW_SECONDS, 3_600);
    // A stop outlives the window with its fuse reset to zero.
    let stored = signed_out(
        SignOutReason::AccessTokenRejected,
        g0,
        fuse(3, g0, T0, None),
    );
    let restored = restore_from_facts(&inputs(T0 + 10_000, g0), &with_stored(stored)).unwrap();
    assert_eq!(restored.status.state, AuthState::SignedOut);
    assert_eq!(restored.status.fuse, FuseSnapshot::default());
}

#[test]
fn a_restored_pause_is_awaiting_confirmation_until_it_ends_then_a_refresh_is_owed() {
    let g0 = gen_id(1);
    let paused = AuthStatus::ready(fuse(2, g0, T0, Some(T0 + 300)), false);
    // Test 76: killed after a forced refresh; restart at t0 + 10 would show the
    // pause set by the second rejection at t0 + 11 ... here the pause is stored.
    let restored = restore_from_facts(&inputs(T0 + 100, g0), &with_stored(paused)).unwrap();
    let status = restored.status;
    assert_eq!(
        (status.state, status.reason),
        (
            AuthState::AwaitingConfirmation,
            Some(StatusReason::SignOut(SignOutReason::AccessTokenRejected))
        )
    );
    assert!(!status.collecting && status.fuse.pause_until == Some(T0 + 300));
    assert!(!restored.pause_elapsed);
    // At and after the end of the pause the state is ready and the forced
    // refresh that follows the pause (R41 count 2) is owed; the pause value is
    // left for the fuse to clear (R42).
    let over = restore_from_facts(&inputs(T0 + 300, g0), &with_stored(paused)).unwrap();
    assert_eq!(over.status.state, AuthState::Ready);
    assert!(over.pause_elapsed && over.status.collecting);
    assert_eq!(over.status.fuse.pause_until, Some(T0 + 300));
    // An observer never pauses (R41 table).
    let mut observer = inputs(T0 + 100, g0);
    observer.observer = true;
    let restored = restore_from_facts(&observer, &with_stored(paused)).unwrap();
    assert_eq!(restored.status.state, AuthState::Ready);
    assert!(!restored.pause_elapsed);
    // A configuration error outranks the pause; a stop outranks both.
    let mut config = inputs(T0 + 100, g0);
    config.configuration = Err(ConfigurationReason::RegionRequired);
    let restored = restore_from_facts(&config, &with_stored(paused)).unwrap();
    assert_eq!(restored.status.state, AuthState::ConfigurationError);
    assert_eq!(restored.status.fuse.pause_until, Some(T0 + 300));
}

// ---------------------------------------------------------------------------
// R44a, R62: external write detection and idempotent persistence (test 87)
// ---------------------------------------------------------------------------

#[test]
fn t87_an_external_write_is_told_from_this_units_own_refresh() {
    let (_temporary, store) = test_store();
    let (g0, g1, g9) = (gen_id(1), gen_id(2), gen_id(9));
    assert_eq!(
        stored_pair_relation(&store, g0).unwrap(),
        StoredPairRelation::Missing
    );
    put_pair(&store, g0);
    assert_eq!(
        stored_pair_relation(&store, g0).unwrap(),
        StoredPairRelation::Current
    );
    // This unit refreshes g0 into g1: from the unit's old view the row holds
    // its own output (a detached exchange persisted it, R15a); from the new
    // view it is current.
    let receipt = store.begin_legacy_refresh(g0).unwrap();
    store
        .complete_legacy_refresh(receipt, g0, g1, &refreshed_pair(g1))
        .unwrap();
    assert_eq!(
        stored_pair_relation(&store, g0).unwrap(),
        StoredPairRelation::OwnOutput(g1)
    );
    assert_eq!(
        stored_pair_relation(&store, g1).unwrap(),
        StoredPairRelation::Current
    );
    assert!(store.legacy_generation_is_refresh_output(g1).unwrap());
    assert!(!store.legacy_generation_is_refresh_output(g0).unwrap());
    // An external writer stores g9: neither current nor produced by a binding.
    put_pair(&store, g9);
    assert_eq!(
        stored_pair_relation(&store, g1).unwrap(),
        StoredPairRelation::External
    );
    assert_eq!(
        stored_pair_relation(&store, g9).unwrap(),
        StoredPairRelation::Current
    );
    // A row with no generation was not written by this unit's refresh.
    let unbound = TeslaMateLegacyTokenStore::imported(vec![3; 8], vec![4; 8]).unwrap();
    store.replace_teslamate_legacy_tokens(&unbound).unwrap();
    assert_eq!(
        stored_pair_relation(&store, g9).unwrap(),
        StoredPairRelation::External
    );
}

#[test]
fn t87_an_external_write_before_a_due_refresh_loads_the_new_pair_and_resets_the_fuse() {
    let g0 = gen_id(1);
    let previous = AuthStatus {
        state: AuthState::AwaitingConfirmation,
        reason: Some(StatusReason::SignOut(SignOutReason::AccessTokenRejected)),
        collecting: false,
        fuse: fuse(2, g0, T0, Some(T0 + 300)),
        clock_skew: true,
        ..ready()
    };
    let g9 = gen_id(9);
    let pair = PairLoad::Loaded {
        generation: g9,
        usable_until: Some(T0 + 28_500),
    };
    let after = status_after_external_write(&previous, AuthMode::Tesla, &pair, Ok(()), true);
    // The fuse is reset, the pause and every retry field are gone, and the
    // account is ready on the new pair; the skew flag is a fact about the host
    // clock and stays.
    assert_eq!(after, AuthStatus::ready(FuseSnapshot::default(), true));
    // Every credential stop is lifted by (a) ...
    for reason in [
        SignOutReason::AccessTokenRejected,
        SignOutReason::RefreshTokenRejected,
        SignOutReason::RefreshOutcomeUnknown,
        SignOutReason::RefreshResponseUnusable,
        SignOutReason::ProviderTokenRejected,
        SignOutReason::ProviderTokenExpired,
        SignOutReason::StoredPairInvalid,
    ] {
        let stopped = signed_out(reason, g0, fuse(3, g0, T0, None));
        let after = status_after_external_write(&stopped, AuthMode::Tesla, &pair, Ok(()), true);
        assert_eq!(after, ready(), "{reason:?}");
    }
    let retrying = AuthStatus {
        state: AuthState::Retrying,
        reason: Some(StatusReason::Retry(RetryReason::TeslaUnavailable)),
        retry_at: Some(T0 + 120),
        ..ready()
    };
    assert_eq!(
        status_after_external_write(&retrying, AuthMode::Tesla, &pair, Ok(()), true),
        ready()
    );
    // ... but a refusal stays: new credentials do not lift it (R44, seam S3).
    let refusal = refused(RefusalReason::OwnerApi, StoppedMode::Legacy);
    assert_eq!(
        status_after_external_write(&refusal, AuthMode::Tesla, &pair, Ok(()), true),
        refusal
    );
    // The configuration is evaluated again for the new pair (R44a), the clock
    // looked at again, and a pair that cannot load is a sign-out of its own.
    let region = Err(ConfigurationReason::RegionRequired);
    let config = status_after_external_write(&retrying, AuthMode::Tesla, &pair, region, true);
    assert_eq!(config.state, AuthState::ConfigurationError);
    assert!(!config.collecting && config.stop().is_none());
    let clock = status_after_external_write(&retrying, AuthMode::Tesla, &pair, Ok(()), false);
    assert_eq!(clock.state, AuthState::ClockInvalid);
    let invalid = PairLoad::Invalid { generation: g9 };
    let bad = status_after_external_write(&refusal, AuthMode::Tesla, &invalid, Ok(()), true);
    assert_eq!(
        bad.stop(),
        Some(AuthStop::SignedOut(SignOutReason::StoredPairInvalid))
    );
    assert_eq!(bad.stopped_generation, Some(g9));
    assert!(bad.is_consistent());
}

#[test]
fn t87_an_external_write_during_an_exchange_discards_the_successor_and_signs_nothing_out() {
    let (_temporary, store) = test_store();
    let (g0, g1, g9) = (gen_id(1), gen_id(2), gen_id(9));
    put_pair(&store, g0);
    // The exchange is in flight: the attempt is begun and the request sent.
    let receipt = store.begin_legacy_refresh(g0).unwrap();
    assert!(store.has_unresolved_legacy_refresh().unwrap());
    // The owner stores a new pair now (K4 closes the attempt, its fence stays).
    put_pair(&store, g9);
    // The exchange ends with a successor: its write fails.
    let completed = store.complete_legacy_refresh(receipt, g0, g1, &refreshed_pair(g1));
    assert!(matches!(
        completed,
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    // The re-read finds the external generation: the successor is discarded.
    assert_eq!(
        persist_reread(&store, g0, g1, None).unwrap(),
        PersistReread::External
    );
    assert_eq!(
        stored_pair_relation(&store, g0).unwrap(),
        StoredPairRelation::External
    );
    // Nothing is unresolved, so the next start does not sign the owner out, and
    // the next request carries the external pair.
    assert!(!store.has_unresolved_legacy_refresh().unwrap());
    let restored = restore_at_start(&store, &inputs(T0 + 3, g9)).unwrap();
    assert_eq!(restored.status, ready());
    // The consumed input stays fenced, so g0 cannot come back (K4).
    let old = TeslaMateLegacyTokenStore::imported(vec![9; 8], vec![9; 8])
        .unwrap()
        .with_credential_generation(g0)
        .unwrap();
    assert!(matches!(
        store.replace_teslamate_legacy_tokens(&old),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
}

#[test]
fn an_ambiguous_write_is_recognised_by_the_successors_generation_and_not_written_twice() {
    let (_temporary, store) = test_store();
    let (g0, g1) = (gen_id(1), gen_id(2));
    assert_eq!(
        persist_reread(&store, g0, g1, None).unwrap(),
        PersistReread::Missing
    );
    put_pair(&store, g0);
    let receipt = store.begin_legacy_refresh(g0).unwrap();
    // The write fails before it commits: the row still holds the input and the
    // same write can be tried again.
    assert_eq!(
        persist_reread(&store, g0, g1, None).unwrap(),
        PersistReread::StillInput
    );
    // The write commits but the answer is lost (the caller saw an error).
    store
        .complete_legacy_refresh(receipt, g0, g1, &refreshed_pair(g1))
        .unwrap();
    // The retry's re-read finds the successor: persistence counts as done. A
    // second write would be refused, as the receipt is closed.
    assert_eq!(
        persist_reread(&store, g0, g1, None).unwrap(),
        PersistReread::AlreadyPersisted
    );
    assert!(matches!(
        store.complete_legacy_refresh(receipt, g0, g1, &refreshed_pair(g1)),
        Err(StoreError::LegacyRefreshOutcomeUnknown)
    ));
    // The pure rule: a generation that is neither is an external write.
    let unbound = TeslaMateLegacyTokenStore::imported(vec![3; 8], vec![4; 8]).unwrap();
    store.replace_teslamate_legacy_tokens(&unbound).unwrap();
    assert_eq!(
        persist_reread(&store, g0, g1, None).unwrap(),
        PersistReread::External
    );
}

// ---------------------------------------------------------------------------
// R43, R52a: memory first, written on change, retried on a schedule (test 85)
// ---------------------------------------------------------------------------

/// A sink that fails a set number of times and counts every call.
struct FlakySink {
    inner: HubStore,
    failures_left: Cell<u32>,
    calls: Cell<u32>,
}

impl FlakySink {
    fn new(inner: HubStore, failures: u32) -> Self {
        Self {
            inner,
            failures_left: Cell::new(failures),
            calls: Cell::new(0),
        }
    }
}

impl StatusSink for FlakySink {
    fn write_status(&self, row: &CollectorAuthRow, now_ms: i64) -> Result<bool, StoreError> {
        self.calls.set(self.calls.get() + 1);
        if self.failures_left.get() > 0 {
            self.failures_left.set(self.failures_left.get() - 1);
            return Err(StoreError::InvalidStoredAuthStatus);
        }
        self.inner.write_collector_auth_row(row, now_ms)
    }
}

#[test]
fn t85_a_failed_status_write_still_stops_requests_and_succeeds_on_a_later_retry() {
    let (_temporary, store) = test_store();
    let g2 = gen_id(3);
    let stop = signed_out(
        SignOutReason::AccessTokenRejected,
        g2,
        fuse(3, g2, T0, None),
    );
    let sink = FlakySink::new(store.clone(), 1);
    let mut recorder = StatusRecorder::new(sink, ready(), None);
    let now_ms = T0 * 1_000;
    // The stop is set in memory first; the write fails.
    let outcome = recorder.set(stop, now_ms);
    assert_eq!(
        outcome,
        StatusWrite::Deferred {
            retry_at_ms: now_ms + 5_000
        }
    );
    assert_eq!(*recorder.status(), stop);
    assert!(!recorder.status().collecting && recorder.status().stop().unwrap().blocks_requests());
    assert!(!recorder.is_synced());
    assert_eq!(store.load_collector_auth_row().unwrap(), None);
    assert_eq!(recorder.retry_due_at_ms(), Some(now_ms + 5_000));
    // Not before the retry time ...
    assert_eq!(
        recorder.flush_if_due(now_ms + 4_999),
        StatusWrite::Deferred {
            retry_at_ms: now_ms + 5_000
        }
    );
    assert_eq!(recorder.sink_calls_for_test(), 1);
    // ... and at +5 s it succeeds with the stop.
    assert_eq!(recorder.flush_if_due(now_ms + 5_000), StatusWrite::Written);
    assert!(recorder.is_synced() && recorder.retry_due_at_ms().is_none());
    let row = store.load_collector_auth_row().unwrap().unwrap();
    assert_eq!(row, stop.to_row());
    assert_eq!(
        (row.stopped_generation, row.fuse_consecutive),
        (Some(g2), 3)
    );
}

#[test]
fn status_write_retries_follow_the_schedule_and_always_write_the_latest_status() {
    assert_eq!(PERSISTENCE_RETRY_SECONDS, [5, 10, 20, 40, 80, 160, 300]);
    let (_temporary, store) = test_store();
    let g = gen_id(3);
    let first = signed_out(
        SignOutReason::AccessTokenRejected,
        g,
        FuseSnapshot::default(),
    );
    let sink = FlakySink::new(store.clone(), 9);
    let mut recorder = StatusRecorder::new(sink, ready(), None);
    let mut now_ms = T0 * 1_000;
    let mut delays = Vec::new();
    let mut outcome = recorder.set(first, now_ms);
    for _ in 0..9 {
        let StatusWrite::Deferred { retry_at_ms } = outcome else {
            panic!("{outcome:?}")
        };
        delays.push((retry_at_ms - now_ms) / 1_000);
        now_ms = retry_at_ms;
        outcome = recorder.flush_if_due(now_ms);
    }
    // Five, ten, twenty, forty, eighty, 160 and then every 300 s.
    assert_eq!(delays, [5, 10, 20, 40, 80, 160, 300, 300, 300]);
    assert_eq!(outcome, StatusWrite::Written);
    assert_eq!(
        store.load_collector_auth_row().unwrap().unwrap(),
        first.to_row()
    );
    // While a write waits for its time a newer status waits with it, so a
    // failing store is not written at every transition; the retry writes the
    // newest status, not the one that failed.
    let (_temporary, store) = test_store();
    let sink = FlakySink::new(store.clone(), 1);
    let mut recorder = StatusRecorder::new(sink, ready(), None);
    let start = T0 * 1_000;
    assert!(matches!(
        recorder.set(first, start),
        StatusWrite::Deferred { .. }
    ));
    let newer = refused(RefusalReason::Client, StoppedMode::Legacy);
    assert_eq!(
        recorder.set(newer, start + 1_000),
        StatusWrite::Deferred {
            retry_at_ms: start + 5_000
        }
    );
    assert_eq!(recorder.sink_calls_for_test(), 1);
    assert_eq!(recorder.flush_if_due(start + 5_000), StatusWrite::Written);
    assert_eq!(
        store.load_collector_auth_row().unwrap().unwrap(),
        newer.to_row()
    );
    // A shutdown can force one final attempt whatever the schedule says.
    let (_temporary, store) = test_store();
    let sink = FlakySink::new(store.clone(), 1);
    let mut recorder = StatusRecorder::new(sink, ready(), None);
    assert!(matches!(
        recorder.set(first, start),
        StatusWrite::Deferred { .. }
    ));
    assert_eq!(recorder.attempt(start + 1), StatusWrite::Written);
    assert!(recorder.is_synced());
}

#[test]
fn the_row_is_written_only_when_the_status_changes() {
    let (_temporary, store) = test_store();
    let sink = FlakySink::new(store.clone(), 0);
    let mut recorder = StatusRecorder::new(sink, ready(), None);
    let now_ms = T0 * 1_000;
    // The first write creates the row; the same status again writes nothing.
    assert_eq!(recorder.set(ready(), now_ms), StatusWrite::Written);
    assert_eq!(
        recorder.set(ready(), now_ms + 1_000),
        StatusWrite::Unchanged
    );
    assert_eq!(
        recorder.flush_if_due(now_ms + 2_000),
        StatusWrite::Unchanged
    );
    assert_eq!(recorder.sink_calls_for_test(), 1);
    // A change of any stored value is a write: the skew flag, a retry time, the
    // passing of `usable_until`, the fuse.
    let skewed = AuthStatus::ready(FuseSnapshot::default(), true);
    assert_eq!(recorder.set(skewed, now_ms + 3_000), StatusWrite::Written);
    let g = gen_id(1);
    let counted = AuthStatus::ready(fuse(1, g, T0, None), true);
    assert_eq!(recorder.set(counted, now_ms + 4_000), StatusWrite::Written);
    let retrying = AuthStatus {
        state: AuthState::Retrying,
        reason: Some(StatusReason::Retry(RetryReason::Network)),
        retry_at: Some(T0 + 60),
        ..counted
    };
    assert_eq!(recorder.set(retrying, now_ms + 5_000), StatusWrite::Written);
    let later = AuthStatus {
        retry_at: Some(T0 + 120),
        ..retrying
    };
    assert_eq!(recorder.set(later, now_ms + 6_000), StatusWrite::Written);
    assert_eq!(
        recorder.set(later.with_usability(false), now_ms + 7_000),
        StatusWrite::Written
    );
    assert_eq!(
        recorder.set(later.with_usability(false), now_ms + 8_000),
        StatusWrite::Unchanged
    );
    assert_eq!(recorder.sink_calls_for_test(), 6);
    // A restart that reaches the status the store already holds writes nothing
    // (R62: restart-safe and idempotent), whatever the write time.
    let stored = store.load_collector_auth_row().unwrap();
    let sink = FlakySink::new(store.clone(), 0);
    let mut restarted = StatusRecorder::new(sink, later.with_usability(false), stored);
    assert!(restarted.is_synced());
    assert_eq!(
        restarted.set(later.with_usability(false), now_ms + 90_000),
        StatusWrite::Unchanged
    );
    assert_eq!(restarted.sink_calls_for_test(), 0);
}

impl StatusRecorder<FlakySink> {
    fn sink_calls_for_test(&self) -> u32 {
        self.sink_for_test().calls.get()
    }
}

// ---------------------------------------------------------------------------
// The effective status a reader without a manager derives (B2 clarification 3,
// C5 R25, R29, R38, R44a): usability and generation are judged against the
// stored pair at the evaluation time, and nothing is written.
// ---------------------------------------------------------------------------

mod effective_view {
    use super::*;
    use crate::legacy_auth::effective::{effective_auth_row, stored_auth_view};

    fn jwt(exp: i64) -> String {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        format!(
            "{}.{}.signature",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#))
        )
    }

    /// Store a real pair under a real key; returns its generation. `schedule` is
    /// `(expires_at, next_refresh_at)` of a refreshed pair, or the imported form.
    fn real_pair(
        directory: &std::path::Path,
        store: &HubStore,
        access: &str,
        refresh: &str,
        schedule: Option<(i64, i64)>,
    ) -> Uuid {
        let key = b"effective view key";
        let tokens = crate::credentials::OwnerTokens::from_secret_parts(
            access.to_owned(),
            refresh.to_owned(),
        )
        .expect("tokens");
        let (access, refresh) =
            crate::teslamate_token::encrypt_legacy_owner_tokens(key, &tokens).expect("encrypt");
        let stored = match schedule {
            Some((expires_at, next_refresh_at)) => {
                TeslaMateLegacyTokenStore::refreshed(access, refresh, expires_at, next_refresh_at)
            }
            None => TeslaMateLegacyTokenStore::imported(access, refresh),
        }
        .expect("pair");
        crate::teslamate_credentials::replace_key_and_tokens(directory, store, key, &stored)
            .expect("persist");
        store
            .load_teslamate_legacy_tokens()
            .expect("load")
            .expect("row")
            .credential_generation()
            .expect("generation")
    }

    fn sign_in_row(generation: Uuid) -> CollectorAuthRow {
        CollectorAuthRow {
            state: "sign_in_required".to_owned(),
            reason: Some("refresh_token_rejected".to_owned()),
            collecting: true,
            retry_at: None,
            stopped_generation: Some(generation),
            stopped_mode: None,
            fuse_consecutive: 0,
            fuse_generation: None,
            fuse_last_rejection_at: None,
            fuse_pause_until: None,
            fuse_rejected_access_digest: None,
            clock_skew: false,
        }
    }

    /// Everything the view must leave alone.
    fn tables(store: &HubStore) -> (Vec<String>, i64, i64, i64) {
        let connection = store.open().expect("catalogue");
        let one = |sql: &str| -> i64 {
            connection
                .query_row(sql, [], |row| row.get(0))
                .expect("count")
        };
        let row: Vec<String> = connection
            .query_row(
                "SELECT state || '|' || COALESCE(reason, '') || '|' || collecting
                        || '|' || COALESCE(stopped_generation, '') || '|' || updated_at_ms
                   FROM collector_auth_state WHERE singleton_id = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .into_iter()
            .collect();
        (
            row,
            one("SELECT COUNT(*) FROM outbound_request_receipts"),
            one("SELECT COUNT(*) FROM legacy_refresh_receipt_bindings"),
            one("SELECT COUNT(*) FROM legacy_refresh_input_fences"),
        )
    }

    #[test]
    fn cadence_effective_auth_view_expires_sign_in_required_without_writes() {
        // The bound passes at exactly `usable_until`, in whole seconds, and the
        // view says signed out with the same reason and generation at that
        // instant and not a millisecond before.
        let usable_until = T0 + 100;
        let cases: [(&str, String, Option<(i64, i64)>, Option<i64>); 4] = [
            // A refreshed pair with an opaque token: the stored expiry is the bound.
            (
                "refreshed opaque",
                "opaque-token".to_owned(),
                Some((usable_until, T0 - 500)),
                Some(usable_until),
            ),
            // A refreshed pair with a JWT: its `exp` less the skew allowance, when
            // that is sooner than the stored expiry.
            (
                "refreshed JWT",
                jwt(usable_until + SKEW_ALLOWANCE_SECONDS),
                Some((usable_until + 5_000, T0 - 500)),
                Some(usable_until),
            ),
            // An imported pair: only the JWT bound, or unknown.
            (
                "imported JWT",
                jwt(usable_until + SKEW_ALLOWANCE_SECONDS),
                None,
                Some(usable_until),
            ),
            ("imported unknown", "opaque-token".to_owned(), None, None),
        ];
        for (label, access, schedule, bound) in cases {
            let (temporary, store) = test_store();
            let generation =
                real_pair(temporary.path(), &store, &access, "refresh-token", schedule);
            let row = sign_in_row(generation);
            store
                .write_collector_auth_row(&row, T0 * 1_000)
                .expect("row");
            let before = tables(&store);
            let at = |now_ms: i64| {
                effective_auth_row(&store, temporary.path(), StoppedMode::Legacy, now_ms)
                    .expect("effective view")
                    .expect("a status")
            };
            match bound {
                Some(bound) => {
                    let usable = at(bound * 1_000 - 1);
                    assert_eq!(usable.row.state, "sign_in_required", "{label}");
                    assert!(usable.row.collecting, "{label}");
                    assert_eq!(usable.usable_until, Some(bound), "{label}");
                    let expired = at(bound * 1_000);
                    assert_eq!(expired.row.state, "signed_out", "{label}");
                    assert_eq!(expired.row.reason, row.reason, "{label}: same reason");
                    assert_eq!(
                        expired.row.stopped_generation,
                        Some(generation),
                        "{label}: same generation"
                    );
                    assert!(!expired.row.collecting, "{label}");
                    assert_eq!(expired.usable_until, None, "{label}");
                    assert_eq!(at(bound * 1_000 + 1).row.state, "signed_out", "{label}");
                }
                None => {
                    // An unknown bound stays usable, however late.
                    for now_ms in [T0 * 1_000, (T0 + 10_000_000) * 1_000] {
                        let view = at(now_ms);
                        assert_eq!(view.row.state, "sign_in_required", "{label}");
                        assert_eq!(view.usable_until, None, "{label}");
                    }
                }
            }
            // No write, no receipt, no binding, no fence, and the stored row says
            // what it said.
            assert_eq!(tables(&store), before, "{label}");
            assert_eq!(
                store.load_collector_auth_row().unwrap(),
                Some(row),
                "{label}"
            );
        }
    }

    #[test]
    fn cadence_effective_auth_view_follows_generation_clock_and_key_generation() {
        let usable_until = T0 + 100;
        let (temporary, store) = test_store();
        let generation = real_pair(
            temporary.path(),
            &store,
            "opaque-token",
            "refresh-token",
            Some((usable_until, T0 - 500)),
        );
        store
            .write_collector_auth_row(&sign_in_row(generation), T0 * 1_000)
            .expect("row");
        let view = |now_ms: i64| {
            effective_auth_row(&store, temporary.path(), StoppedMode::Legacy, now_ms)
                .expect("effective view")
                .expect("a status")
        };
        // A pair that is not the stopped generation lifts the stop in the view
        // only, however late the evaluation time.
        let external = real_pair(
            temporary.path(),
            &store,
            "opaque-token",
            "another-refresh-token",
            Some((usable_until, T0 - 500)),
        );
        assert_ne!(external, generation);
        let before = tables(&store);
        let lifted = view((usable_until + 50) * 1_000);
        assert_eq!(lifted.row.state, "ready");
        assert_eq!(lifted.usable_until, None);
        let stored = store.load_collector_auth_row().unwrap().expect("row");
        assert_eq!(
            stored.state, "sign_in_required",
            "the stored stop is not lifted"
        );
        assert_eq!(stored.stopped_generation, Some(generation));
        assert_eq!(tables(&store), before, "the view wrote nothing");

        // An invalid host clock never converts a stop: the stored word stands.
        store
            .write_collector_auth_row(&sign_in_row(external), T0 * 1_000)
            .expect("row");
        let wrong_clock = view(1_000_000);
        assert_eq!(wrong_clock.row.state, "sign_in_required");
        assert!(
            !wrong_clock.row.collecting,
            "a wrong clock never judges the token usable"
        );
        assert_eq!(view(usable_until * 1_000).row.state, "signed_out");
        // The previous key generation is read when an interrupted replacement left
        // the current one behind, without settling the replacement.
        let secrets = temporary.path().join("secrets");
        let current = crate::teslamate_credentials::key_path(temporary.path());
        let previous = secrets.join(".teslamate-encryption.previous.key");
        std::fs::rename(&current, &previous).expect("previous generation");
        std::fs::write(&current, b"a newer key that does not open the pair").expect("current");
        std::fs::set_permissions(
            &current,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .expect("mode");
        assert_eq!(
            view((usable_until - 10) * 1_000).row.state,
            "sign_in_required"
        );
        assert!(
            previous.exists(),
            "the interrupted replacement is not settled"
        );
        // A real key fault is an error, never a guess.
        std::fs::remove_file(&previous).expect("drop previous");
        assert!(
            effective_auth_row(
                &store,
                temporary.path(),
                StoppedMode::Legacy,
                usable_until * 1_000
            )
            .is_err()
        );
        // A refusal belongs to its mode: another mode lifts it in the view.
        let refused = CollectorAuthRow {
            state: "api_refused".to_owned(),
            reason: Some("owner_api".to_owned()),
            collecting: false,
            retry_at: None,
            stopped_generation: None,
            stopped_mode: Some("legacy".to_owned()),
            fuse_consecutive: 0,
            fuse_generation: None,
            fuse_last_rejection_at: None,
            fuse_pause_until: None,
            fuse_rejected_access_digest: None,
            clock_skew: false,
        };
        store
            .write_collector_auth_row(&refused, T0 * 1_000)
            .expect("row");
        let applies = effective_auth_row(&store, temporary.path(), StoppedMode::Legacy, T0 * 1_000)
            .expect("effective")
            .expect("status");
        assert_eq!(applies.row.state, "api_refused");
        let other = effective_auth_row(&store, temporary.path(), StoppedMode::Provider, T0 * 1_000)
            .expect("effective")
            .expect("status");
        assert_eq!(other.row.state, "ready");
        // The context-free reading of a non-collecting sign-in is the sign-out it
        // has become.
        let converted = stored_auth_view(CollectorAuthRow {
            collecting: false,
            ..sign_in_row(generation)
        });
        assert_eq!(converted.state, "signed_out");
        assert_eq!(converted.reason.as_deref(), Some("refresh_token_rejected"));
    }

    #[test]
    fn cadence_effective_auth_view_never_collects_on_a_wrong_clock() {
        // R38 with C5's own restore: usability needs a valid clock. A wrong clock
        // converts nothing (the stored word stands) yet no state that collects
        // only while the token is usable collects.
        let usable_until = T0 + 100;
        let (temporary, store) = test_store();
        let generation = real_pair(
            temporary.path(),
            &store,
            "opaque-token",
            "refresh-token",
            Some((usable_until, T0 - 500)),
        );
        let rows = [
            sign_in_row(generation),
            CollectorAuthRow {
                state: "persistence_pending".to_owned(),
                reason: None,
                stopped_generation: None,
                ..sign_in_row(generation)
            },
            CollectorAuthRow {
                state: "retrying".to_owned(),
                reason: Some("network".to_owned()),
                retry_at: Some(T0 + 50),
                stopped_generation: None,
                ..sign_in_row(generation)
            },
        ];
        for row in rows {
            store
                .write_collector_auth_row(&row, T0 * 1_000)
                .expect("row");
            let before = tables(&store);
            let at = |now_ms: i64| {
                effective_auth_row(&store, temporary.path(), StoppedMode::Legacy, now_ms)
                    .expect("effective view")
                    .expect("a status")
            };
            let valid = at((usable_until - 10) * 1_000);
            assert_eq!(valid.row.state, row.state);
            assert!(
                valid.row.collecting,
                "{}: usable on a valid clock",
                row.state
            );
            let wrong = at(1_000_000);
            assert_eq!(wrong.row.state, row.state, "{}: no conversion", row.state);
            assert!(!wrong.row.collecting, "{}: not on a wrong clock", row.state);
            assert_eq!(wrong.usable_until, None, "{}", row.state);
            assert_eq!(tables(&store), before, "{}: wrote nothing", row.state);
        }
    }

    #[test]
    fn cadence_effective_auth_view_stops_a_pair_that_fails_the_token_grammar() {
        // The pair decrypts, but its access token is not a b64token: the unit that
        // loads it stops signed out (R46). The view must say so, and neither keep
        // collecting nor lift an older stop to ready.
        let (temporary, store) = test_store();
        let generation = real_pair(
            temporary.path(),
            &store,
            "bad token",
            "refresh-token",
            Some((T0 + 1_000, T0 - 500)),
        );
        let other_generation = Uuid::from_u128(generation.as_u128() ^ 1);
        let mut states = vec![sign_in_row(generation)];
        // An external-write lift would otherwise show `ready`.
        states.push(CollectorAuthRow {
            state: "signed_out".to_owned(),
            reason: Some("access_token_rejected".to_owned()),
            collecting: false,
            stopped_generation: Some(other_generation),
            ..sign_in_row(generation)
        });
        states.push(CollectorAuthRow {
            state: "persistence_pending".to_owned(),
            reason: None,
            stopped_generation: None,
            ..sign_in_row(generation)
        });
        for row in states {
            store
                .write_collector_auth_row(&row, T0 * 1_000)
                .expect("row");
            let before = tables(&store);
            let view =
                effective_auth_row(&store, temporary.path(), StoppedMode::Legacy, T0 * 1_000)
                    .expect("effective view")
                    .expect("a status");
            assert_eq!(view.row.state, "signed_out", "{}", row.state);
            assert_eq!(view.row.reason.as_deref(), Some("stored_pair_invalid"));
            assert_eq!(view.row.stopped_generation, Some(generation));
            assert!(!view.row.collecting, "{}", row.state);
            assert_eq!(view.usable_until, None);
            assert_eq!(tables(&store), before, "{}: wrote nothing", row.state);
        }
        // A grammar-valid pair keeps the earlier answer.
        let (good_dir, good) = test_store();
        let good_generation = real_pair(
            good_dir.path(),
            &good,
            "good-token",
            "refresh-token",
            Some((T0 + 1_000, T0 - 500)),
        );
        good.write_collector_auth_row(&sign_in_row(good_generation), T0 * 1_000)
            .expect("row");
        let valid = effective_auth_row(&good, good_dir.path(), StoppedMode::Legacy, T0 * 1_000)
            .expect("effective view")
            .expect("a status");
        assert_eq!(valid.row.state, "sign_in_required");
        assert!(valid.row.collecting);
    }

    #[test]
    fn stored_pair_grammar_follows_the_mode() {
        use crate::legacy_auth::stored_pair_passes_grammar as passes;
        // Tesla mode needs a refresh token; Provider mode takes an access-token-only
        // pair (R8a). Both trim surrounding whitespace and refuse a bad access token.
        assert!(passes(
            " opaque-token\n",
            "refresh token",
            StoppedMode::Legacy
        ));
        assert!(!passes("opaque-token", "", StoppedMode::Legacy));
        assert!(passes("opaque-token", "", StoppedMode::Provider));
        assert!(!passes("bad token", "refresh", StoppedMode::Provider));
        assert!(!passes("opaque-token", "caf\u{e9}", StoppedMode::Provider));
    }
}

// ---------------------------------------------------------------------------
// B2 fix round, P3b.4 (CL-14, R-P3b-12): an equal-generation successor is proved by
// the exact row this attempt wrote together with its own receipt
// ---------------------------------------------------------------------------

/// The id of the newest receipt.
fn newest_receipt(store: &HubStore) -> crate::db::OutboundRequestReceiptId {
    let id = store
        .open()
        .expect("connection")
        .query_row("SELECT MAX(id) FROM outbound_request_receipts", [], |row| {
            row.get::<_, i64>(0)
        })
        .expect("a receipt");
    crate::db::OutboundRequestReceiptId(id)
}

#[test]
fn nc_receipt_proof_is_exact_and_preserves_row_precedence() {
    // The input pair, a reserved attempt, and a successor that kept the refresh
    // token: the same generation with another access token and schedule.
    let (_temporary, store) = test_store();
    let g0 = gen_id(1);
    put_pair(&store, g0);
    let receipt = store.begin_legacy_refresh(g0).unwrap();
    let successor = refreshed_pair(g0);
    let image = PairImage::of(&successor, g0);
    let own = |receipt| {
        Some(OwnWrite {
            receipt,
            image: &image,
        })
    };
    // Before the write: the row is still the input and the attempt is open.
    assert_eq!(
        persist_reread(&store, g0, g0, own(receipt)).unwrap(),
        PersistReread::StillInput
    );
    // The write commits (the answer is lost): the row is the image and the attempt's
    // own receipt is closed as a successful token refresh.
    store
        .complete_unrotated_legacy_refresh(receipt, g0, &successor)
        .unwrap();
    assert_eq!(
        persist_reread(&store, g0, g0, own(receipt)).unwrap(),
        PersistReread::ClosedForThisAttempt
    );
    // Without the attempt's own write the generation alone cannot tell, as before.
    assert_eq!(
        persist_reread(&store, g0, g0, None).unwrap(),
        PersistReread::AlreadyPersisted
    );

    // Another receipt, a missing one, or one that is not a closed success of a
    // Legacy token refresh proves nothing, however much the row matches.
    let successful = |transport: &str, operation: &str, outcome: &str| {
        put_receipt(&store, T0 * 1_000, outcome, transport, operation);
        newest_receipt(&store)
    };
    put_receipt(
        &store,
        T0 * 1_000,
        "success",
        "legacy_auth",
        "token_refresh",
    );
    let another_success = newest_receipt(&store);
    for (label, receipt) in [
        (
            "started",
            successful("legacy_auth", "token_refresh", "started"),
        ),
        (
            "wrong transport",
            successful("fleet_api", "token_refresh", "success"),
        ),
        (
            "wrong operation",
            successful("legacy_auth", "products", "success"),
        ),
        (
            "http error",
            successful("legacy_auth", "token_refresh", "http_error"),
        ),
        (
            "cancelled",
            successful("legacy_auth", "token_refresh", "cancelled"),
        ),
        (
            "missing id",
            crate::db::OutboundRequestReceiptId(receipt.0 + 10_000),
        ),
    ] {
        assert_eq!(
            persist_reread(&store, g0, g0, own(receipt)).unwrap(),
            PersistReread::StillInput,
            "{label}: no proof"
        );
    }
    // The function proves with the receipt it is handed, and a successful one of
    // another attempt would prove just as well: that the receipt is this attempt's
    // own is the caller's contract. The manager keeps it, handing over a witness only
    // for the attempt that wrote it (`own_write_for`); the manager-level test
    // `nc_another_attempts_success_receipt_is_not_this_attempts_proof` shows an
    // attempt that is handed none does not take another's success for its proof.
    assert_ne!(another_success, receipt);
    assert_eq!(
        persist_reread(&store, g0, g0, own(another_success)).unwrap(),
        PersistReread::ClosedForThisAttempt,
        "the receipt named is the proof, whichever attempt it is: the caller names its own"
    );

    // The row outranks the receipt. A row that is not the image, once the attempt is
    // closed, is an external write: the same generation with another access token,
    // another schedule or merely another encryption of the same pair.
    for (label, access, schedule) in [
        (
            "another access token",
            vec![3_u8; 8],
            (T0 + 28_800, T0 + 21_600),
        ),
        (
            "another schedule",
            vec![1_u8; 8],
            (T0 + 30_000, T0 + 25_000),
        ),
    ] {
        let replaced =
            TeslaMateLegacyTokenStore::refreshed(access, vec![8; 8], schedule.0, schedule.1)
                .unwrap()
                .with_credential_generation(g0)
                .unwrap();
        store.replace_teslamate_legacy_tokens(&replaced).unwrap();
        assert_eq!(
            persist_reread(&store, g0, g0, own(receipt)).unwrap(),
            PersistReread::External,
            "{label}"
        );
    }
    // Unbound, and a different generation.
    let unbound = TeslaMateLegacyTokenStore::imported(vec![3; 8], vec![4; 8]).unwrap();
    store.replace_teslamate_legacy_tokens(&unbound).unwrap();
    assert_eq!(
        persist_reread(&store, g0, g0, own(receipt)).unwrap(),
        PersistReread::External
    );
    put_pair(&store, gen_id(9));
    assert_eq!(
        persist_reread(&store, g0, g0, own(receipt)).unwrap(),
        PersistReread::External
    );
    // Removed: missing, whatever the receipt says.
    store.clear_teslamate_legacy_tokens().unwrap();
    assert_eq!(
        persist_reread(&store, g0, g0, own(receipt)).unwrap(),
        PersistReread::Missing
    );

    // An attempt that is still open: a row that is not the image is the input, and
    // the write is simply tried again.
    let (_open_temporary, open_store) = test_store();
    put_pair(&open_store, g0);
    let open_receipt = open_store.begin_legacy_refresh(g0).unwrap();
    assert_eq!(
        persist_reread(&open_store, g0, g0, own(open_receipt)).unwrap(),
        PersistReread::StillInput
    );
    // The unequal-generation rule is the generation's, as it was.
    assert_eq!(
        persist_reread(&open_store, g0, gen_id(2), own(open_receipt)).unwrap(),
        PersistReread::StillInput
    );
}
