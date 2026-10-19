// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C3 part K7 (Fleet request counter, estimate and cap): CT15 to
// CT20, CT62 and CT63, the Fleet halves of CT13 and CT54, and tests of the rules the
// part owns (R24 to R27, and R25's other senders). Synthetic data only; the fake
// Fleet server is in-process and no test touches the network.

/// A UTC instant in epoch milliseconds.
fn utc_ms(year: i32, month: u8, day: u8, hour: u8) -> i64 {
    ::time::Date::from_calendar_date(year, ::time::Month::try_from(month).expect("month"), day)
        .expect("date")
        .with_hms(hour, 0, 0)
        .expect("time")
        .assume_utc()
        .unix_timestamp()
        * 1_000
}

const K7_DAY_MS: i64 = 24 * 60 * 60 * 1_000;

/// The Fleet policy with the default intervals, the given cap and pricing.
fn k7_policy(cap_cents: u32, pricing: crate::config::FleetPricing) -> crate::config::FleetPolicy {
    let mut policy =
        crate::config::FleetPolicy::resolve(&crate::config::FleetCadence::DEFAULT, pricing);
    policy.monthly_cap_cents = cap_cents;
    policy
}

/// Pricing under which state checks are not billed (C3 R17, CT1).
fn k7_free_state_pricing() -> crate::config::FleetPricing {
    crate::config::FleetPricing {
        state: None,
        ..crate::config::FLEET_PRICING
    }
}

/// A ledger on a manual clock, with no scheduler and no store.
struct LedgerBed {
    ledger: RequestLedger,
    clock: Arc<ManualClock>,
}

impl LedgerBed {
    fn new(
        mode: crate::config::CollectorMode,
        policy: crate::config::FleetPolicy,
        wall_ms: i64,
    ) -> Self {
        let t0 = Instant::now();
        let clock = ManualClock::new(t0, wall_ms);
        let ledger = RequestLedger::new(mode, &policy, clock.clone(), t0);
        Self { ledger, clock }
    }

    fn fleet(cap_cents: u32, wall_ms: i64) -> Self {
        Self::new(
            crate::config::CollectorMode::FleetPolling,
            k7_policy(cap_cents, crate::config::FLEET_PRICING),
            wall_ms,
        )
    }

    /// Plan the requests and treat the raise as committed, as the tick does.
    fn reserve(&self, plan: &[(RequestCategory, u64)]) {
        self.ledger.plan(plan);
        let rows = self.ledger.rows();
        self.ledger.mark_persisted(&rows);
    }

    fn sent(&self, category: RequestCategory) -> u64 {
        self.ledger.view().requests[category.index()].1
    }
}

// ---- CT15: counter, estimate and stop day -------------------------------------

#[test]
fn ct15_a_thousand_billable_data_reads_are_300000_units_and_200_cents() {
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    for _ in 0..1_000 {
        bed.ledger.record_completed(RequestCategory::Data, true);
    }
    assert_eq!(bed.ledger.lock().gross_units(false), 300_000);
    let estimate = bed.ledger.view().estimate.expect("Fleet estimate");
    assert_eq!(estimate.gross_cents, 200);
    // Tesla's monthly discount covers it, so the net is zero; the discount is shown
    // and never used for the cap.
    assert_eq!(estimate.net_cents, 0);
    assert_eq!(estimate.cap_cents, 1_000);
    assert!(!estimate.cap_warning);
}

#[test]
fn ct15_the_discount_is_taken_off_the_net_only() {
    let bed = LedgerBed::fleet(5_000, utc_ms(2026, 10, 15, 0));
    // 6,000 reads are 1,800,000 U: 1,200 cents gross, 200 net of the $10 discount.
    for _ in 0..6_000 {
        bed.ledger.record_completed(RequestCategory::Data, true);
    }
    let estimate = bed.ledger.view().estimate.expect("Fleet estimate");
    assert_eq!((estimate.gross_cents, estimate.net_cents), (1_200, 200));
}

#[test]
fn ct15_a_month_of_30_days_projects_and_stops_from_the_elapsed_share() {
    // September has 30 days. Ten days in, 3,000 billable reads are 900,000 U.
    let september = utc_ms(2026, 9, 1, 0);
    let bed = LedgerBed::fleet(1_000, september + 10 * K7_DAY_MS);
    for _ in 0..3_000 {
        bed.ledger.record_completed(RequestCategory::Data, true);
    }
    let estimate = bed.ledger.view().estimate.expect("Fleet estimate");
    assert_eq!(estimate.gross_cents, 600);
    assert_eq!(estimate.projected_cents, Some(1_800));
    // 10 d x 1,500,000 / 900,000 = 16 d 16 h after the month began.
    assert_eq!(
        estimate.stop_ms,
        Some(september + 16 * K7_DAY_MS + 16 * 60 * 60 * 1_000)
    );
    assert_eq!(estimate.note, None);
    assert!(!estimate.scope_note.is_empty());
}

#[test]
fn ct15_the_projection_and_the_stop_wait_for_the_first_day() {
    let september = utc_ms(2026, 9, 1, 0);
    let bed = LedgerBed::fleet(1_000, september + 23 * 60 * 60 * 1_000);
    for _ in 0..100 {
        bed.ledger.record_completed(RequestCategory::Data, true);
    }
    let estimate = bed.ledger.view().estimate.expect("Fleet estimate");
    assert_eq!(estimate.gross_cents, 20);
    assert_eq!(estimate.projected_cents, None);
    assert_eq!(estimate.stop_ms, None);
    assert_eq!(estimate.note, Some("projection after the first day"));
    // At exactly 24 h both appear.
    bed.clock.advance(Duration::from_secs(60 * 60));
    let estimate = bed.ledger.view().estimate.expect("Fleet estimate");
    assert!(estimate.projected_cents.is_some());
    assert_eq!(estimate.note, None);
}

#[test]
fn ct15_a_stop_after_the_month_ends_is_not_shown() {
    // 1,000 reads in the first 10 days (300,000 U) would reach 1,500,000 U after 50
    // days, which is outside the 30-day month.
    let september = utc_ms(2026, 9, 1, 0);
    let bed = LedgerBed::fleet(1_000, september + 10 * K7_DAY_MS);
    for _ in 0..1_000 {
        bed.ledger.record_completed(RequestCategory::Data, true);
    }
    let estimate = bed.ledger.view().estimate.expect("Fleet estimate");
    assert_eq!(estimate.projected_cents, Some(600));
    assert_eq!(estimate.stop_ms, None);
}

#[test]
fn ct15_consent_example_with_the_defaults_costs_72_cents_a_day_and_stops_on_day_14() {
    let policy = k7_policy(1_000, crate::config::FLEET_PRICING);
    let example = consent_example(&policy, 31, 1_000);
    // 120 driving reads, 60 charging reads, 4 x 8 idle-window reads and 4 wake reads.
    assert_eq!(example.reads_per_day, 216);
    // One list per 600 s state-check interval.
    assert_eq!(example.lists_per_day, 144);
    assert_eq!(example.daily_units, 108_000);
    assert_eq!(example.monthly_gross_cents, 2_232);
    assert_eq!(example.stop_day, Some(14));
}

#[test]
fn ct15_consent_example_with_free_state_checks_costs_64800_units_and_stops_on_day_24() {
    let policy = k7_policy(1_000, k7_free_state_pricing());
    let example = consent_example(&policy, 31, 1_000);
    assert_eq!(example.reads_per_day, 216);
    assert_eq!(example.daily_units, 64_800);
    assert_eq!(example.stop_day, Some(24));
}

#[test]
fn ct15_consent_example_follows_the_owners_intervals_and_cap() {
    let mut policy = k7_policy(2_000, crate::config::FLEET_PRICING);
    policy.intervals.driving = Duration::from_secs(60);
    policy.intervals.charging = Duration::from_secs(300);
    let example = consent_example(&policy, 30, 2_000);
    // 60 + 24 + 32 + 4 reads, 144 lists.
    assert_eq!(example.reads_per_day, 120);
    assert_eq!(example.daily_units, (120 + 144) * 300);
    assert_eq!(example.stop_day, Some(3_000_000_u64.div_ceil(79_200)));
}

// ---- Prices and billing (R24) ----------------------------------------------------

#[test]
fn the_published_prices_are_exact_whole_cost_units() {
    let pricing = crate::config::FLEET_PRICING;
    assert_eq!(price_units(&pricing, RequestCategory::Data), Some(300));
    assert_eq!(price_units(&pricing, RequestCategory::Command), Some(150));
    assert_eq!(price_units(&pricing, RequestCategory::Wake), Some(3_000));
    // State checks are billed at the data rate until gate M-C1 records Tesla's
    // category (C3 R24): the safe side for a cap.
    assert_eq!(price_units(&pricing, RequestCategory::State), Some(300));
    assert_eq!(
        price_units(&k7_free_state_pricing(), RequestCategory::State),
        None
    );
    // 150,000 signals a dollar is one unit each.
    assert_eq!(COST_UNITS_PER_DOLLAR / u64::from(pricing.signal), 1);
}

#[test]
fn a_price_that_does_not_divide_the_unit_is_rounded_up() {
    let pricing = crate::config::FleetPricing {
        data: 400,
        ..crate::config::FLEET_PRICING
    };
    // 150,000 / 400 = 375 exactly; 7 per dollar would be 21,428.57 and rounds up.
    assert_eq!(price_units(&pricing, RequestCategory::Data), Some(375));
    let odd = crate::config::FleetPricing {
        data: 7,
        ..crate::config::FLEET_PRICING
    };
    assert_eq!(price_units(&odd, RequestCategory::Data), Some(21_429));
}

// ---- CT18: billing of outcomes ----------------------------------------------------

#[test]
fn ct18_billing_follows_the_status_class_of_the_answer() {
    let status = |code: u16| Err::<(), _>(FleetApiError::HttpStatus(code));
    for (result, billable) in [
        (Ok::<(), FleetApiError>(()), true),
        (status(200), true),
        (status(401), true),
        (status(408), true),
        (status(429), true),
        (status(499), true),
        (status(500), false),
        (status(503), false),
        (status(540), false),
        (
            Err(FleetApiError::RateLimited {
                retry_after_seconds: 5,
            }),
            true,
        ),
        (
            Err(FleetApiError::ProviderHttpStatus {
                status: 540,
                error: "x".to_owned(),
                description: None,
            }),
            false,
        ),
        (
            Err(FleetApiError::ProviderHttpStatus {
                status: 412,
                error: "x".to_owned(),
                description: None,
            }),
            true,
        ),
        (Err(FleetApiError::RequestNotSent), false),
        // A timeout or a transport error after the request was handed over has an
        // unknown delivery and is counted as billed: the safe side for a cap.
        (Err(FleetApiError::RequestTimeout), true),
        (Err(FleetApiError::Transport), true),
        (Err(FleetApiError::InvalidResponse), true),
    ] {
        assert_eq!(fleet_result_billable(&result), billable, "{result:?}");
    }
}

#[test]
fn ct18_a_timeout_or_transport_error_is_class_unreachable() {
    for error in [FleetApiError::RequestTimeout, FleetApiError::Transport] {
        assert_eq!(
            classify(
                &CollectorError::FleetApi(error),
                RequestKind::Read,
                crate::config::CollectorMode::FleetPolling
            ),
            ErrorClass::Unreachable
        );
    }
}

#[test]
fn ct18_the_ledger_bills_only_what_the_rule_bills() {
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    let outcomes: [Result<(), FleetApiError>; 6] = [
        Ok(()),
        Err(FleetApiError::HttpStatus(408)),
        Err(FleetApiError::HttpStatus(503)),
        Err(FleetApiError::HttpStatus(540)),
        Err(FleetApiError::RequestNotSent),
        Err(FleetApiError::Transport),
    ];
    bed.reserve(&[(RequestCategory::Data, 6)]);
    for outcome in &outcomes {
        let mut counted =
            CountedStart::resend(Some(&bed.ledger), RequestCategory::Data).expect("reserved");
        counted.complete(outcome);
    }
    let count = bed.ledger.lock().counts[RequestCategory::Data.index()];
    // 200, 408 and the transport error are billed; 503, 540 and RequestNotSent are
    // sent requests that are not billed.
    assert_eq!((count.sent, count.billable, count.in_flight), (6, 3, 0));
}

#[test]
fn a_result_that_names_no_request_counts_nothing() {
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    bed.reserve(&[(RequestCategory::Data, 2)]);
    let mut counted =
        CountedStart::resend(Some(&bed.ledger), RequestCategory::Data).expect("start");
    assert_eq!(bed.sent(RequestCategory::Data), 1);
    // The command proxy was not configured: no request was made.
    counted.complete::<()>(&Err(FleetApiError::CommandProxyUnavailable));
    assert_eq!(bed.sent(RequestCategory::Data), 0);
    // The same holds for a request nobody admitted.
    bed.ledger.record_fleet_result::<()>(
        RequestCategory::Command,
        &Err(FleetApiError::InvalidCommand),
    );
    assert_eq!(bed.sent(RequestCategory::Command), 0);
    bed.ledger.record_fleet_result::<()>(
        RequestCategory::Command,
        &Err(FleetApiError::RequestNotSent),
    );
    assert_eq!(bed.sent(RequestCategory::Command), 1);
    assert_eq!(
        bed.ledger.lock().counts[RequestCategory::Command.index()].billable,
        0
    );
}

#[test]
fn a_start_that_never_reaches_the_network_is_taken_back() {
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    bed.reserve(&[(RequestCategory::State, 1)]);
    {
        let _guard =
            CountedStart::resend(Some(&bed.ledger), RequestCategory::State).expect("admitted");
        assert_eq!(bed.sent(RequestCategory::State), 1);
    }
    // The guard was dropped without an answer: no request, no count, no flight.
    let count = bed.ledger.lock().counts[RequestCategory::State.index()];
    assert_eq!((count.sent, count.billable, count.in_flight), (0, 0, 0));
}

// ---- CT16: cap never exceeded ---------------------------------------------------------

#[test]
fn ct16_the_cap_is_checked_before_the_reservation() {
    // One cent is 1,500 U: five requests at 300 U.
    let bed = LedgerBed::fleet(1, utc_ms(2026, 10, 15, 0));
    let mut sent = 0;
    loop {
        bed.reserve(&[(RequestCategory::Data, 1)]);
        match bed.ledger.start(RequestCategory::Data) {
            Admission::Admitted => {
                bed.ledger.complete(RequestCategory::Data, true);
                sent += 1;
            }
            other => {
                // The sixth is a cap, not a wait for a reservation.
                assert_eq!(other, Admission::Withheld(Withheld::Cap));
                break;
            }
        }
        assert!(sent <= 5, "the cap must stop the sixth request");
    }
    assert_eq!(sent, 5);
    assert_eq!(bed.ledger.lock().gross_units(true), 1_500);
}

