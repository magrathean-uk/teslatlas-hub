// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C3 part K6 (refusals, holds and owner actions), the hold half:
// CT36, CT37, CT47, CT48, CT57, CT65, CT73, and the rules the part owns around
// them (R51 to R53, R65). The refusal half (CT42 to CT46, CT72, the CT56 log
// scenarios) is in `refusals.rs`. Synthetic data only; the stores are temporary
// and nothing touches the network.

/// Give the first `cars` cars of a scheduler-only core their Hub ids, so rows and
/// gaps can name them, as `apply_control_settings` does in the loop.
fn k6_hub_ids(core: &mut Core, cars: u64) -> Vec<Uuid> {
    (1..=cars)
        .map(|id| {
            let hub = Uuid::new_v4();
            core.scheduler
                .vehicles
                .get_mut(&core_id(id))
                .expect("a scheduled car")
                .hub_id = Some(hub);
            hub
        })
        .collect()
}

/// The pending tick's gaps of `kind` for `vehicle`, as opens and closes.
fn k6_gap_events(tick: &PendingTick, kind: GapKind, vehicle: Option<VehicleId>) -> (usize, usize) {
    let key = GapKey { kind, vehicle };
    (
        tick.gap_opens.iter().filter(|open| open.key == key).count(),
        tick.gap_closes
            .iter()
            .filter(|close| close.key == key)
            .count(),
    )
}

/// Commit the scheduler's pending tick in memory, as a successful transaction
/// does, and return it.
fn k6_commit_in_memory(core: &mut Core) -> PendingTick {
    let tick = core.scheduler.take_tick();
    let now = core.clock.mono_now();
    core.scheduler.commit_succeeded(tick.clone(), now);
    tick
}

// ---- CT36 ---------------------------------------------------------------------

#[test]
fn ct36_a_persistent_failure_pauses_a_car_and_only_its_own_probe_clears_the_pause() {
    let mut core = ep_online_core(1);
    let hub = k6_hub_ids(&mut core, 1)[0];
    let id = core_id(1);
    let first = core.at(60);
    let fail = |core: &mut Core, at: Instant| {
        ep_sync_clock(core, at);
        core.scheduler
            .vehicle_failed_for_error(id, &ep_status(503), at);
    };
    // Twelve failures spanning 59 minutes: the dozen is reached, the hour is not.
    for step in 0..12u64 {
        fail(
            &mut core,
            first + Duration::from_millis(59 * 60_000 * step / 11),
        );
    }
    assert_eq!(ep_record(&core, 1).consecutive_failures, 12);
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::None);
    // The thirteenth, 61 minutes after the first, pauses the car.
    let paused_at = first + Duration::from_secs(61 * 60);
    fail(&mut core, paused_at);
    let record = ep_record(&core, 1);
    assert_eq!(
        record.hold,
        VehicleHold::Paused(PauseReason::PersistentFailure)
    );
    assert_eq!(record.reason, "paused_persistent_failure");
    assert_eq!(record.next_read_due, None, "a paused car is not read");
    assert_eq!(
        record.next_probe_due,
        Some(paused_at + Duration::from_secs(3_600)),
        "the probe is an hour away"
    );
    let tick = k6_commit_in_memory(&mut core);
    assert_eq!(
        k6_gap_events(&tick, GapKind::VehiclePaused, Some(id)),
        (1, 0),
        "one vehicle_paused gap opens"
    );
    let row = tick
        .vehicles
        .iter()
        .find(|write| write.id == id)
        .expect("the pause is written");
    assert_eq!(
        (
            row.row.hold.as_str(),
            row.row.reason_code.as_str(),
            row.row.vehicle_id
        ),
        ("paused", "paused_persistent_failure", hub)
    );

    // Listed online: the probe is a read, due an hour after the pause and hourly.
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], paused_at);
    let almost = paused_at + Duration::from_millis(3_600_000 - 1);
    assert!(core.scheduler.due_probe_vehicles(almost).is_empty());
    let probe_at = paused_at + Duration::from_secs(3_600);
    assert_eq!(
        core.scheduler.due_probe_vehicles(probe_at),
        vec![(id, ProbeKind::Read)]
    );
    ep_sync_clock(&core, probe_at);
    assert!(
        !core
            .scheduler
            .probe_finished(id, ProbeOutcome::Failed(&ep_status(503)), probe_at)
    );
    assert_eq!(
        ep_record(&core, 1).next_probe_due,
        Some(probe_at + Duration::from_secs(3_600)),
        "a failed probe re-arms the hourly probe"
    );

    // Listed asleep for three hours: nothing is read, and the loop does not spin.
    let mut t = probe_at + Duration::from_secs(1);
    let asleep_until = t + Duration::from_secs(3 * 3_600);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], t);
    while t < asleep_until {
        ep_sync_clock(&core, t);
        assert!(core.scheduler.due_probe_vehicles(t).is_empty());
        assert!(core.scheduler.due_vehicles(t).is_empty());
        if core.scheduler.discovery_due(t) {
            core.scheduler
                .accept_discovery(vec![core_vehicle(1, "asleep")], t);
        } else {
            assert!(core.scheduler.delay_until_next_action(t) > Duration::ZERO);
        }
        t += Duration::from_secs(30);
    }
    // The phase reports moved the car to Asleep, but the stored row still says why it
    // is paused, so a restart brings back the same pause.
    let tick = k6_commit_in_memory(&mut core);
    let row = tick
        .vehicles
        .iter()
        .find(|write| write.id == id)
        .expect("the phase change is written");
    assert_eq!(
        (
            row.row.phase.as_str(),
            row.row.hold.as_str(),
            row.row.reason_code.as_str()
        ),
        ("asleep", "paused", "paused_persistent_failure")
    );
    // A listed change to online makes the probe due at once; the list alone does
    // not clear the pause.
    ep_sync_clock(&core, t);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], t);
    assert_eq!(
        ep_record(&core, 1).hold,
        VehicleHold::Paused(PauseReason::PersistentFailure),
        "a successful list alone does not clear a pause"
    );
    assert_eq!(
        core.scheduler.due_probe_vehicles(t),
        vec![(id, ProbeKind::Read)]
    );
    // Only the car's own answer does: hold None, phase Unknown, and the gap ends.
    assert!(
        !core
            .scheduler
            .probe_finished(id, ProbeOutcome::Read(PollPhase::Online, true), t)
    );
    let record = ep_record(&core, 1);
    assert_eq!(record.hold, VehicleHold::None);
    assert_eq!(record.phase, PollPhase::Unknown);
    assert_eq!(record.reason, "resumed");
    assert_eq!(record.consecutive_failures, 0);
    assert!(!record.in_flight);
    let tick = core.scheduler.take_tick();
    assert_eq!(
        k6_gap_events(&tick, GapKind::VehiclePaused, Some(id)),
        (0, 1),
        "the vehicle_paused gap closes"
    );
}

