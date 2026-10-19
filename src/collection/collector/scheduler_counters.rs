// SPDX-License-Identifier: AGPL-3.0-only

// Fleet request counter, estimate and local cap (C3 part K7: R24 to R27).
//
// Every scheduled request has a category (R24) and is counted per UTC month
// (R25). In Fleet polling each request is billed by Tesla, so the counter is
// also the cost estimate (R26) and the local cap that scheduled polling never
// exceeds (R27). The counts live in a [`RequestLedger`], a cloneable handle on
// one shared state, because three kinds of sender must see the same numbers: the
// scheduler's tick, the Fleet helpers that may resend a request after a token
// refresh, and the resident wake and command actions that run beside the tick
// (R25: other senders add their counts through the same API). One-shot senders
// (the setup commands) build a standalone ledger from the store and flush it.
//
// The scheduler stays the only place that decides what to send (R27): the loops
// ask `admit_request` before each request and `plan_reservation` before a tick's
// requests. The ledger counts, gates and estimates; it sends nothing and writes
// nothing by itself. The tick transaction of `tick.rs` writes its rows
// (`attach_counters`).
//
// It is the fifth file of `impl VehicleScheduler`, kept apart so `scheduler.rs`
// stays under the layout gate.

// ---------------------------------------------------------------------------
// Constants. Every value carries its reason and the requirement that gives it.
// ---------------------------------------------------------------------------

/// Cost units `U` in one US dollar (C3 R26): 150,000, the least common multiple
/// of Tesla's published per-dollar counts (500 data, 1,000 commands, 50 wakes and
/// 150,000 signals), so every published price is an exact whole number of units
/// and the cost arithmetic is integer and exactly testable.
const COST_UNITS_PER_DOLLAR: u64 = 150_000;

/// Cost units in one US cent (C3 R26: `gross_cents = ceil(gross_units / 1,500)`).
const COST_UNITS_PER_CENT: u64 = COST_UNITS_PER_DOLLAR / 100;

/// The largest step by which a long-running process raises a category's persisted
/// reservation (C3 R27, `K_RESERVE_BLOCK`): 20 requests bound the over-count after
/// a crash to $0.04 at the data rate, and a block of that size lets a process
/// that sends a few requests a minute reserve rarely (one lone commit per 20
/// requests at most) instead of committing before every tick.
const K_RESERVE_BLOCK: u64 = 20;

/// The share of the local cap at which status carries a warning, in percent (C3
/// R27, `K_CAP_WARN` = 0.8): it mirrors Tesla's own notice at 80 % of the billing
/// limit and gives the owner time to raise the cap.
const K_CAP_WARN_PERCENT: u64 = 80;

/// How long Owner-wire counters may wait for a transaction of their own (C3 R25,
/// `K_COUNTER_FLUSH`): the counts carry no money, so a crash may lose up to an hour
/// of them, and no durable write is made per empty poll (the Idle target).
const K_COUNTER_FLUSH: Duration = Duration::from_secs(60 * 60);

/// The shortest elapsed time at which the projection and the stop date are shown
/// (C3 R26): a projection from less than a day is dominated by one session.
const K_PROJECTION_MIN_ELAPSED_MS: i64 = 24 * 60 * 60 * 1_000;

/// Tesla's monthly discount in US cents (C3 R26, Tesla's billing page: a $10
/// monthly discount). It is shown as `net = max(0, gross - discount)` and never
/// used for the cap, because the discount is per account and other applications
/// may consume it.
const TESLA_MONTHLY_DISCOUNT_CENTS: u64 = 1_000;

/// The idle window of the consent example (C3 R17, worked example): the Hub-native
/// default of `suspend_after_idle_min`, 15 minutes (R58), during which a parked
/// car is read at the Online interval.
const CONSENT_IDLE_WINDOW: Duration = Duration::from_secs(15 * 60);

/// Hours of driving in the consent example (C3 R17): one hour.
const CONSENT_DRIVING_SECONDS: u64 = 60 * 60;

/// Hours of charging in the consent example (C3 R17): two hours.
const CONSENT_CHARGING_SECONDS: u64 = 2 * 60 * 60;

/// Parkings a day in the consent example (C3 R17): four, each with a full idle
/// window and one wake read.
const CONSENT_PARKINGS: u64 = 4;

/// The status line about the scope of the local cap (C3 R27, review S2): setup and
/// diagnostics requests are counted and shown, never blocked, because blocking
/// them at the cap would stop the owner from fixing what the status asks them to
/// fix.
const CAP_SCOPE_NOTE: &str = "The local cap governs scheduled polling.";

/// What status says before the projection is shown (C3 R26).
const PROJECTION_AFTER_FIRST_DAY: &str = "projection after the first day";

// ---------------------------------------------------------------------------
// Categories and prices (C3 R24).
// ---------------------------------------------------------------------------

impl RequestCategory {
    const ALL: [Self; 4] = [Self::Data, Self::State, Self::Wake, Self::Command];

    const fn index(self) -> usize {
        match self {
            Self::Data => 0,
            Self::State => 1,
            Self::Wake => 2,
            Self::Command => 3,
        }
    }

    /// The value stored in `collector_request_counters.category` (C3 R4).
    const fn code(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::State => "state",
            Self::Wake => "wake",
            Self::Command => "command",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.code() == code)
    }
}

/// The price of one request of `category` in cost units, or `None` when Tesla does
/// not bill it (C3 R24, R26: `FleetPricing.state` is `None` while state checks are
/// free). The division rounds up, so a price that does not divide the unit is
/// never understated; Tesla's published rates divide it exactly.
fn price_units(pricing: &crate::config::FleetPricing, category: RequestCategory) -> Option<u64> {
    let per_dollar = match category {
        RequestCategory::Data => Some(pricing.data),
        RequestCategory::State => pricing.state,
        RequestCategory::Wake => Some(pricing.wake),
        RequestCategory::Command => Some(pricing.command),
    }?;
    Some(COST_UNITS_PER_DOLLAR.div_ceil(u64::from(per_dollar).max(1)))
}

