// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C3 part K5 (errors, back-off, rate limits and auth outcomes):
// CT34, CT35, CT38, CT40, CT41, CT60, CT66 and CT67, plus tests of the rules the
// part owns (R42 to R47, R55 to R57). The wire halves (CT39, R42's statuses,
// R55's size bound) are in `api/owner_api/tests.rs` and `api/fleet_api/tests.rs`.
// Synthetic data only; the fake servers are in-process and nothing touches the
// network.

fn ep_owner(error: OwnerApiError) -> CollectorError {
    CollectorError::OwnerApi(error)
}

fn ep_status(status: u16) -> CollectorError {
    ep_owner(OwnerApiError::HttpStatus(status))
}

fn ep_wrapped(error: OwnerApiError) -> CollectorError {
    CollectorError::OwnerApiAuth(OwnerApiAuthError::Owner(error))
}

fn ep_fleet(error: FleetApiError) -> CollectorError {
    CollectorError::FleetApi(error)
}

fn ep_fleet_status(status: u16) -> CollectorError {
    ep_fleet(FleetApiError::HttpStatus(status))
}

fn ep_fleet_code(status: u16, code: &str) -> CollectorError {
    ep_fleet(FleetApiError::ProviderHttpStatus {
        status,
        error: code.to_owned(),
        description: None,
    })
}

fn ep_transport() -> CollectorError {
    ep_owner(OwnerApiError::Transport)
}

fn ep_rate_limited(seconds: u64) -> CollectorError {
    ep_owner(OwnerApiError::RateLimited {
        retry_after_seconds: seconds,
    })
}

fn ep_forbidden(named: bool) -> CollectorError {
    ep_owner(OwnerApiError::Forbidden {
        fleet_api_named: named,
    })
}

/// Move both clocks of the core to `t`, as real time does.
fn ep_sync_clock(core: &Core, t: Instant) {
    let now = core.clock.mono_now();
    if t > now {
        core.clock.advance(t - now);
    }
}

/// Cars `1..=cars` first-contacted online at t0 and read in use, so each is
/// Online with its next read due at t0 + 60 s and no idle window running.
fn ep_online_core(cars: u64) -> Core {
    let mut core = Core::owner();
    core.scheduler.accept_discovery(
        (1..=cars).map(|id| core_vehicle(id, "online")).collect(),
        core.t0,
    );
    assert_eq!(core.scheduler.due_vehicles(core.t0).len() as u64, cars);
    for id in 1..=cars {
        core.scheduler
            .vehicle_succeeded(core_id(id), PollPhase::Online, false, core.t0);
    }
    core
}

fn ep_record(core: &Core, id: u64) -> &ScheduledVehicle {
    &core.scheduler.vehicles[&core_id(id)]
}

// ---- R42: the one classifier ---------------------------------------------------

#[test]
fn r42_owner_wire_rows_classify_by_status_and_request_kind() {
    use ErrorClass::*;
    use RequestKind::{List, Probe, Read, Record};
    let legacy = crate::config::CollectorMode::Legacy;
    let provider = crate::config::CollectorMode::Provider;
    let class = |error: &CollectorError, request, mode| classify(error, request, mode);

    // Statuses that mean the same on both wires and for every request.
    for mode in [legacy, provider] {
        for (error, request, expected) in [
            (ep_status(401), Read, Auth),
            (ep_wrapped(OwnerApiError::HttpStatus(401)), List, Auth),
            (ep_status(408), Read, Unavailable),
            (ep_status(408), Probe, Unavailable),
            (ep_status(408), List, Transient),
            (ep_rate_limited(0), List, RateLimited),
            (ep_rate_limited(9), Read, RateLimited),
            (ep_status(429), Read, RateLimited),
            (ep_status(451), Read, LegalPrivacy),
            (ep_status(451), List, ClientError),
            (ep_status(500), Read, Transient),
            (ep_status(503), Record, Transient),
            (ep_status(540), Read, Transient),
            (ep_status(599), List, Transient),
            (ep_status(400), Read, ClientError),
            (ep_status(405), Read, ClientError),
            (ep_status(406), Read, ClientError),
            (ep_status(412), Read, ClientError),
            (ep_status(421), Read, ClientError),
            (ep_status(422), Read, ClientError),
            (ep_status(423), Read, ClientError),
            (ep_status(418), Read, ClientError),
            (ep_status(307), Read, Protocol),
            (ep_owner(OwnerApiError::VehicleNotFound), Read, NotFound),
            (ep_owner(OwnerApiError::VehicleNotFound), Record, NotFound),
            (ep_owner(OwnerApiError::VehicleNotFound), List, ClientError),
            (ep_owner(OwnerApiError::VehicleInService), Read, InService),
            (ep_owner(OwnerApiError::VehicleInService), List, ClientError),
            (ep_owner(OwnerApiError::RequestTimeout), Read, Unreachable),
            (ep_owner(OwnerApiError::Transport), List, Unreachable),
            (
                ep_owner(OwnerApiError::StreamPowerNotConfirmed),
                Read,
                GateNotConfirmed,
            ),
            (ep_owner(OwnerApiError::ResponseTooLarge), Read, Protocol),
            (ep_owner(OwnerApiError::ResponseRead), List, Protocol),
            (
                ep_owner(OwnerApiError::InvalidResponseEnvelope),
                Read,
                Protocol,
            ),
            (
                ep_owner(OwnerApiError::InvalidVehicleListCount),
                List,
                Protocol,
            ),
            (
                ep_owner(OwnerApiError::InvalidVehicleRecord),
                List,
                Protocol,
            ),
            (
                ep_owner(OwnerApiError::InvalidVehicleDataEnvelope),
                Read,
                Protocol,
            ),
            (
                ep_owner(OwnerApiError::SensitiveDataInResponse),
                Read,
                Protocol,
            ),
            (
                ep_owner(OwnerApiError::CredentialAuthorityUnavailable),
                Read,
                Auth,
            ),
            (ep_owner(OwnerApiError::LegacyAuth), Read, Auth),
        ] {
            assert_eq!(
                class(&error, request, mode),
                expected,
                "{error:?} {request:?}"
            );
        }
    }

    // Tesla withdrawing the Owner API (Legacy) or a provider refusing the Hub
    // (Provider), per status and request kind (R42, R48, R49).
    for (error, request, legacy_class, provider_class) in [
        (ep_forbidden(true), List, OwnerApiRefusal, ProviderRefusal),
        (ep_forbidden(false), List, ClientError, ClientError),
        (ep_forbidden(true), Read, AccessRefused, AccessRefused),
        (ep_forbidden(true), Record, AccessRefused, AccessRefused),
        (ep_forbidden(true), Probe, AccessRefused, AccessRefused),
        (ep_forbidden(false), Read, ClientError, ClientError),
        (ep_status(403), List, ClientError, ClientError),
        (
            ep_owner(OwnerApiError::Gone),
            List,
            OwnerApiRefusal,
            ProviderRefusal,
        ),
        (
            ep_owner(OwnerApiError::Gone),
            Read,
            OwnerApiRefusal,
            ProviderRefusal,
        ),
        (ep_status(410), Read, OwnerApiRefusal, ProviderRefusal),
        (ep_status(402), Read, ClientError, ProviderRefusal),
        (ep_status(402), List, ClientError, ProviderRefusal),
    ] {
        assert_eq!(
            class(&error, request, legacy),
            legacy_class,
            "{error:?} {request:?}"
        );
        assert_eq!(
            class(&error, request, provider),
            provider_class,
            "{error:?} {request:?}"
        );
    }
}