#[test]
fn a_paused_car_whose_listed_state_is_not_online_never_makes_the_loop_spin() {
    let mut core = ep_online_core(1);
    let id = core_id(1);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(451), core.at(60));
    assert_eq!(
        ep_record(&core, 1).hold,
        VehicleHold::Paused(PauseReason::LegalPrivacy)
    );
    // The probe is due an hour on, but the latest list says asleep: it waits for the
    // list, and the loop's wait is the list's, never zero.
    let mut t = core.at(61);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], t);
    let end = core.at(3 * 3_600);
    while t < end {
        t += Duration::from_secs(5);
        ep_sync_clock(&core, t);
        assert!(core.scheduler.due_probe_vehicles(t).is_empty());
        if core.scheduler.discovery_due(t) {
            core.scheduler
                .accept_discovery(vec![core_vehicle(1, "asleep")], t);
        } else {
            assert!(
                core.scheduler.delay_until_next_action(t) > Duration::ZERO,
                "no busy loop on a past-due probe"
            );
        }
    }
}

#[test]
fn each_pause_reason_follows_its_class_and_only_access_refused_has_no_probe() {
    let reasons = |error: CollectorError, mode: crate::config::CollectorMode| {
        let mut core = Core::new(mode);
        core.scheduler
            .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
        core.scheduler.due_vehicles(core.t0);
        core.scheduler
            .vehicle_succeeded(core_id(1), PollPhase::Online, false, core.t0);
        core.scheduler
            .vehicle_failed_for_error(core_id(1), &error, core.at(60));
        let record = ep_record(&core, 1);
        (record.hold, record.reason, record.next_probe_due.is_some())
    };
    let owner = crate::config::CollectorMode::Legacy;
    let fleet = crate::config::CollectorMode::FleetPolling;
    assert_eq!(
        reasons(ep_fleet_code(401, MOBILE_ACCESS_DISABLED_CODE), fleet),
        (
            VehicleHold::Paused(PauseReason::MobileAccessDisabled),
            "paused_mobile_access_disabled",
            true
        )
    );
    assert_eq!(
        reasons(ep_status(451), owner),
        (
            VehicleHold::Paused(PauseReason::LegalPrivacy),
            "paused_legal_privacy",
            true
        )
    );
    // A Fleet 403 on a vehicle request: no probe, the remedy is the owner's.
    assert_eq!(
        reasons(ep_fleet_status(403), fleet),
        (
            VehicleHold::Paused(PauseReason::AccessRefused),
            "paused_access_refused",
            false
        )
    );
    // An Owner-wire Fleet-named 403 on a read: the same.
    assert_eq!(
        reasons(ep_forbidden(true), owner),
        (
            VehicleHold::Paused(PauseReason::AccessRefused),
            "paused_access_refused",
            false
        )
    );
}