/// Whether Tesla bills a Fleet request that ended in `error` (C3 R24): a status
/// below 500 is billed, 500 and above is not; a timeout or transport error after
/// the request was handed to the network is billed (unknown delivery, which errs
/// towards over-estimating, the safe side for a cap); a request that was never
/// sent is not. Every variant is listed, so a new one must be decided here.
fn fleet_error_billable(error: &FleetApiError) -> bool {
    match error {
        // Never sent: rejected before the network.
        FleetApiError::RequestNotSent
        | FleetApiError::InvalidEndpoint
        | FleetApiError::InvalidCommand
        | FleetApiError::InvalidTelemetryConfig
        | FleetApiError::TelemetryConfigTooLarge
        | FleetApiError::CommandProxyUnavailable
        | FleetApiError::InvalidChargeLimit => false,
        // Unknown delivery.
        FleetApiError::RequestTimeout | FleetApiError::Transport => true,
        FleetApiError::HttpStatus(status) | FleetApiError::ProviderHttpStatus { status, .. } => {
            *status < 500
        }
        // A 429 is below 500.
        FleetApiError::RateLimited { .. } => true,
        // Tesla answered below 500 and the Hub rejected the answer.
        FleetApiError::ResponseTooLarge
        | FleetApiError::ResponseRead
        | FleetApiError::InvalidResponse
        | FleetApiError::CommandRejected => true,
    }
}

/// [`fleet_error_billable`] for a whole result: a success is billed.
fn fleet_result_billable<T>(result: &Result<T, FleetApiError>) -> bool {
    result.as_ref().map_or_else(fleet_error_billable, |_| true)
}

/// Whether a request was attempted at all (C3 R24): the errors that arise before a
/// request exists (an invalid endpoint, command or configuration, no command
/// proxy) are no request and no count. `RequestNotSent` is different: the request
/// was handed to the client and is counted as sent, but it is not billed.
fn fleet_result_attempted<T>(result: &Result<T, FleetApiError>) -> bool {
    !matches!(
        result,
        Err(FleetApiError::InvalidEndpoint
            | FleetApiError::InvalidCommand
            | FleetApiError::InvalidTelemetryConfig
            | FleetApiError::TelemetryConfigTooLarge
            | FleetApiError::CommandProxyUnavailable
            | FleetApiError::InvalidChargeLimit)
    )
}

// ---------------------------------------------------------------------------
// The calendar month (C3 R25).
// ---------------------------------------------------------------------------

/// One UTC calendar month: its key `YYYY-MM` and its first and last-plus-one
/// instants in epoch milliseconds (C3 R25: Tesla's billing cycle starts on the
/// first day of the month without a stated zone, so the zone-neutral reading is
/// UTC).
#[derive(Clone, Debug, PartialEq, Eq)]
struct MonthSpan {
    key: String,
    start_ms: i64,
    end_ms: i64,
}

impl MonthSpan {
    fn from_year_month(year: i32, month: ::time::Month) -> Option<Self> {
        let first = ::time::Date::from_calendar_date(year, month, 1).ok()?;
        let (next_year, next_month) = if month == ::time::Month::December {
            (year.checked_add(1)?, ::time::Month::January)
        } else {
            (year, month.next())
        };
        let next = ::time::Date::from_calendar_date(next_year, next_month, 1).ok()?;
        let millis = |date: ::time::Date| {
            date.midnight()
                .assume_utc()
                .unix_timestamp()
                .checked_mul(1_000)
        };
        Some(Self {
            key: format!("{year:04}-{:02}", u8::from(month)),
            start_ms: millis(first)?,
            end_ms: millis(next)?,
        })
    }

    /// The month that contains `epoch_ms`, if it is in the calendar's range.
    fn containing(epoch_ms: i64) -> Option<Self> {
        let at = ::time::OffsetDateTime::from_unix_timestamp(epoch_ms.div_euclid(1_000)).ok()?;
        Self::from_year_month(at.year(), at.month())
    }

    /// The month a stored key `YYYY-MM` names.
    fn of_key(key: &str) -> Option<Self> {
        let (year, month) = key.split_once('-')?;
        Self::from_year_month(
            year.parse().ok()?,
            ::time::Month::try_from(month.parse::<u8>().ok()?).ok()?,
        )
    }

    fn length_ms(&self) -> i64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// What a month refresh found (C3 R25).
#[derive(Clone, Debug, PartialEq, Eq)]
enum MonthEvent {
    Unchanged,
    /// A new month began: the counts are back to zero and `from` is closed.
    Rolled {
        from: String,
        to: String,
    },
}

// ---------------------------------------------------------------------------
// The ledger.
// ---------------------------------------------------------------------------

/// Why a scheduled request was not started (C3 R27).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Withheld {
    /// The local monthly cap forbids it: gross plus its price would pass the cap,
    /// or the reservation cannot grow within the cap.
    Cap,
    /// The persisted reservation does not cover it: plan, commit, ask again.
    Reservation,
    /// The account is not collecting (an owner-action state or a wait).
    Account,
}

/// The decision on one scheduled request (C3 R27).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    Admitted,
    Withheld(Withheld),
}

/// One category's counts for the current month (C3 R25, R27).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CategoryCount {
    /// Requests started this month (also: every one that may have been billed). The
    /// stored `sent` is this less `in_flight`: only requests whose outcome is known
    /// (C3 R25, B2 clarification).
    sent: u64,
    /// Requests that Tesla bills by R24, counted at completion (Fleet polling).
    billable: u64,
    /// Requests started and not completed.
    in_flight: u64,
    /// The reservation as it will be committed next (C3 R27).
    reserved_pending: u64,
    /// The reservation the store holds: the only one sends may rely on, because a
    /// crash leaves exactly this one behind.
    reserved_persisted: u64,
}

/// The persisted shape of a category's row (C3 R4): memory differs from the store
/// exactly when the row must be written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CounterView {
    sent: u64,
    billable: u64,
    reserved: u64,
}

struct LedgerState {
    mode: crate::config::CollectorMode,
    pricing: crate::config::FleetPricing,
    /// `monthly_cap_cents x 1,500` (C3 R27).
    cap_units: u64,
    clock: Arc<dyn CollectorClock>,
    span: MonthSpan,
    /// The (monotonic, wall) pair a Fleet month change is projected from (C3 R25).
    anchor: (Instant, i64),
    /// A forward divergence has been seen since the last month change: the anchor
    /// stays where it is (C3 R25, S4).
    frozen: bool,
    deferred_logged: bool,
    counts: [CategoryCount; 4],
    persisted: [CounterView; 4],
    /// Requests started by this process, never reset by a month change: the size of
    /// the next reservation block follows it (C3 R27).
    started_since_start: [u64; 4],
    warned: bool,
    last_flush: Instant,
}

impl LedgerState {
    fn billed(&self, category: RequestCategory) -> bool {
        self.mode == crate::config::CollectorMode::FleetPolling
            && price_units(&self.pricing, category).is_some()
    }

    fn price(&self, category: RequestCategory) -> u64 {
        price_units(&self.pricing, category).unwrap_or(0)
    }

