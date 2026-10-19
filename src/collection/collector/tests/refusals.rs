// SPDX-License-Identifier: AGPL-3.0-only

// Spec tests of C3 part K6 (refusals, holds and owner actions), the refusal half:
// CT42 to CT46, CT72, the CT56 log scenarios of CT36, CT42 and CT47, and the
// owner-action loop of CT66 and CT73 end to end. The hold half is in
// `holds_and_actions.rs`. The credential unit (C5) has no types in the tree yet, so
// its outcomes enter through `CredentialOutcome`, `CredentialStateSeen` and
// `RefusalCheck`, which these tests double. Synthetic data only; the one fake HTTP
// server is in-process on loopback.

impl TickBed {
    /// One tick whose only event is a transition of the scheduler itself.
    async fn transition_tick(
        &mut self,
        act: impl FnOnce(&mut VehicleScheduler, Instant),
    ) -> TickOutcome {
        let now = self.now();
        self.scheduler.begin_tick();
        act(&mut self.scheduler, now);
        self.commit(TickBatch::default()).await
    }

    /// A bed with `cars` cars listed online and read in use, so each is Online with
    /// a stream wanted and no idle window running.
    async fn k6_online_bed(mode: crate::config::CollectorMode, cars: u64) -> TickBed {
        let mut bed = TickBed::new(mode);
        bed.list((1..=cars).map(|id| core_vehicle(id, "online")).collect())
            .await;
        let now = bed.now();
        bed.scheduler.begin_tick();
        for id in bed.scheduler.due_vehicles(now) {
            bed.scheduler
                .vehicle_succeeded(id, PollPhase::Online, false, now);
        }
        bed.commit(TickBatch::default()).await;
        assert!(bed.scheduler.streams_to_stop().is_empty());
        bed
    }

    /// The stored account row, if any.
    fn k6_account_row(&self) -> Option<crate::db::CollectorAccountRow> {
        self.store
            .load_collector_state_rows()
            .expect("rows")
            .account
    }

    /// The kinds of the gaps the store holds open.
    fn k6_open_gap_kinds(&self) -> Vec<String> {
        let mut kinds: Vec<String> = self
            .store
            .collection_gaps(50)
            .expect("gaps")
            .into_iter()
            .filter(|gap| gap.ended_at_ms.is_none())
            .map(|gap| gap.kind)
            .collect();
        kinds.sort();
        kinds
    }

    /// The readiness failure the collector tables give now.
    fn k6_readiness(&self) -> Option<crate::db::CollectionAttentionCode> {
        self.store
            .collector_readiness_at(self.wall())
            .expect("readiness")
            .failure
    }
}

fn k6_online_cars(cars: u64) -> Vec<Vehicle> {
    (1..=cars).map(|id| core_vehicle(id, "online")).collect()
}

/// The list of C3 R48 (c) with scripted answers and a doubled credential unit:
/// the checked list, the number of list requests and the number of refresh asks.
async fn k6_checked_list(
    owner_wire: bool,
    answers: Vec<Result<Vec<Vehicle>, CollectorError>>,
    check: RefusalCheck,
) -> (CheckedList, usize, usize) {
    let answers = std::sync::Mutex::new(std::collections::VecDeque::from(answers));
    let list_calls = AtomicUsize::new(0);
    let check_calls = AtomicUsize::new(0);
    let checked = list_with_refusal_check_using(
        owner_wire,
        || {
            list_calls.fetch_add(1, Ordering::SeqCst);
            let next = answers
                .lock()
                .expect("scripted answers")
                .pop_front()
                .expect("a scripted answer for every request");
            async move { next }
        },
        || {
            check_calls.fetch_add(1, Ordering::SeqCst);
            async move { check }
        },
    )
    .await;
    (
        checked,
        list_calls.load(Ordering::SeqCst),
        check_calls.load(Ordering::SeqCst),
    )
}

/// What the Owner loop does with a checked list (C3 R48 c, R57): the list's cars
/// join the scheduler, or the failure is classified and the account's state follows.
/// Returns the delay a failed list waits.
fn k6_apply_checked_list(
    scheduler: &mut VehicleScheduler,
    checked: CheckedList,
    at: Instant,
) -> Option<Duration> {
    match checked.result {
        Ok(vehicles) => {
            scheduler.accept_discovery(vehicles, at);
            None
        }
        Err(_) if checked.refused_after_check => Some(scheduler.list_refused_after_check(at)),
        Err(error) => Some(scheduler.discovery_failed_for_error(&error, at)),
    }
}

/// The scheduler's accounting after a refusal: nothing is due over `minutes`.
fn k6_assert_silent_for(bed: &mut TickBed, minutes: u64) {
    let mut seen = 0;
    while seen < minutes * 12 {
        bed.advance(Duration::from_secs(5));
        let now = bed.now();
        assert!(!bed.scheduler.discovery_due(now));
        assert!(bed.scheduler.due_vehicles(now).is_empty());
        assert!(bed.scheduler.due_probe_vehicles(now).is_empty());
        assert!(bed.scheduler.due_offline_state_vehicles(now).is_empty());
        seen += 1;
    }
}

// ---- CT42 ------------------------------------------------------------------------------------

#[tokio::test]
async fn ct42_a_fleet_named_403_on_the_list_refuses_the_owner_api_at_once() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::k6_online_bed(crate::config::CollectorMode::Legacy, 2).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            bed.advance(Duration::from_secs(300));
            let now = bed.now();
            assert!(bed.scheduler.discovery_due(now));
            // The list answers 403 with a body naming the Fleet API: no refresh is
            // asked and the list is not resent.
            let (checked, list_calls, check_calls) =
                k6_checked_list(true, vec![Err(ep_forbidden(true))], RefusalCheck::Refreshed).await;
            assert_eq!((list_calls, check_calls), (1, 0), "no refresh, no resend");
            bed.scheduler.begin_tick();
            let delay = k6_apply_checked_list(&mut bed.scheduler, checked, now).expect("failed");
            assert!(delay <= CONTROL_SETTINGS_REFRESH);
            let commits = seam.tick_commits();
            bed.commit(TickBatch::default()).await;
            assert_eq!(seam.tick_commits(), commits + 1, "in the tick transaction");
            // One account row, in the tick transaction.
            let row = bed.k6_account_row().expect("the refusal is stored");
            assert_eq!(
                (
                    row.state.as_str(),
                    row.reason_code.as_str(),
                    row.mode.as_str()
                ),
                ("owner_api_refused", "owner_api_refused", "legacy")
            );
            assert_eq!(bed.scheduler.account.state, AccountState::OwnerApiRefused);
            // No further request over ten minutes.
            k6_assert_silent_for(&mut bed, 10);
            // The streams stopped, one gap opened and readiness says so.
            assert_eq!(
                bed.scheduler.streams_to_stop(),
                vec![core_id(1), core_id(2)]
            );
            assert!(!bed.scheduler.should_start_stream(core_id(1)));
            assert_eq!(bed.k6_open_gap_kinds(), vec!["owner_api_refused"]);
            assert_eq!(
                bed.k6_readiness(),
                Some(crate::db::CollectionAttentionCode::CollectorApiRefused)
            );
            // The status says Tesla refused the Owner API, never "credentials".
            let status = bed.scheduler.status(bed.now());
            assert_eq!(status.account_state, AccountState::OwnerApiRefused);
            assert!(status.account_text.contains("refused the Owner API"));
            assert!(!status.account_text.to_lowercase().contains("credential"));
            // A reload keeps the refusal, sends nothing, and the row is what the
            // credential unit restores its `api_refused` from (C5 R57 step 6).
            let wall = bed.wall();
            let (mut reloaded, _clock, t) =
                restarted_scheduler(crate::config::CollectorMode::Legacy, wall);
            reloaded.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
            assert_eq!(reloaded.account.state, AccountState::OwnerApiRefused);
            assert!(!reloaded.discovery_due(t + Duration::from_secs(3_600)));
            assert!(reloaded.due_vehicles(t).is_empty());
            let stored = bed.k6_account_row().expect("row after reload");
            assert_eq!(stored.state, "owner_api_refused");
            assert_eq!(stored.reason_code, "owner_api_refused");
        })
        .await;
}

#[tokio::test]
async fn ct42_a_plain_403_on_the_list_refuses_only_after_one_refresh_and_one_resend() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            // The resent list answers 403 again: refused, with the same row.
            let mut bed = TickBed::k6_online_bed(crate::config::CollectorMode::Legacy, 2).await;
            bed.advance(Duration::from_secs(300));
            let now = bed.now();
            let (checked, list_calls, check_calls) = k6_checked_list(
                true,
                vec![Err(ep_forbidden(false)), Err(ep_forbidden(false))],
                RefusalCheck::Refreshed,
            )
            .await;
            assert_eq!(
                (list_calls, check_calls),
                (2, 1),
                "one refresh, the list resent once"
            );
            assert!(checked.refused_after_check);
            bed.scheduler.begin_tick();
            k6_apply_checked_list(&mut bed.scheduler, checked, now);
            bed.commit(TickBatch::default()).await;
            let row = bed.k6_account_row().expect("the refusal is stored");
            assert_eq!(
                (row.state.as_str(), row.reason_code.as_str()),
                ("owner_api_refused", "owner_api_refused")
            );

            // The resent list answers 200: collecting, no row, the list processed.
            let mut bed = TickBed::k6_online_bed(crate::config::CollectorMode::Legacy, 2).await;
            bed.advance(Duration::from_secs(300));
            let now = bed.now();
            let (checked, list_calls, check_calls) = k6_checked_list(
                true,
                vec![Err(ep_forbidden(false)), Ok(k6_online_cars(2))],
                RefusalCheck::Refreshed,
            )
            .await;
            assert_eq!((list_calls, check_calls), (2, 1));
            assert!(!checked.refused_after_check);
            bed.scheduler.begin_tick();
            assert!(k6_apply_checked_list(&mut bed.scheduler, checked, now).is_none());
            assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
            assert!(bed.scheduler.take_tick().account.is_none(), "no row");

            // The refresh fails (the credential unit's authentication outcome): an
            // authentication wait, no refusal and no resend.
            let mut bed = TickBed::k6_online_bed(crate::config::CollectorMode::Legacy, 2).await;
            bed.advance(Duration::from_secs(300));
            let now = bed.now();
            let (checked, list_calls, check_calls) = k6_checked_list(
                true,
                vec![Err(ep_forbidden(false))],
                RefusalCheck::Failed(ep_owner(OwnerApiError::CredentialAuthorityUnavailable)),
            )
            .await;
            assert_eq!(
                (list_calls, check_calls),
                (1, 1),
                "refreshed once, no resend"
            );
            bed.scheduler.begin_tick();
            k6_apply_checked_list(&mut bed.scheduler, checked, now);
            assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
            assert!(
                bed.scheduler
                    .account
                    .wait
                    .is_some_and(|(_, reason)| { reason == WaitReason::AuthWait })
            );
            assert!(bed.scheduler.take_tick().account.is_none());
        })
        .await;
}