#[test]
fn a_probe_that_finds_the_car_in_service_moves_it_from_pause_to_service() {
    let mut core = ep_online_core(1);
    let id = core_id(1);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(451), core.at(60));
    let probe_at = core.at(60 + 3_600);
    ep_sync_clock(&core, probe_at);
    assert_eq!(
        core.scheduler.due_probe_vehicles(probe_at),
        vec![(id, ProbeKind::Read)]
    );
    let entered = core.scheduler.probe_finished(
        id,
        ProbeOutcome::Failed(&ep_owner(OwnerApiError::VehicleInService)),
        probe_at,
    );
    assert!(entered, "the caller then appends the service_refused event");
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::Service);
    assert_eq!(
        ep_record(&core, 1).next_probe_due,
        Some(probe_at + Duration::from_secs(15 * 60))
    );
}

#[test]
fn probes_share_the_in_flight_cap_with_the_reads_and_wait_for_the_account() {
    // Six cars, read in two batches because the cap is four.
    let mut core = Core::owner();
    core.scheduler.accept_discovery(
        (1..=6).map(|id| core_vehicle(id, "online")).collect(),
        core.t0,
    );
    for _ in 0..2 {
        for id in core.scheduler.due_vehicles(core.t0) {
            core.scheduler
                .vehicle_succeeded(id, PollPhase::Online, false, core.t0);
        }
    }
    for id in 1..=3 {
        core.scheduler
            .vehicle_failed_for_error(core_id(id), &ep_status(451), core.at(60));
    }
    let probe_at = core.at(60 + 3_600);
    ep_sync_clock(&core, probe_at);
    core.scheduler.accept_discovery(
        (1..=6).map(|id| core_vehicle(id, "online")).collect(),
        probe_at,
    );
    // Three probes and then three reads fit the cap of four only in part: the
    // probes are sent first, the reads take what room is left.
    let probes = core.scheduler.due_probe_vehicles(probe_at);
    assert_eq!(probes.len(), 3);
    let reads = core.scheduler.due_vehicles(probe_at);
    assert_eq!(reads.len(), 1, "four requests in flight in all");
    // No probe is due while the account waits.
    let mut waiting = ep_online_core(1);
    waiting
        .scheduler
        .vehicle_failed_for_error(core_id(1), &ep_status(451), waiting.at(60));
    waiting.scheduler.account_failed(
        ErrorClass::RateLimited,
        &ep_rate_limited(7_200),
        waiting.at(61),
    );
    waiting
        .scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], waiting.at(60 + 3_600));
    assert!(
        waiting
            .scheduler
            .due_probe_vehicles(waiting.at(60 + 3_600))
            .is_empty()
    );
}

#[test]
fn the_pause_needs_a_dozen_failures_and_an_hour_each_alone_is_not_enough() {
    // R51 pins both thresholds: a pause is for a fault that outlasted a dozen
    // independent attempts and an hour.
    assert_eq!(K_PAUSE_FAILURES, 12);
    assert_eq!(K_PAUSE_MIN_AGE, Duration::from_secs(60 * 60));
    let mut core = ep_online_core(1);
    k6_hub_ids(&mut core, 1);
    let id = core_id(1);
    let first = core.at(60);
    let fail = |core: &mut Core, at: Instant| {
        ep_sync_clock(core, at);
        core.scheduler
            .vehicle_failed_for_error(id, &ep_status(503), at);
    };
    // Eleven failures over 70 minutes: old enough, one short of a dozen.
    for step in 0..11u64 {
        fail(
            &mut core,
            first + Duration::from_millis(70 * 60_000 * step / 10),
        );
    }
    assert_eq!(ep_record(&core, 1).consecutive_failures, 11);
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::None);
    // The twelfth makes the dozen and, the first being over an hour old, pauses.
    fail(&mut core, first + Duration::from_secs(71 * 60));
    assert_eq!(
        ep_record(&core, 1).hold,
        VehicleHold::Paused(PauseReason::PersistentFailure)
    );
}