    /// The month's gross cost in units: billable requests, and with
    /// `include_in_flight` the ones started and not yet completed, which the cap
    /// must count (C3 R27: concurrent requests must not pass it together).
    fn gross_units(&self, include_in_flight: bool) -> u64 {
        RequestCategory::ALL
            .into_iter()
            .filter(|category| self.billed(*category))
            .fold(0u64, |total, category| {
                let count = &self.counts[category.index()];
                let requests = count.billable.saturating_add(if include_in_flight {
                    count.in_flight
                } else {
                    0
                });
                total.saturating_add(requests.saturating_mul(self.price(category)))
            })
    }

    /// The requests of `category` that a crash could leave counted when the
    /// reservation is `reserved` (C3 R27, B2 clarification 2026-10-12): the larger
    /// of the billable requests plus those in flight, and the reservation less the
    /// requests already answered and known not to be billed (`not_billed`: sent,
    /// not in flight, not billable). The restart converts `reserved - sent` of the
    /// stored row (see `load`), so an answered 5xx or unsent request never counts
    /// again, while a request still in flight at the last commit always does.
    fn exposure_requests(count: &CategoryCount, reserved: u64) -> u64 {
        let not_billed = count
            .sent
            .saturating_sub(count.in_flight)
            .saturating_sub(count.billable);
        count
            .billable
            .saturating_add(count.in_flight)
            .max(reserved.saturating_sub(not_billed))
    }

    /// The cost of the month's exposure (C3 R27): the sum over billed categories of
    /// [`Self::exposure_requests`] at the pending reservation times the price. The
    /// reservation never lets it pass the cap.
    fn exposure_units(&self, except: Option<RequestCategory>) -> u64 {
        RequestCategory::ALL
            .into_iter()
            .filter(|category| self.billed(*category) && Some(*category) != except)
            .fold(0u64, |total, category| {
                let count = &self.counts[category.index()];
                total.saturating_add(
                    Self::exposure_requests(count, count.reserved_pending)
                        .saturating_mul(self.price(category)),
                )
            })
    }

    /// The largest reservation `category` can hold while the exposure of all
    /// categories stays within the cap (C3 R27). Below the category's `sent`, the
    /// month's gross cannot take the request at all.
    fn max_reserved(&self, category: RequestCategory) -> u64 {
        let count = &self.counts[category.index()];
        let not_billed = count
            .sent
            .saturating_sub(count.in_flight)
            .saturating_sub(count.billable);
        let affordable = self.cap_units.saturating_sub(self.exposure_units(Some(category)))
            / self.price(category).max(1);
        affordable.saturating_add(not_billed)
    }

    fn pending_reservation(&self) -> bool {
        self.counts
            .iter()
            .any(|count| count.reserved_pending > count.reserved_persisted)
    }

    /// Decide and, when admitted, count one scheduled request (C3 R25, R27). In
    /// the Owner-wire modes and for a category Tesla does not bill the request is
    /// only counted. Otherwise it is admitted only when the cap and the persisted
    /// reservation allow it: the cap is checked first, so a request that cannot be
    /// afforded is a cap (and ends polling for the month), never a wait for a
    /// reservation.
    fn start(&mut self, category: RequestCategory) -> Admission {
        if self.billed(category) {
            let price = self.price(category);
            if self.gross_units(true).saturating_add(price) > self.cap_units {
                return Admission::Withheld(Withheld::Cap);
            }
            let count = self.counts[category.index()];
            if count.sent >= count.reserved_persisted {
                // A raise that waits for its commit, or one the plan can still
                // make, is a wait; a reservation that cannot grow within the cap
                // is the cap: the month could already count to it after a crash.
                // The raise the plan makes first lifts the reservation to `sent`
                // (a converted restart count or non-billable sends can stand above
                // it, see `plan`), so the exposure must cover that lift and one
                // more request.
                let wanted = count.reserved_pending.max(count.sent).saturating_add(1);
                return if count.reserved_pending > count.reserved_persisted
                    || self.max_reserved(category) >= wanted
                {
                    Admission::Withheld(Withheld::Reservation)
                } else {
                    Admission::Withheld(Withheld::Cap)
                };
            }
        }
        let index = category.index();
        let count = &mut self.counts[index];
        count.sent = count.sent.saturating_add(1);
        count.in_flight = count.in_flight.saturating_add(1);
        self.started_since_start[index] = self.started_since_start[index].saturating_add(1);
        self.check_cap_warning();
        Admission::Admitted
    }

    /// Take back a start whose request was never sent (a credential failure before
    /// the network): it is no request, so it is no count.
    fn unstart(&mut self, category: RequestCategory) {
        let index = category.index();
        let count = &mut self.counts[index];
        count.sent = count.sent.saturating_sub(1);
        count.in_flight = count.in_flight.saturating_sub(1);
        self.started_since_start[index] = self.started_since_start[index].saturating_sub(1);
    }

    /// A started request completed (C3 R24, R25): `billable` is whether Tesla
    /// bills the outcome. The count never passes `sent`, and the reservation never
    /// stays below it, which are the table's CHECKs.
    fn complete(&mut self, category: RequestCategory, billable: bool) {
        let billed = self.billed(category);
        let count = &mut self.counts[category.index()];
        count.in_flight = count.in_flight.saturating_sub(1);
        if billed && billable && count.billable < count.sent {
            count.billable += 1;
            count.reserved_pending = count.reserved_pending.max(count.billable);
        }
        self.check_cap_warning();
    }

    /// A request nobody admitted, counted at completion (C3 R25, R27 scope: setup,
    /// diagnostics and resident actions are counted and shown, never blocked). It
    /// raises the pending reservation to what has been billed, so the table's CHECK
    /// `reserved >= billable` holds. It counts in `sent` too, which uses up the
    /// scheduled path's reservation (a send needs `sent < reserved`), so the scheduled
    /// path becomes more careful, never less.
    fn record_completed(&mut self, category: RequestCategory, billable: bool) {
        let billed = self.billed(category);
        let count = &mut self.counts[category.index()];
        count.sent = count.sent.saturating_add(1);
        if billed {
            if billable {
                count.billable = count.billable.saturating_add(1);
            }
            count.reserved_pending = count.reserved_pending.max(count.billable);
        }
        self.check_cap_warning();
    }

