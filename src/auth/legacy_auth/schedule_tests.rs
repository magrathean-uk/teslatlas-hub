// SPDX-License-Identifier: AGPL-3.0-only

//! The schedule arithmetic of C5 R19 to R22, R27, R29a, R31 and R32 with exact
//! inputs (spec tests 18 to 24, 46 and 78 at the level of the numbers). The
//! behaviour that follows from them is tested with the manager, in
//! `auth/credentials/refresh_tests.rs`.

use serde_json::json;

use super::super::schedule::{
    DUE_GAP_SECONDS, RETRY_AFTER_MAX_SECONDS, RETRY_AFTER_MIN_SECONDS, RetryPlan, Schedule,
    lifetime_seconds, refreshed_form_due, retry_after_seconds, retry_delay,
};
use super::{T0, jwt};

/// An opaque new access token: no `iss`, no `exp`.
const OPAQUE: &str = "AT1";

/// A global-issuer JWT with this `exp`.
fn with_exp(exp: i64) -> String {
    jwt(json!({"iss": "https://auth.tesla.com/oauth2/v3", "exp": exp}))
}

#[test]
fn t18_an_eight_hour_answer_stores_the_documented_times() {
    // `expires_in` 28,800 and an opaque token sent at t0: usable for 8 hours, the
    // lead is a quarter of the span, 7,200 s.
    let schedule = Schedule::after_send(T0, Some(&json!(28_800)), OPAQUE);
    assert_eq!(
        schedule,
        Schedule {
            usable_until: 1_800_028_800,
            expires_at: 1_800_028_800,
            next_refresh_at: 1_800_021_600,
        }
    );
    assert!(!schedule.is_short(T0));
}

#[test]
fn t19_without_expires_in_the_jwt_exp_less_the_skew_allowance_gives_the_lifetime() {
    let token = with_exp(1_800_028_800);
    assert_eq!(lifetime_seconds(None, Some(1_800_028_800), T0), 28_500);
    let schedule = Schedule::after_send(T0, None, &token);
    assert_eq!(schedule.expires_at, 1_800_028_500);
    assert_eq!(schedule.next_refresh_at, 1_800_021_375);
    assert_eq!(schedule.usable_until, 1_800_028_500);
}

#[test]
fn t20_without_expires_in_or_exp_the_documented_eight_hours_apply() {
    let schedule = Schedule::after_send(T0, None, OPAQUE);
    assert_eq!(schedule.expires_at, 1_800_028_800);
    assert_eq!(schedule.next_refresh_at, 1_800_021_600);
}

#[test]
fn t21_a_short_lifetime_is_stored_as_it_is_and_reported_short() {
    let schedule = Schedule::after_send(T0, Some(&json!(30)), OPAQUE);
    assert_eq!(schedule.expires_at, 1_800_000_030);
    assert_eq!(schedule.next_refresh_at, 1_800_000_023);
    assert_eq!(schedule.usable_until, 1_800_000_030);
    assert!(schedule.is_short(T0));
    // 120 s is the smallest lifetime that is not short.
    assert!(Schedule::after_send(T0, Some(&json!(119)), OPAQUE).is_short(T0));
    assert!(!Schedule::after_send(T0, Some(&json!(120)), OPAQUE).is_short(T0));
}

#[test]
fn t21_a_very_long_lifetime_is_held_to_one_day() {
    let schedule = Schedule::after_send(T0, Some(&json!(10_000_000)), OPAQUE);
    assert_eq!(schedule.expires_at, 1_800_086_400);
    assert_eq!(schedule.next_refresh_at, 1_800_064_800);
    // A string of digits is read like the number, and so is one too big for an
    // integer.
    assert_eq!(
        Schedule::after_send(T0, Some(&json!("28800")), OPAQUE),
        Schedule::after_send(T0, Some(&json!(28_800)), OPAQUE)
    );
    assert_eq!(
        Schedule::after_send(T0, Some(&json!("99999999999999999999999")), OPAQUE).expires_at,
        1_800_086_400
    );
}

#[test]
fn t21_an_unusable_expires_in_is_treated_as_absent() {
    let fallback = Schedule::after_send(T0, None, OPAQUE);
    for value in [
        json!(0),
        json!(-5),
        json!(1.5),
        json!(28_800.0),
        json!(null),
        json!(""),
        json!("0"),
        json!("12a"),
        json!(" 5"),
        json!("-5"),
        json!(true),
        json!([30]),
    ] {
        assert_eq!(
            Schedule::after_send(T0, Some(&value), OPAQUE),
            fallback,
            "{value} must be treated as absent"
        );
    }
}

#[test]
fn t22_a_lifetime_that_is_negative_is_stored_at_the_minimum_span_and_is_unusable() {
    let token = with_exp(1_799_999_000);
    assert_eq!(lifetime_seconds(None, Some(1_799_999_000), T0), -1_300);
    let schedule = Schedule::after_send(T0, None, &token);
    assert_eq!(schedule.usable_until, 1_799_998_700);
    assert_eq!(schedule.expires_at, 1_800_000_002);
    assert_eq!(schedule.next_refresh_at, 1_800_000_001);
    assert!(schedule.is_short(T0));
    // The stored times meet the row's CHECK: expires_at > next_refresh_at > 0.
    assert!(schedule.expires_at > schedule.next_refresh_at && schedule.next_refresh_at > 0);
}