// ---- CT37 -----------------------------------------------------------------------

#[test]
fn ct37_owner_wire_a_408_asks_for_the_record_and_the_record_decides() {
    let mut core = ep_online_core(1);
    let id = core_id(1);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(408), t);
    assert_eq!(
        core.scheduler.due_offline_state_vehicles(t),
        vec![id],
        "the record check is due now"
    );
    core.scheduler
        .accept_vehicle_state(id, "asleep".to_owned(), t);
    assert_eq!(ep_record(&core, 1).phase, PollPhase::Asleep);
    assert_eq!(ep_record(&core, 1).consecutive_failures, 0);

    // A record that still says online makes the 408 a failure with back-off.
    let mut core = ep_online_core(1);
    let t = core.at(60);
    core.scheduler.due_vehicles(t);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(408), t);
    core.scheduler
        .accept_vehicle_state(id, "online".to_owned(), t);
    let record = ep_record(&core, 1);
    assert_eq!(record.consecutive_failures, 1);
    assert_eq!(record.last_error, Some(ErrorClass::Transient));
    assert_eq!(
        record.next_read_due,
        Some(t + Duration::from_secs(60)),
        "the interval dominates the first back-off"
    );
}

#[test]
fn ct37_fleet_polling_a_408_makes_the_next_list_due_and_sends_no_record_request() {
    let mut core = Core::fleet();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, core.t0);
    let t = core.at(120);
    ep_sync_clock(&core, t);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_fleet_status(408), t);
    assert!(
        core.scheduler.discovery_due(t),
        "the next list is due at once"
    );
    assert!(
        core.scheduler.due_offline_state_vehicles(t).is_empty(),
        "Fleet polling has no record request"
    );
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], t);
    assert_eq!(ep_record(&core, 1).phase, PollPhase::Asleep);
}

// ---- R53, CT48 ---------------------------------------------------------------------

#[test]
fn ct48_a_car_missing_from_two_lists_is_removed_and_a_later_list_brings_it_back() {
    let mut core = Core::owner();
    let id = core_id(1);
    let cars = |with_one: bool| {
        let mut cars = vec![core_vehicle(2, "asleep")];
        if with_one {
            cars.insert(0, core_vehicle(1, "online"));
        }
        cars
    };
    core.scheduler.accept_discovery(cars(true), core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, core.t0);
    // The stream loop wants a stream for the online car.
    assert!(core.scheduler.streams_to_stop().is_empty());
    assert!(core.scheduler.should_start_stream(id));

    // A read answers 404: the list is due now.
    let t = core.at(60);
    ep_sync_clock(&core, t);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    core.scheduler
        .vehicle_failed_for_error(id, &ep_owner(OwnerApiError::VehicleNotFound), t);
    assert!(core.scheduler.discovery_due(t));
    assert_eq!(ep_record(&core, 1).consecutive_failures, 0, "not a failure");
    // The list without the car: one absence.
    core.scheduler.accept_discovery(cars(false), t);
    assert_eq!(ep_record(&core, 1).missing_lists, 1);
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::None);
    // A list 300 s later without it: removed, its stream stops.
    let later = t + Duration::from_secs(300);
    ep_sync_clock(&core, later);
    core.scheduler.accept_discovery(cars(false), later);
    let record = ep_record(&core, 1);
    assert_eq!(record.hold, VehicleHold::Removed);
    assert_eq!(record.reason, "vehicle_removed");
    assert_eq!(core.scheduler.streams_to_stop(), vec![id]);
    assert!(!core.scheduler.should_start_stream(id));
    assert!(core.scheduler.due_vehicles(later).is_empty());
    assert_eq!(
        core.scheduler.status(later).vehicles[0].detail,
        Some("not in the Tesla account")
    );
    // A later list that names it: Unknown, and the first-contact read is due.
    let back = later + Duration::from_secs(30);
    ep_sync_clock(&core, back);
    core.scheduler.accept_discovery(cars(true), back);
    let record = ep_record(&core, 1);
    assert_eq!(record.hold, VehicleHold::None);
    assert_eq!(record.phase, PollPhase::Unknown);
    assert_eq!(record.reason, "first_contact_read");
    assert_eq!(core.scheduler.due_vehicles(back), vec![id]);
}