#[test]
fn r42_fleet_rows_classify_by_status_code_and_request_kind() {
    use ErrorClass::*;
    use RequestKind::{List, Read};
    let fleet = crate::config::CollectorMode::FleetPolling;
    for (error, request, expected) in [
        (ep_fleet_status(401), Read, Auth),
        (ep_fleet_status(401), List, Auth),
        (
            ep_fleet_code(401, "mobile_access_disabled"),
            Read,
            MobileAccessDisabled,
        ),
        (
            ep_fleet_code(401, "mobile_access_disabled"),
            List,
            ClientError,
        ),
        (ep_fleet_status(402), List, Billing),
        (ep_fleet_status(402), Read, Billing),
        (ep_fleet_status(403), List, FleetAccess),
        (ep_fleet_status(403), Read, AccessRefused),
        (ep_fleet_status(404), Read, NotFound),
        (ep_fleet_code(405, "vehicle_in_service"), Read, InService),
        (ep_fleet_status(405), Read, ClientError),
        (ep_fleet_code(405, "bad_request"), Read, ClientError),
        (ep_fleet_status(408), Read, Unavailable),
        (ep_fleet_status(410), Read, ClientError),
        (ep_fleet_status(412), Read, Config),
        (ep_fleet_status(421), List, Config),
        (ep_fleet_status(423), Read, Locked),
        (ep_fleet_status(429), Read, RateLimited),
        (
            ep_fleet(FleetApiError::RateLimited {
                retry_after_seconds: 5,
            }),
            List,
            RateLimited,
        ),
        (ep_fleet_status(451), Read, LegalPrivacy),
        (ep_fleet_status(500), Read, Transient),
        (ep_fleet_status(540), Read, Transient),
        (ep_fleet_status(504), List, Transient),
        (ep_fleet_status(400), Read, ClientError),
        (ep_fleet_code(400, "invalid_field"), Read, ClientError),
        (ep_fleet(FleetApiError::RequestTimeout), Read, Unreachable),
        (ep_fleet(FleetApiError::RequestNotSent), List, Unreachable),
        (ep_fleet(FleetApiError::Transport), Read, Unreachable),
        (ep_fleet(FleetApiError::ResponseTooLarge), Read, Protocol),
        (ep_fleet(FleetApiError::ResponseRead), List, Protocol),
        (ep_fleet(FleetApiError::InvalidResponse), Read, Protocol),
    ] {
        assert_eq!(
            classify(&error, request, fleet),
            expected,
            "{error:?} {request:?}"
        );
    }
}

#[test]
fn r42_credential_outcomes_are_a_stop_or_an_authentication_wait() {
    use ErrorClass::*;
    let legacy = crate::config::CollectorMode::Legacy;
    let fleet = crate::config::CollectorMode::FleetPolling;
    for request in [
        RequestKind::List,
        RequestKind::Record,
        RequestKind::Read,
        RequestKind::Probe,
    ] {
        // The loss of the process's sensitive access is not an account state (R42,
        // R47, B2 clarification): the loops end the run on it before any scheduler
        // entry point sees it, so it never reaches a stop. Like a store fault it has
        // no collection class of its own.
        assert_eq!(
            classify(&CollectorError::SensitiveAccessUnavailable, request, legacy),
            Transient
        );
        assert_eq!(
            classify(
                &CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn),
                request,
                legacy
            ),
            CredentialStop,
            "signed out is a terminal outcome of the credential unit"
        );
        assert_eq!(
            classify(
                &CollectorError::FleetCredential(FleetCredentialError::SensitiveAccessUnavailable),
                request,
                fleet
            ),
            Transient,
            "the Fleet credential store's sensitive-access failure ends the run too"
        );
        assert_eq!(
            classify(
                &CollectorError::FleetCredential(FleetCredentialError::Api(
                    FleetApiError::RequestNotSent
                )),
                request,
                fleet
            ),
            Auth
        );
    }
    // Not a collection error: the loops propagate it (R6), and the class is the
    // ordinary back-off.
    assert_eq!(
        classify(
            &CollectorError::Store(StoreError::LineageCatalogConflict),
            RequestKind::Read,
            legacy
        ),
        Transient
    );
}

#[test]
fn r42_scope_follows_the_class_and_the_request_kind() {
    use ErrorClass::*;
    use RequestKind::{List, Read};
    for class in [
        Auth,
        AuthWait,
        CredentialStop,
        RateLimited,
        Unreachable,
        OwnerApiRefusal,
        ProviderRefusal,
        FleetAccess,
        Billing,
        Config,
        Locked,
    ] {
        assert!(class.is_account_scope(Read), "{class:?}");
        assert!(class.is_account_scope(List), "{class:?}");
    }
    for class in [Transient, Protocol, ClientError] {
        assert!(!class.is_account_scope(Read), "{class:?}");
        assert!(
            class.is_account_scope(List),
            "{class:?}: no car can be checked without the list"
        );
    }
    for class in [
        Unavailable,
        NotFound,
        InService,
        AccessRefused,
        MobileAccessDisabled,
        LegalPrivacy,
        GateNotConfirmed,
    ] {
        assert!(!class.is_account_scope(Read), "{class:?}");
    }
}

#[test]
fn r42_in_service_and_owner_error_helpers_read_both_wires() {
    assert!(is_vehicle_in_service(&ep_owner(
        OwnerApiError::VehicleInService
    )));
    assert!(is_vehicle_in_service(&ep_wrapped(
        OwnerApiError::VehicleInService
    )));
    assert!(is_vehicle_in_service(&ep_fleet_code(
        405,
        "vehicle_in_service"
    )));
    assert!(!is_vehicle_in_service(&ep_status(405)));
    assert!(!is_vehicle_in_service(&ep_status(503)));
    assert_eq!(
        owner_api_error(&ep_owner(OwnerApiError::Gone)),
        Some(&OwnerApiError::Gone)
    );
    assert_eq!(
        owner_api_error(&ep_wrapped(OwnerApiError::VehicleNotFound)),
        Some(&OwnerApiError::VehicleNotFound)
    );
    assert_eq!(owner_api_error(&ep_fleet_status(503)), None);
}

#[test]
fn r9_the_status_of_a_typed_error_is_logged_as_its_number() {
    for (error, expected) in [
        (ep_owner(OwnerApiError::Gone), Some(410)),
        (ep_forbidden(true), Some(403)),
        (
            ep_wrapped(OwnerApiError::Forbidden {
                fleet_api_named: false,
            }),
            Some(403),
        ),
        (ep_owner(OwnerApiError::VehicleNotFound), Some(404)),
        (ep_owner(OwnerApiError::VehicleInService), Some(405)),
        (ep_rate_limited(3), Some(429)),
        (ep_fleet_code(401, "mobile_access_disabled"), Some(401)),
        (ep_fleet_status(540), Some(540)),
        (ep_transport(), None),
    ] {
        assert_eq!(http_status_of(&error), expected, "{error:?}");
    }
}

// ---- R44, CT34: the one back-off function ------------------------------------