    /// One warning a month when the gross reaches `K_CAP_WARN` of the cap (C3 R27).
    fn check_cap_warning(&mut self) {
        if self.mode != crate::config::CollectorMode::FleetPolling
            || self.warned
            || self.cap_units == 0
        {
            return;
        }
        let gross = self.gross_units(true);
        if gross.saturating_mul(100) >= self.cap_units.saturating_mul(K_CAP_WARN_PERCENT) {
            self.warned = true;
            tracing::info!(
                target: "collector.decision",
                vehicle = "account",
                from = "collecting",
                to = "collecting",
                reason = "cap_warning",
                gross_cents = gross.div_ceil(COST_UNITS_PER_CENT),
                cap_cents = self.cap_units / COST_UNITS_PER_CENT,
                "collector transition"
            );
        }
    }

    /// Raise the pending reservation for the requests the next step plans (C3 R27):
    /// when a category's unused reservation is smaller than its planned requests it
    /// grows by `max(planned, min(K_RESERVE_BLOCK, started since start))`, never
    /// beyond the cap. Returns whether a raise waits for its commit.
    fn plan(&mut self, plan: &[(RequestCategory, u64)]) -> bool {
        if self.mode != crate::config::CollectorMode::FleetPolling {
            return false;
        }
        for &(category, planned) in plan {
            if planned == 0 || !self.billed(category) {
                continue;
            }
            let index = category.index();
            let count = self.counts[index];
            if count.reserved_pending.saturating_sub(count.sent) >= planned {
                continue;
            }
            let desired = planned.max(K_RESERVE_BLOCK.min(self.started_since_start[index]));
            // A send needs `sent < reserved`. After a restart the converted count
            // (see `load`) can stand above the stored reservation, so the raise
            // first lifts the reservation to `sent` and then adds the desired
            // amount: one commit covers the plan, instead of one commit per
            // missing unit before the first request. The lift is part of the
            // exposure the cap bounds, so it is made only while the cap allows it.
            let lift = count.sent.saturating_sub(count.reserved_pending);
            let base = count.reserved_pending.saturating_add(lift);
            let ceiling = self.max_reserved(category);
            if ceiling < base {
                continue;
            }
            let raise = desired.min(ceiling - base);
            self.counts[index].reserved_pending = base.saturating_add(raise);
        }
        self.pending_reservation()
    }

    // ---- The month (C3 R25) ---------------------------------------------------

    /// The wall time a month change is judged by. Owner wire: the wall clock.
    /// Fleet polling: the wall clock until a forward divergence is seen, then the
    /// frozen anchor plus the monotonic time since it (C3 R25, review S4), so a
    /// forward wall step into the next month cannot reset the count inside Tesla's
    /// real billing month. The anchor follows the wall clock while no divergence has
    /// been seen; the divergence test here is the scheduler's own rule (R2) applied
    /// to the anchor, so a step between two loop iterations cannot slip in before
    /// the loop's clock watch notices it.
    fn projected_wall(&mut self, mono: Instant, wall: i64) -> i64 {
        if self.mode != crate::config::CollectorMode::FleetPolling {
            return wall;
        }
        let elapsed_ms = i64::try_from(mono.saturating_duration_since(self.anchor.0).as_millis())
            .unwrap_or(i64::MAX);
        if !self.frozen
            && wall
                .saturating_sub(self.anchor.1)
                .saturating_sub(elapsed_ms)
                > K_CLOCK_DIVERGENCE_MS
        {
            self.frozen = true;
        }
        if self.frozen {
            self.anchor.1.saturating_add(elapsed_ms)
        } else {
            self.anchor = (mono, wall);
            wall
        }
    }

    /// Advance the month when the calendar says so, never back (C3 R25). A new
    /// month starts from zero counts; requests still in flight count in it too.
    fn refresh(&mut self) -> MonthEvent {
        let Ok(wall) = self.clock.wall_now_ms() else {
            return MonthEvent::Unchanged;
        };
        let mono = self.clock.mono_now();
        let judged = self.projected_wall(mono, wall);
        let Some(candidate) = MonthSpan::containing(judged) else {
            return MonthEvent::Unchanged;
        };
        if candidate.key > self.span.key {
            let from = std::mem::replace(&mut self.span, candidate).key;
            self.anchor = (mono, wall);
            self.frozen = false;
            self.deferred_logged = false;
            self.warned = false;
            for count in &mut self.counts {
                let in_flight = count.in_flight;
                *count = CategoryCount {
                    sent: in_flight,
                    in_flight,
                    ..CategoryCount::default()
                };
            }
            self.persisted = [CounterView::default(); 4];
            return MonthEvent::Rolled {
                from,
                to: self.span.key.clone(),
            };
        }
        if self.mode == crate::config::CollectorMode::FleetPolling
            && !self.deferred_logged
            && MonthSpan::containing(wall).is_some_and(|actual| actual.key > self.span.key)
        {
            self.deferred_logged = true;
            tracing::info!(
                target: "collector.decision",
                vehicle = "account",
                from = "collecting",
                to = "collecting",
                reason = "month_change_deferred",
                "collector transition"
            );
        }
        MonthEvent::Unchanged
    }

    // ---- Persistence (C3 R4, R5, R27) ---------------------------------------------

    fn view_of(count: &CategoryCount) -> CounterView {
        // The stored `sent` counts the requests whose outcome is known (C3 R25,
        // B2 clarification): one still in flight is written by the commit that
        // records its outcome, so a crash cannot hide it behind the reservation.
        CounterView {
            sent: count.sent.saturating_sub(count.in_flight),
            billable: count.billable,
            reserved: count.reserved_pending,
        }
    }

    fn dirty(&self) -> bool {
        self.counts
            .iter()
            .zip(&self.persisted)
            .any(|(count, persisted)| Self::view_of(count) != *persisted)
    }

    /// The rows whose values differ from the store's.
    fn rows(&self) -> Vec<crate::db::CollectorCounterRow> {
        let signed = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
        RequestCategory::ALL
            .into_iter()
            .filter_map(|category| {
                let view = Self::view_of(&self.counts[category.index()]);
                (view != self.persisted[category.index()]).then(|| crate::db::CollectorCounterRow {
                    month: self.span.key.clone(),
                    mode: self.mode.code().to_owned(),
                    category: category.code().to_owned(),
                    sent: signed(view.sent),
                    billable: signed(view.billable),
                    reserved: signed(view.reserved),
                })
            })
            .collect()
    }

    /// The rows committed: the store now holds them, and the reservation they carry
    /// is the one sends may use (C3 R27).
    fn mark_persisted(&mut self, rows: &[crate::db::CollectorCounterRow]) {
        let unsigned = |value: i64| u64::try_from(value).unwrap_or(0);
        for row in rows {
            if row.month != self.span.key || row.mode != self.mode.code() {
                continue;
            }
            let Some(category) = RequestCategory::from_code(&row.category) else {
                continue;
            };
            let index = category.index();
            self.persisted[index] = CounterView {
                sent: unsigned(row.sent),
                billable: unsigned(row.billable),
                reserved: unsigned(row.reserved),
            };
            self.counts[index].reserved_persisted = self.counts[index]
                .reserved_persisted
                .max(unsigned(row.reserved));
        }
        self.last_flush = self.clock.mono_now();
    }