#[test]
fn two_absences_count_only_an_inventory_interval_apart_unless_a_404_asked_for_the_second() {
    let mut core = Core::owner();
    let id = core_id(1);
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "asleep")],
        core.t0,
    );
    let without = |core: &mut Core, at: u64| {
        let t = core.at(at);
        ep_sync_clock(core, t);
        core.scheduler
            .accept_discovery(vec![core_vehicle(2, "asleep")], t);
    };
    without(&mut core, 10);
    assert_eq!(ep_record(&core, 1).missing_lists, 1);
    // A list 30 s later is a routine state check, not a second confirmation.
    without(&mut core, 40);
    assert_eq!(ep_record(&core, 1).missing_lists, 1);
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::None);
    // A list that a 404 asked for counts at once, whatever the spacing.
    core.scheduler.vehicles.get_mut(&id).unwrap().in_flight = true;
    core.scheduler.vehicle_failed_for_error(
        id,
        &ep_owner(OwnerApiError::VehicleNotFound),
        core.at(50),
    );
    without(&mut core, 50);
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::Removed);

    // A list that names the car resets its absences.
    let mut core = Core::owner();
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "asleep")],
        core.t0,
    );
    without(&mut core, 10);
    assert_eq!(ep_record(&core, 1).missing_lists, 1);
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "asleep")],
        core.at(20),
    );
    assert_eq!(ep_record(&core, 1).missing_lists, 0);
    assert_eq!(ep_record(&core, 1).missing_last, None);
}

#[test]
fn a_removed_car_is_persisted_and_comes_back_removed_after_a_restart() {
    let mut core = Core::owner();
    let hub = {
        core.scheduler.accept_discovery(
            vec![core_vehicle(1, "online"), core_vehicle(2, "asleep")],
            core.t0,
        );
        k6_hub_ids(&mut core, 2)[0]
    };
    for at in [10, 400] {
        let t = core.at(at);
        ep_sync_clock(&core, t);
        core.scheduler
            .accept_discovery(vec![core_vehicle(2, "asleep")], t);
    }
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::Removed);
    let tick = core.scheduler.take_tick();
    let row = tick
        .vehicles
        .iter()
        .find(|write| write.row.vehicle_id == hub)
        .expect("the removal is written");
    assert_eq!(
        (row.row.hold.as_str(), row.row.reason_code.as_str()),
        ("removed", "vehicle_removed")
    );
}

// ---- CT57 ---------------------------------------------------------------------------

#[test]
fn ct57_a_disabled_car_is_never_requested_listed_or_stored_and_enabling_restarts_it() {
    let mut core = Core::owner();
    let id = core_id(1);
    let hub = Uuid::new_v4();
    let mut settings = crate::hub_pack::ProjectionCarSettings::default();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, core.t0);
    assert!(core.scheduler.streams_to_stop().is_empty());
    settings.enabled = false;
    let t = core.at(1);
    let disconnect = core
        .scheduler
        .apply_control_settings(&[(hub, 1, settings.clone())], t);
    assert_eq!(disconnect, vec![id], "its stream is disconnected");
    // Ten minutes: no request, no discovery observation, no stored frame.
    let mut step = t;
    for _ in 0..120 {
        step += Duration::from_secs(5);
        ep_sync_clock(&core, step);
        assert!(core.scheduler.due_vehicles(step).is_empty());
        assert!(core.scheduler.due_probe_vehicles(step).is_empty());
        assert!(core.scheduler.due_offline_state_vehicles(step).is_empty());
        if core.scheduler.discovery_due(step) {
            let mut disabled = core_vehicle(1, "online");
            disabled.settings = settings.clone();
            let candidates = core.scheduler.accept_discovery(vec![disabled], step);
            assert!(candidates.is_empty(), "no discovery observation");
        }
        assert!(
            !core
                .scheduler
                .should_persist_stream_telemetry(id, Some(5), step),
            "no frame is stored"
        );
    }
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::Disabled);
    // Enabling it again: Unknown, and its first contact follows.
    settings.enabled = true;
    let enabled_at = step + Duration::from_secs(1);
    ep_sync_clock(&core, enabled_at);
    core.scheduler
        .apply_control_settings(&[(hub, 1, settings.clone())], enabled_at);
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::None);
    assert_eq!(ep_record(&core, 1).phase, PollPhase::Unknown);
    assert!(core.scheduler.discovery_due(enabled_at));
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], enabled_at);
    assert_eq!(core.scheduler.due_vehicles(enabled_at), vec![id]);
}

// ---- CT47 ---------------------------------------------------------------------------