#[test]
fn ct34_back_off_values() {
    let cap = Duration::from_secs(300);
    let ms = Duration::from_millis;
    assert_eq!(backoff_delay(1, cap, 0.0), ms(5_000));
    assert_eq!(
        backoff_delay(0, cap, 0.0),
        ms(5_000),
        "0 gives the value of 1"
    );
    assert_eq!(backoff_delay(1, cap, 0.5), ms(7_500));
    assert_eq!(backoff_delay(3, cap, 0.5), ms(30_000));
    assert_eq!(backoff_delay(5, cap, 0.0), ms(80_000));
    assert_eq!(backoff_delay(6, cap, 0.0), ms(150_000));
    assert!(backoff_delay(6, cap, 0.999) < cap);
    assert_eq!(
        backoff_delay(u32::MAX, cap, 0.0),
        ms(150_000),
        "a huge count saturates at half the cap"
    );
    // Always between half the nominal value and all of it.
    for failures in 0..40 {
        for u in [0.0, 0.25, 0.5, 0.999] {
            let delay = backoff_delay(failures, cap, u);
            assert!(
                delay >= ms(5_000) && delay < cap,
                "{failures} {u} {delay:?}"
            );
        }
    }
}

// ---- R22, CT35: a failure never makes a car faster than its cadence ------------

#[test]
fn ct35_failure_then_success() {
    let mut core = ep_online_core(1);
    let id = core_id(1);
    // The interval dominates the first four back-offs (5, 10, 20, 40 s).
    let mut t = core.at(60);
    for failures in 1..=4u32 {
        assert_eq!(
            core.scheduler.due_vehicles(t),
            vec![id],
            "failure {failures}"
        );
        core.scheduler
            .vehicle_failed_for_error(id, &ep_status(503), t);
        assert_eq!(ep_record(&core, 1).consecutive_failures, failures);
        assert_eq!(
            ep_record(&core, 1).next_read_due,
            Some(t + Duration::from_secs(60)),
            "failure {failures}"
        );
        t += Duration::from_secs(60);
    }
    // The fifth: nominal 160 s, 80 s with u = 0, which now dominates.
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(503), t);
    assert_eq!(
        ep_record(&core, 1).next_read_due,
        Some(t + Duration::from_secs(80))
    );
    assert_eq!(ep_record(&core, 1).last_error, Some(ErrorClass::Transient));
    // And the sixth reaches the cap's half.
    t += Duration::from_secs(80);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(503), t);
    assert_eq!(
        ep_record(&core, 1).next_read_due,
        Some(t + Duration::from_secs(150))
    );
    // A success resets the count, and the next read is at the interval.
    t += Duration::from_secs(150);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, t);
    let record = ep_record(&core, 1);
    assert_eq!(record.consecutive_failures, 0);
    assert_eq!(record.first_failure, None);
    assert_eq!(record.last_error, None);
    assert_eq!(record.next_read_due, Some(t + Duration::from_secs(60)));
}

#[test]
fn r56_no_class_is_retried_at_once() {
    // After any failure, nothing of that request is due at the same instant, so
    // no class retries within the tick (the one exception, R47's resend after a
    // refresh, is made inside the request).
    let errors = [
        ep_status(503),
        ep_status(408),
        ep_status(405),
        ep_status(401),
        ep_transport(),
        ep_rate_limited(0),
        ep_rate_limited(30),
        ep_forbidden(true),
        ep_forbidden(false),
        ep_owner(OwnerApiError::VehicleNotFound),
        ep_owner(OwnerApiError::Gone),
        ep_owner(OwnerApiError::ResponseTooLarge),
        ep_wrapped(OwnerApiError::HttpStatus(401)),
    ];
    for error in errors {
        let mut core = ep_online_core(1);
        let id = core_id(1);
        let t = core.at(60);
        assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
        core.scheduler.vehicle_failed_for_error(id, &error, t);
        assert!(
            core.scheduler.due_vehicles(t).is_empty(),
            "{error:?} must not be retried at once"
        );
        // A list that fails is not asked again at once either.
        let mut core = Core::owner();
        assert!(core.scheduler.discovery_due(core.t0));
        core.scheduler.discovery_failed_for_error(&error, core.t0);
        assert!(
            !core.scheduler.discovery_due(core.t0),
            "{error:?} must not be retried at once"
        );
    }
}

// ---- R45: rate limits ---------------------------------------------------------

#[test]
fn ct38_a_429_with_a_header_stops_every_vehicle_until_it_passes() {
    let mut core = ep_online_core(2);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t), vec![core_id(1), core_id(2)]);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_rate_limited(120), t);
    // The other car's read had already come back.
    core.scheduler
        .vehicle_succeeded(core_id(2), PollPhase::Online, false, t);
    // Nothing is due for either car until t + 120 s.
    assert!(core.scheduler.due_vehicles(t).is_empty());
    assert!(
        core.scheduler
            .due_vehicles(t + Duration::from_millis(119_999))
            .is_empty()
    );
    assert!(!core.scheduler.discovery_due(t + Duration::from_secs(119)));
    // R45 asks for a delay of at least the remaining wait (120 s), R11 for at most
    // `K_MAX_IDLE_WAIT`: the loop wakes at the cap, finds nothing allowed to send
    // and sleeps again, so no request is made before the wait passes.
    assert_eq!(core.scheduler.delay_until_next_action(t), K_MAX_IDLE_WAIT);
    let woken = t + K_MAX_IDLE_WAIT;
    assert!(core.scheduler.due_vehicles(woken).is_empty());
    assert!(!core.scheduler.discovery_due(woken));
    assert_eq!(
        core.scheduler.delay_until_next_action(woken),
        Duration::from_secs(60)
    );
    // Ordered by due instant: car 2's own read was due at t + 60 s, car 1's is held
    // to the end of the wait.
    assert_eq!(
        core.scheduler.due_vehicles(t + Duration::from_secs(120)),
        vec![core_id(2), core_id(1)]
    );
    // A 429 is no failure of a car (R45).
    assert_eq!(ep_record(&core, 1).consecutive_failures, 0);
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::None);
    assert_eq!(
        ep_record(&core, 1).last_error,
        Some(ErrorClass::RateLimited)
    );
}

#[test]
fn ct40_429_defaults() {
    let mut core = ep_online_core(1);
    let id = core_id(1);
    let mut t = core.at(60);
    // Absent header, u = 0: 60 s for the first four (back-off 5, 10, 20, 40 s is
    // below the floor), then 80 s and 150 s.
    let mut waits = Vec::new();
    for _ in 0..7 {
        ep_sync_clock(&core, t);
        core.scheduler
            .vehicle_failed_for_error(id, &ep_rate_limited(0), t);
        let (until, reason) = core.scheduler.account.wait.expect("a rate-limit wait");
        assert_eq!(reason, WaitReason::RateLimited);
        waits.push(until - t);
        t = until;
    }
    assert_eq!(
        waits,
        [60, 60, 60, 60, 80, 150, 150].map(Duration::from_secs)
    );
    // A success resets to the first rule.
    ep_sync_clock(&core, t);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, t);
    assert_eq!(core.scheduler.account.consecutive_429, 0);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_rate_limited(0), t);
    assert_eq!(
        core.scheduler.account.wait.expect("wait").0 - t,
        Duration::from_secs(60)
    );
}

#[test]
fn r45_a_header_value_is_clamped_and_never_an_immediate_retry() {
    let mut core = ep_online_core(1);
    let t = core.at(60);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_rate_limited(u64::MAX), t);
    assert_eq!(
        core.scheduler.account.wait.expect("wait").0 - t,
        K_RETRY_AFTER_MAX,
        "an hour at most"
    );
    // A wait that overflows the clock is never a retry now.
    let mut core = ep_online_core(1);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_rate_limited(0), t);
    assert!(core.scheduler.account.wait.expect("wait").0 > t);
}