#[test]
fn ct16_requests_started_together_cannot_pass_the_cap_together() {
    let bed = LedgerBed::fleet(1, utc_ms(2026, 10, 15, 0));
    bed.reserve(&[(RequestCategory::Data, 5)]);
    // Six admissions before any completes: in-flight requests count against the cap.
    let admitted = (0..6)
        .filter(|_| bed.ledger.start(RequestCategory::Data) == Admission::Admitted)
        .count();
    assert_eq!(admitted, 5);
}

#[test]
fn ct16_in_flight_requests_count_against_the_cap_even_when_a_reservation_exceeds_it() {
    // Defence in depth: the reservation is clamped to the cap when it is raised, so
    // the cap check is a second line. It must hold alone, with a reservation that
    // (artificially) allows more than the cap does.
    let bed = LedgerBed::fleet(1, utc_ms(2026, 10, 15, 0));
    {
        let mut state = bed.ledger.lock();
        let count = &mut state.counts[RequestCategory::Data.index()];
        count.reserved_pending = 20;
        count.reserved_persisted = 20;
    }
    let admitted = (0..8)
        .filter(|_| bed.ledger.start(RequestCategory::Data) == Admission::Admitted)
        .count();
    assert_eq!(
        admitted, 5,
        "five requests of 300 U fit 1,500 U, completed or not"
    );
}

#[test]
fn ct16_a_reservation_that_fills_the_cap_ends_polling_without_looping() {
    // Five slots of 300 U in all. The state category has reserved four of them and
    // used one, so data can reserve only the fifth.
    let bed = LedgerBed::fleet(1, utc_ms(2026, 10, 15, 0));
    bed.ledger.record_completed(RequestCategory::State, true);
    bed.ledger.lock().counts[RequestCategory::State.index()].reserved_pending = 4;
    bed.ledger.lock().counts[RequestCategory::State.index()].reserved_persisted = 4;
    bed.reserve(&[(RequestCategory::Data, 1)]);
    assert_eq!(bed.ledger.start(RequestCategory::Data), Admission::Admitted);
    bed.ledger.complete(RequestCategory::Data, true);
    bed.reserve(&[(RequestCategory::Data, 1)]);
    // Gross is only 600 U, but every slot is spoken for: a request that cannot be
    // reserved within the cap is the cap (the month could already count to it after
    // a crash), never an endless wait for a reservation.
    assert_eq!(
        bed.ledger.start(RequestCategory::Data),
        Admission::Withheld(Withheld::Cap)
    );
}

#[test]
fn ct16_the_scheduler_enters_cap_reached_once_and_stops_everything() {
    let t0 = Instant::now();
    let clock = ManualClock::new(t0, utc_ms(2026, 10, 15, 0));
    let mut scheduler = VehicleScheduler::new_with(
        core_cadence(),
        crate::config::CollectorMode::FleetPolling,
        k7_policy(1, crate::config::FLEET_PRICING),
        t0,
        clock.clone(),
        FixedJitter::constant(0.0),
    );
    scheduler.accept_discovery(vec![core_vehicle(1, "online")], t0);
    let log = DecisionLog::start();
    // The list and four reads fill the five slots; reserve and send each.
    for category in [
        RequestCategory::State,
        RequestCategory::Data,
        RequestCategory::Data,
        RequestCategory::Data,
        RequestCategory::Data,
    ] {
        scheduler.plan_reservation(&[(category, 1)], t0);
        let rows = scheduler.ledger.rows();
        scheduler.ledger.mark_persisted(&rows);
        assert_eq!(scheduler.admit_request(category, t0), Admission::Admitted);
        scheduler.ledger.complete(category, true);
    }
    assert_eq!(scheduler.account.state, AccountState::Collecting);
    assert_eq!(
        scheduler.admit_request(RequestCategory::Data, t0),
        Admission::Withheld(Withheld::Cap)
    );
    assert_eq!(scheduler.account.state, AccountState::CapReached);
    assert_eq!(scheduler.account.state_reason, "cap_reached");
    // One event, however often the cap is met again (R10).
    for _ in 0..3 {
        assert_eq!(
            scheduler.admit_request(RequestCategory::Data, t0),
            Admission::Withheld(Withheld::Account)
        );
    }
    assert_eq!(log.lines("INFO", "cap_reached"), 1, "{}", log.contents());
    // State checks stop too, a gap opens with the next tick, and status says so.
    assert!(!scheduler.discovery_due(t0 + Duration::from_secs(10_000)));
    assert!(
        scheduler
            .due_vehicles(t0 + Duration::from_secs(10_000))
            .is_empty()
    );
    let tick = scheduler.take_tick();
    assert_eq!(tick.gap_opens.len(), 1);
    assert_eq!(tick.gap_opens[0].key.kind, GapKind::CapReached);
    let status = scheduler.status(t0);
    assert_eq!(status.account_state, AccountState::CapReached);
    assert!(
        status
            .account_text
            .starts_with("Polling stopped: local monthly cap reached")
    );
    let estimate = status.estimate.expect("Fleet estimate");
    assert_eq!((estimate.gross_cents, estimate.cap_cents), (1, 1));
    assert!(estimate.cap_warning);
}

#[test]
fn ct16_owner_retry_re_checks_the_cap_at_the_next_billable_request() {
    let t0 = Instant::now();
    let clock = ManualClock::new(t0, utc_ms(2026, 10, 15, 0));
    let mut scheduler = VehicleScheduler::new_with(
        core_cadence(),
        crate::config::CollectorMode::FleetPolling,
        k7_policy(0, crate::config::FLEET_PRICING),
        t0,
        clock,
        FixedJitter::constant(0.0),
    );
    // A cap of zero stops every billable request.
    assert_eq!(
        scheduler.admit_request(RequestCategory::State, t0),
        Admission::Withheld(Withheld::Cap)
    );
    assert_eq!(scheduler.account.state, AccountState::CapReached);
    scheduler.owner_retry(t0);
    assert_eq!(scheduler.account.state, AccountState::Collecting);
    // The retry only re-arms the check: the cap still forbids the request.
    assert_eq!(
        scheduler.admit_request(RequestCategory::State, t0),
        Admission::Withheld(Withheld::Cap)
    );
    assert_eq!(scheduler.account.state, AccountState::CapReached);
}

#[test]
fn ct16_free_state_checks_are_never_blocked_by_the_cap() {
    let t0 = Instant::now();
    let clock = ManualClock::new(t0, utc_ms(2026, 10, 15, 0));
    let mut scheduler = VehicleScheduler::new_with(
        core_cadence(),
        crate::config::CollectorMode::FleetPolling,
        k7_policy(0, k7_free_state_pricing()),
        t0,
        clock,
        FixedJitter::constant(0.0),
    );
    for _ in 0..50 {
        assert_eq!(
            scheduler.admit_request(RequestCategory::State, t0),
            Admission::Admitted
        );
        scheduler.ledger.complete(RequestCategory::State, false);
    }
    assert_eq!(scheduler.account.state, AccountState::Collecting);
    let view = scheduler.status(t0);
    assert_eq!(view.requests[RequestCategory::State.index()].1, 50);
    // Not billed, so no estimate cost either.
    assert_eq!(view.estimate.expect("estimate").gross_cents, 0);
}

// ---- CT19: cap warning ---------------------------------------------------------------

#[test]
fn ct19_the_warning_is_one_event_at_80_percent_and_resets_with_the_month() {
    let log = DecisionLog::start();
    // 100 cents is 150,000 U: 80 cents is 400 data requests.
    let bed = LedgerBed::fleet(100, utc_ms(2026, 10, 15, 0));
    for _ in 0..399 {
        bed.ledger.record_completed(RequestCategory::Data, true);
    }
    assert_eq!(log.lines("INFO", "cap_warning"), 0);
    assert!(!bed.ledger.view().estimate.expect("estimate").cap_warning);
    bed.ledger.record_completed(RequestCategory::Data, true);
    assert_eq!(log.lines("INFO", "cap_warning"), 1);
    assert!(bed.ledger.view().estimate.expect("estimate").cap_warning);
    // 90 cents: no second event.
    for _ in 0..50 {
        bed.ledger.record_completed(RequestCategory::Data, true);
    }
    assert_eq!(log.lines("INFO", "cap_warning"), 1);
    // The next month starts again.
    bed.clock.advance(Duration::from_secs(20 * 24 * 60 * 60));
    assert!(matches!(
        bed.ledger.refresh_month(),
        MonthEvent::Rolled { .. }
    ));
    assert!(!bed.ledger.view().estimate.expect("estimate").cap_warning);
    for _ in 0..400 {
        bed.ledger.record_completed(RequestCategory::Data, true);
    }
    assert_eq!(log.lines("INFO", "cap_warning"), 2, "{}", log.contents());
}

#[test]
fn ct19_a_restart_in_a_warned_month_does_not_repeat_the_warning() {
    let log = DecisionLog::start();
    let bed = LedgerBed::fleet(100, utc_ms(2026, 10, 15, 0));
    // 450 reads are 135,000 U: past the 80 % line of the 150,000 U cap.
    bed.ledger.load(&[counter_row(
        "2026-10",
        "fleet_polling",
        "data",
        450,
        450,
        450,
    )]);
    bed.ledger.record_completed(RequestCategory::Data, true);
    assert_eq!(log.lines("INFO", "cap_warning"), 0, "{}", log.contents());
    // The status still says so.
    assert!(bed.ledger.view().estimate.expect("estimate").cap_warning);
}

// ---- CT20: the month never goes back ----------------------------------------------------

fn counter_row(
    month: &str,
    mode: &str,
    category: &str,
    sent: i64,
    billable: i64,
    reserved: i64,
) -> crate::db::CollectorCounterRow {
    crate::db::CollectorCounterRow {
        month: month.to_owned(),
        mode: mode.to_owned(),
        category: category.to_owned(),
        sent,
        billable,
        reserved,
    }
}

#[test]
fn ct20_a_persisted_later_month_is_kept_when_the_clock_is_earlier() {
    for (mode, mode_code) in [
        (crate::config::CollectorMode::Legacy, "legacy"),
        (crate::config::CollectorMode::FleetPolling, "fleet_polling"),
    ] {
        // The wall clock says 2026-10-31.
        let bed = LedgerBed::new(
            mode,
            k7_policy(1_000, crate::config::FLEET_PRICING),
            utc_ms(2026, 10, 31, 0),
        );
        bed.ledger
            .load(&[counter_row("2026-11", mode_code, "data", 40, 40, 40)]);
        let view = bed.ledger.view();
        assert_eq!(view.month, "2026-11", "{mode_code}");
        // The counts were not reset.
        assert_eq!(view.requests[RequestCategory::Data.index()].1, 40);
        // Later refreshes with the earlier clock change nothing.
        assert_eq!(bed.ledger.refresh_month(), MonthEvent::Unchanged);
        assert_eq!(bed.ledger.view().month, "2026-11");
        assert_eq!(bed.sent(RequestCategory::Data), 40);
    }
}

#[test]
fn ct20_a_backward_wall_step_never_reopens_an_earlier_month() {
    for mode in [
        crate::config::CollectorMode::Legacy,
        crate::config::CollectorMode::FleetPolling,
    ] {
        let bed = LedgerBed::new(
            mode,
            k7_policy(1_000, crate::config::FLEET_PRICING),
            utc_ms(2026, 11, 2, 0),
        );
        bed.ledger.record_completed(RequestCategory::Data, true);
        bed.clock.step_wall_ms(-5 * K7_DAY_MS);
        assert_eq!(bed.ledger.refresh_month(), MonthEvent::Unchanged);
        assert_eq!(bed.ledger.view().month, "2026-11");
        assert_eq!(bed.sent(RequestCategory::Data), 1);
    }
}

#[test]
fn ct20_owner_wire_follows_the_calendar_forward() {
    let bed = LedgerBed::new(
        crate::config::CollectorMode::Legacy,
        k7_policy(1_000, crate::config::FLEET_PRICING),
        utc_ms(2026, 10, 31, 23),
    );
    bed.ledger.record_completed(RequestCategory::Data, false);
    assert_eq!(bed.ledger.view().month, "2026-10");
    bed.clock.advance(Duration::from_secs(2 * 60 * 60));
    assert_eq!(
        bed.ledger.refresh_month(),
        MonthEvent::Rolled {
            from: "2026-10".to_owned(),
            to: "2026-11".to_owned()
        }
    );
    // A December month rolls into the next year.
    bed.clock.advance(Duration::from_secs(31 * 24 * 60 * 60));
    bed.ledger.refresh_month();
    assert_eq!(bed.ledger.view().month, "2026-12");
    bed.clock.advance(Duration::from_secs(31 * 24 * 60 * 60));
    bed.ledger.refresh_month();
    assert_eq!(bed.ledger.view().month, "2027-01");
    assert_eq!(bed.sent(RequestCategory::Data), 0);
}

#[test]
fn month_spans_have_the_calendar_lengths() {
    for (year, month, days) in [(2026, 2, 28), (2028, 2, 29), (2026, 10, 31), (2026, 11, 30)] {
        let span = MonthSpan::containing(utc_ms(year, month, 10, 0)).expect("span");
        assert_eq!(span.length_ms(), days * K7_DAY_MS, "{year}-{month}");
        assert_eq!(span.key, format!("{year:04}-{month:02}"));
        assert_eq!(MonthSpan::of_key(&span.key), Some(span));
    }
    assert_eq!(MonthSpan::of_key("2026-13"), None);
    assert_eq!(MonthSpan::of_key("garbage"), None);
}

// ---- CT63: a forward step across a month end ----------------------------------------------

