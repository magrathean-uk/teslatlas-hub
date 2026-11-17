// SPDX-License-Identifier: AGPL-3.0-only

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
async fn r9_every_scheduled_request_is_logged_at_debug_with_its_operation_class_status_and_duration()
 {
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
        assert!(
            line.contains(&format!("operation=\"{operation}\"")),
            "{line}"
        );
        assert!(line.contains(&format!("class={class}")), "{line}");
        assert!(line.contains(&format!("status={status}")), "{line}");
        assert!(line.contains("duration_ms="), "{line}");
    }
    assert!(requests[0].contains("vehicle=account"), "{}", requests[0]);
    assert!(
        requests[1].contains(&format!("vehicle={hub}")),
        "{}",
        requests[1]
    );
    // Nothing that identifies the car, the connection or the credentials.
    // (Only the collector's own lines: the HTTP client logs its connections.)
    let own: String = text
        .lines()
        .filter(|line| line.contains(" collector."))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in [
        "5YJ3E1EA7KF00000",
        "127.0.0.1",
        "api/1",
        "Bearer",
        "e30.",
        "fleet-",
    ] {
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