#[test]
fn r45_the_answers_of_one_burst_count_once_and_a_longer_header_extends_the_wait() {
    let mut core = ep_online_core(3);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t).len(), 3);
    // Three cars get 429 in the same tick: one piece of evidence.
    for id in 1..=3 {
        core.scheduler
            .vehicle_failed_for_error(core_id(id), &ep_rate_limited(0), t);
    }
    assert_eq!(core.scheduler.account.consecutive_429, 1);
    assert_eq!(
        core.scheduler.account.wait.expect("wait").0 - t,
        Duration::from_secs(60)
    );
    // A later answer in the burst that names a longer wait lengthens it.
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_rate_limited(90), t);
    assert_eq!(core.scheduler.account.consecutive_429, 1);
    assert_eq!(
        core.scheduler.account.wait.expect("wait").0 - t,
        Duration::from_secs(90)
    );
    // A shorter one never shortens it.
    core.scheduler
        .vehicle_failed_for_error(core_id(2), &ep_rate_limited(10), t);
    assert_eq!(
        core.scheduler.account.wait.expect("wait").0 - t,
        Duration::from_secs(90)
    );
}

#[test]
fn r45_the_wait_is_persisted_as_a_wall_deadline() {
    let mut core = ep_online_core(1);
    let tick = core.scheduler.take_tick();
    core.scheduler.commit_succeeded(tick, core.t0);
    let t = core.at(60);
    ep_sync_clock(&core, t);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_rate_limited(120), t);
    let tick = core.scheduler.take_tick();
    let account = tick.account.as_ref().expect("the account row changed");
    assert_eq!(account.row.state, "collecting");
    assert_eq!(
        account.row.rate_limited_until_ms,
        Some(CORE_W0 + 60_000 + 120_000)
    );
    // The rate-limit episode is a gap once it has lasted the minimum (R8).
    core.scheduler.commit_succeeded(tick, t);
    assert_eq!(core.scheduler.account.consecutive_429, 1);
}

// ---- R43: unreachable ----------------------------------------------------------

#[test]
fn r43_a_request_with_no_status_waits_the_account_and_counts_no_car() {
    let mut core = ep_online_core(2);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t).len(), 2);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_transport(), t);
    core.scheduler.vehicle_failed_for_error(
        core_id(2),
        &ep_owner(OwnerApiError::RequestTimeout),
        t,
    );
    for id in 1..=2 {
        let record = ep_record(&core, id);
        assert_eq!(record.consecutive_failures, 0);
        assert_eq!(record.first_failure, None);
        assert_eq!(record.phase, PollPhase::Online, "phases are kept");
        assert_eq!(record.hold, VehicleHold::None);
        assert!(!record.in_flight);
    }
    assert_eq!(
        core.scheduler.account.consecutive_unreachable, 1,
        "one tick, one count"
    );
    assert_eq!(
        core.scheduler.account.wait,
        Some((t + Duration::from_secs(5), WaitReason::TeslaUnreachable))
    );
    assert!(
        core.scheduler
            .due_vehicles(t + Duration::from_secs(4))
            .is_empty()
    );
    assert_eq!(
        core.scheduler.due_vehicles(t + Duration::from_secs(5)),
        vec![core_id(1), core_id(2)]
    );
}

#[test]
fn r43_any_answered_request_ends_the_unreachable_wait() {
    // A success.
    let mut core = ep_online_core(1);
    let t = core.at(60);
    core.scheduler.due_vehicles(t);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_transport(), t);
    assert!(core.scheduler.account.wait.is_some());
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t + Duration::from_secs(5));
    assert_eq!(core.scheduler.account.wait, None);
    assert_eq!(core.scheduler.account.consecutive_unreachable, 0);

    // An error that carries an HTTP status is an answer too: the account is
    // reachable, and the 503 is the car's own failure.
    let mut core = ep_online_core(2);
    let t = core.at(60);
    core.scheduler.due_vehicles(t);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_transport(), t);
    core.scheduler
        .vehicle_failed_for_error(core_id(2), &ep_status(503), t);
    assert_eq!(core.scheduler.account.wait, None);
    assert_eq!(core.scheduler.account.consecutive_unreachable, 0);
    assert_eq!(ep_record(&core, 2).consecutive_failures, 1);
    assert_eq!(ep_record(&core, 1).consecutive_failures, 0);
}

#[test]
fn r43_a_failed_list_waits_the_account_the_same_way() {
    let mut core = Core::owner();
    let delay = core
        .scheduler
        .discovery_failed_for_error(&ep_transport(), core.t0);
    assert_eq!(delay, Duration::from_secs(5));
    assert_eq!(core.scheduler.account.consecutive_unreachable, 1);
    assert_eq!(
        core.scheduler.account.discovery_failures, 0,
        "its own count"
    );
    assert!(!core.scheduler.discovery_due(core.at(4)));
    assert!(core.scheduler.discovery_due(core.at(5)));
}

#[test]
fn ct60_two_hours_of_network_loss_pauses_no_car_and_records_one_gap() {
    let mut core = Core::owner();
    // Car 1 is Online and in use (reads due every 60 s); car 2 is Asleep.
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "asleep")],
        core.t0,
    );
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, false, core.t0);
    let tick = core.scheduler.take_tick();
    core.scheduler.commit_succeeded(tick, core.t0);

    let end = core.at(2 * 60 * 60);
    let mut t = core.t0;
    let mut attempts: Vec<Instant> = Vec::new();
    let mut gaps = Vec::new();
    while t < end || !core.scheduler.requests_allowed(t) {
        ep_sync_clock(&core, t);
        let mut sent = false;
        if core.scheduler.discovery_due(t) {
            sent = true;
            core.scheduler
                .discovery_failed_for_error(&ep_transport(), t);
        }
        for id in core.scheduler.due_offline_state_vehicles(t) {
            sent = true;
            core.scheduler
                .offline_state_failed_for_error(id, &ep_transport(), t);
        }
        for id in core.scheduler.due_vehicles(t) {
            sent = true;
            core.scheduler
                .vehicle_failed_for_error(id, &ep_transport(), t);
        }
        if sent {
            attempts.push(t);
        }
        let tick = core.scheduler.take_tick();
        gaps.extend(
            tick.gap_opens
                .iter()
                .map(|open| (open.key.kind, open.alert, t)),
        );
        assert!(
            tick.gap_opens
                .iter()
                .all(|open| open.key.kind != GapKind::VehiclePaused),
            "no car is paused by a network fault"
        );
        core.scheduler.commit_succeeded(tick, t);
        let step = core
            .scheduler
            .delay_until_next_action(t)
            .max(Duration::from_millis(1));
        t += step;
    }

    // No car's failure count moved, and neither car is held.
    for id in 1..=2 {
        let record = ep_record(&core, id);
        assert_eq!(record.consecutive_failures, 0);
        assert_eq!(record.hold, VehicleHold::None);
    }
    // The account waited 5, 10, 20, 40, 80 s and then 150 s each time.
    let spacing: Vec<u64> = attempts
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).as_secs())
        .collect();
    assert_eq!(spacing[..5], [5, 10, 20, 40, 80]);
    assert!(
        spacing[5..].iter().all(|seconds| *seconds == 150),
        "{spacing:?}"
    );
    assert!(
        attempts.len() <= 54 && attempts.len() > 45,
        "{} attempt ticks in two hours",
        attempts.len()
    );
    // One tesla_unreachable gap, once the minimum has passed, alerting because
    // car 1 was awake.
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    let (kind, alert, opened_at) = gaps[0];
    assert_eq!(kind, GapKind::TeslaUnreachable);
    assert!(alert);
    assert!(opened_at - attempts[0] >= K_GAP_MIN);

    // The fake recovers: the next attempt's list succeeds, the wait and count
    // clear, car 1 (listed online, its read long overdue) is read in that tick,
    // and the gap closes.
    ep_sync_clock(&core, t);
    assert!(core.scheduler.discovery_due(t));
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "asleep")],
        t,
    );
    assert_eq!(core.scheduler.account.wait, None);
    assert_eq!(core.scheduler.account.consecutive_unreachable, 0);
    assert_eq!(core.scheduler.due_vehicles(t), vec![core_id(1)]);
    let tick = core.scheduler.take_tick();
    assert_eq!(tick.gap_closes.len(), 1);
    assert_eq!(tick.gap_closes[0].key.kind, GapKind::TeslaUnreachable);
}