#[tokio::test]
async fn ct47_owner_wire_a_405_in_service_stores_one_event_and_holds_the_car() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            let hub = bed.store_open_drive().await;
            let id = core_id(1);
            let w0 = CORE_W0;
            let observation = bed.last_observation_id(hub);
            assert!(
                bed.store
                    .collector_lifecycle_snapshot(hub)
                    .unwrap()
                    .drive_open
            );
            // The car is online and read in use; a read then answers 405 in service.
            bed.scheduler.due_vehicles(bed.now());
            bed.scheduler
                .vehicle_succeeded(id, PollPhase::Online, false, bed.now());
            assert!(bed.scheduler.streams_to_stop().is_empty());
            bed.advance(Duration::from_secs(60));
            let now = bed.now();
            let wall = bed.wall();
            bed.scheduler.begin_tick();
            assert_eq!(bed.scheduler.due_vehicles(now), vec![id]);
            let effect = bed.scheduler.vehicle_failed_for_error(
                id,
                &ep_owner(OwnerApiError::VehicleInService),
                now,
            );
            assert_eq!(effect, FailureEffect::ServiceEntered);
            // The loop's helper appends the Hub event to the tick's batch.
            let mut batch = TickBatch::default();
            {
                let commit = TickCommit {
                    store: &bed.store,
                    cursor_key: &bed.cursor_key,
                    provider: bed.provider,
                    offline_drive_timeout: Duration::from_secs(600),
                };
                let mut run = TickRun {
                    scheduler: &mut bed.scheduler,
                    identities: &mut bed.identities,
                    commit,
                    batch: TickBatch::default(),
                    terrain_wake: &bed.terrain,
                    any_wrote: false,
                };
                push_service_refused_event(&mut run, id);
                batch.hub_events = std::mem::take(&mut run.batch.hub_events);
            }
            assert_eq!(batch.hub_events.len(), 1);
            assert_eq!(batch.hub_events[0].event, "service_refused");
            assert_eq!(batch.hub_events[0].observed_at_ms, wall);
            batch.first_result_at = Some(now);
            let commits = seam.tick_commits();
            bed.commit(batch).await;
            assert_eq!(seam.tick_commits(), commits + 1, "one transaction");
            // C1 took the event through its intake: one observation more, the open
            // drive ended at its last evidence and the refusal flag is set.
            assert_eq!(bed.last_observation_id(hub), observation + 1);
            let state = crate::lifecycle::OpenSessionState::decode(
                &bed.store
                    .load_lifecycle_state(hub)
                    .unwrap()
                    .unwrap()
                    .open_session_json,
            )
            .unwrap();
            assert_eq!(state.service_mode, Some(true));
            assert!(state.service_mode_refused);
            let drives = bed.store.materialised_history(hub).unwrap().drives;
            assert_eq!(drives.len(), 1);
            assert_eq!(drives[0].end_date_ms, w0, "the drive ends at its last fold");
            assert_eq!(bed.hub_event_current_rows(), 0);
            // The hold is stored with its probe deadline.
            let row = bed.store.load_collector_state_rows().unwrap().vehicles;
            assert_eq!(row[0].hold, "service");
            assert_eq!(row[0].reason_code, "service_entered");
            assert_eq!(row[0].next_probe_ms, Some(wall + 15 * 60_000));
            assert_eq!(bed.scheduler.vehicles[&id].hold, VehicleHold::Service);
            // The car's stream is kept while it is listed online, and a frame
            // received during the hold is stored.
            assert!(bed.scheduler.should_start_stream(id));
            assert!(
                bed.scheduler
                    .should_persist_stream_telemetry(id, Some(3), bed.now())
            );
            // The probe is a non-waking record request every 15 minutes.
            assert!(
                bed.scheduler
                    .due_probe_vehicles(now + Duration::from_secs(14 * 60 + 59))
                    .is_empty()
            );
            let probe_at = now + Duration::from_secs(15 * 60);
            bed.advance(Duration::from_secs(15 * 60));
            assert_eq!(
                bed.scheduler.due_probe_vehicles(probe_at),
                vec![(id, ProbeKind::ServiceRecord)]
            );
            // Still in service: the next probe is 15 minutes on.
            assert!(
                !bed.scheduler
                    .probe_finished(id, ProbeOutcome::InService(true), probe_at)
            );
            assert_eq!(
                bed.scheduler.vehicles[&id].next_probe_due,
                Some(probe_at + Duration::from_secs(15 * 60))
            );
            // Out of service: hold None, phase Unknown, the car read at once.
            let exit_at = probe_at + Duration::from_secs(15 * 60);
            bed.advance(Duration::from_secs(15 * 60));
            assert_eq!(
                bed.scheduler.due_probe_vehicles(exit_at),
                vec![(id, ProbeKind::ServiceRecord)]
            );
            bed.scheduler
                .probe_finished(id, ProbeOutcome::InService(false), exit_at);
            let record = &bed.scheduler.vehicles[&id];
            assert_eq!(record.hold, VehicleHold::None);
            assert_eq!(record.phase, PollPhase::Unknown);
            assert_eq!(record.reason, "service_exited");
            assert_eq!(record.next_read_due, Some(exit_at));
            assert_eq!(bed.scheduler.due_vehicles(exit_at), vec![id]);
        })
        .await;
}