#[test]
fn ct42_an_observer_or_an_access_token_only_setup_never_refreshes_and_never_refuses_on_a_plain_403()
{
    let observer = |check: RefusalCheck| {
        let mut core = Core::owner();
        core.scheduler.accept_discovery(k6_online_cars(2), core.t0);
        let at = core.at(300);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        let (checked, list_calls, _) =
            runtime.block_on(k6_checked_list(true, vec![Err(ep_forbidden(false))], check));
        assert_eq!(list_calls, 1, "no resend");
        assert!(!checked.refused_after_check);
        k6_apply_checked_list(&mut core.scheduler, checked, at);
        core
    };
    for check in [
        RefusalCheck::ObserverRefreshDisabled,
        RefusalCheck::RefreshNotPossible,
    ] {
        let core = observer(check);
        // A client error of the list with the account's back-off; no state, no row.
        assert_eq!(core.scheduler.account.state, AccountState::Collecting);
        assert_eq!(core.scheduler.account.discovery_failures, 1);
        assert!(core.scheduler.account.list_floor.is_some());
    }
    // A Fleet-named 403 on the list still refuses in observer mode, with no ask.
    let mut core = Core::owner();
    core.scheduler.accept_discovery(k6_online_cars(1), core.t0);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let (checked, list_calls, check_calls) = runtime.block_on(k6_checked_list(
        true,
        vec![Err(ep_forbidden(true))],
        RefusalCheck::ObserverRefreshDisabled,
    ));
    assert_eq!((list_calls, check_calls), (1, 0));
    let at = core.at(300);
    k6_apply_checked_list(&mut core.scheduler, checked, at);
    assert_eq!(core.scheduler.account.state, AccountState::OwnerApiRefused);
    // Fleet polling asks for nothing at all: the rule is the Owner wire's.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    let (checked, list_calls, check_calls) = runtime.block_on(k6_checked_list(
        false,
        vec![Err(ep_forbidden(false))],
        RefusalCheck::Refreshed,
    ));
    assert_eq!((list_calls, check_calls), (1, 0));
    assert!(checked.result.is_err());
}

#[test]
fn ct42_a_cars_own_403_is_vehicle_scope_and_the_list_decides() {
    // (d) Fleet-named 403 on car 1's read: Paused(AccessRefused), no refresh, one
    // list due now, car 2 still read, the account collecting.
    let mut core = ep_online_core(2);
    k6_hub_ids(&mut core, 2);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t).len(), 2);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_forbidden(true), t);
    core.scheduler
        .vehicle_succeeded(core_id(2), PollPhase::Online, false, t);
    assert_eq!(
        ep_record(&core, 1).hold,
        VehicleHold::Paused(PauseReason::AccessRefused)
    );
    assert_eq!(ep_record(&core, 1).next_probe_due, None, "no probe");
    assert_eq!(
        core.scheduler.account.list_wanted,
        Some(t),
        "one list is due at once (refusal_check)"
    );
    assert!(core.scheduler.discovery_due(t));
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);
    assert!(
        core.scheduler
            .due_vehicles(t + Duration::from_secs(60))
            .contains(&core_id(2))
    );
    // The list answers 200: nothing more changes.
    core.scheduler.accept_discovery(k6_online_cars(2), t);
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);
    // (e) Car 2's read then gets a Fleet-named 403 too: every car is refused, so
    // the account is OwnerApiRefused although the list still answers.
    let t2 = t + Duration::from_secs(60);
    ep_sync_clock(&core, t2);
    core.scheduler
        .vehicle_failed_for_error(core_id(2), &ep_forbidden(true), t2);
    assert_eq!(core.scheduler.account.state, AccountState::OwnerApiRefused);
    assert_eq!(core.scheduler.account.state_reason, "owner_api_refused");
    let tick = core.scheduler.take_tick();
    let row = tick
        .account
        .as_ref()
        .expect("the row is written")
        .row
        .clone();
    assert_eq!(
        (row.state.as_str(), row.reason_code.as_str()),
        ("owner_api_refused", "owner_api_refused")
    );
    assert_eq!(k6_gap_events(&tick, GapKind::OwnerApiRefused, None), (1, 0));

    // A plain 403 on car 1's read with the list answering 200: a client error of
    // that car only, no refresh asked, car 2 unaffected, the list due now.
    let mut core = ep_online_core(2);
    let t = core.at(60);
    core.scheduler.due_vehicles(t);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_forbidden(false), t);
    core.scheduler
        .vehicle_succeeded(core_id(2), PollPhase::Online, false, t);
    let record = ep_record(&core, 1);
    assert_eq!(record.hold, VehicleHold::None);
    assert_eq!(record.consecutive_failures, 1);
    assert_eq!(record.last_error, Some(ErrorClass::ClientError));
    assert_eq!(ep_record(&core, 2).consecutive_failures, 0);
    assert!(core.scheduler.discovery_due(t));
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);

    // Only the Owner wire turns a 403 into a list check: a Fleet 403 on a car's read
    // pauses it and asks for nothing.
    let mut core = Core::fleet();
    core.scheduler.accept_discovery(k6_online_cars(2), core.t0);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, false, core.t0);
    core.scheduler
        .vehicle_succeeded(core_id(2), PollPhase::Online, false, core.t0);
    let t = core.at(120);
    core.scheduler.due_vehicles(t);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_fleet_status(403), t);
    assert!(!core.scheduler.discovery_due(t));
}

#[test]
fn ct42_f_the_fleet_naming_test_reads_the_one_rule() {
    use crate::owner_api::is_fleet_api_named;
    assert!(is_fleet_api_named(
        br#"{"error":"Endpoint deprecated; use the Fleet API"}"#
    ));
    assert!(is_fleet_api_named(
        br#"{"error":"x","error_description":"use fleet-api"}"#
    ));
    assert!(is_fleet_api_named(
        b"see developer.tesla.com/docs/fleet-api"
    ));
    assert!(!is_fleet_api_named(br#"{"error":"forbidden"}"#));
}

// ---- CT43, CT45 -------------------------------------------------------------------------------

#[tokio::test]
async fn ct43_a_410_refuses_the_owner_api_stores_its_row_and_survives_a_restart() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::k6_online_bed(crate::config::CollectorMode::Legacy, 2).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            bed.advance(Duration::from_secs(60));
            let now = bed.now();
            bed.scheduler.begin_tick();
            assert_eq!(bed.scheduler.due_vehicles(now).len(), 2);
            // Both reads answer 410: the first refuses, the second changes nothing.
            for id in 1..=2 {
                bed.scheduler.vehicle_failed_for_error(
                    core_id(id),
                    &ep_owner(OwnerApiError::Gone),
                    now,
                );
            }
            bed.commit(TickBatch::default()).await;
            let row = bed.k6_account_row().expect("the refusal is stored");
            assert_eq!(
                (row.state.as_str(), row.reason_code.as_str()),
                ("owner_api_refused", "owner_api_gone")
            );
            assert_eq!(
                bed.k6_readiness(),
                Some(crate::db::CollectionAttentionCode::CollectorApiRefused)
            );
            assert_eq!(bed.k6_open_gap_kinds(), vec!["owner_api_refused"]);
            k6_assert_silent_for(&mut bed, 10);
            // A reload: still refused, nothing due.
            let wall = bed.wall();
            let (mut reloaded, _clock, t) =
                restarted_scheduler(crate::config::CollectorMode::Legacy, wall);
            reloaded.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
            assert_eq!(reloaded.account.state, AccountState::OwnerApiRefused);
            assert_eq!(reloaded.account.state_reason, "owner_api_gone");
            assert!(!reloaded.discovery_due(t + Duration::from_secs(3_600)));
        })
        .await;
}

#[tokio::test]
async fn ct45_a_mode_change_resets_the_refusal_and_an_owner_retry_checks_again() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::k6_online_bed(crate::config::CollectorMode::Legacy, 1).await;
            let hub = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            bed.advance(Duration::from_secs(60));
            bed.transition_tick(|scheduler, now| {
                scheduler.discovery_failed_for_error(&ep_owner(OwnerApiError::Gone), now);
            })
            .await;
            assert_eq!(bed.k6_open_gap_kinds(), vec!["owner_api_refused"]);

            // The owner configures Fleet polling: the start path resets the row, in
            // the new mode, before anything else starts.
            let start = bed
                .store
                .begin_collector_run(&crate::db::CollectorRunStartRequest {
                    mode: "fleet_polling",
                    config_fingerprint: DS_FP_B,
                    start_ms: bed.wall() + 1_000,
                    previous_heartbeat_ms: None,
                    gap_min_ms: K_GAP_MIN_MS,
                    gap_alert_ms: 600_000,
                })
                .expect("start");
            assert!(start.account_reset);
            let row = bed.k6_account_row().expect("row");
            assert_eq!(
                (
                    row.state.as_str(),
                    row.reason_code.as_str(),
                    row.mode.as_str()
                ),
                ("collecting", "config_changed", "fleet_polling")
            );
            assert!(bed.k6_open_gap_kinds().is_empty(), "the gap closed");
            let (mut fleet, _clock, t) =
                restarted_scheduler(crate::config::CollectorMode::FleetPolling, bed.wall());
            fleet.load(&snapshot_of(&bed.store, hub, DS_FP_B), t);
            assert_eq!(fleet.account.state, AccountState::Collecting);

            // An owner retry with the fake still answering 410: collecting, one list,
            // refused again.
            let mut bed = TickBed::k6_online_bed(crate::config::CollectorMode::Legacy, 1).await;
            bed.advance(Duration::from_secs(60));
            bed.transition_tick(|scheduler, now| {
                scheduler.discovery_failed_for_error(&ep_owner(OwnerApiError::Gone), now);
            })
            .await;
            bed.advance(Duration::from_secs(10));
            let now = bed.now();
            bed.scheduler.begin_tick();
            bed.scheduler.apply_owner_actions(
                &[OwnerActionRow {
                    id: 1,
                    kind: OwnerActionKind::RetryCollection,
                    vehicle: None,
                }],
                now,
            );
            assert_eq!(bed.scheduler.account.state, AccountState::Collecting);
            assert!(bed.scheduler.discovery_due(now), "one list is due at once");
            bed.scheduler
                .discovery_failed_for_error(&ep_owner(OwnerApiError::Gone), now);
            assert_eq!(bed.scheduler.account.state, AccountState::OwnerApiRefused);
            assert_eq!(bed.scheduler.account.state_reason, "owner_api_gone");
            assert!(!bed.scheduler.discovery_due(now + Duration::from_secs(60)));
        })
        .await;
}

// ---- CT44 -------------------------------------------------------------------------------------