#[test]
fn t78_the_token_s_own_exp_bounds_usability_below_expires_in() {
    let token = with_exp(1_800_003_600);
    let schedule = Schedule::after_send(T0, Some(&json!(28_800)), &token);
    assert_eq!(schedule.usable_until, 1_800_003_300);
    assert_eq!(schedule.expires_at, 1_800_003_300);
    assert_eq!(schedule.next_refresh_at, 1_800_002_475);
    assert!(!schedule.is_short(T0));
}

#[test]
fn the_stored_times_always_meet_the_row_check_and_the_lead_is_at_least_one_second() {
    for expires_in in [1_i64, 2, 3, 4, 5, 7, 8, 59, 61, 600, 3_601, 28_800, 86_400] {
        for exp in [None, Some(T0 + 10), Some(T0 + 400), Some(T0 - 1_000)] {
            let token = exp.map_or_else(|| OPAQUE.to_owned(), with_exp);
            let schedule = Schedule::after_send(T0, Some(&json!(expires_in)), &token);
            assert!(
                schedule.expires_at - T0 >= 2
                    && schedule.next_refresh_at > T0
                    && schedule.expires_at > schedule.next_refresh_at,
                "{expires_in} {exp:?} {schedule:?}"
            );
            assert!(schedule.usable_until <= T0 + 86_400);
        }
    }
}

#[test]
fn t23_the_due_rule_of_the_refreshed_form_is_the_next_refresh_time() {
    assert!(!refreshed_form_due(
        1_800_028_800,
        1_800_021_600,
        1_800_021_599
    ));
    assert!(refreshed_form_due(
        1_800_028_800,
        1_800_021_600,
        1_800_021_600
    ));
}

#[test]
fn t24_a_row_that_lies_beyond_a_day_and_the_allowance_is_due_at_once() {
    assert_eq!(DUE_GAP_SECONDS, 86_700);
    assert!(!refreshed_form_due(T0 + 86_700, T0 + 64_800, T0));
    assert!(refreshed_form_due(T0 + 86_701, T0 + 64_801, T0));
}

#[test]
fn t45_the_backoff_doubles_to_the_cap_and_holds_there() {
    let mut step = 0;
    let mut delays = Vec::new();
    for _ in 0..7 {
        let (delay, next) = retry_delay(RetryPlan::Backoff, step, 0.0);
        delays.push(delay);
        step = next;
    }
    assert_eq!(delays, [60, 120, 240, 300, 300, 300, 300]);
}

#[test]
fn r31_the_spread_adds_at_most_ten_percent_and_never_a_negative_or_nan_amount() {
    assert_eq!(retry_delay(RetryPlan::Backoff, 0, 0.5).0, 63);
    assert_eq!(retry_delay(RetryPlan::Backoff, 0, 0.999_999).0, 65);
    assert_eq!(retry_delay(RetryPlan::Backoff, 3, 0.999_999).0, 329);
    for unit in [-1.0, f64::NAN, f64::INFINITY] {
        let delay = retry_delay(RetryPlan::Backoff, 1, unit).0;
        assert!((120..=132).contains(&delay), "{unit} gave {delay}");
    }
}

#[test]
fn t46_retry_after_replaces_the_delay_within_sixty_to_thirty_six_hundred_seconds() {
    assert_eq!(RETRY_AFTER_MIN_SECONDS, 60);
    assert_eq!(RETRY_AFTER_MAX_SECONDS, 3_600);
    assert_eq!(retry_delay(RetryPlan::RetryAfter(7_200), 0, 0.0).0, 3_600);
    assert_eq!(retry_delay(RetryPlan::RetryAfter(5), 0, 0.0).0, 60);
    assert_eq!(retry_delay(RetryPlan::RetryAfter(900), 0, 0.0).0, 900);
    assert_eq!(retry_delay(RetryPlan::RetryAfter(-30), 0, 0.0).0, 60);
}

#[test]
fn r32_the_slow_retry_and_the_client_check_do_not_grow_the_step() {
    assert_eq!(retry_delay(RetryPlan::Slow, 2, 0.5), (3_600, 2));
    assert_eq!(retry_delay(RetryPlan::ClientCheck, 0, 0.5), (60, 0));
}

fn headers(value: &str) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("retry-after", value.parse().unwrap());
    headers
}

#[test]
fn t46_retry_after_is_read_as_seconds_or_as_an_http_date() {
    assert_eq!(retry_after_seconds(&headers("7200"), T0), Some(7_200));
    assert_eq!(retry_after_seconds(&headers(" 5 "), T0), Some(5));
    // 2027-01-15T08:15:00Z is 900 s after t0.
    assert_eq!(
        retry_after_seconds(&headers("Fri, 15 Jan 2027 08:15:00 GMT"), T0),
        Some(900)
    );
    // A date in the past is a wait of zero or less, which the clamp raises to 60.
    assert_eq!(
        retry_after_seconds(&headers("Fri, 15 Jan 2027 07:00:00 GMT"), T0),
        Some(-3_600)
    );
    assert_eq!(
        retry_after_seconds(&headers("99999999999999999999999"), T0),
        Some(i64::MAX)
    );
    for bad in ["", "soon", "-5", "5s", "Friday"] {
        assert_eq!(retry_after_seconds(&headers(bad), T0), None, "{bad:?}");
    }
    assert_eq!(
        retry_after_seconds(&reqwest::header::HeaderMap::new(), T0),
        None
    );
}