#[test]
fn ct63_a_forward_wall_step_cannot_reset_the_count_inside_the_real_month() {
    let t0 = Instant::now();
    let clock = ManualClock::new(t0, utc_ms(2026, 10, 31, 0));
    let mut scheduler = VehicleScheduler::new_with(
        core_cadence(),
        crate::config::CollectorMode::FleetPolling,
        k7_policy(1_000, crate::config::FLEET_PRICING),
        t0,
        clock.clone(),
        FixedJitter::constant(0.0),
    );
    let log = DecisionLog::start();
    scheduler.observe_clock(t0);
    assert_eq!(scheduler.status(t0).month, "2026-10");
    // An hour passes, with the clocks together.
    clock.advance(Duration::from_secs(60 * 60));
    let hour = clock.mono_now();
    assert_eq!(scheduler.observe_clock(hour), None);
    // The wall steps forward two days while monotonic time advances one second.
    clock.advance_mono(Duration::from_secs(1));
    clock.step_wall_ms(2 * K7_DAY_MS);
    let stepped = clock.mono_now();
    let wall_before = utc_ms(2026, 10, 31, 1);
    assert_eq!(
        scheduler.observe_clock(stepped),
        Some(ClockEvent::HostSuspended {
            from_wall_ms: wall_before,
            to_wall_ms: wall_before + 2 * K7_DAY_MS,
        })
    );
    let gaps = scheduler.take_tick().gaps;
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].kind, GapKind::HostSuspended);
    // The month stays, and the deferral is logged once.
    assert_eq!(scheduler.status(stepped).month, "2026-10");
    scheduler.observe_clock(stepped);
    scheduler.observe_clock(stepped);
    assert_eq!(
        log.lines("INFO", "month_change_deferred"),
        1,
        "{}",
        log.contents()
    );
    // Requests in the following monotonic time are counted in 2026-10.
    scheduler
        .ledger
        .record_completed(RequestCategory::Data, true);
    scheduler
        .ledger
        .record_completed(RequestCategory::Data, true);
    let status = scheduler.status(stepped);
    assert_eq!(status.month, "2026-10");
    assert_eq!(status.requests[RequestCategory::Data.index()].1, 2);
    // When monotonic time since the frozen anchor (01:00) reaches 23 h the projected
    // wall time is 2026-11-01T00:00Z and the month becomes 2026-11 with zero counts.
    clock.advance(Duration::from_secs(23 * 60 * 60 - 60));
    scheduler.observe_clock(clock.mono_now());
    assert_eq!(scheduler.status(clock.mono_now()).month, "2026-10");
    clock.advance(Duration::from_secs(60));
    scheduler.observe_clock(clock.mono_now());
    let status = scheduler.status(clock.mono_now());
    assert_eq!(status.month, "2026-11");
    assert_eq!(
        status.requests,
        [
            (RequestCategory::Data, 0),
            (RequestCategory::State, 0),
            (RequestCategory::Wake, 0),
            (RequestCategory::Command, 0),
        ]
    );
    assert_eq!(log.lines("INFO", "month_rolled_over"), 1);
    // The counter never returns to 2026-10, whatever the wall clock then says.
    clock.step_wall_ms(-3 * K7_DAY_MS);
    scheduler.observe_clock(clock.mono_now());
    assert_eq!(scheduler.status(clock.mono_now()).month, "2026-11");
}

#[test]
fn ct63_without_a_divergence_the_wall_clock_decides_the_month() {
    let t0 = Instant::now();
    let clock = ManualClock::new(t0, utc_ms(2026, 10, 31, 23));
    let mut scheduler = VehicleScheduler::new_with(
        core_cadence(),
        crate::config::CollectorMode::FleetPolling,
        k7_policy(1_000, crate::config::FLEET_PRICING),
        t0,
        clock.clone(),
        FixedJitter::constant(0.0),
    );
    scheduler.observe_clock(t0);
    clock.advance(Duration::from_secs(60 * 60 + 30));
    scheduler.observe_clock(clock.mono_now());
    assert_eq!(scheduler.status(clock.mono_now()).month, "2026-11");
}

#[test]
fn a_month_change_probes_a_billing_suspension_and_ends_a_cap_stop() {
    let t0 = Instant::now();
    let clock = ManualClock::new(t0, utc_ms(2026, 10, 15, 0));
    let mut scheduler = VehicleScheduler::new_with(
        core_cadence(),
        crate::config::CollectorMode::FleetPolling,
        k7_policy(0, crate::config::FLEET_PRICING),
        t0,
        clock.clone(),
        FixedJitter::constant(0.0),
    );
    scheduler.admit_request(RequestCategory::State, t0);
    assert_eq!(scheduler.account.state, AccountState::CapReached);
    clock.advance(Duration::from_secs(20 * 24 * 60 * 60));
    let later = clock.mono_now();
    scheduler.observe_clock(later);
    // With a cap of zero the new month's first request stops polling again, but the
    // stop ended with the month, so the account was collecting for this tick.
    assert_eq!(scheduler.account.state, AccountState::Collecting);
    assert_eq!(scheduler.account.state_reason, "cap_cleared");
    assert!(scheduler.discovery_due(later), "a list is wanted at once");
    let tick = scheduler.take_tick();
    assert!(
        tick.gap_closes.is_empty(),
        "no gap was ever stored, so none closes"
    );
}

// ---- Reservation and restart (R27) --------------------------------------------------------

#[test]
fn a_reservation_raise_is_the_plan_then_blocks_up_to_the_largest_step() {
    let reserved =
        |bed: &LedgerBed| bed.ledger.lock().counts[RequestCategory::Data.index()].reserved_pending;
    // A process that has sent nothing reserves exactly what its first step plans.
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    assert!(bed.ledger.plan(&[(RequestCategory::Data, 3)]));
    assert_eq!(reserved(&bed), 3);
    let rows = bed.ledger.rows();
    bed.ledger.mark_persisted(&rows);
    // A covered plan raises nothing and needs no commit.
    assert!(!bed.ledger.plan(&[(RequestCategory::Data, 3)]));
    assert_eq!(reserved(&bed), 3);
    // A process that has started 7 requests reserves 7 more, once a step is needed.
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    bed.ledger.lock().started_since_start[RequestCategory::Data.index()] = 7;
    assert!(bed.ledger.plan(&[(RequestCategory::Data, 1)]));
    assert_eq!(reserved(&bed), 7);
    // A long-running process never raises by more than one block.
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    bed.ledger.lock().started_since_start[RequestCategory::Data.index()] = 500;
    bed.ledger.plan(&[(RequestCategory::Data, 1)]);
    assert_eq!(reserved(&bed), K_RESERVE_BLOCK);
    // But a tick that plans more than a block reserves the whole plan.
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    bed.ledger.lock().started_since_start[RequestCategory::Data.index()] = 500;
    bed.ledger.plan(&[(RequestCategory::Data, 25)]);
    assert_eq!(reserved(&bed), 25);
}

#[test]
fn the_reservation_never_goes_beyond_the_cap() {
    // One cent: five data requests.
    let bed = LedgerBed::fleet(1, utc_ms(2026, 10, 15, 0));
    assert!(bed.ledger.plan(&[(RequestCategory::Data, 20)]));
    assert_eq!(
        bed.ledger.lock().counts[RequestCategory::Data.index()].reserved_pending,
        5
    );
    assert_eq!(bed.ledger.lock().exposure_units(None), 1_500);
}

#[test]
fn a_request_waits_for_a_committed_reservation_not_a_pending_one() {
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    assert!(bed.ledger.plan(&[(RequestCategory::Data, 1)]));
    // Raised in memory, not committed: the request is not sent.
    assert_eq!(
        bed.ledger.start(RequestCategory::Data),
        Admission::Withheld(Withheld::Reservation)
    );
    let rows = bed.ledger.rows();
    bed.ledger.mark_persisted(&rows);
    assert_eq!(bed.ledger.start(RequestCategory::Data), Admission::Admitted);
    // And the reservation is used up.
    assert_eq!(
        bed.ledger.start(RequestCategory::Data),
        Admission::Withheld(Withheld::Reservation)
    );
}

#[test]
fn restart_counts_the_larger_of_billable_and_reserved_and_the_first_commit_writes_it() {
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    bed.ledger.load(&[
        counter_row("2026-10", "fleet_polling", "data", 7, 7, 8),
        counter_row("2026-10", "fleet_polling", "state", 2, 2, 2),
        // Another mode's row in the month is not this mode's.
        counter_row("2026-10", "legacy", "data", 500, 0, 0),
    ]);
    let data = bed.ledger.lock().counts[RequestCategory::Data.index()];
    // reserved 8 > billable 7: the difference of 1 is added to sent and billable.
    assert_eq!(
        (data.sent, data.billable, data.reserved_persisted),
        (8, 8, 8)
    );
    assert_eq!(bed.ledger.lock().gross_units(false), (8 + 2) * 300);
    // The converted row differs from the stored one, so the next commit writes it.
    let rows = bed.ledger.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0],
        counter_row("2026-10", "fleet_polling", "data", 8, 8, 8)
    );
    // CHECK(billable <= sent) and CHECK(reserved >= billable) hold.
    assert!(rows[0].billable <= rows[0].sent && rows[0].reserved >= rows[0].billable);
    // After the commit nothing differs.
    bed.ledger.mark_persisted(&rows);
    assert!(!bed.ledger.dirty());
    // A request needs a new reservation: all of the old one counts as used.
    assert_eq!(
        bed.ledger.start(RequestCategory::Data),
        Admission::Withheld(Withheld::Reservation)
    );
}

#[test]
fn restart_keeps_the_rows_when_nothing_is_reserved_beyond_billable() {
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    bed.ledger
        .load(&[counter_row("2026-10", "fleet_polling", "data", 9, 7, 7)]);
    let data = bed.ledger.lock().counts[RequestCategory::Data.index()];
    assert_eq!((data.sent, data.billable), (9, 7));
    assert!(!bed.ledger.dirty());
}

#[test]
fn a_stored_cap_stop_is_lifted_at_the_start_only_when_the_cap_now_allows_a_request() {
    let rows = [counter_row("2026-10", "fleet_polling", "data", 5, 5, 5)];
    for (cap_cents, resumes) in [(1_u32, false), (2, true)] {
        let t0 = Instant::now();
        let clock = ManualClock::new(t0, utc_ms(2026, 10, 15, 0));
        let mut scheduler = VehicleScheduler::new_with(
            core_cadence(),
            crate::config::CollectorMode::FleetPolling,
            k7_policy(cap_cents, crate::config::FLEET_PRICING),
            t0,
            clock,
            FixedJitter::constant(0.0),
        );
        scheduler.account.state = AccountState::CapReached;
        scheduler.account.state_reason = "cap_reached";
        scheduler.load_counters(&rows, t0);
        assert_eq!(
            scheduler.account.state == AccountState::Collecting,
            resumes,
            "cap of {cap_cents} cents"
        );
        if resumes {
            assert_eq!(scheduler.account.state_reason, "cap_cleared");
        }
    }
}

// ---- Owner wire counts (R25) ------------------------------------------------------------------

#[test]
fn owner_wire_requests_are_counted_by_category_and_never_billed() {
    let t0 = Instant::now();
    let clock = ManualClock::new(t0, utc_ms(2026, 10, 15, 0));
    let mut scheduler = VehicleScheduler::new_with(
        core_cadence(),
        crate::config::CollectorMode::Legacy,
        core_cadence().fleet_policy(),
        t0,
        clock.clone(),
        FixedJitter::constant(0.0),
    );
    for category in [
        RequestCategory::State,
        RequestCategory::Data,
        RequestCategory::Data,
        RequestCategory::State,
    ] {
        assert_eq!(scheduler.admit_request(category, t0), Admission::Admitted);
        scheduler.ledger.complete(category, true);
    }
    let status = scheduler.status(t0);
    assert_eq!(status.month, "2026-10");
    assert_eq!(status.requests[RequestCategory::Data.index()].1, 2);
    assert_eq!(status.requests[RequestCategory::State.index()].1, 2);
    // No estimate, no billing, no reservation, whatever is sent.
    assert_eq!(status.estimate, None);
    let counts = scheduler.ledger.lock().counts;
    assert!(
        counts
            .iter()
            .all(|count| count.billable == 0 && count.reserved_pending == 0)
    );
    // The cap does not apply: a thousand reads are admitted.
    for _ in 0..1_000 {
        assert_eq!(
            scheduler.admit_request(RequestCategory::Data, t0),
            Admission::Admitted
        );
    }
}

#[test]
fn owner_wire_counters_ride_on_a_commit_that_happens_anyway_or_wait_an_hour() {
    let t0 = Instant::now();
    let clock = ManualClock::new(t0, utc_ms(2026, 10, 15, 0));
    let mut scheduler = VehicleScheduler::new_with(
        core_cadence(),
        crate::config::CollectorMode::Legacy,
        core_cadence().fleet_policy(),
        t0,
        clock.clone(),
        FixedJitter::constant(0.0),
    );
    // The stored `sent` counts the requests whose outcome is known (R25, B2
    // clarification), so each request is answered before the counters can be due.
    scheduler.admit_request(RequestCategory::Data, t0);
    scheduler.ledger.complete(RequestCategory::Data, false);
    let mut tick = PendingTick::default();
    // No other write and no hour passed: the counters are not written for a poll.
    scheduler.attach_counters(&mut tick, false, false);
    assert!(tick.counters.is_empty());
    // A write that happens anyway carries them.
    scheduler.attach_counters(&mut tick, true, false);
    assert_eq!(
        tick.counters,
        vec![counter_row("2026-10", "legacy", "data", 1, 0, 0)]
    );
    // Once committed nothing is due; an hour later a new count is flushed alone.
    scheduler.commit_succeeded(std::mem::take(&mut tick), t0);
    scheduler.admit_request(RequestCategory::Data, t0);
    scheduler.ledger.complete(RequestCategory::Data, false);
    scheduler.attach_counters(&mut tick, false, false);
    assert!(tick.counters.is_empty());
    clock.advance(Duration::from_secs(60 * 60));
    scheduler.attach_counters(&mut tick, false, false);
    assert_eq!(
        tick.counters,
        vec![counter_row("2026-10", "legacy", "data", 2, 0, 0)]
    );
    // And a graceful stop flushes whatever is left.
    let mut stop = PendingTick::default();
    scheduler.commit_succeeded(std::mem::take(&mut tick), t0);
    scheduler.admit_request(RequestCategory::State, t0);
    scheduler.ledger.complete(RequestCategory::State, false);
    scheduler.attach_counters(&mut stop, false, true);
    assert_eq!(stop.counters.len(), 1);
}

#[test]
fn a_wake_or_a_command_is_counted_and_reduces_the_headroom_but_is_never_blocked() {
    // One cent is 1,500 U; a wake costs 3,000 U.
    let bed = LedgerBed::fleet(1, utc_ms(2026, 10, 15, 0));
    bed.ledger.record_completed(RequestCategory::Wake, true);
    bed.ledger.record_completed(RequestCategory::Command, true);
    assert_eq!(bed.ledger.lock().gross_units(false), 3_150);
    let estimate = bed.ledger.view().estimate.expect("estimate");
    assert_eq!(estimate.gross_cents, 3);
    // The scheduled path has no headroom left.
    bed.reserve(&[(RequestCategory::Data, 1)]);
    assert_eq!(
        bed.ledger.start(RequestCategory::Data),
        Admission::Withheld(Withheld::Cap)
    );
    // The counted requests stay: a further wake is still counted, not blocked.
    bed.ledger.record_completed(RequestCategory::Wake, true);
    assert_eq!(bed.sent(RequestCategory::Wake), 2);
}