    /// Load the stored rows (C3 R7 step 3, R27 restart rule). The month is the
    /// calendar month, never earlier than the latest month any row holds, so a
    /// clock that is wrong at boot cannot reopen a closed month (C3 R25; a clock in
    /// the past is C5 R38's). With `d = max(0, reserved - sent)` from the stored
    /// row (the requests that may have started after the last commit, because the
    /// stored `sent` counts only requests whose outcome was known), the month's
    /// count per category is `billable + d`: `d` is added to both `sent` and
    /// `billable` in memory (requests that may have been sent before a crash count
    /// as sent and billed), and the first commit writes it. Requests already
    /// answered and not billed are never counted again. Rows of another mode in the
    /// month are another mode's and untouched.
    fn load(&mut self, rows: &[crate::db::CollectorCounterRow]) {
        let mono = self.clock.mono_now();
        if let Ok(wall) = self.clock.wall_now_ms() {
            self.anchor = (mono, wall);
            if let Some(calendar) = MonthSpan::containing(wall) {
                self.span = calendar;
            }
        }
        if let Some(latest) = rows
            .iter()
            .map(|row| row.month.as_str())
            .max()
            .filter(|latest| *latest > self.span.key.as_str())
            .and_then(MonthSpan::of_key)
        {
            self.span = latest;
        }
        self.frozen = false;
        self.deferred_logged = false;
        self.warned = false;
        self.counts = [CategoryCount::default(); 4];
        self.persisted = [CounterView::default(); 4];
        let unsigned = |value: i64| u64::try_from(value).unwrap_or(0);
        for row in rows {
            if row.month != self.span.key || row.mode != self.mode.code() {
                continue;
            }
            let Some(category) = RequestCategory::from_code(&row.category) else {
                continue;
            };
            let index = category.index();
            let (sent, billable, reserved) = (
                unsigned(row.sent),
                unsigned(row.billable),
                unsigned(row.reserved),
            );
            self.persisted[index] = CounterView {
                sent,
                billable,
                reserved,
            };
            let mut count = CategoryCount {
                sent,
                billable,
                reserved_pending: reserved,
                reserved_persisted: reserved,
                ..CategoryCount::default()
            };
            if self.billed(category) && reserved > sent {
                let converted = reserved - sent;
                count.sent = sent.saturating_add(converted);
                count.billable = billable.saturating_add(converted);
            }
            self.counts[index] = count;
        }
        self.last_flush = mono;
        // A month that was already at 80 % when the process started has had its
        // warning (C3 R27: one event a month), so a restart does not repeat it.
        self.warned = self.cap_units > 0
            && self.gross_units(true).saturating_mul(100)
                >= self.cap_units.saturating_mul(K_CAP_WARN_PERCENT);
    }

    /// The month's estimate (C3 R26), Fleet polling only.
    fn estimate(&self) -> Option<CostEstimate> {
        if self.mode != crate::config::CollectorMode::FleetPolling {
            return None;
        }
        let gross_units = self.gross_units(false);
        let gross_cents = gross_units.div_ceil(COST_UNITS_PER_CENT);
        let wall = self.clock.wall_now_ms().ok();
        let elapsed_ms = wall.map_or(0, |wall| wall.saturating_sub(self.span.start_ms));
        let mut estimate = CostEstimate {
            gross_cents,
            net_cents: gross_cents.saturating_sub(TESLA_MONTHLY_DISCOUNT_CENTS),
            cap_cents: u32::try_from(self.cap_units / COST_UNITS_PER_CENT).unwrap_or(u32::MAX),
            cap_warning: self.cap_units > 0
                && self.gross_units(true).saturating_mul(100)
                    >= self.cap_units.saturating_mul(K_CAP_WARN_PERCENT),
            projected_cents: None,
            stop_ms: None,
            note: Some(PROJECTION_AFTER_FIRST_DAY),
            scope_note: CAP_SCOPE_NOTE,
        };
        if elapsed_ms < K_PROJECTION_MIN_ELAPSED_MS {
            return Some(estimate);
        }
        estimate.note = None;
        let elapsed = u128::try_from(elapsed_ms).unwrap_or(0);
        let length = u128::try_from(self.span.length_ms()).unwrap_or(0);
        let gross = u128::from(gross_units);
        // projected = ceil(gross x month length / elapsed / 1,500).
        let projected = (gross * length).div_ceil(elapsed * u128::from(COST_UNITS_PER_CENT));
        estimate.projected_cents = Some(u64::try_from(projected).unwrap_or(u64::MAX));
        if gross_units > 0 {
            // stop = month start + floor(elapsed x cap / gross), shown when it falls
            // inside the month.
            let offset = elapsed * u128::from(self.cap_units) / gross;
            if offset < length {
                estimate.stop_ms = i64::try_from(offset)
                    .ok()
                    .map(|offset| self.span.start_ms.saturating_add(offset));
            }
        }
        Some(estimate)
    }
}

/// The shared request counter (C3 R25 to R27): a cloneable handle. See the file
/// header for who holds one.
#[derive(Clone)]
struct RequestLedger {
    state: Arc<std::sync::Mutex<LedgerState>>,
}

/// What a status reads of the ledger (C3 R13).
#[derive(Clone, Debug, PartialEq, Eq)]
struct LedgerView {
    month: String,
    requests: [(RequestCategory, u64); 4],
    estimate: Option<CostEstimate>,
}

impl RequestLedger {
    fn new(
        mode: crate::config::CollectorMode,
        fleet: &crate::config::FleetPolicy,
        clock: Arc<dyn CollectorClock>,
        now: Instant,
    ) -> Self {
        let wall = clock.wall_now_ms().unwrap_or(0);
        let span = MonthSpan::containing(wall)
            .or_else(|| MonthSpan::containing(0))
            .unwrap_or(MonthSpan {
                key: "1970-01".to_owned(),
                start_ms: 0,
                end_ms: 0,
            });
        Self {
            state: Arc::new(std::sync::Mutex::new(LedgerState {
                mode,
                pricing: fleet.pricing,
                cap_units: u64::from(fleet.monthly_cap_cents).saturating_mul(COST_UNITS_PER_CENT),
                clock,
                span,
                anchor: (now, wall),
                frozen: false,
                deferred_logged: false,
                counts: [CategoryCount::default(); 4],
                persisted: [CounterView::default(); 4],
                started_since_start: [0; 4],
                warned: false,
                last_flush: now,
            })),
        }
    }

