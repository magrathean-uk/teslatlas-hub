// SPDX-License-Identifier: AGPL-3.0-only

//! The refresh schedule and the retry policy of the legacy credential unit
//! (C5 R19 to R22, R27, R29a, R31, R32).
//!
//! Everything here is arithmetic on epoch seconds. Nothing reads a clock, sends
//! a request or touches the store, so every number of the specification can be
//! checked with exact inputs. Times are `i64` epoch seconds (R37).

use std::time::Duration;

use reqwest::header::HeaderMap;
use serde_json::Value;

use super::{
    endpoint::parse_jwt,
    http_date::parse_http_date,
    state::{MIN_REFRESH_INTERVAL_SECONDS, SKEW_ALLOWANCE_SECONDS},
    vocabulary::RetryReason,
};

/// The lifetime assumed when the answer names none and the access token is not
/// a JWT with an `exp` (R20 step 3): the documented 8 hours of a Tesla access
/// token (F5). If it is too long, the 401 path recovers with one rejected
/// request (R41).
pub(crate) const LIFETIME_FALLBACK_SECONDS: i64 = 28_800;

/// The longest lifetime the schedule honours (R20). It keeps every stored time
/// within a day of now, which the due rule of R27 relies on, and it means a
/// wrong long lifetime from a provider defers the next proof that the refresh
/// chain works by at most a day.
pub(crate) const SCHEDULE_CEILING_SECONDS: i64 = 86_400;

/// The smallest span stored between the send time and `expires_at` (R21): the
/// smallest span that leaves room for a lead of 1 s under the stored row's
/// `expires_at > next_refresh_at > 0` CHECK. It is stored only when the token
/// is already unusable, which R29a reports.
pub(crate) const MIN_STORED_SPAN_SECONDS: i64 = 2;

/// A successor that is usable for less than this is short-lived (R29a): the
/// 60 s minimum interval of R19 plus the 60 s exchange budget of R33 is 120 s,
/// so a shorter token cannot be renewed before it expires.
pub(crate) const SHORT_LIFETIME_SECONDS: i64 = 120;

/// How far beyond now a stored `expires_at` may lie before the row is treated
/// as damaged (R27): the schedule ceiling plus the skew allowance. R20 to R22
/// never store an `expires_at` more than the ceiling after its send time, so a
/// larger gap means the clock moved backwards by more than R54's allowance.
pub(crate) const DUE_GAP_SECONDS: i64 = SCHEDULE_CEILING_SECONDS + SKEW_ALLOWANCE_SECONDS;

/// The first retry delay (R31): a refresh that fails on a momentary loss (Wi-Fi
/// roaming, a DHCP renewal) usually succeeds within a minute. It equals the
/// minimum refresh interval of R19, so a retry is never refused by that gate.
pub(crate) const RETRY_BASE_SECONDS: i64 = MIN_REFRESH_INTERVAL_SECONDS;

/// The longest backoff delay (R31): the longest the Hub idles after
/// connectivity returns once the token has expired, with at most 288 small
/// requests a day during a long outage. Legacy collection recovers nothing
/// missed, so the cap is short.
pub(crate) const RETRY_CAP_SECONDS: i64 = 300;

/// The step at which the doubling of the delay reaches the cap (R31): 60, 120,
/// 240, then 480, which the cap holds at 300. A higher step adds nothing.
pub(crate) const RETRY_STEP_LIMIT: u8 = 3;

/// The random share added to a backoff delay, in percent of it (R31). It keeps
/// Hubs that failed together in one Tesla outage from retrying in lockstep.
pub(crate) const RETRY_SPREAD_PERCENT: i64 = 10;

/// The shortest wait a `Retry-After` header may ask for (R31): the first
/// backoff step, so a header never makes the unit retry sooner than it would
/// have anyway.
pub(crate) const RETRY_AFTER_MIN_SECONDS: i64 = 60;

/// The longest wait a `Retry-After` header may ask for (R31): it stops a wrong
/// header from parking the refresh past most of a token's remaining life.
pub(crate) const RETRY_AFTER_MAX_SECONDS: i64 = 3_600;

/// The wait after an answer that signals a request or routing problem (R32):
/// neither the credentials nor a short wait will fix it, but a server-side fix
/// or a configuration change should be picked up without a restart, and an
/// hourly probe is a negligible load.
pub(crate) const SLOW_RETRY_SECONDS: i64 = 3_600;

