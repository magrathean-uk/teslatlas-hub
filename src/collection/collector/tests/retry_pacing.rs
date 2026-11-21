// SPDX-License-Identifier: AGPL-3.0-only

// Retry pacing of the collector account (P04-F01, P04-F06, P04-F07): a provider
// deadline outlives a longer wait of another reason, the transport back-off and
// the inventory outage survive a restart. Synthetic data only; the fake sources
// are in-process and no test touches the network. The helpers are those of
// `durable_state.rs`.

fn retry_stored_hub_id(bed: &TickBed) -> Uuid {
    bed.scheduler.vehicles[&core_id(1)].hub_id.expect("hub id")
}

#[tokio::test]
async fn a_longer_authentication_wait_ending_early_leaves_the_providers_retry_after_in_force() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.list(vec![core_vehicle(1, "online")]).await;
            let start = bed.now();
            bed.scheduler.begin_tick();
            bed.scheduler.rate_limited(120, start);
            bed.scheduler
                .auth_failed(ErrorClass::AuthWait, Some(Duration::from_secs(300)), None, start);
            assert!(matches!(
                bed.scheduler.account.wait,
                Some((_, WaitReason::AuthWait))
            ));
            bed.advance(Duration::from_secs(2));
            let now = bed.now();
            bed.scheduler.end_auth_wait(now);
            assert!(!bed.scheduler.requests_allowed(now));
            assert_eq!(
                bed.scheduler.list_due_at(),
                Some(start + Duration::from_secs(120)),
                "discovery stays withheld until the original deadline"
            );
            bed.commit(TickBatch::default()).await;
            let stored = bed.store.load_collector_state_rows().unwrap();
            assert_eq!(
                stored.account.and_then(|row| row.rate_limited_until_ms),
                Some(CORE_W0 + 120_000),
                "the row keeps the provider's deadline"
            );
            // A failed commit that imposes a longer store wait does the same.
            bed.advance(Duration::from_secs(1));
            let now = bed.now();
            bed.scheduler.begin_tick();
            bed.scheduler.account.store_failures = 8;
            let tick = std::mem::take(&mut bed.scheduler.pending);
            bed.scheduler.commit_failed(tick, now);
            assert!(matches!(
                bed.scheduler.account.wait,
                Some((until, WaitReason::Store)) if until > start + Duration::from_secs(120)
            ));
            bed.scheduler.commit_succeeded(PendingTick::default(), now);
            assert_eq!(bed.scheduler.account.wait.map(|(_, reason)| reason), Some(WaitReason::RateLimited));
            assert!(!bed.scheduler.requests_allowed(now));
            assert_eq!(
                bed.scheduler.list_due_at(),
                Some(start + Duration::from_secs(120))
            );
            // A reload keeps it as well.
            let hub = retry_stored_hub_id(&bed);
            let (mut scheduler, _clock, t) =
                restarted_scheduler(crate::config::CollectorMode::Legacy, CORE_W0 + 3_000);
            scheduler.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
            assert!(!scheduler.requests_allowed(t + Duration::from_secs(116)));
            assert!(scheduler.requests_allowed(t + Duration::from_secs(117)));
        })
        .await;
}

#[tokio::test]
async fn a_restart_keeps_the_transport_back_off_deadline_and_step_until_recovery() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.list(vec![core_vehicle(1, "online")]).await;
            let mut delays = Vec::new();
            for _ in 0..3 {
                let now = bed.now();
                bed.scheduler.begin_tick();
                let delay = bed.scheduler.unreachable(now);
                delays.push(delay);
                bed.commit(TickBatch::default()).await;
                bed.advance(delay);
            }
            assert_eq!(
                delays,
                [5, 10, 20].map(Duration::from_secs),
                "the back-off steps with the failures"
            );
            // The third wait ended at w0 + 35 s; restart a second before it.
            let hub = retry_stored_hub_id(&bed);
            let (mut scheduler, _clock, t) =
                restarted_scheduler(crate::config::CollectorMode::Legacy, CORE_W0 + 34_000);
            scheduler.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
            assert_eq!(scheduler.account.consecutive_unreachable, 3);
            assert!(!scheduler.discovery_due(t));
            assert!(scheduler.discovery_due(t + Duration::from_secs(1)));
            // A further transport failure continues the mature back-off.
            let after = t + Duration::from_secs(1);
            assert_eq!(scheduler.unreachable(after), Duration::from_secs(40));
            // Recovery clears the retained state.
            let now = bed.now();
            bed.scheduler.begin_tick();
            bed.scheduler.note_answered(now);
            bed.commit(TickBatch::default()).await;
            let account = bed.store.load_collector_state_rows().unwrap().account.unwrap();
            assert_eq!(account.unreachable_until_ms, None);
            assert_eq!(account.consecutive_unreachable, 0);
        })
        .await;
}

#[tokio::test]
async fn repeated_list_failures_mark_the_inventory_unavailable_until_a_list_is_accepted() {
    let seam = Arc::new(SupervisedCollectorTestSeam::default());
    SUPERVISED_COLLECTOR_TEST_SEAM
        .scope(seam.clone(), async {
            let mut bed = TickBed::owner();
            bed.list(vec![core_vehicle(1, "online")]).await;
            for failure in 1..=3u32 {
                bed.advance(Duration::from_secs(600));
                let now = bed.now();
                bed.scheduler.begin_tick();
                bed.scheduler.discovery_failed(now);
                bed.commit(TickBatch::default()).await;
                // The row is written only once the account's stored shape changes.
                let marked = bed
                    .store
                    .load_collector_state_rows()
                    .unwrap()
                    .account
                    .is_some_and(|row| row.inventory_unavailable);
                assert_eq!(
                    marked,
                    failure >= K_INVENTORY_FAILURES,
                    "after failure {failure}"
                );
            }
            // A restart in the outage keeps the account marked and its back-off step.
            let hub = retry_stored_hub_id(&bed);
            let (mut scheduler, _clock, t) =
                restarted_scheduler(crate::config::CollectorMode::Legacy, bed.wall());
            scheduler.load(&snapshot_of(&bed.store, hub, DS_FP_A), t);
            assert_eq!(scheduler.account.discovery_failures, K_INVENTORY_FAILURES);
            assert_eq!(scheduler.account_view(), scheduler.persisted_account);
            bed.advance(Duration::from_secs(600));
            bed.list(vec![core_vehicle(1, "online")]).await;
            let account = bed.store.load_collector_state_rows().unwrap().account.unwrap();
            assert!(!account.inventory_unavailable, "one accepted list clears it");
        })
        .await;
}