    /// A ledger for a process that has no scheduler (C3 R25: the one-shot setup
    /// commands count the requests they send): the month's stored rows are loaded,
    /// and `flush_to_store` writes the result. The setup commands run while no
    /// collector holds the store, so nothing else counts meanwhile.
    fn standalone(
        store: &HubStore,
        fleet: &crate::config::FleetPolicy,
    ) -> Result<Self, StoreError> {
        let clock: Arc<dyn CollectorClock> = Arc::new(SystemCollectorClock);
        let ledger = Self::new(
            crate::config::CollectorMode::FleetPolling,
            fleet,
            clock.clone(),
            clock.mono_now(),
        );
        let rows = store.load_collector_state_rows()?.counters;
        ledger.lock().load(&rows);
        Ok(ledger)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Decide and count one scheduled request (see [`LedgerState::start`]).
    fn start(&self, category: RequestCategory) -> Admission {
        self.lock().start(category)
    }

    fn unstart(&self, category: RequestCategory) {
        self.lock().unstart(category);
    }

    fn complete(&self, category: RequestCategory, billable: bool) {
        self.lock().complete(category, billable);
    }

    /// Count a request nobody admitted, at completion (C3 R25).
    fn record_completed(&self, category: RequestCategory, billable: bool) {
        self.lock().record_completed(category, billable);
    }

    /// Count a request nobody admitted from its result (C3 R24, R25); a result that
    /// says no request was made counts nothing.
    fn record_fleet_result<T>(&self, category: RequestCategory, result: &Result<T, FleetApiError>) {
        if fleet_result_attempted(result) {
            self.record_completed(category, fleet_result_billable(result));
        }
    }

    /// A forward divergence of the wall clock was seen (C3 R2, R25): the Fleet
    /// month anchor stays where it is until the month changes.
    fn freeze_month_anchor(&self) {
        self.lock().frozen = true;
    }

    fn refresh_month(&self) -> MonthEvent {
        self.lock().refresh()
    }

    fn load(&self, rows: &[crate::db::CollectorCounterRow]) {
        self.lock().load(rows);
    }

    fn plan(&self, plan: &[(RequestCategory, u64)]) -> bool {
        self.lock().plan(plan)
    }

    fn dirty(&self) -> bool {
        self.lock().dirty()
    }

    /// Owner-wire counters wait for a transaction that happens anyway, and
    /// otherwise for `K_COUNTER_FLUSH` (C3 R25).
    fn flush_due(&self) -> bool {
        let state = self.lock();
        state.dirty()
            && state
                .clock
                .mono_now()
                .saturating_duration_since(state.last_flush)
                >= K_COUNTER_FLUSH
    }

    fn rows(&self) -> Vec<crate::db::CollectorCounterRow> {
        self.lock().rows()
    }

    fn mark_persisted(&self, rows: &[crate::db::CollectorCounterRow]) {
        self.lock().mark_persisted(rows);
    }

    fn view(&self) -> LedgerView {
        let state = self.lock();
        LedgerView {
            month: state.span.key.clone(),
            requests: RequestCategory::ALL
                .map(|category| (category, state.counts[category.index()].sent)),
            estimate: state.estimate(),
        }
    }

    /// Whether one more cheapest scheduled request would fit under the cap: the
    /// test a restart applies to a stored `cap_reached` after the owner raised the
    /// cap (C3 R27 resume).
    fn cap_allows_one_more(&self) -> bool {
        let state = self.lock();
        let cheapest = [RequestCategory::Data, RequestCategory::State]
            .into_iter()
            .filter(|category| state.billed(*category))
            .map(|category| state.price(category))
            .min()
            .unwrap_or(0);
        state.gross_units(true).saturating_add(cheapest) <= state.cap_units
    }

    /// Write the rows that differ from the store in a transaction of their own, for
    /// a sender with no tick to ride on: a one-shot setup command, or the Fleet
    /// Telemetry maintenance loop, which makes no scheduled requests (C3 R25).
    fn flush_to_store(&self, store: &HubStore) -> Result<(), StoreError> {
        let (rows, now_ms) = {
            let state = self.lock();
            (state.rows(), state.clock.wall_now_ms().unwrap_or(0))
        };
        if rows.is_empty() {
            return Ok(());
        }
        store.collector_write_transaction(
            |transaction| {
                let write = crate::db::CollectorRowsWrite {
                    now_ms,
                    counters: rows.clone(),
                    ..crate::db::CollectorRowsWrite::default()
                };
                let wrote = HubStore::write_collector_rows_in_transaction(transaction, &write)?;
                Ok::<_, StoreError>(((), wrote))
            },
            || Ok::<(), StoreError>(()),
        )?;
        self.mark_persisted(&rows);
        Ok(())
    }
}

/// One Fleet request's count, tied to the request's life (C3 R24, R25). The loop
/// admitted and counted the request before the helper ran; the helper completes
/// the count when Tesla's answer is known. A helper that fails before anything
/// reaches the network drops the guard unconsumed, and the start is taken back: a
/// request that was never sent is no count (a `RequestNotSent` answer, by
/// contrast, is a sent request that is not billed). A resend after a token refresh
/// is admitted under the same cap and reservation as any request and is `None`
/// when withheld, and the caller then returns the first answer.
struct CountedStart<'a> {
    ledger: Option<&'a RequestLedger>,
    category: RequestCategory,
    armed: bool,
}

impl<'a> CountedStart<'a> {
    /// The first send of a helper, already admitted by the loop.
    fn admitted(ledger: Option<&'a RequestLedger>, category: RequestCategory) -> Self {
        Self {
            ledger,
            category,
            armed: ledger.is_some(),
        }
    }

    /// A resend: admitted here.
    fn resend(ledger: Option<&'a RequestLedger>, category: RequestCategory) -> Option<Self> {
        match ledger {
            None => Some(Self::admitted(None, category)),
            // Built only when admitted: an unadmitted guard must not unstart.
            Some(ledger) => (ledger.start(category) == Admission::Admitted).then(|| Self {
                ledger: Some(ledger),
                category,
                armed: true,
            }),
        }
    }

    /// The resend of the request this guard counted, admitted under the same cap
    /// and reservation as any request (C3 R27, R47); `None` when it is withheld.
    fn resend_start(&self) -> Option<CountedStart<'a>> {
        Self::resend(self.ledger, self.category)
    }

    fn complete<T>(&mut self, result: &Result<T, FleetApiError>) {
        if let Some(ledger) = self.ledger
            && std::mem::take(&mut self.armed)
        {
            if fleet_result_attempted(result) {
                ledger.complete(self.category, fleet_result_billable(result));
            } else {
                ledger.unstart(self.category);
            }
        }
    }
}