#[test]
fn ct60_variant_a_list_that_answers_leaves_the_account_alone_while_reads_fail() {
    let mut core = Core::owner();
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "asleep")],
        core.t0,
    );
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, false, core.t0);
    // Every read of car 1 gets 503 for two hours, the list answering.
    let end = core.at(2 * 60 * 60);
    let mut t = core.t0;
    let mut failures = Vec::new();
    while t < end {
        ep_sync_clock(&core, t);
        if core.scheduler.discovery_due(t) {
            core.scheduler.accept_discovery(
                vec![core_vehicle(1, "online"), core_vehicle(2, "asleep")],
                t,
            );
        }
        for id in core.scheduler.due_vehicles(t) {
            assert_eq!(id, core_id(1), "car 2 sleeps and is never read");
            core.scheduler
                .vehicle_failed_for_error(id, &ep_status(503), t);
            failures.push(t);
        }
        assert_eq!(core.scheduler.account.wait, None, "the account never waits");
        t += core
            .scheduler
            .delay_until_next_action(t)
            .max(Duration::from_millis(1));
    }
    // Reads at the interval while it dominates, then at the back-off's half cap.
    let spacing: Vec<u64> = failures
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).as_secs())
        .collect();
    assert_eq!(spacing[..6], [60, 60, 60, 60, 80, 150]);
    assert!(spacing[6..].iter().all(|seconds| *seconds == 150));
    assert_eq!(
        ep_record(&core, 1).consecutive_failures as usize,
        failures.len()
    );
    assert_eq!(ep_record(&core, 2).consecutive_failures, 0);
    assert_eq!(ep_record(&core, 2).phase, PollPhase::Asleep);
    // The thresholds of R51 (a dozen failures over at least an hour) pause car 1 and
    // leave car 2 alone.
    assert_eq!(
        ep_record(&core, 1).hold,
        VehicleHold::Paused(PauseReason::PersistentFailure)
    );
    assert_eq!(ep_record(&core, 2).hold, VehicleHold::None);
}

// ---- R47: authentication outcomes ----------------------------------------------

#[test]
fn ct41_token_expiry_mid_tick_counts_no_failure_and_waits_the_account_once() {
    // The credential unit refreshes and resends inside each request (its part,
    // C5 R40); what reaches the scheduler is each request's outcome. Here three
    // reads all come back as the Owner-wire authentication outcome.
    let mut core = ep_online_core(3);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t).len(), 3);
    let unauthorised = ep_wrapped(OwnerApiError::HttpStatus(401));
    for id in 1..=3 {
        core.scheduler
            .vehicle_failed_for_error(core_id(id), &unauthorised, t);
    }
    for id in 1..=3 {
        let record = ep_record(&core, id);
        assert_eq!(
            record.consecutive_failures, 0,
            "auth never counts towards a pause"
        );
        assert_eq!(record.first_failure, None);
        assert_eq!(record.last_error, Some(ErrorClass::Auth));
        assert_eq!(record.phase, PollPhase::Online);
        assert!(!record.in_flight);
    }
    // One wait step for the tick, not three, and nothing is sent before it.
    assert_eq!(core.scheduler.account.auth_failures, 1);
    assert_eq!(
        core.scheduler.account.wait,
        Some((t + Duration::from_secs(5), WaitReason::AuthWait))
    );
    assert!(
        core.scheduler
            .due_vehicles(t + Duration::from_secs(4))
            .is_empty()
    );
    // After the wait all three are due again, and their success ends it all.
    let later = t + Duration::from_secs(5);
    assert_eq!(core.scheduler.due_vehicles(later).len(), 3);
    for id in 1..=3 {
        core.scheduler
            .vehicle_succeeded(core_id(id), PollPhase::Online, false, later);
        assert_eq!(ep_record(&core, id).last_error, None);
    }
    assert_eq!(core.scheduler.account.auth_failures, 0);
    assert_eq!(core.scheduler.account.wait, None);
}

#[test]
fn r47_repeated_authentication_failures_back_off_by_the_one_function() {
    let mut core = Core::owner();
    let mut t = core.t0;
    let mut waits = Vec::new();
    for _ in 0..7 {
        ep_sync_clock(&core, t);
        let delay = core
            .scheduler
            .discovery_failed_for_error(&ep_wrapped(OwnerApiError::HttpStatus(401)), t);
        waits.push(delay);
        assert_eq!(
            core.scheduler.account.wait.expect("wait").1,
            WaitReason::AuthWait
        );
        t += delay;
    }
    assert_eq!(
        waits,
        [5, 10, 20, 40, 80, 150, 150].map(Duration::from_secs)
    );
    // A list that answers ends the sequence.
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], t);
    assert_eq!(core.scheduler.account.auth_failures, 0);
    assert_eq!(core.scheduler.account.wait, None);
}

#[test]
fn r47_a_terminal_credential_outcome_stops_the_account_and_opens_its_gap() {
    let mut core = ep_online_core(1);
    core.scheduler.vehicles.get_mut(&core_id(1)).unwrap().hub_id = Some(Uuid::new_v4());
    let tick = core.scheduler.take_tick();
    core.scheduler.commit_succeeded(tick, core.t0);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t), vec![core_id(1)]);
    core.scheduler.vehicle_failed_for_error(
        core_id(1),
        &CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn),
        t,
    );
    assert_eq!(core.scheduler.account.state, AccountState::CredentialStop);
    assert_eq!(core.scheduler.account.state_reason, "auth_terminal");
    assert_eq!(ep_record(&core, 1).consecutive_failures, 0);
    // No request of any kind is sent, whatever is due.
    assert!(!core.scheduler.requests_allowed(t));
    assert!(
        core.scheduler
            .due_vehicles(t + Duration::from_secs(3_600))
            .is_empty()
    );
    // It is derived, never stored by the scheduler, and its gap opens.
    let tick = core.scheduler.take_tick();
    assert!(tick.account.is_none());
    assert_eq!(tick.gap_opens.len(), 1);
    assert_eq!(tick.gap_opens[0].key.kind, GapKind::AuthTerminal);
    assert!(tick.gap_opens[0].alert, "the car was awake");
    core.scheduler.commit_succeeded(tick, t);
    // A second terminal answer changes nothing.
    core.scheduler.vehicle_failed_for_error(
        core_id(1),
        &CollectorError::SensitiveAccessUnavailable,
        t,
    );
    assert!(core.scheduler.take_tick().is_empty());
    // The credential unit lifts it: collection resumes and the gap closes.
    ep_sync_clock(&core, t + Duration::from_secs(30));
    core.scheduler
        .clear_credential_stop(t + Duration::from_secs(30));
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);
    assert!(core.scheduler.discovery_due(t + Duration::from_secs(30)));
    let tick = core.scheduler.take_tick();
    assert_eq!(tick.gap_closes.len(), 1);
    assert_eq!(tick.gap_closes[0].key.kind, GapKind::AuthTerminal);
}