#[test]
fn ct47_fleet_polling_a_405_in_service_holds_the_car_and_probes_only_while_listed_online() {
    let mut core = Core::fleet();
    let id = core_id(1);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(id, PollPhase::Online, false, core.t0);
    let t = core.at(120);
    ep_sync_clock(&core, t);
    assert_eq!(core.scheduler.due_vehicles(t), vec![id]);
    let effect = core.scheduler.vehicle_failed_for_error(
        id,
        &ep_fleet_code(405, VEHICLE_IN_SERVICE_CODE),
        t,
    );
    assert_eq!(effect, FailureEffect::ServiceEntered);
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::Service);
    // At +15 min the latest list says asleep: no read.
    let probe_at = t + Duration::from_secs(15 * 60);
    ep_sync_clock(&core, probe_at);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "asleep")], probe_at);
    assert!(core.scheduler.due_probe_vehicles(probe_at).is_empty());
    // With the list online: one probe read.
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], probe_at);
    assert_eq!(
        core.scheduler.due_probe_vehicles(probe_at),
        vec![(id, ProbeKind::Read)]
    );
    // 405 again: the next probe is 15 minutes on.
    assert!(!core.scheduler.probe_finished(
        id,
        ProbeOutcome::Failed(&ep_fleet_code(405, VEHICLE_IN_SERVICE_CODE)),
        probe_at
    ));
    assert_eq!(ep_record(&core, 1).hold, VehicleHold::Service);
    assert_eq!(
        ep_record(&core, 1).next_probe_due,
        Some(probe_at + Duration::from_secs(15 * 60))
    );
    // A 200: the hold is cleared and the snapshot's phase applies.
    let exit_at = probe_at + Duration::from_secs(15 * 60);
    ep_sync_clock(&core, exit_at);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], exit_at);
    assert_eq!(
        core.scheduler.due_probe_vehicles(exit_at),
        vec![(id, ProbeKind::Read)]
    );
    core.scheduler
        .probe_finished(id, ProbeOutcome::Read(PollPhase::Online, false), exit_at);
    let record = ep_record(&core, 1);
    assert_eq!(record.hold, VehicleHold::None);
    assert_eq!(record.phase, PollPhase::Online, "the snapshot's phase");
    assert_eq!(
        record.next_read_due,
        Some(exit_at + Duration::from_secs(120)),
        "the next read is at the Fleet Online interval"
    );
}

#[test]
fn a_snapshot_with_the_service_flag_set_creates_no_hold() {
    let mut core = ep_online_core(1);
    let id = core_id(1);
    let snapshot = snapshot_with(&[("vehicle_state", "service_mode", json!(true))]);
    core.scheduler
        .vehicle_succeeded(id, poll_phase(&snapshot), false, core.at(60));
    let record = ep_record(&core, 1);
    assert_eq!(record.hold, VehicleHold::None);
    assert_eq!(record.phase, PollPhase::Online);
    assert!(record.next_read_due.is_some(), "reads continue (R52)");
}

// ---- CT65 ----------------------------------------------------------------------------