#[tokio::test]
async fn ct44_provider_refusals_have_their_own_state_reasons_and_readiness() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let provider = crate::config::CollectorMode::Provider;
            let reason_of = |error: CollectorError, list: bool| {
                let mut core = Core::new(provider);
                core.scheduler.accept_discovery(k6_online_cars(1), core.t0);
                let t = core.at(300);
                if list {
                    core.scheduler.discovery_failed_for_error(&error, t);
                } else {
                    core.scheduler.due_vehicles(t);
                    core.scheduler
                        .vehicle_failed_for_error(core_id(1), &error, t);
                }
                (
                    core.scheduler.account.state,
                    core.scheduler.account.state_reason,
                )
            };
            assert_eq!(
                reason_of(ep_status(402), true),
                (AccountState::ProviderRefused, "provider_payment_required")
            );
            assert_eq!(
                reason_of(ep_status(410), true),
                (AccountState::ProviderRefused, "provider_gone")
            );
            assert_eq!(
                reason_of(ep_owner(OwnerApiError::Gone), false),
                (AccountState::ProviderRefused, "provider_gone")
            );
            // A Fleet-named 403 on the list: the provider's, never the Owner API's.
            assert_eq!(
                reason_of(ep_forbidden(true), true),
                (AccountState::ProviderRefused, "provider_forbidden")
            );
            // The credential unit's own refusals map by mode.
            for (kind, expected) in [
                (CredentialRefusal::OwnerApi, "provider_forbidden"),
                (CredentialRefusal::Client, "provider_client_refused"),
            ] {
                let mut core = Core::new(provider);
                core.scheduler.credential_outcome(
                    None,
                    CredentialOutcome::OwnerApiRefused(kind),
                    core.t0,
                );
                assert_eq!(core.scheduler.account.state, AccountState::ProviderRefused);
                assert_eq!(core.scheduler.account.state_reason, expected);
            }

            // The stored row, readiness and gap of a provider refusal.
            let mut bed = TickBed::k6_online_bed(provider, 1).await;
            bed.advance(Duration::from_secs(300));
            bed.transition_tick(|scheduler, now| {
                scheduler.discovery_failed_for_error(&ep_status(402), now);
            })
            .await;
            let row = bed.k6_account_row().expect("row");
            assert_eq!(
                (
                    row.state.as_str(),
                    row.reason_code.as_str(),
                    row.mode.as_str()
                ),
                ("provider_refused", "provider_payment_required", "provider")
            );
            assert_eq!(
                bed.k6_readiness(),
                Some(crate::db::CollectionAttentionCode::CollectorProviderRefused)
            );
            assert_eq!(bed.k6_open_gap_kinds(), vec!["provider_refused"]);
            assert!(
                !bed.scheduler
                    .status(bed.now())
                    .account_text
                    .to_lowercase()
                    .contains("credential")
            );

            // A plain 403 after the refresh and resend: the provider's refusal.
            let mut core = Core::new(provider);
            core.scheduler.accept_discovery(k6_online_cars(1), core.t0);
            let (checked, list_calls, _) = k6_checked_list(
                true,
                vec![Err(ep_forbidden(false)), Err(ep_forbidden(true))],
                RefusalCheck::Refreshed,
            )
            .await;
            assert_eq!(list_calls, 2);
            let at = core.at(300);
            k6_apply_checked_list(&mut core.scheduler, checked, at);
            assert_eq!(core.scheduler.account.state, AccountState::ProviderRefused);
            assert_eq!(core.scheduler.account.state_reason, "provider_forbidden");
        })
        .await;
}

// ---- CT46 -------------------------------------------------------------------------------------

#[test]
fn ct46_fleet_refusals_have_their_states_probes_and_gaps() {
    let log = DecisionLog::start();
    // 402: billing suspended, probed by a list every 6 h and at a new month.
    let mut core = Core::fleet();
    core.scheduler.accept_discovery(k6_online_cars(1), core.t0);
    k6_hub_ids(&mut core, 1);
    core.scheduler.due_vehicles(core.t0);
    core.scheduler
        .vehicle_succeeded(core_id(1), PollPhase::Online, false, core.t0);
    let t = core.at(120);
    ep_sync_clock(&core, t);
    let delay = core
        .scheduler
        .discovery_failed_for_error(&ep_fleet_status(402), t);
    assert!(delay <= CONTROL_SETTINGS_REFRESH);
    assert_eq!(core.scheduler.account.state, AccountState::BillingSuspended);
    assert_eq!(core.scheduler.account.state_reason, "billing_suspended");
    let tick = k6_commit_in_memory(&mut core);
    assert_eq!(
        k6_gap_events(&tick, GapKind::BillingSuspended, None),
        (1, 0)
    );
    // No read and no list until the probe: the probe is a list, not a read.
    let probe_at = t + Duration::from_secs(6 * 3_600);
    let mut step = t;
    while step + Duration::from_secs(60) < probe_at {
        step += Duration::from_secs(60);
        assert!(!core.scheduler.discovery_due(step));
        assert!(core.scheduler.due_vehicles(step).is_empty());
        assert!(core.scheduler.due_probe_vehicles(step).is_empty());
    }
    assert!(
        !core
            .scheduler
            .discovery_due(probe_at - Duration::from_millis(1))
    );
    assert!(core.scheduler.discovery_due(probe_at));
    // The probe answers 402 again: no new transition, the next probe 6 h on.
    ep_sync_clock(&core, probe_at);
    core.scheduler
        .discovery_failed_for_error(&ep_fleet_status(402), probe_at);
    assert_eq!(core.scheduler.account.state, AccountState::BillingSuspended);
    assert!(
        !core
            .scheduler
            .discovery_due(probe_at + Duration::from_secs(6 * 3_600 - 1))
    );
    // The first tick of the next month makes the probe due at once.
    let month = probe_at + Duration::from_secs(3_600);
    ep_sync_clock(&core, month);
    core.scheduler.note_new_month(month);
    assert!(core.scheduler.discovery_due(month));
    // A probe answered 200 resumes: collecting, the telemetry check due, one event.
    core.scheduler.accept_discovery(k6_online_cars(1), month);
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);
    assert!(core.scheduler.account.telemetry_check_due);
    assert!(core.scheduler.status(month).telemetry_check_due);
    assert_eq!(
        log.lines("INFO", "billing_resumed"),
        1,
        "one billing_resumed event\n{}",
        log.contents()
    );
    let tick = k6_commit_in_memory(&mut core);
    assert_eq!(
        k6_gap_events(&tick, GapKind::BillingSuspended, None),
        (0, 1)
    );

    // 403 on the list: FleetAccessRefused, no probe in 24 hours.
    let mut core = Core::fleet();
    core.scheduler.accept_discovery(k6_online_cars(1), core.t0);
    core.scheduler
        .discovery_failed_for_error(&ep_fleet_status(403), core.at(10));
    assert_eq!(
        core.scheduler.account.state,
        AccountState::FleetAccessRefused
    );
    assert_eq!(core.scheduler.account.state_reason, "fleet_access_refused");
    let mut step = core.at(10);
    while step < core.at(10 + 24 * 3_600) {
        step += Duration::from_secs(60);
        assert!(!core.scheduler.discovery_due(step));
    }
    // 421: wrong region; 412: partner not registered; 423: locked.
    for (error, state, reason) in [
        (
            ep_fleet_status(421),
            AccountState::ConfigError,
            "config_wrong_region",
        ),
        (
            ep_fleet_status(412),
            AccountState::ConfigError,
            "config_partner_unregistered",
        ),
        (
            ep_fleet_status(423),
            AccountState::AccountLocked,
            "account_locked",
        ),
    ] {
        let mut core = Core::fleet();
        core.scheduler.accept_discovery(k6_online_cars(1), core.t0);
        core.scheduler
            .discovery_failed_for_error(&error, core.at(10));
        assert_eq!(
            (
                core.scheduler.account.state,
                core.scheduler.account.state_reason
            ),
            (state, reason)
        );
        assert!(!core.scheduler.discovery_due(core.at(24 * 3_600)));
        assert!(core.scheduler.due_vehicles(core.at(24 * 3_600)).is_empty());
    }
}

// ---- CT72 -------------------------------------------------------------------------------------

#[test]
fn ct72_a_c5_outcome_enters_the_scheduler_by_its_row_of_the_table() {
    // (a) VehicleAccessRejected: a client error of that car only.
    let mut core = ep_online_core(2);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t).len(), 2);
    core.scheduler.credential_outcome(
        Some(core_id(1)),
        CredentialOutcome::VehicleAccessRejected,
        t,
    );
    core.scheduler
        .vehicle_succeeded(core_id(2), PollPhase::Online, false, t);
    let record = ep_record(&core, 1);
    assert_eq!(record.consecutive_failures, 1);
    assert_eq!(record.last_error, Some(ErrorClass::ClientError));
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);
    assert_eq!(core.scheduler.account.wait, None);
    assert_eq!(
        core.scheduler.due_vehicles(t + Duration::from_secs(60)),
        vec![core_id(1), core_id(2)],
        "both cars are read again at their cadence"
    );

    // (b) A terminal stop: CredentialStop, one auth_terminal gap, no C3 row; the
    // credential unit's lifting gives Collecting at the next refresh.
    let mut core = ep_online_core(1);
    k6_hub_ids(&mut core, 1);
    let t = core.at(60);
    core.scheduler.due_vehicles(t);
    core.scheduler
        .credential_outcome(Some(core_id(1)), CredentialOutcome::TerminalStop, t);
    assert_eq!(core.scheduler.account.state, AccountState::CredentialStop);
    assert!(!core.scheduler.requests_allowed(t));
    assert!(!core.scheduler.should_start_stream(core_id(1)));
    let tick = k6_commit_in_memory(&mut core);
    assert!(tick.account.is_none(), "no C3 account row");
    assert_eq!(k6_gap_events(&tick, GapKind::AuthTerminal, None), (1, 0));
    let later = t + Duration::from_secs(10);
    ep_sync_clock(&core, later);
    core.scheduler
        .credential_state_seen(CredentialStateSeen::Ready, later);
    // The same stop seen again at a settings refresh stops the account once more.
    core.scheduler
        .credential_state_seen(CredentialStateSeen::TerminalStop, later);
    assert_eq!(core.scheduler.account.state, AccountState::CredentialStop);
    core.scheduler
        .credential_state_seen(CredentialStateSeen::Ready, later);
    assert_eq!(core.scheduler.account.state, AccountState::Collecting);
    assert!(core.scheduler.discovery_due(later));
    let tick = core.scheduler.take_tick();
    assert_eq!(k6_gap_events(&tick, GapKind::AuthTerminal, None), (0, 1));

    // (c) AuthUnavailable on a tick's first read: the rest are not sent, nothing is
    // sent before the credential unit's time, no failure is counted.
    let mut core = ep_online_core(3);
    let t = core.at(60);
    assert_eq!(core.scheduler.due_vehicles(t).len(), 3);
    core.scheduler.credential_outcome(
        Some(core_id(1)),
        CredentialOutcome::AuthUnavailable {
            retry_after: Duration::from_secs(90),
        },
        t,
    );
    assert!(core.scheduler.due_vehicles(t).is_empty());
    assert!(
        core.scheduler
            .due_vehicles(t + Duration::from_secs(89))
            .is_empty()
    );
    assert!(
        core.scheduler.delay_until_next_action(t) >= K_MAX_IDLE_WAIT,
        "the loop sleeps its full idle wait, then the account's wait ends"
    );
    assert_eq!(ep_record(&core, 1).consecutive_failures, 0);
    assert_eq!(
        core.scheduler
            .due_vehicles(t + Duration::from_secs(90))
            .len(),
        1,
        "the car whose read was stopped is due again; the others were in flight"
    );

    // (d) A refusal of kind Client, by mode, each with its C3 row; never a stop.
    for (mode, state, reason, row_state) in [
        (
            crate::config::CollectorMode::Legacy,
            AccountState::OwnerApiRefused,
            "owner_api_client_refused",
            "owner_api_refused",
        ),
        (
            crate::config::CollectorMode::Provider,
            AccountState::ProviderRefused,
            "provider_client_refused",
            "provider_refused",
        ),
    ] {
        let mut core = Core::new(mode);
        core.scheduler.accept_discovery(k6_online_cars(1), core.t0);
        core.scheduler.credential_outcome(
            None,
            CredentialOutcome::OwnerApiRefused(CredentialRefusal::Client),
            core.at(5),
        );
        assert_eq!(
            (
                core.scheduler.account.state,
                core.scheduler.account.state_reason
            ),
            (state, reason)
        );
        let tick = core.scheduler.take_tick();
        let row = tick.account.expect("the C3 row").row;
        assert_eq!(
            (row.state.as_str(), row.reason_code.as_str()),
            (row_state, reason)
        );
    }

    // (e) The credential unit's state changes with no request outcome: the next
    // settings refresh sees it.
    let mut core = Core::owner();
    core.scheduler.accept_discovery(k6_online_cars(1), core.t0);
    core.scheduler.credential_state_seen(
        CredentialStateSeen::ApiRefused(CredentialRefusal::OwnerApi),
        core.at(5),
    );
    assert_eq!(core.scheduler.account.state, AccountState::OwnerApiRefused);
    assert_eq!(core.scheduler.account.state_reason, "owner_api_refused");
    let row = core.scheduler.take_tick().account.expect("row").row;
    assert_eq!(row.state, "owner_api_refused");
}