#[test]
fn counts_never_break_the_tables_checks() {
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    // A foreign request after a scheduled one raises the reservation with it.
    bed.reserve(&[(RequestCategory::Data, 1)]);
    assert_eq!(bed.ledger.start(RequestCategory::Data), Admission::Admitted);
    bed.ledger.complete(RequestCategory::Data, true);
    bed.ledger.record_completed(RequestCategory::Data, true);
    bed.ledger.record_completed(RequestCategory::Data, true);
    for row in bed.ledger.rows() {
        assert!(row.billable <= row.sent, "{row:?}");
        assert!(row.reserved >= row.billable, "{row:?}");
    }
    // A completion without a start (a month change in between) cannot pass `sent`.
    let other = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    other.ledger.complete(RequestCategory::Data, true);
    assert_eq!(
        other.ledger.lock().counts[RequestCategory::Data.index()].billable,
        0
    );
}

#[test]
fn a_request_in_flight_at_a_month_change_counts_in_the_new_month() {
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 31, 23));
    bed.reserve(&[(RequestCategory::Data, 1)]);
    assert_eq!(bed.ledger.start(RequestCategory::Data), Admission::Admitted);
    bed.clock.advance(Duration::from_secs(2 * 60 * 60));
    assert!(matches!(
        bed.ledger.refresh_month(),
        MonthEvent::Rolled { .. }
    ));
    // It finishes in the new month, whose count it joined, without breaking the CHECKs.
    bed.ledger.complete(RequestCategory::Data, true);
    let count = bed.ledger.lock().counts[RequestCategory::Data.index()];
    assert_eq!((count.sent, count.billable, count.in_flight), (1, 1, 0));
    assert!(count.reserved_pending >= count.billable);
}

#[test]
fn only_a_ledger_that_loaded_the_store_flushes_what_a_one_shot_sender_counted() {
    let dir = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(dir.path()).expect("store");
    let policy = k7_policy(1_000, crate::config::FLEET_PRICING);
    let first = RequestLedger::standalone(&store, &policy).expect("ledger");
    first.record_fleet_result(RequestCategory::State, &Ok::<(), FleetApiError>(()));
    first.record_fleet_result(RequestCategory::Command, &Ok::<(), FleetApiError>(()));
    first.record_fleet_result(
        RequestCategory::Command,
        &Err::<(), _>(FleetApiError::HttpStatus(503)),
    );
    first.flush_to_store(&store).expect("flush");
    let month = first.view().month;
    let rows = store.load_collector_state_rows().expect("rows").counters;
    // Rows come back ordered by category; the 503 is a sent request that is not
    // billed.
    assert_eq!(
        rows,
        vec![
            counter_row(&month, "fleet_polling", "command", 2, 1, 1),
            counter_row(&month, "fleet_polling", "state", 1, 1, 1),
        ]
    );
    // A second command, like the next setup run, adds to the stored count.
    let second = RequestLedger::standalone(&store, &policy).expect("ledger");
    second.record_fleet_result(RequestCategory::State, &Ok::<(), FleetApiError>(()));
    second.flush_to_store(&store).expect("flush");
    let rows = store.load_collector_state_rows().expect("rows").counters;
    assert_eq!(
        rows.iter().find(|row| row.category == "state"),
        Some(&counter_row(&month, "fleet_polling", "state", 2, 2, 2))
    );
    // The command row was not touched: nothing reserved beyond billable to convert.
    assert_eq!(
        rows.iter().find(|row| row.category == "command"),
        Some(&counter_row(&month, "fleet_polling", "command", 2, 1, 1))
    );
    // Nothing is written when nothing changed.
    second.flush_to_store(&store).expect("flush");
}

// ---- The Fleet helpers count their resends (R24, R47) ----------------------------------------------

#[tokio::test]
async fn a_resend_after_a_refresh_is_a_second_counted_and_billed_request() {
    let bed = ct67_bed().await;
    let ledger = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    ledger.reserve(&[(RequestCategory::Data, 2)]);
    assert_eq!(
        ledger.ledger.start(RequestCategory::Data),
        Admission::Admitted
    );
    // The first read meets the expired token, refreshes once and is resent once.
    fleet_vehicle_data_with_auth(
        &bed.api,
        &bed.auth_api,
        &bed.manager,
        &bed.vehicles[0],
        true,
        CountedStart::admitted(Some(&ledger.ledger), RequestCategory::Data),
        &TickRefresh::default(),
    )
    .await
    .expect("the resend succeeds");
    assert_eq!(bed.wire.data_authorisations.lock().expect("log").len(), 2);
    let count = ledger.ledger.lock().counts[RequestCategory::Data.index()];
    // Both requests were sent and both are billed (a 401 is below 500).
    assert_eq!((count.sent, count.billable, count.in_flight), (2, 2, 0));
}

#[tokio::test]
async fn a_resend_the_reservation_does_not_cover_is_not_sent() {
    let bed = ct67_bed().await;
    let ledger = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    ledger.reserve(&[(RequestCategory::Data, 1)]);
    assert_eq!(
        ledger.ledger.start(RequestCategory::Data),
        Admission::Admitted
    );
    let error = fleet_vehicle_data_with_auth(
        &bed.api,
        &bed.auth_api,
        &bed.manager,
        &bed.vehicles[0],
        true,
        CountedStart::admitted(Some(&ledger.ledger), RequestCategory::Data),
        &TickRefresh::default(),
    )
    .await
    .expect_err("the resend is withheld");
    // The resend is no answer, no failure and no wait: the request stays due (C3 R27,
    // R47, B2 clarification). The first 401 is not what the caller sees.
    assert!(matches!(error, CollectorError::FleetResendWithheld), "{error:?}");
    // One data request went out, and none beyond the reservation.
    assert_eq!(bed.wire.data_authorisations.lock().expect("log").len(), 1);
    let count = ledger.ledger.lock().counts[RequestCategory::Data.index()];
    assert_eq!((count.sent, count.billable), (1, 1));
}

// ---- The Fleet tick against a fake Fleet server ----------------------------------------------------

/// What the fake Fleet server saw and what it answers.
#[derive(Clone)]
struct FleetFake {
    lists: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
    /// The `state` the vehicle list reports.
    state: Arc<Mutex<String>>,
    /// The Hub wall time the read's group timestamps carry.
    timestamp_ms: Arc<std::sync::atomic::AtomicI64>,
    /// The reads say the car is driving (gear D, moving): it is read every driving
    /// interval and never goes idle.
    driving: Arc<std::sync::atomic::AtomicBool>,
    store: HubStore,
    /// Whether lists are billed, and so need a reservation (C3 R24).
    audit_state: bool,
    /// Requests that arrived with no committed reservation covering them.
    violations: Arc<Mutex<Vec<String>>>,
    /// The status the list answers with (200 unless a test changes it).
    list_status: Arc<std::sync::atomic::AtomicU16>,
    /// The status a read answers with (200 unless a test changes it).
    read_status: Arc<std::sync::atomic::AtomicU16>,
    /// Requests answer 401 unless they carry the token the refresh endpoint hands
    /// out (`fleet-next-access`), as when the stored access token has expired.
    token_expired: Arc<std::sync::atomic::AtomicBool>,
    /// Every request answers 401, whatever token it carries: the credential itself
    /// is refused (C3 R47, CT67c, CT74).
    reject_every_token: Arc<std::sync::atomic::AtomicBool>,
    /// Requests to the token endpoint.
    token_requests: Arc<AtomicUsize>,
    /// The status the token endpoint answers with (200 unless a test changes it).
    token_status: Arc<std::sync::atomic::AtomicU16>,
    /// While set, the token endpoint holds every request unanswered, so an exchange
    /// the collector started stays in flight.
    token_hold: Arc<std::sync::atomic::AtomicBool>,
    /// While set, an answer to the account list is held back once the request has been
    /// counted (the response of a tick that has not completed).
    list_hold: Arc<std::sync::atomic::AtomicBool>,
    /// While set, every answer of the token endpoint carries a refresh token of its own
    /// (numbered by the request), so each refresh records a successor; unset, they all
    /// carry the same pair.
    rotate_refresh: Arc<std::sync::atomic::AtomicBool>,
    /// The `Authorization` header of every list and read, in arrival order.
    authorisations: Arc<Mutex<Vec<String>>>,
    /// How many cars the list names (ids 1..=cars).
    cars: usize,
    /// A test's script for the account list: called inside the handler at each list's
    /// arrival, after it was counted and before its response is chosen, with the
    /// request's ordinal. `Some(status)` answers that request with the status; `None`
    /// leaves the answer to `list_status`. The request stays unanswered while the script
    /// runs, so what it reads of the store and the loop is the state the request met.
    list_script: Arc<Mutex<Option<ListScript>>>,
    /// Every list the double answered: its ordinal, the places of its arrival and of
    /// its release in one shared sequence, and the status it was released with.
    list_responses: Arc<Mutex<Vec<ListResponse>>>,
    /// The shared sequence of list arrivals and releases.
    list_sequence: Arc<AtomicUsize>,
    /// The in-process transport of the bed (the loop beds that serve a socket leave it
    /// unused).
    transport: FleetTransportControl,
}

/// See [`FleetFake::list_script`].
type ListScript = Arc<dyn Fn(usize) -> Option<u16> + Send + Sync>;

/// One list the double answered (see [`FleetFake::list_responses`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ListResponse {
    ordinal: usize,
    arrived: usize,
    released: usize,
    status: u16,
}