/// The wait between the two answers that refuse the client id (R45 row 7): one
/// confirming retry separates a deployment fault from a withdrawal at the cost
/// of a minute, the spacing of C3's data-plane confirmation.
pub(crate) const CLIENT_CHECK_SECONDS: i64 = 60;

/// How many consecutive answers refusing the client id stop the account (R45
/// row 7): the first is a confirmation, the second is the answer.
pub(crate) const CLIENT_REFUSAL_LIMIT: u8 = 2;

/// The longest a caller waits for an exchange in flight (R15, R33): the total
/// timeout of the auth client, so the exchange has ended by then.
pub(crate) const EXCHANGE_WAIT_AT_MOST: Duration = Duration::from_secs(60);

/// The `expires_in` member as a lifetime in seconds (R20 step 1): a JSON
/// integer, or a JSON string of ASCII digits, with a value of at least 1.
/// Anything else (a float, `null`, zero, a negative number, text with other
/// characters) is not a usable lifetime. A value too large for `i64` is still
/// "at least 1" and is returned as the largest, which the schedule ceiling
/// then bounds.
fn expires_in_seconds(value: &Value) -> Option<i64> {
    let seconds = match value {
        Value::Number(number) => {
            let unsigned = number.as_u64()?;
            i64::try_from(unsigned).unwrap_or(i64::MAX)
        }
        Value::String(text) => {
            if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            text.parse::<i64>().unwrap_or(i64::MAX)
        }
        _ => return None,
    };
    (seconds >= 1).then_some(seconds)
}

/// The lifetime `L` of a successor, in seconds (R20): `expires_in` when it is
/// usable, else the JWT's `exp` less the skew allowance and the send time (which
/// may be zero or negative), else the documented 8 hours.
///
/// `expires_in` is relative to the response and so immune to clock skew; F6
/// makes it optional, so `exp` (absolute, on the server's clock) is the next
/// source, less the allowance of R54.
pub(crate) fn lifetime_seconds(expires_in: Option<&Value>, jwt_exp: Option<i64>, t_s: i64) -> i64 {
    if let Some(seconds) = expires_in.and_then(expires_in_seconds) {
        return seconds;
    }
    if let Some(exp) = jwt_exp {
        return exp
            .saturating_sub(SKEW_ALLOWANCE_SECONDS)
            .saturating_sub(t_s);
    }
    LIFETIME_FALLBACK_SECONDS
}

/// The times that follow from one successful refresh (R20 to R22).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Schedule {
    /// The epoch second after which the access token is not sent (R21, R29).
    pub usable_until: i64,
    /// The stored `expires_at`.
    pub expires_at: i64,
    /// The stored `next_refresh_at`.
    pub next_refresh_at: i64,
}

impl Schedule {
    /// The schedule of a successor whose request was sent at `t_s` (R18, R20 to
    /// R22). `expires_in` is the answer's member as received.
    ///
    /// `L_s = min(L, 86,400)` with no lower clamp: raising a short lifetime
    /// made the Hub send a token for up to 570 s after it had really expired,
    /// and those 401s were counted (C5R-08). Short lifetimes are reported by
    /// [`Schedule::is_short`] instead. `u = t_s + L_s`, lowered to `exp - 300`
    /// for a JWT with an `exp`; `usable_until = u` and the stored
    /// `expires_at = max(u, t_s + 2)`. The lead is a quarter of the stored span,
    /// at least 1 s: for Tesla's 8 hour tokens that is 7,200 s, which leaves room
    /// for 25 retries on the R31 schedule before expiry, and a fraction scales
    /// with whatever lifetime a provider issues where a fixed margin would be
    /// too large for short tokens and too small for long ones.
    pub(crate) fn after_send(t_s: i64, expires_in: Option<&Value>, access_token: &str) -> Self {
        let exp = parse_jwt(access_token).and_then(|claims| claims.exp);
        let lifetime = lifetime_seconds(expires_in, exp, t_s).min(SCHEDULE_CEILING_SECONDS);
        let mut usable_until = t_s.saturating_add(lifetime);
        if let Some(exp) = exp {
            usable_until = usable_until.min(exp.saturating_sub(SKEW_ALLOWANCE_SECONDS));
        }
        let expires_at = usable_until.max(t_s.saturating_add(MIN_STORED_SPAN_SECONDS));
        let lead = ((expires_at - t_s) / 4).max(1);
        Self {
            usable_until,
            expires_at,
            next_refresh_at: expires_at - lead,
        }
    }