// ---- CT72 (f) to (h): one lift rule with C5 (B2 clarification) ---------------------------------

/// The owner stores credentials: a new pair with its own generation (the external
/// write of C5 R62), encrypted with the data directory's key. Returns the generation.
fn ct72_owner_stores_credentials(
    store: &HubStore,
    data_dir: &std::path::Path,
    access: &str,
    refresh: &str,
) -> Uuid {
    let key = crate::teslamate_credentials::load_key(data_dir).unwrap_or_else(|_| {
        crate::teslamate_credentials::replace_key(data_dir, b"ct72-cloak-key")
            .expect("0600 Cloak key");
        crate::teslamate_credentials::load_key(data_dir).expect("Cloak key")
    });
    let tokens = OwnerTokens::from_secret_parts(access.to_owned(), refresh.to_owned())
        .expect("a pair");
    let generation = crate::teslamate_token::legacy_refresh_credential_generation(&tokens);
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &tokens)
            .expect("Cloak pair");
    let (expires, next) = not_due_schedule();
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, expires, next)
                .expect("a schedule")
                .with_credential_generation(generation)
                .expect("generation"),
        )
        .expect("store pair");
    generation
}

/// The credential unit built over the store, as the collector's start path builds
/// it, for `mode`.
fn ct72_unit(
    store: &HubStore,
    data_dir: &std::path::Path,
    mode: crate::legacy_auth::AuthMode,
    token_url: &url::Url,
) -> LegacyAuthManager {
    ct72_unit_with_env(store, data_dir, mode, token_url, Arc::new(SkippingEnv::default()))
}

/// [`ct72_unit`] with an environment the test holds, to move the unit's clock.
fn ct72_unit_with_env(
    store: &HubStore,
    data_dir: &std::path::Path,
    mode: crate::legacy_auth::AuthMode,
    token_url: &url::Url,
    env: Arc<SkippingEnv>,
) -> LegacyAuthManager {
    LegacyAuthManager::from_hub_teslamate_store_for_test(
        store.clone(),
        data_dir,
        &crate::legacy_auth::AuthEndpointConfig {
            mode,
            region: Some(StreamRegion::Global),
            token_url: (mode == crate::legacy_auth::AuthMode::Provider)
                .then(|| token_url.to_string()),
            allow_loopback_http: true,
        },
        Some(token_url.clone()),
        false,
        env,
    )
    .expect("the unit starts")
}

fn ct72_state_of(unit: &LegacyAuthManager) -> String {
    unit.status_now().to_row().state
}

/// The collector's real start path on a bed's store and configuration.
fn ct72_start(bed: &TickBed, config: &HubConfig) -> CollectorStart {
    start_collector_run(
        &bed.store,
        config,
        &core_cadence(),
        "wss://streaming.example.test",
        None,
    )
    .expect("start")
}

#[tokio::test]
async fn ct72_f_an_external_pair_write_resets_a_stored_refusal_before_c5_starts() {
    // C3 R4, R7 (B2 clarification): the credential identity of the Owner wire is
    // the generation of the last external credential write, read through C5's store
    // function. An owner storing credentials changes the fingerprint at the next
    // start: C3 resets its row (and tells C5's start), so C5 restores no `api_refused`.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            crate::crypto::install_default_provider();
            let (_fake, base, _client, server) = auth_fake().await;
            let token_url = base.join("oauth2/v3/token").expect("token endpoint");
            let mut bed = TickBed::k6_online_bed(crate::config::CollectorMode::Legacy, 1).await;
            let config = supervised_restart_test_config(bed._dir.path());
            let dir = bed._dir.path().to_path_buf();
            let g0 = ct72_owner_stores_credentials(&bed.store, &dir, "owner-access-0", "owner-refresh-0");
            // The scheduler's ticks carry the fingerprint the start path computes.
            bed.scheduler.config_fingerprint = collector_config_fingerprint(
                crate::config::CollectorMode::Legacy,
                &crate::config::owner_api_origin_text(&config.collector),
                "wss://streaming.example.test",
                &g0.to_string(),
            );
            bed.advance(Duration::from_secs(300));
            bed.transition_tick(|scheduler, now| {
                scheduler.discovery_failed_for_error(&ep_forbidden(true), now);
            })
            .await;
            assert_eq!(bed.k6_account_row().expect("row").state, "owner_api_refused");
            // The same credentials: the refusal stands, and the unit restores its
            // `api_refused` from the row.
            let same = ct72_start(&bed, &config);
            assert!(!same.account_reset);
            assert_eq!(bed.k6_account_row().expect("row").state, "owner_api_refused");
            let mut unit = ct72_unit(&bed.store, &dir, crate::legacy_auth::AuthMode::Tesla, &token_url);
            unit.apply_collector_start(&same.credential_view());
            assert_eq!(ct72_state_of(&unit), "api_refused");
            drop(unit);
            // The owner stores new credentials: C3 resets the row at its own start,
            // and the unit's restore reads that, so it lifts its refusal too.
            let g1 = ct72_owner_stores_credentials(&bed.store, &dir, "owner-access-1", "owner-refresh-1");
            assert_ne!(g0, g1);
            let changed = ct72_start(&bed, &config);
            assert!(changed.account_reset);
            let row = bed.k6_account_row().expect("row");
            assert_eq!(
                (row.state.as_str(), row.reason_code.as_str()),
                ("collecting", "config_changed")
            );
            // B2 P3: the start's own transaction lifted C5's durable refusal, before any
            // unit is rebuilt over the store.
            assert_eq!(p3_c5_state(&bed.store).as_deref(), Some("ready"));
            let mut unit = ct72_unit(&bed.store, &dir, crate::legacy_auth::AuthMode::Tesla, &token_url);
            unit.apply_collector_start(&changed.credential_view());
            assert_ne!(ct72_state_of(&unit), "api_refused", "no refusal is restored");
            assert!(legacy_auth_error_of_wait(&unit).is_none());
            server.abort();
        })
        .await;
}

/// Whether the unit would hold a request back for a stop (R39).
fn legacy_auth_error_of_wait(unit: &LegacyAuthManager) -> Option<LegacyAuthError> {
    unit.stop_view(epoch_seconds_of(SystemTime::now()))
}

#[tokio::test]
async fn ct72_g_a_refresh_between_two_starts_changes_no_fingerprint_and_keeps_a_paused_car() {
    // The unit's own refreshes (about four a day) never change the identity, so they
    // never clear an owner-visible refusal: a `Paused(AccessRefused)` hold survives
    // the restart and no read is sent to that car.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let mut bed = TickBed::k6_online_bed(crate::config::CollectorMode::Legacy, 2).await;
            let config = supervised_restart_test_config(bed._dir.path());
            let dir = bed._dir.path().to_path_buf();
            let g0 = ct72_owner_stores_credentials(&bed.store, &dir, "owner-access-0", "owner-refresh-0");
            let fingerprint = |generation: &str| {
                collector_config_fingerprint(
                    crate::config::CollectorMode::Legacy,
                    &crate::config::owner_api_origin_text(&config.collector),
                    "wss://streaming.example.test",
                    generation,
                )
            };
            bed.scheduler.config_fingerprint = fingerprint(&g0.to_string());
            // Car 1's access is refused by a Fleet-named 403 on its read, and the
            // account holds a refusal row of its own, so a reset would be visible.
            bed.advance(Duration::from_secs(60));
            bed.transition_tick(|scheduler, now| {
                scheduler.vehicle_failed_for_error(core_id(1), &ep_forbidden(true), now);
                scheduler.discovery_failed_for_error(&ep_forbidden(true), now);
            })
            .await;
            assert_eq!(bed.k6_account_row().expect("row").state, "owner_api_refused");
            assert_eq!(
                bed.scheduler.vehicles[&core_id(1)].hold,
                VehicleHold::Paused(PauseReason::AccessRefused)
            );
            let before = ct72_start(&bed, &config);
            assert!(!before.account_reset);
            // The unit's own refresh: a completed binding from g0 to a new generation.
            let g1 = Uuid::from_bytes([0x71; 16]);
            let receipt = bed.store.begin_legacy_refresh(g0).expect("begin");
            let successor = crate::db::TeslaMateLegacyTokenStore::refreshed(
                vec![7; 8],
                vec![3; 8],
                epoch_seconds_of(SystemTime::now()) + 28_800,
                epoch_seconds_of(SystemTime::now()) + 21_600,
            )
            .expect("refreshed pair")
            .with_credential_generation(g1)
            .expect("generation");
            bed.store
                .complete_legacy_refresh(receipt, g0, g1, &successor)
                .expect("complete");
            assert_eq!(
                bed.store.collector_credential_identity(false).expect("identity"),
                g0.to_string(),
                "the identity is the external write's, not the refresh's"
            );
            let after = ct72_start(&bed, &config);
            assert!(!after.account_reset, "a refresh is not a configuration change");
            let row = bed.k6_account_row().expect("row");
            assert_eq!(row.config_fingerprint, fingerprint(&g0.to_string()));
            assert_eq!(row.state, "owner_api_refused", "the refusal stands");
            // The restart restores the hold, and nothing is read for that car.
            let now = bed.now();
            let mut restarted = VehicleScheduler::new_with(
                core_cadence(),
                crate::config::CollectorMode::Legacy,
                core_cadence().fleet_policy(),
                now,
                bed.clock.clone(),
                FixedJitter::constant(0.0),
            );
            let configured: Vec<_> = [1u64, 2]
                .into_iter()
                .map(|number| {
                    (
                        bed.scheduler.vehicles[&core_id(number)].hub_id.expect("hub id"),
                        i64::try_from(number).expect("id"),
                        crate::hub_pack::ProjectionCarSettings::default(),
                    )
                })
                .collect();
            let snapshot = CollectorStateSnapshot::from_rows(
                bed.store.load_collector_state_rows().expect("rows"),
                after.snapshot.config_fingerprint.clone(),
                &configured,
            );
            restarted.load(&snapshot, now);
            restarted.apply_control_settings(&configured, now);
            // The first list names both cars: car 1's stored hold is restored with it.
            restarted.accept_discovery(
                vec![core_vehicle(1, "online"), core_vehicle(2, "online")],
                now,
            );
            assert_eq!(
                restarted.vehicles[&core_id(1)].hold,
                VehicleHold::Paused(PauseReason::AccessRefused),
                "the hold survives the restart"
            );
            let due = restarted.due_vehicles(now);
            assert!(!due.contains(&core_id(1)), "no read is sent to the car: {due:?}");
            assert!(
                restarted.due_probe_vehicles(now).iter().all(|(id, _)| *id != core_id(1)),
                "an access refusal has no probe (R51)"
            );
        })
        .await;
}

