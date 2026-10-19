// SPDX-License-Identifier: AGPL-3.0-only

//! The one HTTP-date reader of the auth unit: the `Date` header of an answer
//! (C5 R55) and, for the refresh schedule, a `Retry-After` given as a date
//! (R31, F10).
//!
//! Only the preferred form of RFC 9110 section 5.6.7, IMF-fixdate
//! (`Sun, 06 Nov 1994 08:49:37 GMT`), is read. Section 6.6.1 requires an origin
//! server to send `Date` in that form, and a value in another form is treated
//! as "no valid `Date`", which changes nothing (R55): a missed skew reading
//! costs a diagnostic, never a schedule. Nothing here panics on any input.

/// Day names in the order of `weekday_index` (Sunday first, as the weekday of
/// 1970-01-01, a Thursday, is derived from).
const DAY_NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// Month names in calendar order.
const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// 1970-01-01 was a Thursday: index 4 counting from Sunday.
const EPOCH_WEEKDAY_FROM_SUNDAY: i64 = 4;

/// Parse an IMF-fixdate into epoch seconds, or `None` for any other text, an
/// impossible date, a time outside 00:00:00 to 23:59:59, a year before 1970 or
/// a weekday that does not match the date. A leap second (`:60`) is refused:
/// the header is compared with a clock that never shows one.
pub(super) fn parse_http_date(text: &str) -> Option<i64> {
    let mut fields = text.split(' ');
    let day_name = fields.next()?.strip_suffix(',')?;
    let day = fixed_digits(fields.next()?, 2)?;
    let month_name = fields.next()?;
    let month = MONTH_NAMES.iter().position(|name| *name == month_name)? as i64 + 1;
    let year = fixed_digits(fields.next()?, 4)?;
    let mut clock = fields.next()?.split(':');
    let hour = fixed_digits(clock.next()?, 2)?;
    let minute = fixed_digits(clock.next()?, 2)?;
    let second = fixed_digits(clock.next()?, 2)?;
    if clock.next().is_some() || fields.next()? != "GMT" || fields.next().is_some() {
        return None;
    }
    if year < 1970 || !(1..=days_in_month(year, month)).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let weekday = (days + EPOCH_WEEKDAY_FROM_SUNDAY).rem_euclid(7);
    if DAY_NAMES[usize::try_from(weekday).ok()?] != day_name {
        return None;
    }
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// A number of exactly `width` ASCII digits, so a sign, a space or a shorter
/// or longer run is refused.
fn fixed_digits(text: &str, width: usize) -> Option<i64> {
    if text.len() != width || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if is_leap_year(year) => 29,
        _ => 28,
    }
}

/// Days from 1970-01-01 to a proleptic Gregorian date. The year starts in
/// March so the leap day is the last day of the cycle (Hinnant's civil-date
/// algorithm); a 400-year era has 146,097 days.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