impl Drop for CountedStart<'_> {
    fn drop(&mut self) {
        if let Some(ledger) = self.ledger
            && std::mem::take(&mut self.armed)
        {
            ledger.unstart(self.category);
        }
    }
}

// ---------------------------------------------------------------------------
// The consent example (C3 R17, R26).
// ---------------------------------------------------------------------------

/// The worked example of C3 R17 for the owner's intervals and the pricing in
/// force (C3 R26, consent screen): shown before any data is polled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ConsentExample {
    /// `vehicle_data` reads in the example day.
    reads_per_day: u64,
    /// The most state checks and inventory lists in a day.
    lists_per_day: u64,
    /// The day's cost in units.
    daily_units: u64,
    /// The month's gross in cents for the month length given.
    monthly_gross_cents: u64,
    /// The day of the month on which the cap stops polling: `ceil(cap / daily)`;
    /// `None` when the example costs nothing.
    stop_day: Option<u64>,
}

/// The consent figures for `fleet` over a month of `days_in_month` days and a cap
/// of `cap_cents` (C3 R17, R26): one hour of driving, two hours of charging, four
/// parkings each with a full idle window and a wake read, and at most one list per
/// state-check interval (inventory lists run only when no state check is due, so
/// they are inside that bound). With the defaults that is 216 reads and 144 lists
/// a day, 108,000 U (72 cents), $22.32 gross over 31 days, and a stop on day 14
/// for a cap of 1,000 cents.
// Consumer: the consent screen of the Mac GUI (G2-8, C3 R26) and the control CLI; tested here.
#[allow(dead_code)]
fn consent_example(
    fleet: &crate::config::FleetPolicy,
    days_in_month: u64,
    cap_cents: u32,
) -> ConsentExample {
    let intervals = &fleet.intervals;
    let per = |window_seconds: u64, interval: Duration| {
        window_seconds.div_ceil(interval.as_secs().max(1))
    };
    let reads_per_day = per(CONSENT_DRIVING_SECONDS, intervals.driving)
        + per(CONSENT_CHARGING_SECONDS, intervals.charging)
        + CONSENT_PARKINGS * per(CONSENT_IDLE_WINDOW.as_secs(), intervals.online)
        + CONSENT_PARKINGS;
    let lists_per_day = per(24 * 60 * 60, intervals.state_check.min(intervals.inventory));
    let data_price = price_units(&fleet.pricing, RequestCategory::Data).unwrap_or(0);
    let list_price = price_units(&fleet.pricing, RequestCategory::State).unwrap_or(0);
    let daily_units = reads_per_day * data_price + lists_per_day * list_price;
    let cap_units = u64::from(cap_cents) * COST_UNITS_PER_CENT;
    ConsentExample {
        reads_per_day,
        lists_per_day,
        daily_units,
        monthly_gross_cents: (daily_units * days_in_month).div_ceil(COST_UNITS_PER_CENT),
        stop_day: (daily_units > 0).then(|| cap_units.div_ceil(daily_units)),
    }
}

// ---------------------------------------------------------------------------
// The scheduler's side (C3 R25 to R27).
// ---------------------------------------------------------------------------

impl VehicleScheduler {
    /// The shared counter, for the Fleet helpers that may resend a request and for
    /// the resident actions that count beside the tick (C3 R25).
    fn request_ledger(&self) -> RequestLedger {
        self.ledger.clone()
    }