#[tokio::test]
async fn ct72_h_a_provider_url_change_inside_provider_mode_resets_c3_and_c5_and_one_list_is_due() {
    // Provider mode: `ProviderRefused` is restored once by C5 (its own row, written
    // at a start). The provider's base URL changes inside Provider mode: C3 resets its
    // row, C5 clears `api_refused` (it does not restore it, because the mode is
    // unchanged), and one list is due.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            crate::crypto::install_default_provider();
            let (_fake, base, _client, server) = auth_fake().await;
            let token_url = base.join("oauth2/v3/token").expect("token endpoint");
            let mut bed =
                TickBed::k6_online_bed(crate::config::CollectorMode::Provider, 1).await;
            let dir = bed._dir.path().to_path_buf();
            let mut config = supervised_restart_test_config(&dir);
            config.collector.owner_api_base_url =
                Some("https://provider-a.example.test/".to_owned());
            assert_eq!(
                crate::config::collector_mode(&config.collector),
                crate::config::CollectorMode::Provider
            );
            let g0 = ct72_owner_stores_credentials(&bed.store, &dir, "provider-access", "provider-refresh");
            bed.scheduler.config_fingerprint = collector_config_fingerprint(
                crate::config::CollectorMode::Provider,
                &crate::config::owner_api_origin_text(&config.collector),
                "wss://streaming.example.test",
                &g0.to_string(),
            );
            bed.advance(Duration::from_secs(300));
            bed.transition_tick(|scheduler, now| {
                scheduler.discovery_failed_for_error(&ep_forbidden(true), now);
            })
            .await;
            assert_eq!(bed.k6_account_row().expect("row").state, "provider_refused");
            // C5 restores its refusal from C3's row at this start.
            let same = ct72_start(&bed, &config);
            assert!(!same.account_reset);
            let mut unit = ct72_unit(&bed.store, &dir, crate::legacy_auth::AuthMode::Provider, &token_url);
            unit.apply_collector_start(&same.credential_view());
            assert_eq!(ct72_state_of(&unit), "api_refused");
            drop(unit);
            // The provider's base URL changes, the mode does not.
            config.collector.owner_api_base_url =
                Some("https://provider-b.example.test/".to_owned());
            let changed = ct72_start(&bed, &config);
            assert!(changed.account_reset, "C3 resets its row");
            assert_eq!(bed.k6_account_row().expect("row").state, "collecting");
            // B2 P3: C5's durable refusal is lifted by the same transaction.
            assert!(changed.refusal_reset);
            assert_eq!(p3_c5_state(&bed.store).as_deref(), Some("ready"));
            let mut unit = ct72_unit(&bed.store, &dir, crate::legacy_auth::AuthMode::Provider, &token_url);
            unit.apply_collector_start(&changed.credential_view());
            assert_ne!(ct72_state_of(&unit), "api_refused", "C5 clears its refusal");
            assert!(legacy_auth_error_of_wait(&unit).is_none());
            // One list is due at once.
            let now = bed.now();
            let mut restarted = VehicleScheduler::new_with(
                core_cadence(),
                crate::config::CollectorMode::Provider,
                core_cadence().fleet_policy(),
                now,
                bed.clock.clone(),
                FixedJitter::constant(0.0),
            );
            restarted.load(&changed.snapshot, now);
            assert_eq!(restarted.account.state, AccountState::Collecting);
            assert!(restarted.discovery_due(now), "one list is due");
            server.abort();
        })
        .await;
}

// ---- B2 P3: the ordinary start's refusal lift, across the manager and the durable rows -------

/// The configuration of one scenario: the collector mode and, for Provider, the
/// provider's base URL.
#[derive(Clone, Copy, Debug)]
enum P3Config {
    Legacy,
    Provider(&'static str),
    Fleet,
}

fn p3_config(directory: &std::path::Path, shape: P3Config) -> HubConfig {
    let mut config = supervised_restart_test_config(directory);
    match shape {
        P3Config::Legacy => {}
        P3Config::Provider(url) => config.collector.owner_api_base_url = Some(url.to_owned()),
        P3Config::Fleet => config.collector.provider = CollectorProvider::Fleet,
    }
    config
}

fn p3_auth_mode(shape: P3Config) -> crate::legacy_auth::AuthMode {
    match shape {
        P3Config::Provider(_) => crate::legacy_auth::AuthMode::Provider,
        _ => crate::legacy_auth::AuthMode::Tesla,
    }
}

/// The ordinary start on the store, as the supervised run makes it.
fn p3_start(store: &HubStore, config: &HubConfig) -> Result<CollectorStart, CollectorError> {
    start_collector_run(store, config, &core_cadence(), "wss://streaming.example.test", None)
}

/// The fingerprint the start computes for `shape` with the pair of generation
/// `generation` (the Owner wire's credential identity).
fn p3_fingerprint(config: &HubConfig, shape: P3Config, generation: Uuid) -> String {
    let mode = crate::config::collector_mode(&config.collector);
    collector_config_fingerprint(
        mode,
        &crate::config::owner_api_origin_text(&config.collector),
        if matches!(shape, P3Config::Fleet) { "" } else { "wss://streaming.example.test" },
        &generation.to_string(),
    )
}

/// Store C3's account row as an earlier run left it.
fn p3_seed_c3(store: &HubStore, mode: &str, fingerprint: &str, state: &str) {
    store
        .collector_write_transaction(
            |transaction| {
                let wrote = HubStore::write_collector_rows_in_transaction(
                    transaction,
                    &crate::db::CollectorRowsWrite {
                        now_ms: 1_700_000_000_000,
                        account: Some(crate::db::CollectorAccountRow {
                            mode: mode.to_owned(),
                            config_fingerprint: fingerprint.to_owned(),
                            state: state.to_owned(),
                            reason_code: state.to_owned(),
                            since_ms: 1_700_000_000_000,
                            rate_limited_until_ms: None,
                            next_probe_ms: None,
                        }),
                        ..crate::db::CollectorRowsWrite::default()
                    },
                )?;
                Ok::<_, StoreError>(((), wrote))
            },
            || Ok(()),
        )
        .expect("C3 account row");
}

/// Store C5's `api_refused` row stopped in `stopped_mode`.
fn p3_seed_c5_refusal(store: &HubStore, stopped_mode: &str) {
    store
        .write_collector_auth_row(
            &crate::db::CollectorAuthRow {
                state: "api_refused".to_owned(),
                reason: Some("owner_api".to_owned()),
                collecting: false,
                retry_at: None,
                stopped_generation: None,
                stopped_mode: Some(stopped_mode.to_owned()),
                fuse_consecutive: 0,
                fuse_generation: None,
                fuse_last_rejection_at: None,
                fuse_pause_until: None,
                fuse_rejected_access_digest: None,
                clock_skew: false,
            },
            1_700_000_000_000,
        )
        .expect("C5 refusal row");
}

fn p3_c5_state(store: &HubStore) -> Option<String> {
    store.load_collector_auth_row().expect("auth row").map(|row| row.state)
}

/// One list through the unit, as the loop would make it: the result and the
/// number of Owner API requests the fake then saw.
async fn p3_list_once(
    fake: &AuthFake,
    client: &OwnerApi,
    unit: &mut LegacyAuthManager,
) -> (bool, usize) {
    let mut fuse = LegacyAuthFuse::default();
    let before = fake.owner_requests();
    let listed = client.list_vehicles_with_legacy_auth_fused(unit, &mut fuse).await;
    (listed.is_ok(), fake.owner_requests() - before)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3_real_start_external_write_lifts_only_c3_refusal() {
    // Each scenario: a stored C3 row, a stored C5 refusal, a change of the
    // configuration (a credential written externally, a Provider URL, the mode),
    // the unit built over the store BEFORE the start, the real start, and then the
    // durable C5 row (read before any unit is rebuilt) and the live unit. Only the
    // three conditions of C3 R7 lift the refusal; every false positive stays
    // stopped, in the row and in the unit, and sends nothing.
    struct Scenario {
        label: &'static str,
        before: P3Config,
        c3: Option<&'static str>,
        c5_stopped_mode: Option<&'static str>,
        after: P3Config,
        external_write: bool,
        lifts: bool,
        /// The refusal stands in the live unit as well: false only where the
        /// durable refusal belongs to another mode than the unit's, which the
        /// start declined to lift and which the unit must therefore neither hold
        /// nor overwrite.
        live_refusal: bool,
    }
    const A: &str = "https://provider-a.example.test/";
    const B: &str = "https://provider-b.example.test/";
    let scenarios = [
        Scenario { label: "refusal, credentials replaced", before: P3Config::Legacy, c3: Some("owner_api_refused"), c5_stopped_mode: Some("legacy"), after: P3Config::Legacy, external_write: true, lifts: true, live_refusal: false },
        Scenario { label: "collecting row, mode change", before: P3Config::Legacy, c3: Some("collecting"), c5_stopped_mode: Some("legacy"), after: P3Config::Provider(A), external_write: false, lifts: true, live_refusal: false },
        Scenario { label: "absent row, refusal of another mode", before: P3Config::Provider(A), c3: None, c5_stopped_mode: Some("provider"), after: P3Config::Legacy, external_write: false, lifts: true, live_refusal: false },
        Scenario { label: "Provider URL change", before: P3Config::Provider(A), c3: Some("provider_refused"), c5_stopped_mode: Some("provider"), after: P3Config::Provider(B), external_write: false, lifts: true, live_refusal: false },
        Scenario { label: "same-mode account_locked, credentials replaced", before: P3Config::Legacy, c3: Some("account_locked"), c5_stopped_mode: Some("legacy"), after: P3Config::Legacy, external_write: true, lifts: false, live_refusal: true },
        Scenario { label: "same-mode collecting row, credentials replaced", before: P3Config::Legacy, c3: Some("collecting"), c5_stopped_mode: Some("legacy"), after: P3Config::Legacy, external_write: true, lifts: false, live_refusal: true },
        Scenario { label: "absent row, refusal of the same mode", before: P3Config::Legacy, c3: None, c5_stopped_mode: Some("legacy"), after: P3Config::Legacy, external_write: true, lifts: false, live_refusal: true },
        Scenario { label: "collecting row of the same mode, refusal of another mode", before: P3Config::Legacy, c3: Some("collecting"), c5_stopped_mode: Some("provider"), after: P3Config::Legacy, external_write: false, lifts: false, live_refusal: false },
        Scenario { label: "refusal row, nothing changed", before: P3Config::Legacy, c3: Some("owner_api_refused"), c5_stopped_mode: Some("legacy"), after: P3Config::Legacy, external_write: false, lifts: false, live_refusal: true },
    ];
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            for scenario in scenarios {
                let label = scenario.label;
                let (_fake_state, base, client, server) = auth_fake().await;
                let fake = _fake_state;
                let token_url = base.join("oauth2/v3/token").expect("token endpoint");
                let (temporary, store) = cadence_wait_hub();
                let dir = temporary.path().to_path_buf();
                let g0 = ct72_owner_stores_credentials(&store, &dir, "owner-access-0", "owner-refresh-0");
                let config_before = p3_config(&dir, scenario.before);
                let mut seeded_c3: Option<(&str, String)> = None;
                if let Some(state) = scenario.c3 {
                    let mode = crate::config::collector_mode(&config_before.collector).code();
                    let fingerprint = p3_fingerprint(&config_before, scenario.before, g0);
                    p3_seed_c3(&store, mode, &fingerprint, state);
                    seeded_c3 = Some((mode, fingerprint));
                }
                if let Some(stopped_mode) = scenario.c5_stopped_mode {
                    p3_seed_c5_refusal(&store, stopped_mode);
                }
                // The configuration change, then the unit, then the start: the
                // order of a real restart.
                let config = p3_config(&dir, scenario.after);
                let generation = if scenario.external_write {
                    ct72_owner_stores_credentials(&store, &dir, "owner-access-1", "owner-refresh-1")
                } else {
                    g0
                };
                let mut unit = ct72_unit(&store, &dir, p3_auth_mode(scenario.after), &token_url);
                let started = p3_start(&store, &config).unwrap_or_else(|error| panic!("{label}: {error}"));
                assert_eq!(started.refusal_reset, scenario.lifts, "{label}");
                // C3's own row, which the same start transaction owns: a different mode or
                // fingerprint, the credentials replaced in the same mode included, makes it
                // carry the new pair and ends a stored refusal; an unchanged one stays.
                if let (Some(state), Some((mode_before, fingerprint_before))) =
                    (scenario.c3, &seeded_c3)
                {
                    let mode_after = crate::config::collector_mode(&config.collector).code();
                    let fingerprint_after = p3_fingerprint(&config, scenario.after, generation);
                    let changed = mode_after != *mode_before || fingerprint_after != *fingerprint_before;
                    let account = store
                        .load_collector_state_rows()
                        .expect("C3 rows")
                        .account
                        .expect("C3 account row");
                    assert_eq!(account.mode, mode_after, "{label}: the row carries the mode");
                    assert_eq!(account.config_fingerprint, fingerprint_after, "{label}: the row carries the fingerprint");
                    let expected_state = if changed && state != "collecting" { "collecting" } else { state };
                    assert_eq!(account.state, expected_state, "{label}: the stored state after the start");
                }
                // The durable C5 refusal is read before any unit is rebuilt: the start's
                // transaction lifted it, or left it.
                let durable = p3_c5_state(&store);
                if scenario.lifts {
                    assert_eq!(durable.as_deref(), Some("ready"), "{label}: lifted in the start's transaction");
                } else {
                    assert_eq!(durable.as_deref(), Some("api_refused"), "{label}: not lifted");
                }
                unit.apply_collector_start(&started.credential_view());
                let (listed, requests) = p3_list_once(&fake, &client, &mut unit).await;
                if scenario.lifts {
                    assert_ne!(ct72_state_of(&unit), "api_refused", "{label}");
                    assert_eq!(p3_c5_state(&store).as_deref(), Some("ready"), "{label}: the status stays lifted");
                    assert!(legacy_auth_error_of_wait(&unit).is_none(), "{label}");
                    assert!(listed && requests == 1, "{label}: one list goes out ({listed}, {requests})");
                } else if scenario.live_refusal {
                    assert_eq!(ct72_state_of(&unit), "api_refused", "{label}");
                    assert_eq!(p3_c5_state(&store).as_deref(), Some("api_refused"), "{label}");
                    assert!(!listed && requests == 0, "{label}: a false positive stays stopped ({listed}, {requests})");
                } else {
                    // The start declined to lift a refusal that belongs to another
                    // mode (C3 R7: one decision for the helper and the unit): the
                    // unit is not stopped by it, and nothing it does clears it, not
                    // its application of the start, a serviced list, the shutdown
                    // flush or a rebuilt unit of the refusal's own mode.
                    let row_after_start = store.load_collector_auth_row().expect("row");
                    assert_eq!(p3_c5_state(&store).as_deref(), Some("api_refused"), "{label}: kept after the application");
                    assert_ne!(ct72_state_of(&unit), "api_refused", "{label}: the live mode is not stopped by it");
                    assert!(listed && requests == 1, "{label}: the live mode lists ({listed}, {requests})");
                    unit.service();
                    unit.shutdown().await.expect("an orderly shutdown");
                    assert_eq!(
                        store.load_collector_auth_row().expect("row"),
                        row_after_start,
                        "{label}: the whole row is untouched by servicing and the shutdown flush"
                    );
                    let other = ct72_unit(&store, &dir, crate::legacy_auth::AuthMode::Provider, &token_url);
                    assert_eq!(ct72_state_of(&other), "api_refused", "{label}: the refusal is still its own mode's");
                }
                server.abort();
            }
        })
        .await;
}