impl FleetFake {
    /// The `state` requests the store holds a reservation for this month, and the ones
    /// it counts as sent (C3 R27: the reservation covers the requests before they are
    /// sent, the sent count is written by the tick that sent them).
    fn state_reservation(&self) -> (i64, i64) {
        self.store
            .open()
            .expect("connection")
            .query_row(
                "SELECT COALESCE(SUM(reserved), 0), COALESCE(SUM(sent), 0)
                   FROM collector_request_counters
                  WHERE mode = 'fleet_polling' AND category = 'state'
                    AND month = (SELECT MAX(month) FROM collector_request_counters)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("reservation")
    }

    fn set_list_script(&self, script: Option<ListScript>) {
        *self.list_script.lock().expect("list script") = script;
    }

    /// At every arrival, the requests of the category received so far (this one
    /// included) must not exceed the reservation the store holds (C3 R27): the
    /// reservation is committed before the request is sent, so a crash cannot hide
    /// a request from the cap.
    fn audit(&self, category: &str, received: usize) {
        if category == "state" && !self.audit_state {
            return;
        }
        let reserved: i64 = self
            .store
            .open()
            .expect("connection")
            .query_row(
                "SELECT COALESCE(SUM(reserved), 0) FROM collector_request_counters
                  WHERE mode = 'fleet_polling' AND category = ?1
                    AND month = (SELECT MAX(month) FROM collector_request_counters)",
                [category],
                |row| row.get(0),
            )
            .expect("reservation");
        if i64::try_from(received).expect("count") > reserved {
            self.violations.lock().expect("violations").push(format!(
                "{category} request {received} exceeds the persisted reservation {reserved}"
            ));
        }
    }
}

/// The answer a fake request gets before its own handling: 401 while the token is
/// expired and the request does not carry the refreshed one.
fn fleet_fake_unauthorised(fake: &FleetFake, headers: &HeaderMap) -> bool {
    let authorisation = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    fake.authorisations
        .lock()
        .expect("authorisations")
        .push(authorisation.clone());
    fake.reject_every_token.load(Ordering::SeqCst)
        || (fake.token_expired.load(Ordering::SeqCst)
            && authorisation != "Bearer fleet-next-access")
}

async fn fleet_fake_token(State(fake): State<FleetFake>) -> axum::response::Response {
    let ordinal = fake.token_requests.fetch_add(1, Ordering::SeqCst) + 1;
    while fake.token_hold.load(Ordering::SeqCst) {
        sleep(Duration::from_millis(10)).await;
    }
    let status = fake.token_status.load(Ordering::SeqCst);
    if status != 200 {
        return (
            StatusCode::from_u16(status).expect("a status"),
            r#"{"error":"invalid_grant"}"#,
        )
            .into_response();
    }
    let refresh = if fake.rotate_refresh.load(Ordering::SeqCst) {
        format!("fleet-next-refresh-{ordinal}")
    } else {
        "fleet-next-refresh".to_owned()
    };
    (
        StatusCode::OK,
        format!(
            r#"{{"access_token":"fleet-next-access","refresh_token":"{refresh}","expires_in":28800,"token_type":"Bearer"}}"#
        ),
    )
        .into_response()
}

async fn fleet_fake_list(
    State(fake): State<FleetFake>,
    headers: HeaderMap,
) -> axum::response::Response {
    let received = fake.lists.fetch_add(1, Ordering::SeqCst) + 1;
    let arrived = fake.list_sequence.fetch_add(1, Ordering::SeqCst) + 1;
    let response = fleet_fake_list_answer(&fake, &headers, received).await;
    // The response is released here: its status is the one the request got.
    let released = fake.list_sequence.fetch_add(1, Ordering::SeqCst) + 1;
    fake.list_responses.lock().expect("list responses").push(ListResponse {
        ordinal: received,
        arrived,
        released,
        status: response.status().as_u16(),
    });
    response
}

async fn fleet_fake_list_answer(
    fake: &FleetFake,
    headers: &HeaderMap,
    received: usize,
) -> axum::response::Response {
    fake.audit("state", received);
    // The test's script sees the request unanswered, before its response is chosen.
    let script = fake.list_script.lock().expect("list script").clone();
    let scripted = script.and_then(|script| script(received));
    while fake.list_hold.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    if fleet_fake_unauthorised(fake, headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let status = scripted.unwrap_or_else(|| fake.list_status.load(Ordering::SeqCst));
    if status != 200 {
        return StatusCode::from_u16(status)
            .expect("a status")
            .into_response();
    }
    let state = fake.state.lock().expect("state").clone();
    let cars = (1..=fake.cars)
        .map(|id| {
            format!(
                r#"{{"id":{id},"vehicle_id":{id},"vin":"5YJ3E1EA7KF00000{id}","state":"{state}"}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    (
        StatusCode::OK,
        format!(r#"{{"response":[{cars}],"count":{}}}"#, fake.cars),
    )
        .into_response()
}

async fn fleet_fake_read(
    State(fake): State<FleetFake>,
    headers: HeaderMap,
) -> axum::response::Response {
    let received = fake.reads.fetch_add(1, Ordering::SeqCst) + 1;
    fake.audit("data", received);
    if fleet_fake_unauthorised(&fake, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let status = fake.read_status.load(Ordering::SeqCst);
    if status != 200 {
        return StatusCode::from_u16(status)
            .expect("a status")
            .into_response();
    }
    // Zero means the Hub wall clock now: the real-time loop tests (CT55b, CT74) have
    // no manual clock to keep the timestamps in step with.
    let timestamp = match fake.timestamp_ms.load(Ordering::SeqCst) {
        0 => current_epoch_millis().unwrap_or(0),
        fixed => fixed,
    };
    // A driving car has a position that moves with the time and an odometer, so the
    // drive it makes is stored as a drive (C1 R43).
    let (motion, vehicle_state) = if fake.driving.load(Ordering::SeqCst) {
        let step = timestamp.rem_euclid(1_000_000);
        (
            format!(
                r#""shift_state":"D","speed":40,"power":12,"latitude":{},"longitude":19.0,"#,
                47.5 + step as f64 * 1e-7
            ),
            format!(r#","vehicle_state":{{"odometer":{}}}"#, 1000.0 + step as f64 * 1e-6),
        )
    } else {
        (String::new(), String::new())
    };
    (
        StatusCode::OK,
        format!(
            r#"{{"response":{{"drive_state":{{{motion}"timestamp":{timestamp}}},"charge_state":{{"battery_level":80,"timestamp":{timestamp}}}{vehicle_state}}}}}"#
        ),
    )
        .into_response()
}

/// A store with one configured Fleet car, a scheduler on a manual clock, and the
/// Fleet clients pointed at an in-process fake server.
struct FleetBed {
    _dir: tempfile::TempDir,
    store: HubStore,
    scheduler: VehicleScheduler,
    clock: Arc<ManualClock>,
    policy: crate::config::FleetPolicy,
    identities: IdentityCache,
    cursor_key: CursorKey,
    terrain: mpsc::Sender<()>,
    _terrain_rx: mpsc::Receiver<()>,
    api: FleetApi,
    auth_api: FleetAuthApi,
    manager: Arc<tokio::sync::Mutex<FleetAuthManager>>,
    collector_state: watch::Sender<SupervisedCollectorState>,
    _collector_state_rx: watch::Receiver<SupervisedCollectorState>,
    fake: FleetFake,
    /// Whether the tick may refresh the token and resend (the owner's own
    /// credentials; an observer may not).
    allow_refresh: bool,
}

impl FleetBed {
    async fn new(policy: crate::config::FleetPolicy, reported_state: &str) -> Self {
        Self::with_cars(policy, reported_state, 1).await
    }

    async fn with_cars(
        policy: crate::config::FleetPolicy,
        reported_state: &str,
        cars: usize,
    ) -> Self {
        let dir = crate::private_tempdir().expect("temporary Hub");
        let store = HubStore::initialize(dir.path()).expect("Hub store");
        let cursor_key = crate::teslamate_credentials::load_or_create_cursor_key(dir.path())
            .expect("cursor key");
        finish_collection_for_provider(
            &store,
            &cursor_key,
            &ManualCollection {
                vehicles: (1..=cars)
                    .map(|id| {
                        Vehicle::for_test(
                            u64::try_from(id).expect("id"),
                            &format!("5YJ3E1EA7KF00000{id}"),
                            "online",
                        )
                    })
                    .collect(),
                snapshots: Vec::new(),
                failures: Vec::new(),
            },
            CollectorProvider::Fleet,
        )
        .await
        .expect("configure the cars");
        let credentials = FleetSetupCredentials::new(
            CT67_SCOPED_ACCESS.to_owned(),
            "fleet-refresh".to_owned(),
            "fleet-client".to_owned(),
            crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
            28_800,
        )
        .expect("setup credentials");
        crate::fleet_credentials::persist_fleet_setup_credentials(
            &store,
            dir.path(),
            &credentials,
            SystemTime::now(),
        )
        .expect("persist credentials");
        let manager = FleetAuthManager::from_store(store.clone(), dir.path()).expect("manager");
        let t0 = Instant::now();
        let clock = ManualClock::new(t0, CORE_W0);
        let fake = FleetFake {
            lists: Arc::new(AtomicUsize::new(0)),
            reads: Arc::new(AtomicUsize::new(0)),
            state: Arc::new(Mutex::new(reported_state.to_owned())),
            timestamp_ms: Arc::new(std::sync::atomic::AtomicI64::new(CORE_W0)),
            driving: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            store: store.clone(),
            audit_state: policy.pricing.state.is_some(),
            violations: Arc::new(Mutex::new(Vec::new())),
            list_status: Arc::new(std::sync::atomic::AtomicU16::new(200)),
            read_status: Arc::new(std::sync::atomic::AtomicU16::new(200)),
            token_expired: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reject_every_token: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            token_requests: Arc::new(AtomicUsize::new(0)),
            token_status: Arc::new(std::sync::atomic::AtomicU16::new(200)),
            token_hold: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            list_hold: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rotate_refresh: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            authorisations: Arc::new(Mutex::new(Vec::new())),
            cars,
            list_script: Arc::new(Mutex::new(None)),
            list_responses: Arc::new(Mutex::new(Vec::new())),
            list_sequence: Arc::new(AtomicUsize::new(0)),
            transport: FleetTransportControl::default(),
        };
        let router = Router::new()
            .route("/api/1/vehicles", get(fleet_fake_list))
            .route("/api/1/vehicles/{vin}/vehicle_data", get(fleet_fake_read))
            .route("/oauth2/v3/token", post(fleet_fake_token))
            .with_state(fake.clone());
        // No socket, no server task and no transport deadline: each request is awaited
        // in the caller's task and answered by the router in process (R-P3bF4-1).
        let transport = fake.transport.transport(router);
        let base = url::Url::parse("http://127.0.0.1:1/").expect("base");
        let api = FleetApi::for_in_process_test(base.clone(), transport.clone())
            .expect("Fleet client");
        let auth_api = FleetAuthApi::for_in_process_test(
            base.join("oauth2/v3/token").expect("token URL"),
            transport,
        )
        .expect("auth client");
        let (terrain, terrain_rx) = mpsc::channel(8);
        let (collector_state, collector_state_rx) =
            watch::channel(SupervisedCollectorState::Active);
        let mut bed = Self {
            _dir: dir,
            store,
            scheduler: VehicleScheduler::new_with(
                core_cadence(),
                crate::config::CollectorMode::FleetPolling,
                policy,
                t0,
                clock.clone(),
                FixedJitter::constant(0.0),
            ),
            clock,
            policy,
            identities: IdentityCache::default(),
            cursor_key,
            terrain,
            _terrain_rx: terrain_rx,
            api,
            auth_api,
            manager: Arc::new(tokio::sync::Mutex::new(manager)),
            collector_state,
            _collector_state_rx: collector_state_rx,
            fake,
            allow_refresh: false,
        };
        bed.reload();
        bed
    }

    /// A restarted process: a new scheduler that loads what the store holds, on the
    /// same clocks.
    fn reload(&mut self) {
        let now = self.clock.mono_now();
        let mut scheduler = VehicleScheduler::new_with(
            core_cadence(),
            crate::config::CollectorMode::FleetPolling,
            self.policy,
            now,
            self.clock.clone(),
            FixedJitter::constant(0.0),
        );
        scheduler.config_fingerprint = DS_FP_A.to_owned();
        let snapshot = CollectorStateSnapshot::from_rows(
            self.store.load_collector_state_rows().expect("rows"),
            DS_FP_A.to_owned(),
            &self.store.configured_tesla_vehicles().expect("configured"),
        );
        scheduler.load(&snapshot, now);
        self.scheduler = scheduler;
        self.identities = IdentityCache::default();
    }

    fn advance(&self, by: Duration) {
        self.clock.advance(by);
        self.fake
            .timestamp_ms
            .store(self.clock.wall_now_ms().expect("wall"), Ordering::SeqCst);
    }

    /// The loop's iteration: settings, the clock watch, then the tick itself.
    async fn tick(&mut self) -> FleetTickOutcome {
        let configured = self.store.configured_tesla_vehicles().expect("configured");
        let now = self.clock.mono_now();
        self.scheduler.apply_control_settings(&configured, now);
        self.scheduler.observe_clock(now);
        let ledger = self.scheduler.request_ledger();
        let env = FleetTickEnv {
            store: &self.store,
            cursor_key: &self.cursor_key,
            terrain_wake: &self.terrain,
            offline_drive_timeout: Duration::from_secs(600),
            api: &self.api,
            auth_api: &self.auth_api,
            manager: &self.manager,
            ledger: &ledger,
            allow_refresh: self.allow_refresh,
            collector_state: &self.collector_state,
            configured: &configured,
        };
        run_fleet_tick(&env, &mut self.scheduler, &mut self.identities, &[])
            .await
            .expect("tick")
    }

    /// The same tick, returning the loop's own error instead of expecting none.
    async fn try_tick(&mut self) -> Result<FleetTickOutcome, CollectorError> {
        let configured = self.store.configured_tesla_vehicles().expect("configured");
        let now = self.clock.mono_now();
        self.scheduler.apply_control_settings(&configured, now);
        self.scheduler.observe_clock(now);
        let ledger = self.scheduler.request_ledger();
        let env = FleetTickEnv {
            store: &self.store,
            cursor_key: &self.cursor_key,
            terrain_wake: &self.terrain,
            offline_drive_timeout: Duration::from_secs(600),
            api: &self.api,
            auth_api: &self.auth_api,
            manager: &self.manager,
            ledger: &ledger,
            allow_refresh: self.allow_refresh,
            collector_state: &self.collector_state,
            configured: &configured,
        };
        run_fleet_tick(&env, &mut self.scheduler, &mut self.identities, &[]).await
    }

    /// The first half of a tick that is cut short: the list is reserved, sent and
    /// read, and the process dies before the tick commits (C3 CT62).
    async fn crash_after_the_list(&mut self) {
        let configured = self.store.configured_tesla_vehicles().expect("configured");
        let now = self.clock.mono_now();
        self.scheduler.apply_control_settings(&configured, now);
        self.scheduler.observe_clock(now);
        let ledger = self.scheduler.request_ledger();
        self.scheduler.begin_tick();
        assert!(self.scheduler.discovery_due(now));
        let mut run = TickRun {
            scheduler: &mut self.scheduler,
            identities: &mut self.identities,
            commit: TickCommit {
                store: &self.store,
                cursor_key: &self.cursor_key,
                provider: CollectorProvider::Fleet,
                offline_drive_timeout: Duration::from_secs(600),
            },
            batch: TickBatch::default(),
            terrain_wake: &self.terrain,
            any_wrote: false,
        };
        reserve_for(&mut run, &[(RequestCategory::State, 1)])
            .await
            .expect("reserve");
        assert_eq!(
            run.scheduler.admit_request(RequestCategory::State, now),
            Admission::Admitted
        );
        fleet_list_vehicles_with_auth(
            &self.api,
            &self.auth_api,
            &self.manager,
            self.allow_refresh,
            Some(&ledger),
            &TickRefresh::default(),
        )
        .await
        .expect("the list");
        // The process is gone: no commit, nothing carried over but the store.
    }

    /// A read cut short the same way.
    async fn crash_after_a_read(&mut self) {
        let now = self.clock.mono_now();
        self.scheduler.observe_clock(now);
        let ledger = self.scheduler.request_ledger();
        self.scheduler.begin_tick();
        let due = self.scheduler.due_vehicles(now);
        assert_eq!(due.len(), 1, "the car's read is due");
        let vehicle = self.scheduler.vehicles()[0].clone();
        let mut run = TickRun {
            scheduler: &mut self.scheduler,
            identities: &mut self.identities,
            commit: TickCommit {
                store: &self.store,
                cursor_key: &self.cursor_key,
                provider: CollectorProvider::Fleet,
                offline_drive_timeout: Duration::from_secs(600),
            },
            batch: TickBatch::default(),
            terrain_wake: &self.terrain,
            any_wrote: false,
        };
        reserve_for(&mut run, &[(RequestCategory::Data, 1)])
            .await
            .expect("reserve");
        assert_eq!(
            run.scheduler.admit_request(RequestCategory::Data, now),
            Admission::Admitted
        );
        fleet_vehicle_data_with_auth(
            &self.api,
            &self.auth_api,
            &self.manager,
            &vehicle,
            self.allow_refresh,
            CountedStart::admitted(Some(&ledger), RequestCategory::Data),
            &TickRefresh::default(),
        )
        .await
        .expect("the read");
    }

    fn lists(&self) -> usize {
        self.fake.lists.load(Ordering::SeqCst)
    }

    fn reads(&self) -> usize {
        self.fake.reads.load(Ordering::SeqCst)
    }

    fn rows(&self) -> Vec<crate::db::CollectorCounterRow> {
        self.store
            .load_collector_state_rows()
            .expect("rows")
            .counters
    }

    fn row(&self, category: &str) -> Option<crate::db::CollectorCounterRow> {
        self.rows().into_iter().find(|row| row.category == category)
    }

    fn violations(&self) -> Vec<String> {
        self.fake.violations.lock().expect("violations").clone()
    }

    fn stored_account_state(&self) -> Option<String> {
        self.store
            .load_collector_state_rows()
            .expect("rows")
            .account
            .map(|account| account.state)
    }
}

#[tokio::test]
async fn ct13_a_fleet_tick_whose_request_is_not_covered_makes_one_lone_reservation_commit() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::new(crate::config::CollectorMode::FleetPolling);
            let policy = core_cadence().fleet_policy();
            assert_eq!(policy.monthly_cap_cents, 1_000);
            let now = bed.now();
            bed.scheduler.begin_tick();
            let mut run = TickRun {
                scheduler: &mut bed.scheduler,
                identities: &mut bed.identities,
                commit: TickCommit {
                    store: &bed.store,
                    cursor_key: &bed.cursor_key,
                    provider: bed.provider,
                    offline_drive_timeout: Duration::from_secs(600),
                },
                batch: TickBatch::default(),
                terrain_wake: &bed.terrain,
                any_wrote: false,
            };
            // The next request, a list, is not covered by the persisted reservation and
            // no result is pending: one lone commit.
            reserve_for(&mut run, &[(RequestCategory::State, 1)])
                .await
                .expect("reserve");
            assert_eq!(seam.tick_commits(), 1);
            assert_eq!(run.scheduler.status(now).reservation_commits, 1);
            // Covered now: asking again commits nothing and counts nothing.
            reserve_for(&mut run, &[(RequestCategory::State, 1)])
                .await
                .expect("reserve");
            assert_eq!(seam.tick_commits(), 1);
            assert_eq!(run.scheduler.status(now).reservation_commits, 1);
            let rows = bed
                .store
                .load_collector_state_rows()
                .expect("rows")
                .counters;
            assert_eq!(
                rows,
                vec![counter_row(
                    &bed.scheduler.status(now).month,
                    "fleet_polling",
                    "state",
                    0,
                    0,
                    1
                )]
            );
        })
        .await;
}

#[tokio::test]
async fn ct13_a_reservation_that_rides_on_waiting_results_is_not_a_lone_commit() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::new(crate::config::CollectorMode::FleetPolling);
            let now = bed.now();
            let candidates = bed
                .scheduler
                .accept_discovery(vec![core_vehicle(1, "online")], now);
            bed.register(&candidates[0]);
            bed.scheduler.begin_tick();
            let mut run = TickRun {
                scheduler: &mut bed.scheduler,
                identities: &mut bed.identities,
                commit: TickCommit {
                    store: &bed.store,
                    cursor_key: &bed.cursor_key,
                    provider: bed.provider,
                    offline_drive_timeout: Duration::from_secs(600),
                },
                batch: TickBatch::default(),
                terrain_wake: &bed.terrain,
                any_wrote: false,
            };
            run.batch.discoveries.push(DiscoveryItem {
                vehicle: candidates[0].clone(),
                received_at_ms: CORE_W0,
            });
            run.batch.note_result(now);
            reserve_for(&mut run, &[(RequestCategory::Data, 1)])
                .await
                .expect("reserve");
            // One commit carried the list's result and the reservation, and it was not
            // a lone reservation commit.
            assert_eq!(seam.tick_commits(), 1);
            assert_eq!(run.scheduler.status(now).reservation_commits, 0);
            assert!(
                bed.store
                    .load_collector_state_rows()
                    .expect("rows")
                    .counters
                    .len()
                    == 1
            );
        })
        .await;
}

#[tokio::test]
async fn ct16_the_cap_stops_the_fake_at_five_requests_and_the_stop_survives_a_restart() {
    let mut bed = FleetBed::new(k7_policy(1, crate::config::FLEET_PRICING), "online").await;
    // Tick 1: the list (300 U) and the first read (300 U).
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 1));
    // Reads 2, 3 and 4 follow at the Online interval: 1,500 U in all.
    for expected in 2..=4 {
        bed.advance(Duration::from_secs(120));
        bed.tick().await;
        assert_eq!(bed.reads(), expected);
    }
    assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
    // The next billable request is not sent: the fake still counts five in all.
    bed.advance(Duration::from_secs(120));
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 4));
    assert_eq!(bed.scheduler.account.state, AccountState::CapReached);
    assert_eq!(bed.stored_account_state().as_deref(), Some("cap_reached"));
    let gaps: i64 = bed
        .store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COUNT(*) FROM collection_gaps WHERE kind = 'cap_reached' AND ended_at_ms IS NULL",
            [],
            |row| row.get(0),
        )
        .expect("gaps");
    assert_eq!(gaps, 1);
    // Whatever time passes in this month, nothing more is sent, state checks included.
    for _ in 0..3 {
        bed.advance(Duration::from_secs(3_600));
        bed.tick().await;
    }
    assert_eq!((bed.lists(), bed.reads()), (1, 4));
    // Drop and reload the scheduler from the store: still CapReached, no request.
    bed.reload();
    assert_eq!(bed.scheduler.account.state, AccountState::CapReached);
    bed.advance(Duration::from_secs(120));
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 4));
    assert!(bed.violations().is_empty(), "{:?}", bed.violations());
    // The next month's first tick: collecting again, the gap closed, requests resume.
    let next_month = MonthSpan::containing(bed.clock.wall_now_ms().expect("wall"))
        .expect("month")
        .end_ms;
    let wall = bed.clock.wall_now_ms().expect("wall");
    bed.advance(Duration::from_millis(
        u64::try_from(next_month - wall + 60_000).expect("ms"),
    ));
    bed.tick().await;
    assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
    assert_eq!((bed.lists(), bed.reads()), (2, 5), "requests resume");
    assert_eq!(bed.stored_account_state().as_deref(), Some("collecting"));
    let (open, closed): (i64, i64) = bed
        .store
        .open()
        .expect("connection")
        .query_row(
            "SELECT COALESCE(SUM(ended_at_ms IS NULL), 0), COALESCE(SUM(ended_at_ms IS NOT NULL), 0)
               FROM collection_gaps WHERE kind = 'cap_reached'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("gaps");
    assert_eq!((open, closed), (0, 1), "one gap, closed");
}