    /// Advance the month (C3 R25) and act on a new one: the counts are zero, a
    /// billing suspension is probed at the first tick (R50), and a stop by the cap
    /// ends (R27 resume), with one decision event each (R10).
    fn refresh_month(&mut self, now: Instant) {
        let MonthEvent::Rolled { from, to } = self.ledger.refresh_month() else {
            return;
        };
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = "collecting",
            to = "collecting",
            reason = "month_rolled_over",
            month_from = %from,
            month_to = %to,
            "collector transition"
        );
        self.note_new_month(now);
        if self.account.state == AccountState::CapReached {
            self.resume_from_cap(now);
        }
    }

    /// Leave `CapReached` (C3 R27 resume): a new month, a raised cap at the start, or
    /// the owner's retry. One list is wanted at once to see the cars again.
    fn resume_from_cap(&mut self, now: Instant) {
        let wall_ms = self.wall_or_last();
        self.account.state = AccountState::Collecting;
        self.account.state_reason = "cap_cleared";
        self.account.state_since_ms = wall_ms;
        self.account.list_wanted = Some(now);
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = AccountState::CapReached.code(),
            to = AccountState::Collecting.code(),
            reason = "cap_cleared",
            gated = false,
            "collector transition"
        );
    }

    /// Enter `CapReached` (C3 R27): all scheduled Fleet requests stop, state checks
    /// included, and a `cap_reached` gap opens with the tick (R8). Only a collecting
    /// account enters a state (the first state wins, as in R48).
    fn enter_cap_reached(&mut self) {
        if self.account.state != AccountState::Collecting {
            return;
        }
        let wall_ms = self.wall_or_last();
        self.account.state = AccountState::CapReached;
        self.account.state_reason = "cap_reached";
        self.account.state_since_ms = wall_ms;
        self.account.list_wanted = None;
        self.account.list_floor = None;
        let view = self.ledger.view();
        tracing::info!(
            target: "collector.decision",
            vehicle = "account",
            from = AccountState::Collecting.code(),
            to = AccountState::CapReached.code(),
            reason = "cap_reached",
            gated = false,
            gross_cents = view.estimate.map(|estimate| estimate.gross_cents),
            cap_cents = view.estimate.map(|estimate| estimate.cap_cents),
            "collector transition"
        );
    }

    /// Decide one scheduled request before it is sent (C3 R25, R27). Admitted: it is
    /// counted as started and the caller sends it. Withheld by the cap: the account
    /// is `CapReached` and nothing scheduled is sent until the month ends or the
    /// owner acts. Withheld for the reservation: the persisted reservation does not
    /// cover it, so the caller plans and commits first (`plan_reservation`). In the
    /// Owner-wire modes the request is only counted.
    fn admit_request(&mut self, category: RequestCategory, now: Instant) -> Admission {
        self.refresh_month(now);
        let probe = self.billing_probe_admissible(category, now);
        if !(self.requests_allowed(now) || probe) {
            return Admission::Withheld(Withheld::Account);
        }
        let admission = self.ledger.start(category);
        if probe && admission == Admission::Admitted {
            // C3 R50, R11, R57 (B2 round 3, C3-R3-01): the probe is armed again as it
            // is admitted, whatever Tesla answers. Only a success resumes collection
            // and only a new month or the owner's retry makes the next probe due
            // sooner; a 5xx, a 403, a refused token, a lost request or a 429 must not
            // leave a probe time in the past, or the loop would send it again at loop
            // speed and spend the owner's cap on it.
            let armed = deadline_after(now, K_BILLING_PROBE);
            self.account.next_billing_probe = Some(armed);
            self.account.probe_time_of_the_tick = Some(armed);
        }
        if admission == Admission::Withheld(Withheld::Cap) {
            if probe {
                // The suspension is not a collecting account, so the cap cannot
                // be entered; the probe is asked again at the next probe time
                // (or the new month's, which makes it due at once, R50).
                self.account.next_billing_probe = Some(deadline_after(now, K_BILLING_PROBE));
            } else {
                self.enter_cap_reached();
            }
        }
        admission
    }

    /// Whether `category` is the billing suspension's probe and it is due (C3 R50):
    /// one `state` list, the only scheduled request a billing-suspended account
    /// sends. It still passes the reservation and the cap like any request, so it
    /// is counted (R25); only the account-state check is waived, because without
    /// it the suspension, which Tesla ends at the next billing cycle or when the
    /// limit is raised, could end only through the owner's retry.
    fn billing_probe_admissible(&self, category: RequestCategory, now: Instant) -> bool {
        self.mode == crate::config::CollectorMode::FleetPolling
            && self.account.state == AccountState::BillingSuspended
            && category == RequestCategory::State
            && self.account.wait.is_none_or(|(until, _)| now >= until)
            && self
                .account
                .next_billing_probe
                .is_some_and(|probe| now >= probe)
    }

    /// Raise the pending reservation for the requests the next step plans (C3 R27);
    /// true when a raise waits for its commit, which the caller must make before
    /// sending. The loop's commit is a lone reservation commit when no result is
    /// waiting (R5).
    fn plan_reservation(&mut self, plan: &[(RequestCategory, u64)], now: Instant) -> bool {
        self.refresh_month(now);
        let probe = plan
            .iter()
            .any(|(category, _)| self.billing_probe_admissible(*category, now));
        if !(self.requests_allowed(now) || probe) {
            return false;
        }
        self.ledger.plan(plan)
    }

    /// A Fleet resend after the tick's token refresh was not sent, because the
    /// persisted reservation or the cap does not cover a second billed request
    /// (C3 R27, R47: a resend is admitted like any request and no slot is planned
    /// for it). The raise it needs is planned now, so the tick's own commit
    /// carries it and the request goes out after that commit. No failure is
    /// counted and no wait is set. If the cap forbids the raise, the request's
    /// next admission is the cap.
    fn resend_withheld(&mut self, category: RequestCategory, now: Instant) {
        let _ = self.plan_reservation(&[(category, 1)], now);
    }

    /// The list's resend was withheld (see [`Self::resend_withheld`]). Nothing was
    /// accepted, so the list is due exactly as it was and is asked for again at
    /// once; only the raise its resend needs is planned.
    fn list_resend_withheld(&mut self, now: Instant) {
        if self.account.state == AccountState::BillingSuspended {
            // The probe list went out and its resend did not: the probe is still
            // owed, so it stays due (admitting the list armed the next period).
            self.account.next_billing_probe = Some(now);
            self.account.probe_time_of_the_tick = Some(now);
        }
        self.resend_withheld(RequestCategory::State, now);
    }

    /// A lone reservation commit was made (C3 R5, R13: `reservation_commits`).
    fn note_reservation_commit(&mut self) {
        self.account.reservation_commits = self.account.reservation_commits.saturating_add(1);
    }

    /// A car's request was not started because it was withheld (C3 R27): the car is
    /// no longer in flight and its read stays due, so the next tick asks again once
    /// the reason has passed. A cap ends polling, a failed commit makes the account
    /// wait, and neither makes it due before then (R22).
    fn request_withheld(&mut self, id: VehicleId) {
        if let Some(vehicle) = self.vehicles.get_mut(&id) {
            vehicle.in_flight = false;
        }
    }

    /// Add the counter rows to a tick (C3 R5, R25): Fleet polling writes them in
    /// every tick commit that has any change (R66); the Owner-wire counts carry no
    /// money and ride on a transaction that happens anyway, are flushed at most once
    /// per `K_COUNTER_FLUSH`, and are flushed at a graceful stop (`force`).
    fn attach_counters(&self, tick: &mut PendingTick, other_writes: bool, force: bool) {
        if !self.ledger.dirty() {
            return;
        }
        let write = self.mode == crate::config::CollectorMode::FleetPolling
            || other_writes
            || force
            || self.ledger.flush_due();
        if !write {
            return;
        }
        for row in self.ledger.rows() {
            tick.counters.retain(|existing| {
                (&existing.month, &existing.mode, &existing.category)
                    != (&row.month, &row.mode, &row.category)
            });
            tick.counters.push(row);
        }
    }

    /// Load the month's counters (C3 R7 step 3, R27 restart rule) after the account
    /// row, and resume a stop by the cap that the owner's raised cap or a new month
    /// has lifted.
    fn load_counters(&mut self, rows: &[crate::db::CollectorCounterRow], now: Instant) {
        self.ledger.load(rows);
        if self.account.state == AccountState::CapReached && self.ledger.cap_allows_one_more() {
            self.resume_from_cap(now);
        }
        // C3 R50, R7 (B2 round 3, C3-R3-02): a month that began while the Hub was
        // down produced no `Rolled` event, so a stored billing suspension is probed
        // at the first tick of the new month here, as a stored cap stop is lifted
        // above. The month the Hub last counted in is the latest month this mode's
        // counter rows hold (a probe is counted, so a restart after a probe in this
        // month does not probe again); without a row, the month the suspension began.
        if self.account.state == AccountState::BillingSuspended {
            let mode = self.mode.code();
            let last_counted = rows
                .iter()
                .filter(|row| row.mode == mode)
                .map(|row| row.month.clone())
                .max()
                .or_else(|| MonthSpan::containing(self.account.state_since_ms).map(|span| span.key));
            if last_counted.is_some_and(|last| last < self.ledger.view().month) {
                self.note_new_month(now);
            }
        }
    }

    /// The month, the counts and the estimate for status (C3 R13).
    fn ledger_view(&self) -> LedgerView {
        self.ledger.view()
    }
}