#[test]
fn r47_a_credential_stop_never_replaces_a_stored_refusal() {
    let mut core = Core::owner();
    core.scheduler.account.state = AccountState::OwnerApiRefused;
    core.scheduler.discovery_failed_for_error(
        &CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn),
        core.t0,
    );
    assert_eq!(core.scheduler.account.state, AccountState::OwnerApiRefused);
}

// ---- R46: 408 --------------------------------------------------------------------

#[test]
fn r46_a_408_asks_for_the_state_check_and_asleep_moves_the_car() {
    let mut core = ep_online_core(1);
    let id = core_id(1);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(408), t);
    let record = ep_record(&core, 1);
    assert_eq!(record.consecutive_failures, 0, "a state, not a fault");
    assert_eq!(record.last_error, None);
    // Not retried: the read is next due a whole interval later.
    assert_eq!(record.next_read_due, Some(t + Duration::from_secs(60)));
    assert!(core.scheduler.due_vehicles(t).is_empty());
    // A record request for this car is due now.
    assert_eq!(core.scheduler.due_offline_state_vehicles(t), vec![id]);
    assert_eq!(core.scheduler.account.wait, None);
    // The record says asleep: the car is Asleep and its read is withdrawn.
    core.scheduler
        .accept_vehicle_state(id, "asleep".to_owned(), t);
    let record = ep_record(&core, 1);
    assert_eq!(record.phase, PollPhase::Asleep);
    assert_eq!(record.next_read_due, None);
    assert_eq!(record.consecutive_failures, 0);
    assert!(!record.unavailable_check);
}

#[test]
fn r46_a_record_that_says_offline_marks_a_session_car_offline_from_session() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Driving, false, core.t0);
    let t = core.at(10);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(408), t);
    assert_eq!(core.scheduler.due_offline_state_vehicles(t), vec![id]);
    core.scheduler
        .accept_vehicle_state(id, "offline".to_owned(), t);
    let record = ep_record(&core, 1);
    assert_eq!(record.phase, PollPhase::Offline);
    assert!(record.offline_from_session);
    assert_eq!(record.consecutive_failures, 0);
}

#[test]
fn r46_a_car_still_reported_online_after_a_408_has_failed_transiently() {
    let mut core = ep_online_core(1);
    let id = core_id(1);
    let t = core.at(60);
    core.scheduler.due_vehicles(t);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(408), t);
    assert_eq!(ep_record(&core, 1).consecutive_failures, 0);
    // The record answers online: poor connectivity, a transient failure.
    core.scheduler
        .accept_vehicle_state(id, "online".to_owned(), t);
    let record = ep_record(&core, 1);
    assert_eq!(record.consecutive_failures, 1);
    assert_eq!(record.last_error, Some(ErrorClass::Transient));
    assert_eq!(record.next_read_due, Some(t + Duration::from_secs(60)));
    assert!(!record.unavailable_check);
    // A second online answer is not a second failure.
    core.scheduler
        .accept_vehicle_state(id, "online".to_owned(), t);
    assert_eq!(ep_record(&core, 1).consecutive_failures, 1);
    // The back-off grows with repeated 408s on a car that stays online.
    for expected in 2..=3u32 {
        let due = ep_record(&core, 1).next_read_due.expect("due");
        core.scheduler.due_vehicles(due);
        core.scheduler
            .vehicle_failed_for_error(id, &ep_status(408), due);
        core.scheduler
            .accept_vehicle_state(id, "online".to_owned(), due);
        assert_eq!(ep_record(&core, 1).consecutive_failures, expected);
    }
}

#[test]
fn r46_in_fleet_polling_the_check_is_the_next_list_due_now() {
    let mut core = Core::fleet();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, core.t0);
    let t = core.at(120);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    assert!(
        !core.scheduler.discovery_due(t),
        "the routine list is not due"
    );
    core.scheduler
        .vehicle_failed_for_error(id, &ep_fleet_status(408), t);
    assert!(
        core.scheduler.due_offline_state_vehicles(t).is_empty(),
        "no records in Fleet"
    );
    assert!(core.scheduler.discovery_due(t), "a list is due now");
    // The list says offline.
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "offline")], t);
    let record = ep_record(&core, 1);
    assert_eq!(record.phase, PollPhase::Offline);
    assert_eq!(record.consecutive_failures, 0);

    // And a list that says online makes it a transient failure.
    let mut core = Core::fleet();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, core.t0);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_fleet_status(408), t);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t);
    assert_eq!(ep_record(&core, 1).consecutive_failures, 1);
    assert_eq!(ep_record(&core, 1).last_error, Some(ErrorClass::Transient));
}

#[test]
fn r46_while_updating_a_408_changes_nothing_and_asks_for_nothing() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Updating, false, core.t0);
    let t = core.at(120);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(408), t);
    let record = ep_record(&core, 1);
    assert_eq!(record.consecutive_failures, 0);
    assert_eq!(record.last_error, None);
    assert_eq!(record.phase, PollPhase::Updating);
    assert_eq!(record.next_read_due, Some(t + Duration::from_secs(120)));
    assert!(!record.unavailable_check);
    assert!(core.scheduler.due_offline_state_vehicles(t).is_empty());
}

// ---- R57: discovery failures ----------------------------------------------------

#[test]
fn r57_a_failed_list_waits_the_longer_of_its_interval_and_the_back_off() {
    // A sleeping car keeps the list at the 30 s state interval, so the back-off
    // shows once it exceeds it: 5, 10, 20 s below, then 40, 80 and 150 s.
    let mut core = Core::owner();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
    let t = core.at(30);
    for (class_error, label) in [
        (ep_status(503), "Transient"),
        (ep_owner(OwnerApiError::InvalidVehicleListCount), "Protocol"),
        (ep_status(400), "ClientError"),
        (ep_forbidden(false), "plain 403 on the list"),
        (ep_owner(OwnerApiError::VehicleNotFound), "404 on the list"),
        (ep_status(451), "451 on the list"),
        (ep_fleet_status(540), "540"),
    ] {
        let mut core = Core::owner();
        core.scheduler
            .accept_discovery(vec![core_vehicle(1, "asleep")], core.t0);
        let delays: Vec<Duration> = (0..7)
            .map(|_| core.scheduler.discovery_failed_for_error(&class_error, t))
            .collect();
        assert_eq!(
            delays,
            [30, 30, 30, 40, 80, 150, 150].map(Duration::from_secs),
            "{label}"
        );
        assert_eq!(core.scheduler.account.discovery_failures, 7, "{label}");
        assert_eq!(
            core.scheduler.account.wait, None,
            "{label}: no account wait"
        );
    }
    // A list that answers resets the count.
    core.scheduler
        .discovery_failed_for_error(&ep_status(503), t);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], t);
    assert_eq!(core.scheduler.account.discovery_failures, 0);
}