#[tokio::test]
async fn ct16_free_state_checks_leave_room_for_a_fifth_read() {
    let mut bed = FleetBed::new(k7_policy(1, k7_free_state_pricing()), "online").await;
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 1));
    // The list is not billed and takes no reservation: reads 1 to 5 fit in 1,500 U.
    assert!(bed.row("state").is_none_or(|row| row.reserved == 0));
    for _ in 0..4 {
        bed.advance(Duration::from_secs(120));
        bed.tick().await;
    }
    assert_eq!(bed.reads(), 5);
    bed.advance(Duration::from_secs(120));
    bed.tick().await;
    assert_eq!(bed.reads(), 5);
    assert_eq!(bed.scheduler.account.state, AccountState::CapReached);
    assert!(bed.violations().is_empty(), "{:?}", bed.violations());
}

#[tokio::test]
async fn ct17_a_crash_after_a_send_leaves_a_reservation_that_counts_as_sent() {
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    // A long-running process sends 7 reads; the reservation grows with use, by
    // steps of at most 20, always within the cap.
    bed.tick().await;
    for _ in 0..6 {
        bed.advance(Duration::from_secs(120));
        bed.tick().await;
    }
    assert_eq!(bed.reads(), 7);
    let data = bed.row("data").expect("data row");
    assert_eq!((data.sent, data.billable), (7, 7));
    assert!(data.reserved >= 7 && data.reserved <= 7 + 20, "{data:?}");
    // An eighth read is sent and the process dies before the tick commit.
    bed.advance(Duration::from_secs(120));
    bed.crash_after_a_read().await;
    assert_eq!(bed.reads(), 8);
    let stored = bed.row("data").expect("data row");
    bed.reload();
    // The month's count is `billable + max(0, reserved - sent)` of the stored row
    // (C3 R27, B2 clarification): at least 8 and at most 27.
    let counted = bed.scheduler.ledger.lock().counts[RequestCategory::Data.index()];
    assert!(
        counted.billable >= 8 && counted.billable <= 27,
        "{counted:?}"
    );
    assert_eq!(
        counted.billable,
        (stored.billable + (stored.reserved - stored.sent).max(0)).unsigned_abs()
    );
    // The first commit adds `max(0, reserved - sent)` to both `sent` and `billable`.
    bed.advance(Duration::from_secs(120));
    bed.tick().await;
    let after = bed.row("data").expect("data row");
    assert!(
        after.billable <= after.sent && after.reserved >= after.billable,
        "{after:?}"
    );
    assert!(
        after.sent >= i64::try_from(counted.sent).expect("count"),
        "{after:?}"
    );
    assert!(bed.violations().is_empty(), "{:?}", bed.violations());
}

#[tokio::test]
async fn ct17_in_a_run_to_the_cap_no_request_passes_the_reservation_and_gross_stays_under_it() {
    // 40 cents: 60,000 U, two hundred requests in all.
    let mut bed = FleetBed::new(k7_policy(40, crate::config::FLEET_PRICING), "online").await;
    for _ in 0..5_000 {
        bed.tick().await;
        if bed.scheduler.account.state == AccountState::CapReached {
            break;
        }
        // The next moment the scheduler has something to do.
        let wait = bed
            .scheduler
            .delay_until_next_action(bed.clock.mono_now())
            .max(Duration::from_secs(1));
        bed.advance(wait);
    }
    assert_eq!(bed.scheduler.account.state, AccountState::CapReached);
    let billed = bed.lists() + bed.reads();
    assert!(billed <= 200, "{billed} billable requests passed 60,000 U");
    // The stop is early by at most the unused reservation: a block per category.
    assert!(
        billed + 2 * 20 >= 200,
        "the run stopped far from the cap: {billed}"
    );
    let gross: i64 = bed.rows().iter().map(|row| row.billable * 300).sum();
    assert!(gross <= 60_000, "gross {gross} U");
    // Every request arrived with a committed reservation covering it.
    assert!(bed.violations().is_empty(), "{:?}", bed.violations());
}

/// CT62's course: ten starts, each making a lone reservation commit for one list,
/// sending it and dying before the tick commits.
async fn ct62_course(allow_refresh: bool) {
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "asleep").await;
    bed.allow_refresh = allow_refresh;
    for start in 1..=10_usize {
        if start > 1 {
            bed.reload();
        }
        bed.advance(Duration::from_secs(1));
        // The first tick reserves exactly one state request in a lone commit, sends
        // the list and is dropped before the tick commits. No slot is planned for a
        // resend, whether or not the tick may refresh (C3 R27, R47).
        bed.crash_after_the_list().await;
        assert_eq!(bed.lists(), start);
        let state = bed.row("state").expect("state row");
        if start == 1 {
            assert_eq!((state.sent, state.billable, state.reserved), (0, 0, 1));
        } else {
            // The previous start's reservation was converted to sent and billed by
            // this start's first commit, and the new reservation is one more.
            assert_eq!(
                state.reserved,
                i64::try_from(start).expect("count"),
                "{state:?}"
            );
            assert_eq!(state.sent, state.billable, "{state:?}");
            assert_eq!(
                state.sent,
                i64::try_from(start - 1).expect("count"),
                "{state:?}"
            );
        }
        assert!(state.billable <= state.sent && state.reserved >= state.billable);
    }
    // After the tenth reload the month's state count is 10, as many as the fake saw.
    bed.reload();
    let counted = bed.scheduler.ledger.lock().counts[RequestCategory::State.index()];
    assert_eq!(counted.billable, 10);
    assert_eq!(bed.lists(), 10);
    assert_eq!(bed.reads(), 0, "the car sleeps: no read, ever");
    assert!(bed.violations().is_empty(), "{:?}", bed.violations());
}

#[tokio::test]
async fn ct62_a_crash_loop_over_counts_at_most_one_lists_reservation_per_start() {
    ct62_course(false).await;
}

#[tokio::test]
async fn ct62_the_same_crash_loop_in_managed_mode_reserves_no_resend_slot() {
    // CT62 also runs with `allow_refresh = true` (B2 clarification, option H): a
    // just-started managed process reserves exactly one `state` request for one list.
    ct62_course(true).await;
}

#[tokio::test]
async fn a_list_that_carries_the_ticks_refreshed_token_is_not_refreshed_again() {
    // C3 R47: at most one token refresh per tick. A list whose tick has already
    // refreshed (the latch is set) meets 401 with that token: it is a credential stop,
    // with no second token request and no resend.
    let bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    bed.fake.reject_every_token.store(true, Ordering::SeqCst);
    let tick = TickRefresh::default();
    tick.note();
    let error = fleet_list_vehicles_with_auth(
        &bed.api,
        &bed.auth_api,
        &bed.manager,
        true,
        None,
        &tick,
    )
    .await
    .expect_err("refused");
    assert!(fleet_credential_stop(&error), "{error:?}");
    assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), 0, "no second refresh");
    assert_eq!(bed.lists(), 1, "no resend");
}

#[tokio::test]
async fn a_managed_tick_reserves_exactly_what_it_sends_and_plans_no_resend_slot() {
    // C3 R27, R47 (B2 clarification, option H): `run_fleet_tick` plans no slot for a
    // resend, whether or not the tick may refresh. The first tick of three online cars
    // reserves one `state` request for its list and three `data` requests for its reads.
    let mut bed =
        FleetBed::with_cars(k7_policy(1_000, crate::config::FLEET_PRICING), "online", 3).await;
    bed.allow_refresh = true;
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 3));
    let state = bed.row("state").expect("state row");
    let data = bed.row("data").expect("data row");
    assert_eq!((state.sent, state.reserved), (1, 1), "{state:?}");
    assert_eq!((data.sent, data.reserved), (3, 3), "{data:?}");
    assert!(bed.violations().is_empty(), "{:?}", bed.violations());
}

#[tokio::test]
async fn ct62b_a_crash_loop_after_unbilled_sends_counts_only_what_may_have_been_sent() {
    // C3 R27 (B2 clarification): the stored `state` row is (sent 100, billable 46,
    // reserved 100), so 54 sends were answered and not billed. Ten times: start, one
    // lone reservation commit, one list answered 200, drop before the tick commit.
    // After the tenth reload the `state` count is 56 and `sent` 110 (the fake
    // received 10 lists); the CHECKs hold at every commit; no start makes more than
    // one lone reservation commit before its list.
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "asleep").await;
    let month = bed.scheduler.ledger.lock().span.key.clone();
    let seed = crate::db::CollectorRowsWrite {
        now_ms: CORE_W0,
        counters: vec![counter_row(&month, "fleet_polling", "state", 100, 46, 100)],
        ..crate::db::CollectorRowsWrite::default()
    };
    bed.store
        .collector_write_transaction(
            |transaction| {
                let wrote = HubStore::write_collector_rows_in_transaction(transaction, &seed)?;
                Ok::<_, StoreError>(((), wrote))
            },
            || Ok::<(), StoreError>(()),
        )
        .expect("seed the stored row");
    for start in 1..=10_usize {
        bed.reload();
        bed.advance(Duration::from_secs(1));
        bed.crash_after_the_list().await;
        assert_eq!(bed.lists(), start);
        assert_eq!(
            bed.scheduler.account.reservation_commits, 1,
            "one lone reservation commit before the list at start {start}"
        );
        let state = bed.row("state").expect("state row");
        assert!(
            state.billable <= state.sent && state.reserved >= state.billable,
            "{state:?}"
        );
        // The 54 unbilled sends are never counted again: the stored row advances by
        // one request a start.
        let expected = i64::try_from(start).expect("count");
        assert_eq!(state.reserved, 100 + expected, "{state:?}");
        assert_eq!(state.sent, if start == 1 { 100 } else { 99 + expected }, "{state:?}");
        assert_eq!(state.billable, if start == 1 { 46 } else { 45 + expected }, "{state:?}");
    }
    bed.reload();
    let counted = bed.scheduler.ledger.lock().counts[RequestCategory::State.index()];
    assert_eq!((counted.billable, counted.sent), (56, 110), "{counted:?}");
    assert_eq!(bed.lists(), 10);
    assert!(bed.violations().is_empty(), "{:?}", bed.violations());
}