    /// R29a: the successor is usable for less than [`SHORT_LIFETIME_SECONDS`]
    /// from its send time, so it cannot be renewed before it expires. Such a
    /// token is persisted as usual (rejecting the answer would discard the only
    /// valid refresh token, R50) and the next attempt follows the retry
    /// schedule instead of `next_refresh_at`.
    pub(crate) fn is_short(&self, t_s: i64) -> bool {
        self.usable_until.saturating_sub(t_s) < SHORT_LIFETIME_SECONDS
    }
}

/// R27: whether a pair of the refreshed form is due. It is due when
/// `now >= next_refresh_at`, and at once when `expires_at` lies more than
/// [`DUE_GAP_SECONDS`] ahead, which R20 to R22 never produce: the clock moved
/// backwards, or the row is damaged, and one refresh rebuilds a sound schedule.
pub(crate) fn refreshed_form_due(expires_at: i64, next_refresh_at: i64, now: i64) -> bool {
    now >= next_refresh_at || expires_at > now.saturating_add(DUE_GAP_SECONDS)
}

/// How the next attempt after a retryable outcome is timed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetryPlan {
    /// R31: `min(60 * 2^step, 300)` seconds plus the spread, and the step grows.
    Backoff,
    /// R31: a `Retry-After` of this many seconds, clamped to 60 to 3,600.
    RetryAfter(i64),
    /// R32: an hour, with no growth of the step.
    Slow,
    /// R45 row 7: one fixed confirming minute, with no growth of the step.
    ClientCheck,
}

/// The delay of a retry, in whole seconds, and the step that follows it.
///
/// `spread_unit` is a number in `[0, 1)` from the injectable random source of
/// the environment (tests use 0); the spread is that share of
/// [`RETRY_SPREAD_PERCENT`] of the backoff delay, rounded down.
pub(crate) fn retry_delay(plan: RetryPlan, step: u8, spread_unit: f64) -> (i64, u8) {
    match plan {
        RetryPlan::Backoff => {
            let step = step.min(RETRY_STEP_LIMIT);
            let base = (RETRY_BASE_SECONDS << step).min(RETRY_CAP_SECONDS);
            let unit = if spread_unit.is_finite() {
                spread_unit.clamp(0.0, 1.0)
            } else {
                0.0
            };
            let spread = (base as f64 * RETRY_SPREAD_PERCENT as f64 / 100.0 * unit) as i64;
            (base + spread, step.saturating_add(1).min(RETRY_STEP_LIMIT))
        }
        RetryPlan::RetryAfter(seconds) => (
            seconds.clamp(RETRY_AFTER_MIN_SECONDS, RETRY_AFTER_MAX_SECONDS),
            step.saturating_add(1).min(RETRY_STEP_LIMIT),
        ),
        RetryPlan::Slow => (SLOW_RETRY_SECONDS, step),
        RetryPlan::ClientCheck => (CLIENT_CHECK_SECONDS, step),
    }
}

/// The `Retry-After` header as a number of seconds from now (R31, F10, RFC 9110
/// section 10.2.3): `delay-seconds`, or an HTTP-date, whose wait is the date
/// minus `now` (zero or negative when it is not in the future). Anything else
/// is no header. A value too large for `i64` is returned as the largest, which
/// the clamp of [`retry_delay`] bounds.
pub(crate) fn retry_after_seconds(headers: &HeaderMap, now: i64) -> Option<i64> {
    let value = headers.get("retry-after")?.to_str().ok()?.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(value.parse::<i64>().unwrap_or(i64::MAX));
    }
    parse_http_date(value).map(|date| date.saturating_sub(now))
}

/// The retry plan an answer class asks for (R45 table, "Next" column) and the
/// reason shown while it waits. Only retryable classes have one.
pub(crate) fn plan_for_reason(reason: RetryReason) -> RetryPlan {
    match reason {
        RetryReason::RefreshRequestRejected | RetryReason::TokenEndpointMisrouted => {
            RetryPlan::Slow
        }
        RetryReason::ClientCheck => RetryPlan::ClientCheck,
        RetryReason::Network
        | RetryReason::SensitiveAccess
        | RetryReason::TeslaUnavailable
        | RetryReason::RefreshBlocked
        | RetryReason::ShortTokenLifetime
        | RetryReason::ClockSkew
        | RetryReason::RefreshNotRenewed
        | RetryReason::StoreUnavailable => RetryPlan::Backoff,
    }
}