#[test]
fn r57_the_account_classes_return_their_own_waits() {
    let mut core = Core::owner();
    let t = core.at(5);
    assert_eq!(
        core.scheduler
            .discovery_failed_for_error(&ep_rate_limited(120), t),
        Duration::from_secs(120)
    );
    assert_eq!(core.scheduler.account.consecutive_429, 1);
    assert_eq!(core.scheduler.account.discovery_failures, 0);
    // A terminal credential outcome returns the settings refresh, so the loop
    // keeps reading settings while it waits for the credential unit.
    let mut core = Core::owner();
    assert_eq!(
        core.scheduler.discovery_failed_for_error(
            &CollectorError::OwnerApiAuth(OwnerApiAuthError::NotSignedIn),
            t
        ),
        CONTROL_SETTINGS_REFRESH
    );
    assert_eq!(core.scheduler.account.state, AccountState::CredentialStop);
}

#[test]
fn r57_a_fleet_refresh_that_was_not_sent_waits_the_credential_units_retry_time() {
    let mut core = Core::fleet();
    let error =
        CollectorError::FleetCredential(FleetCredentialError::Api(FleetApiError::RequestNotSent));
    assert_eq!(
        core.scheduler.discovery_failed_for_error(&error, core.t0),
        FLEET_REFRESH_REQUEST_NOT_SENT_RETRY
    );
    assert_eq!(
        core.scheduler.account.wait,
        Some((
            core.t0 + FLEET_REFRESH_REQUEST_NOT_SENT_RETRY,
            WaitReason::AuthWait
        ))
    );
    assert_eq!(core.scheduler.account.discovery_failures, 0);
}

#[test]
fn ct66_owner_action_states_do_not_stall_the_loop() {
    // Fleet, with the account in a state that waits for the owner: a failed list
    // probe returns at most the settings refresh, and the loop sleeps at most
    // that long, so an owner action is picked up within seconds. The owner actions
    // themselves are tested with part K6 (CT73 and the loop test of CT66).
    for state in [
        AccountState::FleetAccessRefused,
        AccountState::BillingSuspended,
        AccountState::ConfigError,
        AccountState::AccountLocked,
        AccountState::OwnerApiRefused,
    ] {
        let mut core = Core::fleet();
        core.scheduler.account.state = state;
        let delay = core
            .scheduler
            .discovery_failed_for_error(&ep_fleet_status(403), core.t0);
        assert!(delay <= Duration::from_secs(5), "{state:?}: {delay:?}");
        assert_eq!(delay, CONTROL_SETTINGS_REFRESH, "{state:?}");
        for idle in [false, true] {
            assert!(
                core.scheduler.next_wait(core.t0, false, idle)
                    <= Duration::from_secs(5).max(K_LOOP_FLOOR),
                "{state:?}"
            );
        }
        // A collecting account keeps the back-off row.
        let mut collecting = Core::fleet();
        let delay = collecting
            .scheduler
            .discovery_failed_for_error(&ep_fleet_status(503), collecting.t0);
        assert!(delay >= Duration::from_secs(5));
    }
}

// ---- R22 with a gate: an account failure keeps a gated read's gate -----------------

#[test]
fn r22_an_account_failure_keeps_the_gate_of_a_stream_triggered_read() {
    let mut core = suspended_core();
    let id = core_id(1);
    let t = core.at(20 * 60);
    core.scheduler
        .schedule_stream_evidence(id, Some("P"), Some(0.0), Some(-7), t);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    assert!(core.scheduler.requires_live_stream_power_gate(id));
    core.scheduler
        .vehicle_failed_for_error(id, &ep_transport(), t);
    let record = ep_record(&core, 1);
    assert_eq!(record.phase, PollPhase::Suspended);
    assert_eq!(record.consecutive_failures, 0);
    // After the account's wait the read is due again, and still gated.
    let later = t + Duration::from_secs(5);
    assert_eq!(core.scheduler.due_vehicles(later), vec![id]);
    assert!(core.scheduler.requires_live_stream_power_gate(id));

    // A car the stream showed in motion is Driving whatever the failure was.
    let mut core = suspended_core();
    core.scheduler
        .schedule_stream_evidence(id, Some("D"), Some(12.0), None, t);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_rate_limited(30), t);
    assert_eq!(ep_record(&core, 1).phase, PollPhase::Driving);
    assert_eq!(ep_record(&core, 1).consecutive_failures, 0);
}

#[test]
fn r22_the_gate_refusal_is_no_failure_and_no_answer() {
    let mut core = suspended_core();
    let id = core_id(1);
    let t = core.at(20 * 60);
    core.scheduler
        .schedule_stream_evidence(id, Some("P"), Some(0.0), Some(-7), t);
    core.scheduler.due_vehicles(t);
    // No request was sent: the account's reachability is untouched.
    core.scheduler.account.consecutive_unreachable = 3;
    core.scheduler.account.wait = Some((t + Duration::from_secs(40), WaitReason::TeslaUnreachable));
    core.scheduler.vehicle_failed_for_error(
        id,
        &ep_owner(OwnerApiError::StreamPowerNotConfirmed),
        t,
    );
    assert_eq!(core.scheduler.account.consecutive_unreachable, 3);
    assert!(core.scheduler.account.wait.is_some());
    assert_eq!(ep_record(&core, 1).consecutive_failures, 0);
}

// ---- R9, R10: one event per transition ------------------------------------------------

#[test]
fn r10_an_account_wait_is_one_event_however_many_answers_make_it() {
    let log = DecisionLog::start();
    let mut core = ep_online_core(3);
    let t = core.at(60);
    core.scheduler.due_vehicles(t);
    for id in 1..=3 {
        core.scheduler
            .vehicle_failed_for_error(core_id(id), &ep_transport(), t);
    }
    assert_eq!(
        log.lines("INFO", "tesla_unreachable"),
        1,
        "{}",
        log.contents()
    );
    // Recovery is one event, too.
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t + Duration::from_secs(5));
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t + Duration::from_secs(35));
    assert_eq!(log.lines("INFO", "tesla_reachable"), 1);
    // Rate limits and authentication waits likewise.
    for id in 1..=3 {
        core.scheduler.vehicle_failed_for_error(
            core_id(id),
            &ep_rate_limited(0),
            t + Duration::from_secs(40),
        );
    }
    assert_eq!(log.lines("INFO", "rate_limited"), 1);
    let log_text = log.contents();
    assert!(
        !log_text.contains("VIN0000"),
        "no vehicle identifier is logged"
    );
}

// ---- CT67: token expiry mid-tick, Fleet --------------------------------------------------

/// What the fake Fleet and token servers saw.
#[derive(Clone, Default)]
struct FleetWire {
    data_authorisations: Arc<Mutex<Vec<String>>>,
    token_requests: Arc<AtomicUsize>,
    /// Every read answers 401, whatever token it carries (C3 R47, CT67c).
    reject_every_token: Arc<std::sync::atomic::AtomicBool>,
}

const CT67_SCOPED_ACCESS: &str = "e30.eyJzY3AiOlsib3BlbmlkIiwidmVoaWNsZV9kZXZpY2VfZGF0YSIsInZlaGljbGVfbG9jYXRpb24iLCJ2ZWhpY2xlX2NtZHMiLCJ2ZWhpY2xlX2NoYXJnaW5nX2NtZHMiXX0.sig";