#[tokio::test]
async fn ct54_with_every_commit_failing_sends_stop_once_the_persisted_reservation_is_used() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let witness = seam.clone();
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async move {
            let mut bed =
                FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
            // A driving car is read every 30 s and never goes idle, so reads keep
            // coming for as long as the reservation allows them.
            bed.fake.driving.store(true, Ordering::SeqCst);
            // Three healthy ticks: the list and three reads, with the reservation
            // grown to what they needed (data: 1, 2, then 4 in all).
            bed.tick().await;
            for _ in 0..2 {
                bed.advance(Duration::from_secs(30));
                bed.tick().await;
            }
            assert_eq!((bed.lists(), bed.reads()), (1, 3));
            assert_eq!(bed.row("data").expect("data").reserved, 4);
            assert_eq!(
                bed.scheduler.vehicles[&core_id(1)].phase,
                PollPhase::Driving
            );
            // The store now fails every commit. The fourth read uses the last persisted
            // slot, and its count stays in memory although its commit fails.
            witness.set_tick_commit_fault(true);
            for _ in 0..12 {
                bed.advance(Duration::from_secs(30));
                bed.tick().await;
            }
            // No read passed the persisted reservation, whatever the store did.
            assert_eq!(bed.reads(), 4, "sends stop at the persisted reservation");
            assert_eq!(bed.lists(), 1);
            assert!(bed.violations().is_empty(), "{:?}", bed.violations());
            let count = bed.scheduler.ledger.lock().counts[RequestCategory::Data.index()];
            assert_eq!(count.sent, 4);
            assert!(count.reserved_pending > count.reserved_persisted);
            assert_eq!(
                bed.row("data").expect("data").sent,
                3,
                "the store is behind"
            );
            assert!(witness.tick_commit_attempts.load(Ordering::SeqCst) > 4);
            // The fault clears: the pending reservation commits, and sends resume.
            witness.set_tick_commit_fault(false);
            bed.advance(Duration::from_secs(1_000));
            bed.tick().await;
            assert_eq!(
                bed.reads(),
                5,
                "sends resume once a reservation is committed"
            );
            let data = bed.row("data").expect("data");
            assert_eq!(data.sent, 5);
            assert!(data.reserved >= 5);
            assert!(bed.violations().is_empty(), "{:?}", bed.violations());
        })
        .await;
}

#[tokio::test]
async fn a_fleet_tick_writes_its_counters_in_its_own_transaction_and_an_idle_tick_writes_nothing() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let witness = seam.clone();
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async move {
            let mut bed =
                FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "asleep").await;
            bed.tick().await;
            assert_eq!(bed.lists(), 1);
            let after_first = witness.tick_commits();
            let list_row = bed.row("state").expect("state row");
            assert_eq!((list_row.sent, list_row.billable), (1, 1));
            // Ten seconds later nothing is due: no request, no commit.
            bed.advance(Duration::from_secs(10));
            bed.tick().await;
            assert_eq!(bed.lists(), 1);
            assert_eq!(witness.tick_commits(), after_first);
            // The next state check (600 s) is billed and counted in a commit of its own.
            bed.advance(Duration::from_secs(600));
            bed.tick().await;
            assert_eq!(bed.lists(), 2);
            assert!(witness.tick_commits() > after_first);
            let list_row = bed.row("state").expect("state row");
            assert_eq!((list_row.sent, list_row.billable), (2, 2));
            // Status shows the month's counts and the estimate.
            let status = bed.scheduler.status(bed.clock.mono_now());
            assert_eq!(status.requests[RequestCategory::State.index()].1, 2);
            assert_eq!(status.estimate.expect("estimate").gross_cents, 1);
        })
        .await;
}

// ---- Review round: R50 probe, R47 refresh, R27 restart reservation -------------------------------

#[tokio::test]
async fn r50_a_billing_suspension_probes_with_one_counted_list_and_resumes_without_idling() {
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "asleep").await;
    // The first tick lists; the car sleeps, so nothing is read.
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 0));
    // Tesla answers 402 to the next list (a billed state check is due at 600 s).
    bed.fake.list_status.store(402, Ordering::SeqCst);
    bed.advance(Duration::from_secs(600));
    bed.tick().await;
    assert_eq!(bed.lists(), 2);
    assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
    // Suspended: nothing is sent for six hours, and until then the loop waits for
    // the settings refresh, not at the floor rate.
    for hour in 1..=3 {
        bed.advance(Duration::from_secs(3_600));
        let outcome = bed.tick().await;
        assert!(!outcome.requests_sent, "hour {hour}");
        assert_eq!(bed.lists(), 2, "hour {hour}");
    }
    // At the probe time one list goes out (admitted through the account check, the
    // reservation and the cap, and counted); Tesla still says 402, so the probe is
    // armed again six hours on, and the loop still does not idle.
    bed.advance(Duration::from_secs(3 * 3_600));
    let outcome = bed.tick().await;
    assert!(outcome.requests_sent);
    assert_eq!(bed.lists(), 3, "the probe list reaches Tesla");
    assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
    let now = bed.clock.mono_now();
    assert_eq!(
        bed.scheduler.next_wait(now, false, true),
        CONTROL_SETTINGS_REFRESH,
        "no idle pass faster than the settings refresh while suspended"
    );
    let state = bed.row("state").expect("state row");
    assert_eq!(state.sent, 3, "every list, the probes included, is counted");
    assert!(state.reserved >= state.sent, "{state:?}");
    // Tesla lifts the suspension: the next probe, six hours on, resumes collection.
    bed.fake.list_status.store(200, Ordering::SeqCst);
    bed.advance(Duration::from_secs(6 * 3_600));
    bed.tick().await;
    assert_eq!(bed.lists(), 4);
    assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
    assert!(bed.scheduler.account.telemetry_check_due);
    assert!(bed.violations().is_empty(), "{:?}", bed.violations());
}

#[tokio::test]
async fn r50_a_probe_the_cap_forbids_is_asked_again_at_the_next_probe_time() {
    // One cent is five requests at the data price: the list, the 402 and what the
    // probes add reach the cap, and the suspended account neither spins nor sends.
    let mut bed = FleetBed::new(k7_policy(1, crate::config::FLEET_PRICING), "asleep").await;
    bed.tick().await;
    bed.fake.list_status.store(402, Ordering::SeqCst);
    bed.advance(Duration::from_secs(600));
    bed.tick().await;
    assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
    // Fill the cap with counted requests of another sender (setup, diagnostics).
    let ledger = bed.scheduler.request_ledger();
    for _ in 0..3 {
        ledger.record_completed(RequestCategory::Data, true);
    }
    bed.advance(Duration::from_secs(6 * 3_600));
    let lists = bed.lists();
    bed.tick().await;
    assert_eq!(bed.lists(), lists, "the cap withholds the probe");
    let now = bed.clock.mono_now();
    assert_eq!(
        bed.scheduler.next_wait(now, false, true),
        CONTROL_SETTINGS_REFRESH,
        "and the loop does not spin on a probe it cannot send"
    );
    assert_eq!(bed.scheduler.account.state, AccountState::BillingSuspended);
}

#[tokio::test]
async fn r47_a_fleet_403_sends_one_request_and_makes_no_token_refresh() {
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    bed.allow_refresh = true;
    // A 403 on the list is the account's `FleetAccessRefused`, not a credential
    // outcome: one list, no refresh, no resend.
    bed.fake.list_status.store(403, Ordering::SeqCst);
    bed.tick().await;
    assert_eq!(bed.lists(), 1);
    assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), 0);
    assert_eq!(
        bed.scheduler.account.state,
        AccountState::FleetAccessRefused
    );
    // A 403 on a car's read pauses that car as `AccessRefused`: one read, no refresh.
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    bed.allow_refresh = true;
    bed.fake.read_status.store(403, Ordering::SeqCst);
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 1));
    assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), 0);
    assert_eq!(
        bed.scheduler.vehicles[&core_id(1)].hold,
        VehicleHold::Paused(PauseReason::AccessRefused)
    );
    assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
}

impl FleetBed {
    /// The persisted data reservation not yet used by a start (C3 R27).
    fn spare_data_reservation(&self) -> u64 {
        let ledger = self.scheduler.request_ledger();
        let state = ledger.lock();
        let count = state.counts[RequestCategory::Data.index()];
        count.reserved_persisted.saturating_sub(count.sent)
    }

    /// Run ticks of three online cars, two minutes apart, until the persisted
    /// reservation leaves room for a resend beside the tick's three reads (a
    /// long-running process reserves blocks, C3 R27).
    async fn warm_up_until_a_resend_is_covered(&mut self) {
        self.allow_refresh = true;
        self.tick().await;
        for _ in 0..12 {
            if self.spare_data_reservation() >= 4 {
                return;
            }
            self.advance(Duration::from_secs(120));
            self.tick().await;
        }
        panic!("the reservation never grew a spare slot");
    }

    fn data_reads_stored_at(&self, wall: i64) -> i64 {
        self.store
            .open()
            .expect("connection")
            .query_row(
                "SELECT COUNT(*) FROM current_observations
                  WHERE record_type = 'fleet_api_vehicle_data_v1' AND received_at_ms = ?1",
                [wall],
                |row| row.get(0),
            )
            .expect("count")
    }
}

impl FleetBed {
    /// The process loses its sensitive access (C3 R47): the Fleet credential store's
    /// admission, which was valid, stops being so. The next request that needs the
    /// bearer meets `FleetCredentialError::SensitiveAccessUnavailable`.
    async fn lose_sensitive_access(&mut self) {
        let admission = crate::hub_user_process::AdmittedUserHub::for_test(self._dir.path())
            .expect("admit");
        let manager = FleetAuthManager::from_store_for_admitted_user(
            self.store.clone(),
            self._dir.path(),
            Arc::clone(&admission),
            None,
        )
        .expect("manager");
        self.manager = Arc::new(tokio::sync::Mutex::new(manager));
        let lock = self._dir.path().join(crate::user_lifetime_lock::LOCK_FILE_NAME);
        std::fs::remove_file(&lock).expect("remove the lock file");
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&lock)
                .expect("a different lock file");
        }
        assert!(admission.assert_sensitive_access().is_err());
    }
}

#[tokio::test]
async fn the_loss_of_sensitive_access_on_the_list_or_a_read_ends_the_tick_with_no_stop_and_no_gap() {
    // C3 R47, CT74 (b) at the tick: a Fleet request that cannot use the credential
    // because the admission failed ends the run with the error: no `CredentialStop`,
    // no `auth_terminal` gap, no account row written, and nothing stored for the tick.
    // On the list:
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    bed.lose_sensitive_access().await;
    let error = bed.try_tick().await.expect_err("the run ends");
    assert!(loses_sensitive_access(&error), "{error:?}");
    assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
    assert!(bed.store.collection_gaps(10).expect("gaps").is_empty());
    assert_eq!(bed.stored_account_state(), None, "no account row is written");
    // On a read: the list works, then the admission fails before the reads.
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 1));
    let stored_before: i64 = bed
        .store
        .open()
        .expect("connection")
        .query_row("SELECT COUNT(*) FROM current_observations", [], |row| row.get(0))
        .expect("count");
    bed.lose_sensitive_access().await;
    bed.advance(Duration::from_secs(120));
    let error = bed.try_tick().await.expect_err("the run ends");
    assert!(loses_sensitive_access(&error), "{error:?}");
    assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
    assert!(bed.store.collection_gaps(10).expect("gaps").is_empty());
    let stored_after: i64 = bed
        .store
        .open()
        .expect("connection")
        .query_row("SELECT COUNT(*) FROM current_observations", [], |row| row.get(0))
        .expect("count");
    assert_eq!(stored_after, stored_before, "nothing is stored for the tick");
}

#[tokio::test(start_paused = true)]
async fn ct67_three_reads_with_an_expired_token_make_one_refresh_and_resend_what_the_reservation_covers() {
    // C3 R47 (B2 clarification, option H): exactly one refresh; a read that carried
    // token A is resent once with token B in the same tick when the reservation
    // covers it; a read that waited for the refresh is sent once, with token B; no
    // read is sent more than twice; no failure counted.
    //
    // The tick's commit deadline (`first result + K_TICK_COMMIT_AGE`) is a manual-clock
    // instant that the driver awaits with `tokio::time::sleep_until`: two clocks. The
    // test runs on the paused Tokio clock, so that wait ends only if the test advances
    // it (the in-process bed idles on nothing), and the "<= 2 commits" assertion below
    // counts the commits of the tick's own grouping and never a deadline that a loaded
    // machine let the real clock reach (R-P3bF4-3). The bound is unchanged. The test also
    // asserts that the paused clock never moved: it would have, had the runtime ever
    // idled with a deadline pending.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let frozen = tokio::time::Instant::now();
            let mut bed =
                FleetBed::with_cars(k7_policy(1_000, crate::config::FLEET_PRICING), "online", 3)
                    .await;
            bed.warm_up_until_a_resend_is_covered().await;
            assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), 0);
            // The stored access token expires: the answers are 401 until a request
            // carries the refreshed one.
            bed.fake.token_expired.store(true, Ordering::SeqCst);
            bed.advance(Duration::from_secs(120));
            let commits = seam.tick_commits();
            let reads_before = bed.reads();
            let authorisations_before = bed.fake.authorisations.lock().expect("log").len();
            bed.tick().await;
            assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), 1, "exactly one refresh");
            for id in 1..=3 {
                assert_eq!(bed.scheduler.vehicles[&core_id(id)].consecutive_failures, 0);
            }
            let sent = bed.fake.authorisations.lock().expect("log")[authorisations_before..]
                .to_vec();
            let with_old = sent.iter().filter(|header| header.starts_with("Bearer e30.")).count();
            assert_eq!(with_old, 1, "only the first read carried the old token: {sent:?}");
            assert_eq!(bed.reads() - reads_before, 4, "one resend, two reads with token B: {sent:?}");
            assert_eq!(bed.data_reads_stored_at(bed.clock.wall_now_ms().expect("wall")), 3);
            assert!(seam.tick_commits() - commits <= 2, "{}", seam.tick_commits() - commits);
            assert_eq!(
                tokio::time::Instant::now(),
                frozen,
                "the paused clock moved: a commit deadline could have been awaited to its end"
            );
            assert!(bed.violations().is_empty(), "{:?}", bed.violations());
            assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
        })
        .await;
}