#[test]
fn ct65_fleet_a_403_on_one_cars_request_pauses_only_that_car() {
    let mut core = Core::fleet();
    core.scheduler.accept_discovery(
        vec![core_vehicle(1, "online"), core_vehicle(2, "online")],
        core.t0,
    );
    let hubs = k6_hub_ids(&mut core, 2);
    assert_eq!(core.scheduler.due_vehicles(core.t0).len(), 2);
    for id in 1..=2 {
        core.scheduler
            .vehicle_succeeded(core_id(id), PollPhase::Online, false, core.t0);
    }
    let t = core.at(120);
    ep_sync_clock(&core, t);
    assert_eq!(core.scheduler.due_vehicles(t).len(), 2);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_fleet_status(403), t);
    core.scheduler
        .vehicle_succeeded(core_id(2), PollPhase::Online, false, t);
    let record = ep_record(&core, 1);
    assert_eq!(record.hold, VehicleHold::Paused(PauseReason::AccessRefused));
    assert_eq!(record.next_probe_due, None);
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);
    let tick = k6_commit_in_memory(&mut core);
    assert_eq!(
        k6_gap_events(&tick, GapKind::VehiclePaused, Some(core_id(1))),
        (1, 0),
        "one vehicle_paused gap"
    );
    let row = tick
        .vehicles
        .iter()
        .find(|write| write.row.vehicle_id == hubs[0])
        .expect("the pause is written");
    assert_eq!(row.row.reason_code, "paused_access_refused");
    // No probe over three hours, while car 2 is read at its interval.
    let mut reads_of_two = 0;
    let mut step = t;
    while step < t + Duration::from_secs(3 * 3_600) {
        step += Duration::from_secs(30);
        ep_sync_clock(&core, step);
        if core.scheduler.discovery_due(step) {
            core.scheduler.accept_discovery(
                vec![core_vehicle(1, "online"), core_vehicle(2, "online")],
                step,
            );
        }
        assert!(core.scheduler.due_probe_vehicles(step).is_empty());
        for id in core.scheduler.due_vehicles(step) {
            assert_eq!(id, core_id(2), "the paused car is never read");
            reads_of_two += 1;
            core.scheduler
                .vehicle_succeeded(id, PollPhase::Online, false, step);
        }
    }
    assert_eq!(reads_of_two, 90, "one read of car 2 every 120 s for 3 h");
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);

    // A 403 on the list is the account's: FleetAccessRefused, nothing for an hour.
    let mut core = Core::fleet();
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, false, core.t0);
    let t = core.at(30);
    core.scheduler
        .discovery_failed_for_error(&ep_fleet_status(403), t);
    assert_eq!(
        core.scheduler.account.state,
        AccountState::FleetAccessRefused
    );
    let mut step = t;
    while step < t + Duration::from_secs(3_600) {
        step += Duration::from_secs(5);
        ep_sync_clock(&core, step);
        assert!(!core.scheduler.discovery_due(step));
        assert!(core.scheduler.due_vehicles(step).is_empty());
        assert!(core.scheduler.due_probe_vehicles(step).is_empty());
    }
}

// ---- R51 and the interplay with the loop's tick --------------------------------------

#[test]
fn a_pause_entered_in_a_tick_whose_commit_fails_is_dropped() {
    let mut core = ep_online_core(1);
    let id = core_id(1);
    k6_hub_ids(&mut core, 1);
    core.scheduler.begin_tick();
    core.scheduler
        .vehicle_failed_for_error(id, &ep_status(451), core.at(60));
    assert_eq!(
        ep_record(&core, 1).hold,
        VehicleHold::Paused(PauseReason::LegalPrivacy)
    );
    let tick = core.scheduler.take_tick();
    core.scheduler.commit_failed(tick, core.at(61));
    assert_eq!(
        ep_record(&core, 1).hold,
        VehicleHold::None,
        "memory returns"
    );
    assert_eq!(ep_record(&core, 1).next_probe_due, None);
}

#[test]
fn a_restored_access_refused_pause_has_no_probe_and_a_restart_rewrites_no_row_for_a_held_car() {
    let (bed, hub) = bed_with_stored_car();
    write_collector_rows(
        &bed.store,
        &crate::db::CollectorRowsWrite {
            now_ms: CORE_W0,
            vehicles: vec![stored_vehicle_row(
                hub,
                "unknown",
                "paused",
                "paused_access_refused",
            )],
            ..Default::default()
        },
    );
    let (mut scheduler, _clock, t) =
        restarted_scheduler(crate::config::CollectorMode::Legacy, CORE_W0 + 60_000);
    scheduler.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
    scheduler.accept_discovery(vec![core_vehicle(1, "online")], t);
    let car = &scheduler.vehicles[&core_id(1)];
    assert_eq!(car.hold, VehicleHold::Paused(PauseReason::AccessRefused));
    assert_eq!(car.next_probe_due, None, "a restart invents no probe");
    assert!(
        scheduler
            .due_probe_vehicles(t + Duration::from_secs(3 * 3_600))
            .is_empty()
    );
    assert!(
        scheduler.take_tick().vehicles.is_empty(),
        "the held car's row is already what the store holds"
    );
}