/// Which non-stop change of the live unit the retained-refusal scenario makes.
#[derive(Clone, Copy, Debug)]
enum P3NonStopChange {
    /// A matching-generation fuse count restored at construction, cleared by a
    /// successful list.
    FuseCleared,
    /// The response `Date` flips the clock-skew flag.
    ClockSkew,
}

/// R-P3F-1: the scenario of one non-stop change (see the tests that call it).
async fn p3f2_non_stop_change_keeps_the_retained_refusal(change: P3NonStopChange) {
    // The start declined to lift a refusal of another mode (C3 collecting
    // row of the live mode, `refusal_reset` false). A change of the fuse or of the
    // clock-skew flag is the unit's own, non-stop news: it is persisted, but only
    // a start that lifts the refusal, or a stop of the unit's own, ends the
    // refusal's retention. After the list, the servicing, the shutdown and a
    // rebuilt unit the refusal's state, reason and mode are those the row held.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            {
                let label = format!("{change:?}");
                let (fake, base, client, server) = auth_fake().await;
                let token_url = base.join("oauth2/v3/token").expect("token endpoint");
                let (temporary, store) = cadence_wait_hub();
                let dir = temporary.path().to_path_buf();
                let g0 = ct72_owner_stores_credentials(&store, &dir, "owner-access-0", "owner-refresh-0");
                let config = p3_config(&dir, P3Config::Legacy);
                p3_seed_c3(&store, "legacy", &p3_fingerprint(&config, P3Config::Legacy, g0), "collecting");
                p3_seed_c5_refusal(&store, "provider");
                if matches!(change, P3NonStopChange::FuseCleared) {
                    // A recent rejection of the pair the unit holds.
                    let mut row = store.load_collector_auth_row().expect("row").expect("refusal row");
                    row.fuse_consecutive = 1;
                    row.fuse_generation = Some(g0);
                    row.fuse_last_rejection_at = Some(epoch_seconds_of(SystemTime::now()));
                    store.write_collector_auth_row(&row, 1_700_000_000_000).expect("fuse on the row");
                }
                let refusal = store.load_collector_auth_row().expect("row").expect("refusal row");
                let env = Arc::new(SkippingEnv::default());
                let mut unit = ct72_unit_with_env(
                    &store,
                    &dir,
                    crate::legacy_auth::AuthMode::Tesla,
                    &token_url,
                    env.clone(),
                );
                let started = p3_start(&store, &config).unwrap_or_else(|error| panic!("{label}: {error}"));
                assert!(!started.refusal_reset, "{label}: the start declines to lift");
                unit.apply_collector_start(&started.credential_view());
                if matches!(change, P3NonStopChange::ClockSkew) {
                    // The unit's clock runs far from the server's `Date`.
                    env.sleep_for_test(1_000);
                }
                let (listed, requests) = p3_list_once(&fake, &client, &mut unit).await;
                assert!(listed && requests == 1, "{label}: the live mode lists ({listed}, {requests})");
                // The refusal is kept, and the unit's own change is persisted.
                let kept_refusal = |row: &crate::db::CollectorAuthRow, when: &str| {
                    assert_eq!(
                        (row.state.as_str(), row.reason.as_deref(), row.stopped_mode.as_deref(), row.collecting),
                        ("api_refused", Some("owner_api"), Some("provider"), false),
                        "{label}: {when}: the refusal of the other mode is retained"
                    );
                };
                let after_list = store.load_collector_auth_row().expect("row").expect("row");
                kept_refusal(&after_list, "after the list");
                match change {
                    P3NonStopChange::FuseCleared => {
                        assert_eq!(refusal.fuse_consecutive, 1, "{label}: the row carried a fuse");
                        assert_eq!(after_list.fuse_consecutive, 0, "{label}: the cleared fuse is persisted");
                    }
                    P3NonStopChange::ClockSkew => {
                        assert!(!refusal.clock_skew);
                        assert!(after_list.clock_skew, "{label}: the skew flag is persisted");
                    }
                }
                assert_ne!(ct72_state_of(&unit), "api_refused", "{label}: the live mode is not stopped by it");
                unit.service();
                kept_refusal(&store.load_collector_auth_row().expect("row").expect("row"), "after servicing");
                unit.shutdown().await.expect("an orderly shutdown");
                let after_shutdown = store.load_collector_auth_row().expect("row").expect("row");
                kept_refusal(&after_shutdown, "after the shutdown flush");
                assert_eq!(after_shutdown, after_list, "{label}: servicing and shutdown add nothing");
                let rebuilt = ct72_unit(&store, &dir, crate::legacy_auth::AuthMode::Provider, &token_url);
                assert_eq!(ct72_state_of(&rebuilt), "api_refused", "{label}: the refusal is still its own mode's");
                kept_refusal(&store.load_collector_auth_row().expect("row").expect("row"), "after reconstruction");
                server.abort();
            }
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3f2_a_cleared_fuse_keeps_the_retained_refusal() {
    p3f2_non_stop_change_keeps_the_retained_refusal(P3NonStopChange::FuseCleared).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3f2_a_changed_clock_skew_keeps_the_retained_refusal() {
    p3f2_non_stop_change_keeps_the_retained_refusal(P3NonStopChange::ClockSkew).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3_different_mode_construction_leaves_the_refusal_to_the_start() {
    // The unit may be built before the start, but its construction must not consume
    // a refusal of another mode: the start decides, and the status is written only
    // after its commit. A start that fails before its commit leaves the durable
    // refusal as it was.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            let (_fake, base, _client, server) = auth_fake().await;
            let token_url = base.join("oauth2/v3/token").expect("token endpoint");
            let (temporary, store) = cadence_wait_hub();
            let dir = temporary.path().to_path_buf();
            ct72_owner_stores_credentials(&store, &dir, "owner-access-0", "owner-refresh-0");
            p3_seed_c5_refusal(&store, "provider");
            let config = p3_config(&dir, P3Config::Legacy);
            let mut unit = ct72_unit(&store, &dir, crate::legacy_auth::AuthMode::Tesla, &token_url);
            assert_eq!(
                p3_c5_state(&store).as_deref(),
                Some("api_refused"),
                "construction did not consume the refusal"
            );
            // The start fails before it commits: the C5 update aborts.
            store
                .open()
                .expect("connection")
                .execute_batch(
                    "CREATE TRIGGER p3_fail_lift BEFORE UPDATE ON collector_auth_state
                        BEGIN SELECT RAISE(ABORT, 'injected start fault'); END;",
                )
                .expect("trigger");
            assert!(p3_start(&store, &config).is_err(), "the start fails");
            assert_eq!(p3_c5_state(&store).as_deref(), Some("api_refused"));
            store
                .open()
                .expect("connection")
                .execute_batch("DROP TRIGGER p3_fail_lift;")
                .expect("drop trigger");
            // The retried start decides: no C3 row and a refusal of another mode is
            // condition (iii). The unit then writes its status after the commit.
            let started = p3_start(&store, &config).expect("start");
            assert!(started.refusal_reset);
            assert_eq!(p3_c5_state(&store).as_deref(), Some("ready"));
            unit.apply_collector_start(&started.credential_view());
            assert_eq!(p3_c5_state(&store).as_deref(), Some("ready"));
            assert_ne!(ct72_state_of(&unit), "api_refused");
            server.abort();
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cs_p3_fleet_roundtrip_does_not_restore_refusal() {
    // Legacy -> Fleet -> Legacy, each an ordinary start: the Fleet start lifts the
    // durable C5 refusal (Fleet included), and the return to Legacy restores no old
    // refusal, from a refusal row, a collecting row and an absent row alike.
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam, async {
            for (label, c3) in [
                ("C3 refusal", Some("owner_api_refused")),
                ("C3 collecting", Some("collecting")),
                ("C3 absent", None),
            ] {
                let (fake, base, client, server) = auth_fake().await;
                let token_url = base.join("oauth2/v3/token").expect("token endpoint");
                let (temporary, store) = cadence_wait_hub();
                let dir = temporary.path().to_path_buf();
                let g0 = ct72_owner_stores_credentials(&store, &dir, "owner-access-0", "owner-refresh-0");
                let legacy = p3_config(&dir, P3Config::Legacy);
                if let Some(state) = c3 {
                    p3_seed_c3(&store, "legacy", &p3_fingerprint(&legacy, P3Config::Legacy, g0), state);
                }
                p3_seed_c5_refusal(&store, "legacy");
                // The ordinary Fleet start (no credential unit exists in Fleet).
                let fleet = p3_config(&dir, P3Config::Fleet);
                let started = p3_start(&store, &fleet).unwrap_or_else(|error| panic!("{label}: {error}"));
                assert!(started.refusal_reset, "{label}");
                assert_eq!(
                    p3_c5_state(&store).as_deref(),
                    Some("ready"),
                    "{label}: the Fleet start lifted the durable refusal, before any unit exists"
                );
                // Back to Legacy: the unit restores no refusal, and the start lifts nothing
                // more (and restores nothing).
                let mut unit = ct72_unit(&store, &dir, crate::legacy_auth::AuthMode::Tesla, &token_url);
                let started = p3_start(&store, &legacy).unwrap_or_else(|error| panic!("{label}: {error}"));
                unit.apply_collector_start(&started.credential_view());
                assert_ne!(ct72_state_of(&unit), "api_refused", "{label}: no restored refusal");
                assert_eq!(p3_c5_state(&store).as_deref(), Some("ready"), "{label}");
                let (listed, requests) = p3_list_once(&fake, &client, &mut unit).await;
                assert!(listed && requests == 1, "{label}: one list ({listed}, {requests})");
                server.abort();
            }
        })
        .await;
}

// ---- CT73 and CT66: the owner's actions -------------------------------------------------------

#[tokio::test]
async fn ct73_a_retry_row_is_applied_in_one_transaction_and_sends_exactly_one_list() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed =
                TickBed::k6_online_bed(crate::config::CollectorMode::FleetPolling, 1).await;
            bed.advance(Duration::from_secs(120));
            bed.transition_tick(|scheduler, now| {
                scheduler.discovery_failed_for_error(&ep_fleet_status(402), now);
            })
            .await;
            assert_eq!(
                bed.k6_account_row().expect("row").state,
                "billing_suspended"
            );
            assert_eq!(bed.k6_open_gap_kinds(), vec!["billing_suspended"]);
            // The control CLI inserts the row; the collector reads it at its next
            // settings refresh and applies it inside the tick.
            bed.advance(Duration::from_secs(3));
            let wall = bed.wall();
            let action = bed
                .store
                .insert_collector_owner_action("retry_collection", None, wall)
                .expect("row");
            let rows = read_owner_actions(&bed.store);
            assert_eq!(rows.len(), 1);
            let commits = seam.tick_commits();
            let now = bed.now();
            bed.scheduler.begin_tick();
            assert_eq!(bed.scheduler.apply_owner_actions(&rows, now), vec![action]);
            bed.commit(TickBatch::default()).await;
            assert_eq!(seam.tick_commits(), commits + 1, "one transaction");
            let row = bed.k6_account_row().expect("row");
            assert_eq!(
                (row.state.as_str(), row.reason_code.as_str()),
                ("collecting", "owner_retry")
            );
            assert!(bed.k6_open_gap_kinds().is_empty(), "the gap closed");
            let applied: Option<i64> = bed.query(
                "SELECT applied_at_ms FROM collector_owner_actions WHERE applied_at_ms IS NOT NULL",
            );
            assert!(applied.is_some());
            // Exactly one list is due; once it answers, none is.
            assert!(bed.scheduler.discovery_due(now));
            bed.scheduler.accept_discovery(k6_online_cars(1), now);
            assert!(!bed.scheduler.discovery_due(now));
            // The next refresh applies nothing.
            assert!(read_owner_actions(&bed.store).is_empty());
        })
        .await;
}