#[tokio::test]
async fn ct67b_a_resend_the_reservation_does_not_cover_stays_due_without_a_wait_or_a_failure() {
    // A just-started managed process with three cars: the tick's list gets 401 with
    // token A, the refresh gives token B, and the list's resend is not covered (one
    // `state` slot): no `AuthWait`, no failure; one lone reservation commit raises
    // the reservation for it (besides the first list's own); the list is sent
    // with token B at once in the next tick and the three first-contact reads follow
    // with token B; the persisted reservation is never exceeded (the fake's audit).
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed =
                FleetBed::with_cars(k7_policy(1_000, crate::config::FLEET_PRICING), "online", 3)
                    .await;
            bed.allow_refresh = true;
            bed.fake.token_expired.store(true, Ordering::SeqCst);
            // The first tick: list 401, refresh, resend withheld.
            let outcome = bed.try_tick().await.expect("no error: nothing failed");
            assert!(outcome.requests_sent);
            assert_eq!(bed.lists(), 1, "the resend was not sent");
            assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), 1);
            assert!(bed.scheduler.account.wait.is_none(), "no AuthWait");
            assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
            // One lone commit for the first list, and one for the raise that its
            // withheld resend needs (it does not ride on a result: none waits).
            assert_eq!(bed.scheduler.account.reservation_commits, 2, "no more than two");
            assert!(bed.scheduler.discovery_due(bed.clock.mono_now()), "the list stays due");
            // The raise is persisted by that tick's own commit.
            assert!(bed.row("state").is_some_and(|row| row.reserved >= 2), "{:?}", bed.row("state"));
            // The next tick starts at once: the list goes out with token B, and the
            // three first-contact reads follow with token B.
            let outcome = bed.try_tick().await.expect("the next tick");
            assert!(outcome.requests_sent);
            assert_eq!(bed.lists(), 2);
            // It failed once under load (reads 2, not 3). The cause was a request that
            // never reached the double on a loopback socket (a transport error counts
            // no car failure): the bed is in process now (R-P3bF4-1), and the message
            // names the per-car attempt trace first, then which cars failed or are
            // still due and what the double was sent.
            let failures: Vec<_> = (1..=3)
                .map(|id| bed.scheduler.vehicles[&core_id(id)].consecutive_failures)
                .collect();
            assert_eq!(
                bed.reads(),
                3,
                "attempts {:#?}, reads {}, lists {}, failures per car {failures:?}, violations \
                 {:?}, sent {:?}, cars {:?}, account {:?}, data row {:?}, state row {:?}",
                bed.fake.transport.trace(),
                bed.reads(),
                bed.lists(),
                bed.violations(),
                bed.fake.authorisations.lock().expect("log"),
                bed.scheduler.vehicles,
                bed.scheduler.account,
                bed.row("data"),
                bed.row("state"),
            );
            let log = bed.fake.authorisations.lock().expect("log").clone();
            assert!(log[0].starts_with("Bearer e30."), "the first list carried token A");
            assert!(log[1..].iter().all(|header| header == "Bearer fleet-next-access"), "{log:?}");
            assert_eq!(bed.fake.token_requests.load(Ordering::SeqCst), 1, "no second refresh");
            assert_eq!(
                bed.scheduler.account.reservation_commits, 2,
                "the next tick needed no lone commit for its list"
            );
            assert!(bed.scheduler.account.wait.is_none());
            for id in 1..=3 {
                assert_eq!(bed.scheduler.vehicles[&core_id(id)].consecutive_failures, 0);
            }
            assert!(bed.violations().is_empty(), "{:?}", bed.violations());
        })
        .await;
}

#[test]
fn a_restart_after_non_billable_sends_reserves_what_one_commit_can_cover() {
    // R27 (B2 clarification): the restart adds `d = max(0, reserved - sent)` of the
    // stored row, so sends already answered and not billed (5xx answers) are never
    // counted as billed again. The stored row (100, 46, 50) loads as count 46 and
    // `sent` 100, which stands above the stored reservation: the first plan must lift
    // the reservation above `sent` in one raise, not one lone commit per missing unit.
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    bed.ledger
        .load(&[counter_row("2026-10", "fleet_polling", "data", 100, 46, 50)]);
    let data = bed.ledger.lock().counts[RequestCategory::Data.index()];
    assert_eq!((data.sent, data.billable, data.reserved_persisted), (100, 46, 50));
    assert!(!bed.ledger.dirty(), "nothing is converted, so nothing differs");
    assert_eq!(
        bed.ledger.start(RequestCategory::Data),
        Admission::Withheld(Withheld::Reservation)
    );
    assert!(bed.ledger.plan(&[(RequestCategory::Data, 1)]));
    let rows = bed.ledger.rows();
    let reserved = rows[0].reserved;
    assert!(reserved >= 101, "{reserved}: one raise reaches past sent");
    bed.ledger.mark_persisted(&rows);
    assert_eq!(
        bed.ledger.start(RequestCategory::Data),
        Admission::Admitted,
        "one commit covers the plan"
    );
    // The raise stays inside the cap: nothing was exposed beyond it, and the 54
    // requests answered and not billed use none of it.
    let state = bed.ledger.lock();
    assert!(state.exposure_units(None) <= state.cap_units);
    assert_eq!(state.exposure_units(None), 47 * 300, "46 billed and one in flight");
}

#[test]
fn a_request_the_cap_cannot_reserve_for_is_the_cap_even_while_the_gross_still_fits() {
    // C3 R27 (B2 clarification): the exposure sums every category. One cent is five
    // requests (1,500 U at 300 U). Data has 20 sends of which one was billed, so the
    // reservation its next send needs (21) is an exposure of 1 billed plus 19 answered
    // and not billed, and the running process holds a `state` reservation of 4 requests
    // beside it. The gross is 300 U, so one more request fits the gross, but the
    // reservation does not fit the cap: this is the cap, never a wait for a commit.
    let bed = LedgerBed::fleet(1, utc_ms(2026, 10, 15, 0));
    bed.ledger
        .load(&[counter_row("2026-10", "fleet_polling", "data", 20, 1, 1)]);
    bed.reserve(&[(RequestCategory::State, 4)]);
    assert_eq!(bed.ledger.lock().gross_units(true), 300);
    assert_eq!(
        bed.ledger.start(RequestCategory::Data),
        Admission::Withheld(Withheld::Cap)
    );
    // With one state request reserved instead of four it does fit: a wait.
    let room = LedgerBed::fleet(1, utc_ms(2026, 10, 15, 0));
    room.ledger
        .load(&[counter_row("2026-10", "fleet_polling", "data", 20, 1, 1)]);
    room.reserve(&[(RequestCategory::State, 1)]);
    assert_eq!(
        room.ledger.start(RequestCategory::Data),
        Admission::Withheld(Withheld::Reservation)
    );
}

#[test]
fn requests_in_flight_at_a_month_change_use_the_new_months_exposure() {
    // C3 R27 (B2 clarification): the exposure of a category is at least its billable
    // requests plus those in flight, whatever its reservation. Two data reads in
    // flight when the month changes (the new month starts with reservation 0) take
    // two of the five requests one cent allows, so a plan for state may reserve at
    // most three.
    let bed = LedgerBed::fleet(1, utc_ms(2026, 10, 31, 23));
    bed.reserve(&[(RequestCategory::Data, 2)]);
    assert_eq!(bed.ledger.start(RequestCategory::Data), Admission::Admitted);
    assert_eq!(bed.ledger.start(RequestCategory::Data), Admission::Admitted);
    bed.clock.advance(Duration::from_secs(2 * 60 * 60));
    assert!(matches!(bed.ledger.refresh_month(), MonthEvent::Rolled { .. }));
    assert!(bed.ledger.plan(&[(RequestCategory::State, 5)]));
    let state = bed.ledger.lock().counts[RequestCategory::State.index()];
    assert_eq!(state.reserved_pending, 3, "{state:?}");
    assert_eq!(bed.ledger.lock().exposure_units(None), 1_500);
}

#[test]
fn a_restart_whose_unused_reservation_converts_counts_the_difference_to_sent_once() {
    // Stored (sent 10, billable 6, reserved 14): `d = 4` requests may have started
    // after the last commit. The month counts `billable + d` = 10, `sent` 14, and the
    // first commit writes both with the reservation unchanged. A second restart
    // before any send changes nothing more.
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    bed.ledger
        .load(&[counter_row("2026-10", "fleet_polling", "data", 10, 6, 14)]);
    let data = bed.ledger.lock().counts[RequestCategory::Data.index()];
    assert_eq!((data.sent, data.billable, data.reserved_persisted), (14, 10, 14));
    let rows = bed.ledger.rows();
    assert_eq!(rows[0], counter_row("2026-10", "fleet_polling", "data", 14, 10, 14));
    let again = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    again.ledger.load(&rows);
    let data = again.ledger.lock().counts[RequestCategory::Data.index()];
    assert_eq!((data.sent, data.billable), (14, 10), "d is 0 now");
    assert!(!again.ledger.dirty());
}

#[test]
fn a_restart_whose_lift_does_not_fit_the_cap_is_the_cap_not_a_wait() {
    // One cent is five data requests (1,500 U at 300 U). The stored row (20, 3, 3):
    // 17 sends were answered and not billed, so they use no cap room (C3 R27, B2
    // clarification): the exposure at the reservation the lift needs (20, plus the
    // plan) is `3 + raise`, and the cap bounds the raise at two more reads. The
    // third further read is the cap at gross 1,500 U.
    let bed = LedgerBed::fleet(1, utc_ms(2026, 10, 15, 0));
    bed.ledger
        .load(&[counter_row("2026-10", "fleet_polling", "data", 20, 3, 3)]);
    assert_eq!(
        bed.ledger.start(RequestCategory::Data),
        Admission::Withheld(Withheld::Reservation),
        "the cap allows the lift, so this is a wait for the commit"
    );
    assert!(bed.ledger.plan(&[(RequestCategory::Data, 2)]));
    let rows = bed.ledger.rows();
    assert_eq!(rows[0].reserved, 22, "the lift of 17 and two more, no more");
    bed.ledger.mark_persisted(&rows);
    for _ in 0..2 {
        assert_eq!(bed.ledger.start(RequestCategory::Data), Admission::Admitted);
        bed.ledger.complete(RequestCategory::Data, true);
    }
    assert_eq!(
        bed.ledger.start(RequestCategory::Data),
        Admission::Withheld(Withheld::Cap)
    );
    let state = bed.ledger.lock();
    assert_eq!(state.gross_units(false), 1_500);
    assert!(state.exposure_units(None) <= state.cap_units);
}

#[test]
fn a_request_still_in_flight_at_a_commit_is_not_in_the_stored_sent() {
    // CT17 variant (C3 R25, R27, B2 clarification): reservation 4; two reads started;
    // read 1 answered 200 and the commit runs while read 2 is in flight. The stored
    // row is (sent 1, billable 1, reserved 4), so a restart counts
    // `billable + (4 - 1)` = 4, which covers read 2 whatever its answer.
    let bed = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    bed.reserve(&[(RequestCategory::Data, 4)]);
    assert_eq!(bed.ledger.start(RequestCategory::Data), Admission::Admitted);
    assert_eq!(bed.ledger.start(RequestCategory::Data), Admission::Admitted);
    bed.ledger.complete(RequestCategory::Data, true);
    let rows = bed.ledger.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0], counter_row("2026-10", "fleet_polling", "data", 1, 1, 4));
    bed.ledger.mark_persisted(&rows);
    // The in-memory count still has both starts (R27's `used_c`).
    assert_eq!(bed.sent(RequestCategory::Data), 2);
    // Drop and reload: the count is 4.
    let reloaded = LedgerBed::fleet(1_000, utc_ms(2026, 10, 15, 0));
    reloaded.ledger.load(&rows);
    let data = reloaded.ledger.lock().counts[RequestCategory::Data.index()];
    assert_eq!((data.sent, data.billable), (4, 4));
    assert_eq!(reloaded.ledger.lock().gross_units(false), 4 * 300);
    // Read 2's answer commits next: the stored row then holds it.
    bed.ledger.complete(RequestCategory::Data, false);
    assert_eq!(
        bed.ledger.rows()[0],
        counter_row("2026-10", "fleet_polling", "data", 2, 1, 4),
        "a 5xx answer is known now, and unbilled"
    );
}

#[tokio::test]
async fn r47_a_terminal_credential_outcome_stops_the_account_inside_the_loop_and_stores_the_gap() {
    // An observer may not refresh: a 401 on the list is a Fleet credential stop. The
    // run does not end (C3 R47, B2 clarification): the account is `CredentialStop`,
    // the tick's rows, the `auth_terminal` gap among them, are stored, and the
    // iteration goes on without sending anything.
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    bed.fake.token_expired.store(true, Ordering::SeqCst);
    let outcome = bed.try_tick().await.expect("the loop goes on");
    assert!(outcome.requests_sent && outcome.tick_wrote, "{outcome:?}");
    assert_eq!(bed.scheduler.account.state, AccountState::CredentialStop);
    let gaps = bed.store.collection_gaps(10).expect("gaps");
    assert!(
        gaps.iter()
            .any(|gap| gap.kind == "auth_terminal" && gap.ended_at_ms.is_none()),
        "{gaps:?}"
    );
    // Nothing more is sent while the account is stopped.
    bed.advance(Duration::from_secs(600));
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 0));
}

#[tokio::test]
async fn r9_every_scheduled_request_is_logged_at_debug_with_its_operation_class_status_and_duration() {
    // One event per request at its completion: the operation, the class the
    // classifier gives the answer (`ok` for a success), the numeric status, the
    // duration in milliseconds and the Hub id. Never the VIN, a Tesla id, a URL or
    // path, a token or a body.
    let log = DecisionLog::start();
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 1));
    // A failing read: 503 is a transient failure of the car, still one event.
    bed.fake.read_status.store(503, Ordering::SeqCst);
    bed.advance(Duration::from_secs(120));
    bed.tick().await;
    assert_eq!(bed.reads(), 2);
    let text = log.contents();
    let requests: Vec<&str> = text
        .lines()
        .filter(|line| line.contains(" DEBUG ") && line.contains("collector.request"))
        .collect();
    assert_eq!(requests.len(), 3, "a list and two reads:\n{text}");
    let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
    for (line, operation, class, status) in [
        (requests[0], "list", "ok", "200"),
        (requests[1], "read", "ok", "200"),
        (requests[2], "read", "Transient", "503"),
    ] {
        assert!(line.contains(&format!("operation=\"{operation}\"")), "{line}");
        assert!(line.contains(&format!("class={class}")), "{line}");
        assert!(line.contains(&format!("status={status}")), "{line}");
        assert!(line.contains("duration_ms="), "{line}");
    }
    assert!(requests[0].contains("vehicle=account"), "{}", requests[0]);
    assert!(requests[1].contains(&format!("vehicle={hub}")), "{}", requests[1]);
    // Nothing that identifies the car, the connection or the credentials.
    // (Only the collector's own lines: the HTTP client logs its connections.)
    let own: String = text
        .lines()
        .filter(|line| line.contains(" collector."))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in ["5YJ3E1EA7KF00000", "127.0.0.1", "api/1", "Bearer", "e30.", "fleet-"] {
        assert!(!own.contains(forbidden), "{forbidden}:\n{own}");
    }
}

#[tokio::test]
async fn r47_a_terminal_outcome_on_a_read_stops_the_account_and_stores_the_gap_too() {
    // The same as for the list, on the read path: the closure that handles the
    // result enters `CredentialStop`, and `drive_requests` stores the tick's rows;
    // the loop goes on.
    let mut bed = FleetBed::new(k7_policy(1_000, crate::config::FLEET_PRICING), "online").await;
    bed.tick().await;
    assert_eq!((bed.lists(), bed.reads()), (1, 1));
    bed.fake.token_expired.store(true, Ordering::SeqCst);
    bed.advance(Duration::from_secs(120));
    bed.try_tick().await.expect("the loop goes on");
    assert_eq!(bed.scheduler.account.state, AccountState::CredentialStop);
    let gaps = bed.store.collection_gaps(10).expect("gaps");
    assert!(
        gaps.iter()
            .any(|gap| gap.kind == "auth_terminal" && gap.ended_at_ms.is_none()),
        "{gaps:?}"
    );
}