async fn ct67_data_handler(
    State(wire): State<FleetWire>,
    headers: HeaderMap,
) -> axum::response::Response {
    let authorisation = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    wire.data_authorisations
        .lock()
        .expect("ledger")
        .push(authorisation.clone());
    if authorisation == "Bearer fleet-next-access"
        && !wire.reject_every_token.load(Ordering::SeqCst)
    {
        (
            StatusCode::OK,
            r#"{"response":{"drive_state":{"timestamp":1700000000000},"charge_state":{"battery_level":80}}}"#,
        )
            .into_response()
    } else {
        (StatusCode::UNAUTHORIZED, r#"{"error":"token expired"}"#).into_response()
    }
}

async fn ct67_token_handler(State(wire): State<FleetWire>) -> axum::response::Response {
    wire.token_requests.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        r#"{"access_token":"fleet-next-access","refresh_token":"fleet-next-refresh","expires_in":28800,"token_type":"Bearer"}"#,
    )
        .into_response()
}

struct Ct67Bed {
    _dir: tempfile::TempDir,
    wire: FleetWire,
    api: FleetApi,
    auth_api: FleetAuthApi,
    manager: Arc<tokio::sync::Mutex<FleetAuthManager>>,
    vehicles: Vec<Vehicle>,
    _server: tokio::task::JoinHandle<()>,
}

async fn ct67_bed() -> Ct67Bed {
    let dir = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(dir.path()).expect("Hub store");
    crate::teslamate_credentials::load_or_create_cursor_key(dir.path()).expect("cursor key");
    let credentials = FleetSetupCredentials::new(
        CT67_SCOPED_ACCESS.to_owned(),
        "fleet-refresh".to_owned(),
        "fleet-client".to_owned(),
        crate::fleet_api::FleetRegion::EuropeMiddleEastAndAfrica,
        28_800,
    )
    .expect("setup credentials");
    // Stored now, so no refresh is due until a 401 makes one so.
    crate::fleet_credentials::persist_fleet_setup_credentials(
        &store,
        dir.path(),
        &credentials,
        SystemTime::now(),
    )
    .expect("persist credentials");
    let manager = FleetAuthManager::from_store(store, dir.path()).expect("Fleet manager");
    let wire = FleetWire::default();
    let router = Router::new()
        .route("/oauth2/v3/token", post(ct67_token_handler))
        .route("/api/1/vehicles/{vin}/vehicle_data", get(ct67_data_handler))
        .with_state(wire.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake server");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("base");
    let api = FleetApi::for_fake_http(base.clone(), Duration::from_secs(2)).expect("Fleet client");
    let auth_api = FleetAuthApi::for_fake_http(
        base.join("oauth2/v3/token").expect("token URL"),
        Duration::from_secs(2),
    )
    .expect("auth client");
    Ct67Bed {
        _dir: dir,
        wire,
        api,
        auth_api,
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        vehicles: (1..=3)
            .map(|id| Vehicle::for_test(id, &format!("5YJ3E1EA7KF00000{id}"), "online"))
            .collect(),
        _server: server,
    }
}

#[tokio::test]
async fn ct67_three_fleet_reads_with_an_expired_token_cause_one_refresh_and_no_failure() {
    let bed = ct67_bed().await;
    // One tick: its requests share one refresh latch (R47).
    let tick = TickRefresh::default();
    let results = futures_util::future::join_all(bed.vehicles.iter().map(|vehicle| {
        fleet_vehicle_data_with_auth(
            &bed.api,
            &bed.auth_api,
            &bed.manager,
            vehicle,
            true,
            CountedStart::admitted(None, RequestCategory::Data),
            &tick,
        )
    }))
    .await;
    // Every read succeeded in the same tick, so the scheduler counts no failure.
    let mut core = Core::fleet();
    core.scheduler
        .accept_discovery(bed.vehicles.clone(), core.t0);
    core.scheduler.due_vehicles(core.t0);
    for (vehicle, result) in bed.vehicles.iter().zip(results) {
        let snapshot = result.expect("a read that survived the expiry");
        core.scheduler
            .vehicle_succeeded(vehicle.id, poll_phase(&snapshot), true, core.t0);
        assert_eq!(core.scheduler.vehicles[&vehicle.id].consecutive_failures, 0);
    }
    // Exactly one refresh for the three 401s (the manager is behind one mutex).
    assert_eq!(bed.wire.token_requests.load(Ordering::SeqCst), 1);
    let sent = bed.wire.data_authorisations.lock().expect("ledger").clone();
    let with_old = sent
        .iter()
        .filter(|header| header.starts_with("Bearer e30."))
        .count();
    let with_new = sent
        .iter()
        .filter(|header| *header == "Bearer fleet-next-access")
        .count();
    // The first read meets the expired token once and is resent once; the other
    // two wait for the manager's mutex and go out with the new token, so they
    // are never sent with the dead one (the spec expects each sent twice: the
    // retained helpers serialise on the mutex, which sends fewer requests).
    assert_eq!(with_old, 1, "{sent:?}");
    assert_eq!(with_new, 3, "{sent:?}");
}

#[tokio::test]
async fn ct67_an_observer_makes_no_refresh_and_no_resend() {
    let bed = ct67_bed().await;
    let error = fleet_vehicle_data_with_auth(
        &bed.api,
        &bed.auth_api,
        &bed.manager,
        &bed.vehicles[0],
        false,
        CountedStart::admitted(None, RequestCategory::Data),
        &TickRefresh::default(),
    )
    .await
    .expect_err("the expired token is rejected");
    assert_eq!(
        bed.wire.token_requests.load(Ordering::SeqCst),
        0,
        "no refresh"
    );
    assert_eq!(
        bed.wire.data_authorisations.lock().expect("ledger").len(),
        1,
        "no resend"
    );
    // The outcome is the retained classifier's, and it is an authentication
    // outcome, never a car's failure.
    assert!(fleet_credential_stop(&error));
    assert_eq!(
        classify(
            &error,
            RequestKind::Read,
            crate::config::CollectorMode::FleetPolling
        ),
        ErrorClass::Auth
    );
}

#[tokio::test]
async fn ct67c_a_token_the_request_refreshed_for_itself_is_not_refreshed_again() {
    // At most one token refresh per tick (C3 R47): a request whose own start refreshed
    // the token (it was due) and still gets 401 carried the tick's refreshed token, so
    // it is a credential stop, not a reason for a second refresh and a resend. The
    // helper makes one read, one refresh and no resend; what the loop then does with
    // the requests still queued (it cancels them) is the loop's, proved at its own
    // tests, so nothing here invokes a second request or claims the cancellation.
    let bed = ct67_bed().await;
    bed.wire.reject_every_token.store(true, Ordering::SeqCst);
    bed.manager.lock().await.mark_refresh_due();
    let tick = TickRefresh::default();
    let error = fleet_vehicle_data_with_auth(
        &bed.api,
        &bed.auth_api,
        &bed.manager,
        &bed.vehicles[0],
        true,
        CountedStart::admitted(None, RequestCategory::Data),
        &tick,
    )
    .await
    .expect_err("the refreshed token is refused too");
    assert_eq!(bed.wire.token_requests.load(Ordering::SeqCst), 1, "the start's refresh only");
    assert_eq!(bed.wire.data_authorisations.lock().expect("ledger").len(), 1, "no resend");
    assert!(fleet_credential_stop(&error), "{error:?}");
    assert!(tick.refreshed());
}