#[tokio::test]
async fn ct73_resume_clears_a_pause_and_changes_nothing_for_a_car_that_is_not_paused() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::k6_online_bed(crate::config::CollectorMode::Legacy, 2).await;
            let hub1 = bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id");
            let hub2 = bed.scheduler.vehicles[&core_id(2)].hub_id.expect("hub id");
            bed.advance(Duration::from_secs(60));
            bed.transition_tick(|scheduler, now| {
                scheduler.vehicle_failed_for_error(core_id(1), &ep_status(451), now);
            })
            .await;
            assert_eq!(bed.k6_open_gap_kinds(), vec!["vehicle_paused"]);
            let wall = bed.wall();
            bed.store
                .insert_collector_owner_action("resume_vehicle", Some(hub1), wall)
                .expect("resume row");
            // Car 2 is not paused: its resume is applied and changes nothing.
            bed.store
                .insert_collector_owner_action("resume_vehicle", Some(hub2), wall)
                .expect("resume row");
            let rows = read_owner_actions(&bed.store);
            assert_eq!(rows.len(), 2);
            let now = bed.now();
            bed.scheduler.begin_tick();
            let before = bed.scheduler.vehicles[&core_id(2)].phase;
            assert_eq!(bed.scheduler.apply_owner_actions(&rows, now).len(), 2);
            bed.commit(TickBatch::default()).await;
            let one = &bed.scheduler.vehicles[&core_id(1)];
            assert_eq!(one.hold, VehicleHold::None);
            assert_eq!(one.phase, PollPhase::Unknown);
            assert_eq!(one.reason, "owner_resume");
            assert_eq!(bed.scheduler.vehicles[&core_id(2)].phase, before);
            assert!(bed.k6_open_gap_kinds().is_empty());
            assert!(read_owner_actions(&bed.store).is_empty());
        })
        .await;
}

#[test]
fn a_retry_clears_every_pause_and_wait_and_a_restored_pause_does_not_come_back() {
    let mut core = ep_online_core(2);
    k6_hub_ids(&mut core, 2);
    let t = core.at(60);
    core.scheduler.due_vehicles(t);
    core.scheduler
        .vehicle_failed_for_error(core_id(1), &ep_status(451), t);
    core.scheduler
        .vehicle_failed_for_error(core_id(2), &ep_fleet_status(403), t);
    core.scheduler
        .account_failed(ErrorClass::RateLimited, &ep_rate_limited(600), t);
    assert!(core.scheduler.account.wait.is_some());
    core.scheduler.apply_owner_actions(
        &[OwnerActionRow {
            id: 7,
            kind: OwnerActionKind::RetryCollection,
            vehicle: None,
        }],
        t,
    );
    for id in 1..=2 {
        assert_eq!(ep_record(&core, id).hold, VehicleHold::None);
        assert_eq!(ep_record(&core, id).phase, PollPhase::Unknown);
        assert_eq!(ep_record(&core, id).reason, "owner_retry");
    }
    assert_eq!(core.scheduler.account.wait, None);
    assert!(core.scheduler.discovery_due(t), "one list at once");
    assert!(core.scheduler.requests_allowed(t));
    assert_eq!(core.scheduler.pending.applied_actions, vec![7]);
}

// ---- CT66, the owner's actions in the real loop ---------------------------------------------------

#[derive(Clone)]
struct K6ListWire {
    status: u16,
    body: &'static str,
    calls: Arc<AtomicUsize>,
    token_calls: Arc<AtomicUsize>,
}

/// An authentication environment whose waits take no time: its clock is the real
/// one plus everything that was asked to be slept (C5 R19, R58).
#[derive(Default)]
struct SkippingEnv {
    skipped: std::sync::atomic::AtomicI64,
}

impl SkippingEnv {
    /// Move the unit's clock on by `seconds` without waiting.
    fn sleep_for_test(&self, seconds: i64) {
        self.skipped.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl crate::legacy_auth::attempt::AuthEnv for SkippingEnv {
    fn now(&self) -> i64 {
        epoch_seconds_of(SystemTime::now()) + self.skipped.load(Ordering::SeqCst)
    }

    fn sleep(
        &self,
        seconds: i64,
    ) -> std::pin::Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.skipped.fetch_add(seconds, Ordering::SeqCst);
        Box::pin(async {})
    }

    fn spread_unit(&self) -> f64 {
        0.0
    }
}

/// The fake token endpoint: every refresh rotates to a new pair, as Tesla's does.
async fn k6_token(State(wire): State<K6ListWire>) -> axum::response::Response {
    let n = wire.token_calls.fetch_add(1, Ordering::SeqCst) + 1;
    (
        StatusCode::OK,
        json!({
            "access_token": format!("rotated-access-{n}"),
            "refresh_token": format!("rotated-refresh-{n}"),
            "token_type": "Bearer",
            "expires_in": 1_000_000_000u64,
            "created_at": 1_800_000_000i64 + n as i64,
        })
        .to_string(),
    )
        .into_response()
}

async fn k6_products(State(wire): State<K6ListWire>) -> axum::response::Response {
    wire.calls.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::from_u16(wire.status).expect("a status"),
        wire.body,
    )
        .into_response()
}

/// Wait for a condition while the collector runs; if the collector ends first, say
/// how (a wait that only times out hides the reason).
async fn k6_wait_for(
    task: &mut JoinHandle<Result<(), CollectorError>>,
    label: &str,
    mut condition: impl FnMut() -> bool,
) {
    let deadline = Instant::now() + SUPERVISED_RESTART_WAIT;
    while !condition() {
        if task.is_finished() {
            let result = (&mut *task).await;
            panic!("{label}: the collector ended first: {result:?}");
        }
        assert!(Instant::now() < deadline, "{label} timeout");
        sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ct66_the_owner_loop_refuses_after_a_repeated_403_and_an_owner_retry_sends_one_list() {
    use crate::{
        credentials::{LegacyAuthManager, OwnerTokens},
        owner_api::OwnerApi,
    };

    crate::crypto::install_default_provider();
    let temporary = crate::private_tempdir().expect("temporary Hub");
    let store = HubStore::initialize(temporary.path()).expect("Hub store");
    seed_supervised_restart_import(&store);
    crate::teslamate_credentials::replace_key(temporary.path(), b"chain-cloak-key")
        .expect("0600 Cloak key");
    let key = crate::teslamate_credentials::load_key(temporary.path()).expect("Cloak key");
    let initial = OwnerTokens::from_secret_parts("k6-access".to_owned(), "k6-refresh".to_owned())
        .expect("initial pair");
    let (access, refresh) =
        crate::teslamate_token::encrypt_legacy_owner_tokens(key.as_bytes(), &initial)
            .expect("Cloak pair");
    store
        .replace_teslamate_legacy_tokens(
            &crate::db::TeslaMateLegacyTokenStore::refreshed(access, refresh, 2, 1)
                .expect("due refresh schedule"),
        )
        .expect("store pair");
    // The Owner list answers a plain 403 every time; the token endpoint rotates the
    // pair at every refresh. Both are this test's own in-process fakes.
    let wire = K6ListWire {
        status: 403,
        body: r#"{"error":"forbidden"}"#,
        calls: Arc::new(AtomicUsize::new(0)),
        token_calls: Arc::new(AtomicUsize::new(0)),
    };
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake listener");
    let address = listener.local_addr().expect("fake address");
    let router = Router::new()
        .route("/api/1/products", get(k6_products))
        .route("/oauth2/v3/token", post(k6_token))
        .with_state(wire.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("fake Owner");
    });
    let base = url::Url::parse(&format!("http://{address}/")).expect("fake URL");

    // The refusal check's refresh follows the startup refresh within a minute, so
    // C5 R19 holds it back for the remainder; the environment skips that wait
    // instead of sitting through it.
    let manager = LegacyAuthManager::from_hub_teslamate_store_for_test(
        store.clone(),
        temporary.path(),
        &crate::legacy_auth::AuthEndpointConfig {
            mode: crate::legacy_auth::AuthMode::Tesla,
            region: Some(StreamRegion::Global),
            token_url: None,
            allow_loopback_http: true,
        },
        Some(base.join("oauth2/v3/token").expect("token endpoint")),
        false,
        Arc::new(SkippingEnv::default()),
    )
    .expect("load pair");
    let region = manager.region();
    let auth = CollectionAuth::Legacy {
        manager: Arc::new(tokio::sync::Mutex::new(manager)),
        fuse: Arc::new(tokio::sync::Mutex::new(LegacyAuthFuse::default())),
        refresh: Arc::new(LegacyRefreshCoordinator::default()),
        allow_refresh: true,
        region,
    };
    let client = OwnerApi::for_fake_http(base, Duration::from_secs(2)).expect("client");
    let config = supervised_restart_test_config(temporary.path());
    // No car is known while the list is refused, so no stream starts.
    let stream_endpoint = "ws://127.0.0.1:9/streaming/".to_owned();
    // The loop runs under a controlled cadence (R-P2F3-8, as ct74a does): its
    // scheduler reads the clock the test moves by exactly the timers the loop asks
    // for, and the retry's bound is measured on that clock from one origin taken
    // before the retry row's insertion, so no load of the machine can move it.
    let clock = controlled_cadence_clock();
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    let control = seam.control_cadence(&clock);
    {
        let calls = Arc::clone(&wire.calls);
        seam.set_requests_probe(Arc::new(move || calls.load(Ordering::SeqCst)));
    }
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task_store = store.clone();
    let task_seam = Arc::clone(&seam);
    let mut task = tokio::spawn(SUPERVISED_COLLECTOR_TEST_SEAM.scope(task_seam, async move {
        run_supervised_with_access(
            &task_store,
            &config,
            supervised_restart_test_cadence(),
            client,
            auth,
            stream_endpoint,
            CursorKey::from_bytes([0xC6; 32]),
            Some(ready_tx),
            RunWriteGuard::for_run(None),
            async move {
                let _ = shutdown_rx.await;
            },
        )
        .await
    }));
    let _cursor =
        wait_for_supervised_signal_or_abort("collector readiness", &mut task, ready_rx).await;
    // Every step is bounded in the loop's logical time: a missing event fails on the
    // scheduler's clock, never by running until a guard ends the test.
    let step_bound = 4 * CONTROL_SETTINGS_REFRESH;
    // A plain 403: one refresh, the list resent once, the second 403 refuses.
    let refused = warm_up_the_controlled_loop_within(&control, clock.mono_now(), step_bound, |tick| {
        tick.account == "owner_api_refused"
    })
    .await;
    assert_eq!(
        wire.calls.load(Ordering::SeqCst),
        2,
        "the list and its resend"
    );
    assert_eq!(
        wire.token_calls.load(Ordering::SeqCst),
        2,
        "the startup refresh and the one refusal-check refresh"
    );
    let reason: String = store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT reason_code FROM collector_account_state",
            [],
            |row| row.get(0),
        )
        .expect("reason");
    assert_eq!(reason, "owner_api_refused");
    assert_eq!(
        store
            .collector_readiness_at(current_epoch_millis().expect("clock"))
            .expect("readiness")
            .failure,
        Some(crate::db::CollectionAttentionCode::CollectorApiRefused)
    );
    // Nothing more is sent while refused: two more settings refreshes of the loop's
    // own cadence complete (the completed ticks are the witness, not a sleep).
    let refreshes = refused.settings_refreshes_completed;
    step_the_controlled_loop_to_a_tick_within(&control, clock.mono_now(), step_bound, |tick| {
        tick.settings_refreshes_completed >= refreshes + 2
    })
    .await;
    assert_eq!(
        wire.calls.load(Ordering::SeqCst),
        2,
        "no request while refused"
    );
    // The owner's retry is applied within a settings refresh: exactly one list (and
    // its resend), refused again, and the row is marked applied. One origin on the
    // scheduler's clock, taken before the retry row is inserted, starts the bound
    // (R65, CT66: the next settings refresh, 5 s, and two seconds).
    let origin = control.clock().mono_now();
    let lists_before = wire.calls.load(Ordering::SeqCst);
    store
        .insert_collector_owner_action(
            "retry_collection",
            None,
            current_epoch_millis().expect("clock"),
        )
        .expect("retry row");
    let bound = CONTROL_SETTINGS_REFRESH + Duration::from_secs(2);
    let retry_tick = step_the_controlled_loop_to_a_tick_within(&control, origin, bound, |tick| {
        tick.at >= origin && tick.requests > lists_before
    })
    .await;
    assert!(
        retry_tick.at <= origin + bound,
        "the retry tick completed {:?} after the origin on the scheduler's clock (bound {bound:?})",
        retry_tick.at.saturating_duration_since(origin)
    );
    let applied: i64 = store
        .open()
        .expect("catalogue")
        .query_row(
            "SELECT COUNT(*) FROM collector_owner_actions WHERE applied_at_ms IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("applied actions");
    assert_eq!(applied, 1, "the retry row is applied");
    assert_eq!(
        wire.calls.load(Ordering::SeqCst),
        4,
        "one list and its resend after the retry"
    );
    assert_eq!(retry_tick.account, "owner_api_refused", "refused again");
    shutdown_tx.send(()).expect("shutdown");
    join_supervised_restart_task("collector join", &mut task)
        .await
        .expect("collector result");
    server.abort();
}

// ---- CT56: the decision log of the holds and refusals ---------------------------------------------

#[test]
fn ct56_the_decision_log_of_holds_and_refusals_names_the_hub_id_and_leaks_nothing() {
    let log = DecisionLog::start();
    let mut core = ep_online_core(1);
    let hub = k6_hub_ids(&mut core, 1)[0];
    let id = core_id(1);
    core.scheduler
        .vehicles
        .get_mut(&id)
        .unwrap()
        .vehicle
        .display_name = Some("Garbage Lorry".to_owned());
    // CT36: a persistent failure pauses the car and a probe clears it.
    let first = core.at(60);
    for step in 0..13u64 {
        let at = first + Duration::from_millis(61 * 60_000 * step / 12);
        ep_sync_clock(&core, at);
        core.scheduler
            .vehicle_failed_for_error(id, &ep_status(503), at);
    }
    let probe_at = first + Duration::from_secs(61 * 60 + 3_600);
    ep_sync_clock(&core, probe_at);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], probe_at);
    core.scheduler
        .probe_finished(id, ProbeOutcome::Read(PollPhase::Online, true), probe_at);
    // CT47: the car is refused as in service, probed, and leaves service.
    let service_at = probe_at + Duration::from_secs(10);
    ep_sync_clock(&core, service_at);
    core.scheduler
        .accept_discovery(vec![core_vehicle(1, "online")], service_at);
    core.scheduler.enter_service_mode(id, service_at);
    core.scheduler.probe_finished(
        id,
        ProbeOutcome::InService(false),
        service_at + Duration::from_secs(15 * 60),
    );
    // CT42: the account is refused by a 403 whose body named the Fleet API; the
    // owner retries.
    let refuse_at = service_at + Duration::from_secs(20 * 60);
    ep_sync_clock(&core, refuse_at);
    core.scheduler
        .discovery_failed_for_error(&ep_forbidden(true), refuse_at);
    core.scheduler.apply_owner_actions(
        &[OwnerActionRow {
            id: 1,
            kind: OwnerActionKind::RetryCollection,
            vehicle: None,
        }],
        refuse_at + Duration::from_secs(5),
    );
    // A removal and a reappearance.
    let t = refuse_at + Duration::from_secs(10);
    for at in [0u64, 400] {
        let at = t + Duration::from_secs(at);
        ep_sync_clock(&core, at);
        core.scheduler
            .accept_discovery(vec![core_vehicle(2, "asleep")], at);
    }

    let text = log.contents();
    assert!(
        text.contains(&hub.to_string()),
        "the Hub id is logged\n{text}"
    );
    for reason in [
        "paused_persistent_failure",
        "resumed",
        "service_entered",
        "service_exited",
        "owner_api_refused",
        "owner_retry",
        "vehicle_removed",
    ] {
        assert!(text.contains(reason), "{reason}\n{text}");
    }
    for forbidden in [
        "VIN00000000000001",
        "Garbage Lorry",
        "source_vehicle_id",
        "Bearer",
        "https://",
        "/api/1/",
        "vehicle_data",
        "Endpoint deprecated",
        "Fleet API",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} in the log\n{text}");
    }
    // Every transition is one INFO event (R10): the pause, the resume, the service
    // entry and exit, the refusal, the retry and the removal.
    assert_eq!(log.lines("INFO", "paused_persistent_failure"), 1);
    assert_eq!(log.lines("INFO", "service_entered"), 1);
    assert_eq!(log.lines("INFO", "vehicle_removed"), 1);
}
