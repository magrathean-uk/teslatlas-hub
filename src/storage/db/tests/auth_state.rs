// SPDX-License-Identifier: AGPL-3.0-only

// Tests of the legacy credential unit's durable status (C5 part P2: section
// 3.3, R43, R52, R52a, R57, R58, R62, and the readiness halves of spec tests
// 66, 72 and 89). Synthetic data only, in a temporary store; no network. The
// state machine that decides what the row says is tested with it, in
// `auth/legacy_auth/state_tests.rs`.

const AS_T0: i64 = 1_800_000_000;

fn as_store() -> (tempfile::TempDir, HubStore) {
    cs_store()
}

fn as_gen(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

fn as_row(state: &str, reason: Option<&str>, collecting: bool) -> CollectorAuthRow {
    CollectorAuthRow {
        state: state.to_owned(),
        reason: reason.map(str::to_owned),
        collecting,
        retry_at: None,
        stopped_generation: None,
        stopped_mode: None,
        fuse_consecutive: 0,
        fuse_generation: None,
        fuse_last_rejection_at: None,
        fuse_pause_until: None,
        fuse_rejected_access_digest: None,
        clock_skew: false,
    }
}

fn as_put_pair(store: &HubStore, generation: Option<Uuid>) {
    if crate::auth::legacy_auth::state_fixture::for_store(store).expect("component fixture shape") {
        crate::auth::legacy_auth::state_fixture::write(store,
            &crate::auth::legacy_auth::state_fixture::row(generation, vec![0;8], vec![0;8], (0,0))).expect("pre-upgrade component replacement");
        return;
    }
    let Some(generation)=generation else {
        // X8: nullable generations exist only in a pre-C10 component fixture.
        crate::auth::legacy_auth::state_fixture::write(store,&crate::auth::legacy_auth::state_fixture::row(None,vec![0;8],vec![0;8],(0,0))).expect("pre-upgrade component row");
        return;
    };
    s_test_with_credential_connection(store, |connection| {
        connection.set_test_now_ms(AS_T0 * 1_000);
        let refresh = format!("synthetic-refresh-{generation}");
        let pair = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            generation,
            b"synthetic-access",
            refresh.as_bytes(),
            0,
            0,
        )
        .expect("synthetic C10 pair");
        connection
            .replace_credential_pair(
                &pair,
                &EntryProvenance::Supplied,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny,
            )
            .expect("store synthetic C10 pair");
        connection
            .credential_durability_barrier()
            .expect("pair durability barrier");
    });
}

fn as_refresh(store: &HubStore, input: Uuid, output: Uuid) {
    s_test_with_credential_connection(store, |connection| {
        connection.set_test_now_ms(AS_T0 * 1_000);
        let refresh = format!("synthetic-refresh-{output}");
        let successor = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            output,
            b"synthetic-successor-access",
            refresh.as_bytes(),
            AS_T0 + 28_800,
            AS_T0 + 21_600,
        )
        .expect("synthetic successor");
        let receipt = connection
            .begin_credential_refresh(CredentialSlot::OwnerApi, input)
            .expect("begin real C10 refresh");
        connection
            .credential_durability_barrier()
            .expect("begin durability barrier");
        connection
            .complete_credential_refresh(receipt, input, &successor, &LegacyCandidates(Vec::new()))
            .expect("complete real C10 refresh");
        connection
            .credential_durability_barrier()
            .expect("completion durability barrier");
    });
}

#[test]
fn the_table_refuses_every_row_that_breaks_its_checks() {
    let (_temporary, store) = as_store();
    let connection = store.open().unwrap();
    let generation = as_gen(1).to_string();
    let insert = |values: &str| {
        connection.execute(
            &format!(
                "INSERT INTO collector_auth_state(
                    singleton_id, state, reason, collecting, retry_at, stopped_generation,
                    stopped_mode, fuse_consecutive, fuse_generation, fuse_last_rejection_at,
                    fuse_pause_until, clock_skew, updated_at_ms) VALUES {values}"
            ),
            [],
        )
    };
    let fails = |values: &str| {
        let outcome = insert(values);
        if outcome.is_ok() {
            connection
                .execute("DELETE FROM collector_auth_state", [])
                .unwrap();
        }
        outcome.is_err()
    };
    let long_reason = "x".repeat(65);
    let bad_generation = "short";
    // Valid rows pass: each stop state with the column the table ties to it.
    for good in [
        "(1, 'ready', NULL, 1, NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        format!(
            "(1, 'signed_out', 'access_token_rejected', 0, NULL, '{generation}', NULL, 3, '{generation}', 5, NULL, 0, 1)"
        ),
        format!(
            "(1, 'sign_in_required', 'refresh_token_rejected', 1, NULL, '{generation}', NULL, 0, NULL, NULL, NULL, 1, 1)"
        ),
        "(1, 'api_refused', 'owner_api', 0, NULL, NULL, 'legacy', 0, NULL, NULL, NULL, 0, 1)"
            .to_owned(),
        "(1, 'api_refused', 'client', 0, NULL, NULL, 'provider', 0, NULL, NULL, NULL, 0, 1)"
            .to_owned(),
        "(1, 'retrying', 'network', 0, 1800000060, NULL, NULL, 0, NULL, NULL, NULL, 0, 1)"
            .to_owned(),
    ] {
        assert!(!fails(&good), "{good}");
    }
    let bad = [
        // The singleton, the state list and the bounded reason.
        "(2, 'ready', NULL, 1, NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        "(1, 'frozen', NULL, 1, NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        "(1, 'retrying', '', 0, NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        format!("(1, 'retrying', '{long_reason}', 0, NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)"),
        // Flags are booleans and times are not negative.
        "(1, 'ready', NULL, 2, NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        "(1, 'ready', NULL, 1, NULL, NULL, NULL, 0, NULL, NULL, NULL, 2, 0)".to_owned(),
        "(1, 'ready', NULL, 1, -1, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        "(1, 'ready', NULL, 1, NULL, NULL, NULL, 0, NULL, -1, NULL, 0, 0)".to_owned(),
        "(1, 'ready', NULL, 1, NULL, NULL, NULL, 0, NULL, NULL, -1, 0, 0)".to_owned(),
        "(1, 'ready', NULL, 1, NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, -1)".to_owned(),
        // A sign-in or sign-out needs its generation, and no other state has one.
        "(1, 'signed_out', 'access_token_rejected', 0, NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        "(1, 'sign_in_required', 'refresh_token_rejected', 1, NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        format!("(1, 'ready', NULL, 1, NULL, '{generation}', NULL, 0, NULL, NULL, NULL, 0, 0)"),
        format!("(1, 'api_refused', 'owner_api', 0, NULL, '{generation}', 'legacy', 0, NULL, NULL, NULL, 0, 0)"),
        format!("(1, 'signed_out', 'access_token_rejected', 0, NULL, '{bad_generation}', NULL, 0, NULL, NULL, NULL, 0, 0)"),
        // A refusal needs its mode, only a refusal has one, and the mode is C3 R1's.
        "(1, 'api_refused', 'owner_api', 0, NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        "(1, 'ready', NULL, 1, NULL, NULL, 'legacy', 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        "(1, 'api_refused', 'owner_api', 0, NULL, NULL, 'fleet_polling', 0, NULL, NULL, NULL, 0, 0)".to_owned(),
        // The fuse is 0 to 3 and its generation is present exactly when it counts.
        format!("(1, 'ready', NULL, 1, NULL, NULL, NULL, 4, '{generation}', NULL, NULL, 0, 0)"),
        format!("(1, 'ready', NULL, 1, NULL, NULL, NULL, 0, '{generation}', NULL, NULL, 0, 0)"),
        "(1, 'ready', NULL, 1, NULL, NULL, NULL, 1, NULL, NULL, NULL, 0, 0)".to_owned(),
        format!("(1, 'ready', NULL, 1, NULL, NULL, NULL, 1, '{bad_generation}', NULL, NULL, 0, 0)"),
    ];
    for values in &bad {
        assert!(fails(values), "{values}");
    }
    // The strict table also refuses a value of the wrong type.
    assert!(fails(
        "(1, 'ready', NULL, 'yes', NULL, NULL, NULL, 0, NULL, NULL, NULL, 0, 0)"
    ));
}

#[test]
fn a_status_row_round_trips_and_is_rewritten_only_when_a_value_changes() {
    let (_temporary, store) = as_store();
    let g = as_gen(7);
    let row = CollectorAuthRow {
        stopped_generation: Some(g),
        fuse_consecutive: 3,
        fuse_generation: Some(g),
        fuse_last_rejection_at: Some(AS_T0),
        fuse_pause_until: Some(AS_T0 + 300),
        clock_skew: true,
        ..as_row("signed_out", Some("access_token_rejected"), false)
    };
    assert!(
        store.write_collector_auth_row(&row, 1_000).unwrap(),
        "created"
    );
    assert_eq!(store.load_collector_auth_row().unwrap(), Some(row.clone()));
    let stamp = |store: &HubStore| -> i64 {
        store
            .open()
            .unwrap()
            .query_row(
                "SELECT updated_at_ms FROM collector_auth_state",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(stamp(&store), 1_000);
    // The same status again is no write, and the write time stays (R52a).
    assert!(!store.write_collector_auth_row(&row, 9_000).unwrap());
    assert_eq!(stamp(&store), 1_000);
    // Any single changed value is a write.
    let changed = [
        CollectorAuthRow {
            reason: Some("refresh_token_rejected".to_owned()),
            ..row.clone()
        },
        CollectorAuthRow {
            stopped_generation: Some(as_gen(8)),
            ..row.clone()
        },
        CollectorAuthRow {
            fuse_consecutive: 2,
            ..row.clone()
        },
        CollectorAuthRow {
            fuse_generation: Some(as_gen(9)),
            ..row.clone()
        },
        CollectorAuthRow {
            fuse_last_rejection_at: Some(AS_T0 + 1),
            ..row.clone()
        },
        CollectorAuthRow {
            fuse_pause_until: None,
            ..row.clone()
        },
        CollectorAuthRow {
            clock_skew: false,
            ..row.clone()
        },
        CollectorAuthRow {
            fuse_rejected_access_digest: Some(as_gen(11)),
            ..row.clone()
        },
    ];
    for (index, next) in changed.iter().enumerate() {
        let at = 10_000 + i64::try_from(index).unwrap();
        assert!(store.write_collector_auth_row(next, at).unwrap(), "{index}");
        assert_eq!(stamp(&store), at);
        assert_eq!(
            store.load_collector_auth_row().unwrap().as_ref(),
            Some(next)
        );
        assert!(!store.write_collector_auth_row(next, at + 1).unwrap());
        // Back to the first row: also a change.
        assert!(store.write_collector_auth_row(&row, at + 2).unwrap());
    }
    // Retry time, collecting and the NULL columns count too.
    let retrying = CollectorAuthRow {
        retry_at: Some(AS_T0 + 60),
        collecting: true,
        ..as_row("retrying", Some("network"), true)
    };
    assert!(store.write_collector_auth_row(&retrying, 20_000).unwrap());
    let later = CollectorAuthRow {
        retry_at: Some(AS_T0 + 120),
        ..retrying.clone()
    };
    assert!(store.write_collector_auth_row(&later, 20_001).unwrap());
    assert!(
        store
            .write_collector_auth_row(
                &CollectorAuthRow {
                    collecting: false,
                    ..later.clone()
                },
                20_002
            )
            .unwrap()
    );
    assert!(
        store
            .write_collector_auth_row(
                &CollectorAuthRow {
                    retry_at: None,
                    collecting: false,
                    ..later
                },
                20_003
            )
            .unwrap()
    );
    // A row that breaks the table's checks is an error and writes nothing.
    let before = store.load_collector_auth_row().unwrap();
    assert!(
        store
            .write_collector_auth_row(
                &as_row("signed_out", Some("access_token_rejected"), false),
                30_000
            )
            .is_err()
    );
    assert_eq!(store.load_collector_auth_row().unwrap(), before);
    assert!(matches!(
        store.write_collector_auth_row(&row, -1),
        Err(StoreError::NegativeTimestamp(_))
    ));
    // No token, URL or Tesla identifier has a column to live in.
    let columns: Vec<String> = store
        .open()
        .unwrap()
        .prepare("SELECT name FROM pragma_table_info('collector_auth_state')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(columns.len(), 14);
    // `fuse_rejected_access_digest` holds a 36-character digest of the access token a
    // counted rejection rejected (hub fix pass 3; the column's CHECK pins the length),
    // not a token.
    assert!(
        columns
            .iter()
            .filter(|name| *name != "fuse_rejected_access_digest")
            .all(|name| {
                ![
                    "token",
                    "access",
                    "refresh",
                    "url",
                    "vin",
                    "latitude",
                    "longitude",
                ]
                .iter()
                .any(|word| name.contains(word))
            })
    );
}

#[test]
fn a_stored_row_with_a_bad_generation_is_an_error_not_a_guess() {
    let (_temporary, store) = as_store();
    let connection = store.open().unwrap();
    connection
        .execute(
            "INSERT INTO collector_auth_state(
                singleton_id, state, reason, collecting, fuse_consecutive, clock_skew,
                stopped_generation, updated_at_ms)
             VALUES (1, 'signed_out', 'access_token_rejected', 0, 0, 0,
                     'zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz', 0)",
            [],
        )
        .unwrap();
    assert!(store.load_collector_auth_row().is_err());
}

// ---------------------------------------------------------------------------
// Readiness: service failures, attention and healthy detail (C5 R52, C3 R13;
// B2 clarification 3; spec tests 66, 72, 81, 89)
// ---------------------------------------------------------------------------

/// The context-free read of a required collector (stored-mode fallback).
fn as_view(store: &HubStore, now_ms: i64) -> CollectorReadinessView {
    store.collector_readiness_view(&ReadinessContext::context_free(true, now_ms))
}

/// A read under the loaded configuration.
fn as_view_in(
    store: &HubStore,
    now_ms: i64,
    mode: crate::config::CollectorMode,
    cadence: Option<crate::config::CadenceViolation>,
) -> CollectorReadinessView {
    store.collector_readiness_view(&ReadinessContext::configured(true, now_ms, mode, cadence))
}

fn as_service(view: &CollectorReadinessView) -> Option<ReadinessReasonCode> {
    view.service.err().map(|failure| failure.code)
}

/// The service reason of a context-free required read.
fn as_readiness(store: &HubStore, now_ms: i64) -> Option<ReadinessReasonCode> {
    as_service(&as_view(store, now_ms))
}

/// The attention of a context-free required read.
fn as_attention(store: &HubStore, now_ms: i64) -> Option<CollectionAttentionCode> {
    as_view(store, now_ms).attention
}

/// The cadence fault the readiness tests use: a back-off at the tolerance.
fn as_cadence_fault() -> crate::config::CadenceViolation {
    crate::config::CadenceViolation {
        key: "collector.max_backoff_seconds",
        value: 300,
        rule: crate::config::CadenceRule::BelowOfflineTimeout {
            bound_key: "collector.offline_drive_timeout_seconds",
            bound_seconds: 300,
        },
    }
}

fn as_jwt(exp: i64) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    format!(
        "{}.{}.signature",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#))
    )
}

/// Store a real encrypted pair under a real key and return its generation.
/// `schedule` is `(expires_at, next_refresh_at)` of a refreshed pair, or `None`
/// for the imported form.
fn as_real_pair(
    temporary: &tempfile::TempDir,
    store: &HubStore,
    access: &str,
    schedule: Option<(i64, i64)>,
) -> Uuid {
    let _ = temporary;
    s_test_with_credential_connection(store, |connection| {
        connection.set_test_now_ms(AS_T0 * 1_000);
        let generation = Uuid::new_v4();
        let (expires_at, next_refresh_at) = schedule.unwrap_or((0, 0));
        let refresh = format!("synthetic-refresh-{generation}");
        let pair = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            generation,
            access.as_bytes(),
            refresh.as_bytes(),
            expires_at,
            next_refresh_at,
        )
        .expect("real encrypted synthetic C10 pair");
        connection
            .replace_credential_pair(
                &pair,
                &EntryProvenance::Supplied,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny,
            )
            .expect("store encrypted synthetic pair");
        connection
            .credential_durability_barrier()
            .expect("pair durability barrier");
        generation
    })
}

fn as_account(store: &HubStore, mode: &str, state: &str) {
    cs_write(
        store,
        &CollectorRowsWrite {
            now_ms: CS_W0,
            account: Some(CollectorAccountRow {
                mode: mode.to_owned(),
                ..cs_account(state, state)
            }),
            ..CollectorRowsWrite::default()
        },
    );
}

/// A Fleet pair with an identified unresolved refresh (a started receipt bound
/// to the stored generation, with its fence).
fn as_unresolved_fleet(store: &HubStore) -> Uuid {
    s_test_with_credential_connection(store, |connection| {
        connection.set_test_now_ms(AS_T0 * 1_000);
        let generation = Uuid::new_v4();
        let refresh = format!("synthetic-fleet-refresh-{generation}");
        let pair = s_test_synthetic_pair(
            connection,
            CredentialSlot::FleetApi,
            generation,
            b"synthetic-fleet-access",
            refresh.as_bytes(),
            AS_T0 + 28_800,
            AS_T0 + 21_600,
        )
        .expect("synthetic Fleet pair");
        connection
            .replace_credential_pair(
                &pair,
                &EntryProvenance::Supplied,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny,
            )
            .expect("store Fleet pair");
        connection
            .credential_durability_barrier()
            .expect("pair durability barrier");
        connection
            .begin_credential_refresh(CredentialSlot::FleetApi, generation)
            .expect("begin unresolved Fleet refresh");
        connection
            .credential_durability_barrier()
            .expect("begin durability barrier");
        generation
    })
}

#[test]
fn readiness_tiers_and_wire_codes_are_fixed() {
    // Exactly six service reasons, in the order of the service check, and the
    // serialized word is the wire word.
    let service: Vec<&str> = ReadinessReasonCode::ALL
        .iter()
        .map(|code| code.code())
        .collect();
    assert_eq!(
        service,
        [
            "catalogue_unavailable",
            "lifecycle_quarantined",
            "published_content_unservable",
            "collector_absent",
            "collector_stale",
            "collector_auth_terminal",
        ]
    );
    for code in ReadinessReasonCode::ALL {
        assert_eq!(serde_json::to_value(code).unwrap(), code.code());
    }
    // The attention registry: the eight C3 words, the three C5 words, the
    // cadence word and the Fleet unknown-refresh word. None of them is a service
    // reason.
    let attention: Vec<&str> = CollectionAttentionCode::ALL
        .iter()
        .map(|code| code.code())
        .collect();
    assert_eq!(
        attention,
        [
            "collector_api_refused",
            "collector_provider_refused",
            "collector_fleet_access_refused",
            "collector_billing_suspended",
            "collector_cap_reached",
            "collector_config_error",
            "collector_account_locked",
            "collector_stream_audit_failed",
            "collector_auth_waiting",
            "collector_auth_configuration_error",
            "collector_auth_sign_in_required",
            "collector_cadence_invalid",
            "collector_fleet_refresh_outcome_unknown",
        ]
    );
    for code in CollectionAttentionCode::ALL {
        assert_eq!(serde_json::to_value(code).unwrap(), code.code());
        assert!(!service.contains(&code.code()), "{}", code.code());
    }
    // Sign-in, cadence and Fleet unknown refresh are attention.
    for word in [
        "collector_auth_sign_in_required",
        "collector_cadence_invalid",
        "collector_fleet_refresh_outcome_unknown",
    ] {
        assert!(attention.contains(&word));
        assert!(!service.contains(&word));
    }
    // The healthy detail never repeats attention.
    for word in COLLECTOR_DETAIL_CODES {
        assert!(!attention.contains(&word), "{word}");
    }
    assert_eq!(
        COLLECTOR_DETAIL_CODES,
        [
            "collector_cap_warning",
            "collector_rate_limited",
            "collector_vehicle_paused",
            "collector_tesla_unreachable",
            "collector_gap_unnotified",
        ]
    );
}

#[test]
fn every_state_gives_the_readiness_code_of_the_table_of_r52() {
    use CollectionAttentionCode::{
        CollectorAuthConfigurationError, CollectorAuthSignInRequired, CollectorAuthWaiting,
    };
    let (_temporary, store) = as_store();
    store
        .acquire_supervised_collector_lease(1_000)
        .expect("lease");
    let g = as_gen(1);
    let now = 1_001;
    assert_eq!(
        as_readiness(&store, now),
        None,
        "no status row: nothing to report"
    );
    assert_eq!(as_attention(&store, now), None);
    let write = |row: CollectorAuthRow| {
        store.write_collector_auth_row(&row, now).expect("row");
    };
    let signed = |state: &str, reason: &str, collecting: bool| CollectorAuthRow {
        stopped_generation: Some(g),
        ..as_row(state, Some(reason), collecting)
    };
    let expect = |attention: Option<CollectionAttentionCode>,
                  service: Option<ReadinessReasonCode>,
                  label: &str| {
        let view = as_view(&store, now);
        assert_eq!(view.attention, attention, "{label}: attention");
        assert_eq!(as_service(&view), service, "{label}: service");
    };
    // States that need no code, and a refusal that C3 reports by itself.
    write(as_row("ready", None, true));
    expect(None, None, "ready");
    write(as_row("refreshing", None, true));
    expect(None, None, "refreshing");
    write(CollectorAuthRow {
        stopped_mode: Some("legacy".to_owned()),
        ..as_row("api_refused", Some("owner_api"), false)
    });
    expect(None, None, "api_refused adds no duplicate refusal");
    // `retrying` and `persistence_pending` wait once the token is unusable
    // (`collecting = 0`), not before (test 89).
    for state in ["retrying", "persistence_pending"] {
        let reason = (state == "retrying").then_some("network");
        write(as_row(state, reason, true));
        expect(None, None, &format!("{state} while usable"));
        write(as_row(state, reason, false));
        expect(
            Some(CollectorAuthWaiting),
            None,
            &format!("{state} once unusable"),
        );
    }
    // Every other waiting state, whatever its collecting value.
    write(CollectorAuthRow {
        fuse_consecutive: 2,
        fuse_generation: Some(g),
        fuse_pause_until: Some(1),
        ..as_row(
            "awaiting_confirmation",
            Some("access_token_rejected"),
            false,
        )
    });
    expect(Some(CollectorAuthWaiting), None, "awaiting_confirmation");
    write(as_row("observer_waiting", Some("token_expired"), false));
    expect(Some(CollectorAuthWaiting), None, "observer_waiting");
    write(as_row("clock_invalid", None, false));
    expect(Some(CollectorAuthWaiting), None, "clock_invalid");
    // Test 72: a configuration error has its own code and the lease stays active.
    write(as_row(
        "configuration_error",
        Some("region_conflict"),
        false,
    ));
    expect(
        Some(CollectorAuthConfigurationError),
        None,
        "configuration_error",
    );
    // Test 81: sign-in required collects, so it is attention and the Hub stays
    // ready; once it no longer collects it has become signed out, a service
    // failure; a signed-out stop is a service failure at once.
    write(signed("sign_in_required", "refresh_outcome_unknown", true));
    expect(
        Some(CollectorAuthSignInRequired),
        None,
        "sign_in_required collecting",
    );
    write(signed("sign_in_required", "refresh_outcome_unknown", false));
    expect(
        None,
        Some(ReadinessReasonCode::CollectorAuthTerminal),
        "sign_in_required expired",
    );
    write(signed("signed_out", "access_token_rejected", false));
    expect(
        None,
        Some(ReadinessReasonCode::CollectorAuthTerminal),
        "signed_out",
    );
}

#[test]
fn readiness_keeps_lease_precedence_and_expiry_boundary() {
    let (_temporary, store) = as_store();
    let signed_out = CollectorAuthRow {
        stopped_generation: Some(as_gen(1)),
        ..as_row("signed_out", Some("access_token_rejected"), false)
    };
    store.write_collector_auth_row(&signed_out, 1_000).unwrap();
    as_account(&store, "legacy", "cap_reached");
    // No lease: absent, with the independent attention preserved.
    let view = as_view(&store, 1_001);
    assert_eq!(
        as_service(&view),
        Some(ReadinessReasonCode::CollectorAbsent)
    );
    assert_eq!(
        view.attention,
        Some(CollectionAttentionCode::CollectorCapReached)
    );
    assert!(!view.running);
    // A malformed lease is unavailable, never a guess.
    store
        .open()
        .unwrap()
        .execute(
            "INSERT INTO supervised_collector_lease(singleton_id, instance_id, state,
                 started_at_ms, heartbeat_at_ms, lease_until_ms)
             VALUES (1, ?1, 'active', 40, 50, 90)",
            params![Uuid::nil().to_string()],
        )
        .unwrap();
    let view = as_view(&store, 60);
    assert_eq!(
        as_service(&view),
        Some(ReadinessReasonCode::CatalogueUnavailable)
    );
    assert_eq!(
        view.attention,
        Some(CollectionAttentionCode::CollectorCapReached)
    );
    assert!(!view.running, "an incoherent lease witnesses nothing");
    store
        .open()
        .unwrap()
        .execute("DELETE FROM supervised_collector_lease", [])
        .unwrap();
    // A fresh lease: the signed-out status is the service failure, the unrelated
    // cap stop stays attention.
    let lease = store.acquire_supervised_collector_lease(10_000).unwrap();
    let view = as_view(&store, 10_001);
    assert_eq!(
        as_service(&view),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
    assert_eq!(
        view.attention,
        Some(CollectionAttentionCode::CollectorCapReached)
    );
    assert!(view.running);
    // The expiry boundary: equal expiry is stale; one millisecond earlier is not.
    let until = 10_000 + SUPERVISED_COLLECTOR_LEASE_MS;
    assert_eq!(
        as_readiness(&store, until - 1),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
    let stale = as_view(&store, until);
    assert_eq!(
        as_service(&stale),
        Some(ReadinessReasonCode::CollectorStale)
    );
    assert_eq!(
        stale.attention,
        Some(CollectionAttentionCode::CollectorCapReached)
    );
    assert!(!stale.running);
    // A terminal lease wins over the credential status and still witnesses a
    // running process.
    store
        .heartbeat_supervised_collector_lease(
            lease,
            SupervisedCollectorState::AuthenticationTerminal,
            11_000,
        )
        .unwrap();
    let terminal = as_view(&store, 11_001);
    assert_eq!(
        as_service(&terminal),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
    assert!(terminal.running);
    // With the status quiet again the same terminal lease still fails, and an
    // active lease is ready.
    store
        .write_collector_auth_row(&as_row("ready", None, true), 11_002)
        .unwrap();
    assert_eq!(
        as_readiness(&store, 11_003),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
    store
        .heartbeat_supervised_collector_lease(lease, SupervisedCollectorState::Active, 12_000)
        .unwrap();
    assert_eq!(as_readiness(&store, 12_001), None);
}

#[test]
fn attention_precedence_is_independent_of_the_lease() {
    use CollectionAttentionCode as Code;
    // Under every lease condition the attention is the same, and the service
    // reason is the lease's own.
    for lease_case in ["absent", "expired", "terminal", "fresh"] {
        let (_temporary, store) = as_store();
        let lease = store.acquire_supervised_collector_lease(1_000).unwrap();
        let now = match lease_case {
            "absent" => {
                store.release_supervised_collector_lease(lease).unwrap();
                1_001
            }
            "expired" => 1_000 + SUPERVISED_COLLECTOR_LEASE_MS,
            "terminal" => {
                store
                    .heartbeat_supervised_collector_lease(
                        lease,
                        SupervisedCollectorState::AuthenticationTerminal,
                        1_500,
                    )
                    .unwrap();
                1_501
            }
            _ => 1_001,
        };
        let service = |store: &HubStore| as_service(&as_view(store, now));
        let service_expected = match lease_case {
            "absent" => Some(ReadinessReasonCode::CollectorAbsent),
            "expired" => Some(ReadinessReasonCode::CollectorStale),
            "terminal" => Some(ReadinessReasonCode::CollectorAuthTerminal),
            _ => None,
        };
        // C3 account stops, one at a time, in the registry's own order.
        for (state, code) in [
            ("owner_api_refused", Code::CollectorApiRefused),
            ("provider_refused", Code::CollectorProviderRefused),
            ("fleet_access_refused", Code::CollectorFleetAccessRefused),
            ("billing_suspended", Code::CollectorBillingSuspended),
            ("cap_reached", Code::CollectorCapReached),
            ("config_error", Code::CollectorConfigError),
            ("account_locked", Code::CollectorAccountLocked),
        ] {
            as_account(&store, "legacy", state);
            let view = as_view(&store, now);
            assert_eq!(view.attention, Some(code), "{lease_case} {state}");
            assert_eq!(as_service(&view), service_expected, "{lease_case} {state}");
        }
        // Owner or provider refusal outranks an applicable credential wait; a
        // credential wait outranks the other C3 stops and the audit failure.
        store
            .write_collector_auth_row(
                &as_row("observer_waiting", Some("token_expired"), false),
                1_002,
            )
            .unwrap();
        as_account(&store, "legacy", "owner_api_refused");
        assert_eq!(
            as_view(&store, now).attention,
            Some(Code::CollectorApiRefused)
        );
        as_account(&store, "legacy", "provider_refused");
        assert_eq!(
            as_view(&store, now).attention,
            Some(Code::CollectorProviderRefused)
        );
        for state in ["billing_suspended", "cap_reached", "fleet_access_refused"] {
            as_account(&store, "legacy", state);
            assert_eq!(
                as_view(&store, now).attention,
                Some(Code::CollectorAuthWaiting),
                "{state}"
            );
        }
        // The stream audit comes last of all, and an active cadence first.
        store
            .write_collector_auth_row(&as_row("ready", None, true), 1_003)
            .unwrap();
        as_account(&store, "legacy", "collecting");
        store.set_stream_audit_failed_cars(1);
        assert_eq!(
            as_view(&store, now).attention,
            Some(Code::CollectorStreamAuditFailed)
        );
        as_account(&store, "legacy", "cap_reached");
        assert_eq!(
            as_view(&store, now).attention,
            Some(Code::CollectorCapReached)
        );
        let faulted = as_view_in(
            &store,
            now,
            crate::config::CollectorMode::Legacy,
            Some(as_cadence_fault()),
        );
        assert_eq!(faulted.attention, Some(Code::CollectorCadenceInvalid));
        assert_eq!(
            as_service(&faulted),
            service_expected,
            "{lease_case}: cadence is attention only"
        );
        store.set_stream_audit_failed_cars(0);
        assert_eq!(service(&store), service_expected);
        // An identified unresolved Fleet refresh, under the same lease: the
        // registry order is cadence, Owner or provider refusal, unknown, then the
        // other C3 stops, and the service reason stays the lease's own.
        use crate::config::CollectorMode::FleetPolling;
        as_unresolved_fleet(&store);
        let fleet = |cadence: Option<crate::config::CadenceViolation>| {
            as_view_in(&store, now, FleetPolling, cadence)
        };
        for state in [
            "collecting",
            "fleet_access_refused",
            "billing_suspended",
            "cap_reached",
            "config_error",
            "account_locked",
        ] {
            as_account(&store, "fleet_polling", state);
            let view = fleet(None);
            assert_eq!(
                view.attention,
                Some(Code::CollectorFleetRefreshOutcomeUnknown),
                "{lease_case}: unknown over {state}"
            );
            assert_eq!(as_service(&view), service_expected, "{lease_case} {state}");
        }
        for (state, code) in [
            ("owner_api_refused", Code::CollectorApiRefused),
            ("provider_refused", Code::CollectorProviderRefused),
        ] {
            as_account(&store, "fleet_polling", state);
            let view = fleet(None);
            assert_eq!(
                view.attention,
                Some(code),
                "{lease_case}: {state} over unknown"
            );
            assert_eq!(as_service(&view), service_expected, "{lease_case} {state}");
        }
        let cadence = fleet(Some(as_cadence_fault()));
        assert_eq!(
            cadence.attention,
            Some(Code::CollectorCadenceInvalid),
            "{lease_case}"
        );
        assert_eq!(
            as_service(&cadence),
            service_expected,
            "{lease_case}: cadence beside unknown"
        );
        // The stream audit failure ranks below the unknown refresh.
        as_account(&store, "fleet_polling", "collecting");
        store.set_stream_audit_failed_cars(1);
        assert_eq!(
            fleet(None).attention,
            Some(Code::CollectorFleetRefreshOutcomeUnknown)
        );
        store.set_stream_audit_failed_cars(0);
    }
}

#[test]
fn cadence_service_failures_keep_their_precedence() {
    use crate::config::CollectorMode::Legacy;
    let (_temporary, store) = as_store();
    let fault = Some(as_cadence_fault());
    // Absent, invalid, expired and terminal leases keep their original reasons;
    // the fault is attention beside each.
    let view = as_view_in(&store, 1_001, Legacy, fault.clone());
    assert_eq!(
        as_service(&view),
        Some(ReadinessReasonCode::CollectorAbsent)
    );
    assert_eq!(
        view.attention,
        Some(CollectionAttentionCode::CollectorCadenceInvalid)
    );
    store
        .open()
        .unwrap()
        .execute(
            "INSERT INTO supervised_collector_lease(singleton_id, instance_id, state,
                 started_at_ms, heartbeat_at_ms, lease_until_ms)
             VALUES (1, ?1, 'active', 40, 50, 90)",
            params![Uuid::nil().to_string()],
        )
        .unwrap();
    assert_eq!(
        as_service(&as_view_in(&store, 60, Legacy, fault.clone())),
        Some(ReadinessReasonCode::CatalogueUnavailable)
    );
    store
        .open()
        .unwrap()
        .execute("DELETE FROM supervised_collector_lease", [])
        .unwrap();
    let lease = store.acquire_supervised_collector_lease(10_000).unwrap();
    // A fresh wait alone is ready, with the fault as attention.
    let fresh = as_view_in(&store, 10_001, Legacy, fault.clone());
    assert_eq!(as_service(&fresh), None);
    assert_eq!(
        fresh.attention,
        Some(CollectionAttentionCode::CollectorCadenceInvalid)
    );
    assert!(fresh.running);
    let expired = as_view_in(
        &store,
        10_000 + SUPERVISED_COLLECTOR_LEASE_MS,
        Legacy,
        fault.clone(),
    );
    assert_eq!(
        as_service(&expired),
        Some(ReadinessReasonCode::CollectorStale)
    );
    store
        .heartbeat_supervised_collector_lease(
            lease,
            SupervisedCollectorState::AuthenticationTerminal,
            11_000,
        )
        .unwrap();
    assert_eq!(
        as_service(&as_view_in(&store, 11_001, Legacy, fault)),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
}

#[test]
fn cadence_does_not_mask_signed_out_or_revive_legacy_auth_in_fleet() {
    use crate::config::CollectorMode::{FleetPolling, Legacy};
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    // A refreshed pair whose access token is usable until 1_800_000_100.
    let usable_until = 1_800_000_100;
    let generation = as_real_pair(
        &temporary,
        &store,
        &as_jwt(usable_until + 300),
        Some((usable_until, usable_until - 500)),
    );
    // A lease renewed just before the bound, so only the credential can change.
    store
        .acquire_supervised_collector_lease(usable_until * 1_000 - 10_000)
        .unwrap();
    store
        .write_collector_auth_row(
            &CollectorAuthRow {
                stopped_generation: Some(generation),
                ..as_row("sign_in_required", Some("refresh_token_rejected"), true)
            },
            1_800_000_000_000,
        )
        .unwrap();
    let fault = Some(as_cadence_fault());
    let at = |ms: i64, mode| as_view_in(&store, ms, mode, fault.clone());
    // Before the bound: ready, the cadence fault outranks the sign-in attention.
    let before = at(usable_until * 1_000 - 1, Legacy);
    assert_eq!(as_service(&before), None);
    assert_eq!(
        before.attention,
        Some(CollectionAttentionCode::CollectorCadenceInvalid)
    );
    assert_eq!(before.service_valid_until_ms, Some(usable_until * 1_000));
    // At equality the account is effectively signed out: a service failure that
    // the cadence attention does not mask.
    let equal = at(usable_until * 1_000, Legacy);
    assert_eq!(
        as_service(&equal),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
    assert_eq!(
        equal.attention,
        Some(CollectionAttentionCode::CollectorCadenceInvalid)
    );
    // No write: the stored row still says sign_in_required.
    assert_eq!(
        store.load_collector_auth_row().unwrap().unwrap().state,
        "sign_in_required"
    );
    // Configured Fleet ignores the retained Legacy status entirely, whatever the
    // account row says, and the retained status comes back when the mode does.
    as_account(&store, "legacy", "collecting");
    let fleet = at(usable_until * 1_000, FleetPolling);
    assert_eq!(as_service(&fleet), None);
    assert_eq!(
        fleet.attention,
        Some(CollectionAttentionCode::CollectorCadenceInvalid)
    );
    assert_eq!(fleet.service_valid_until_ms, None);
    assert_eq!(
        as_service(&at(usable_until * 1_000, Legacy)),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
}

#[test]
fn cs_p1_c5_mode_gates_all_readiness_tiers() {
    use crate::config::CollectorMode::{FleetPolling, Legacy, Provider};
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let generation = as_real_pair(&temporary, &store, "access", None);
    let lease = store
        .acquire_supervised_collector_lease(1_800_000_000_000)
        .unwrap();
    let now = 1_800_000_001_000;
    let write = |row: CollectorAuthRow| store.write_collector_auth_row(&row, now).unwrap();
    // Provider mode compares the stored refusal's mode, so a Legacy refusal does
    // not apply to a loaded Provider configuration, and the reverse.
    let refused = |mode: &str| CollectorAuthRow {
        stopped_mode: Some(mode.to_owned()),
        ..as_row("api_refused", Some("owner_api"), false)
    };
    let waiting = as_row("observer_waiting", Some("token_expired"), false);
    let signed_out = CollectorAuthRow {
        stopped_generation: Some(generation),
        ..as_row("signed_out", Some("access_token_rejected"), false)
    };
    for stored_mode in [
        Some("legacy"),
        Some("provider"),
        Some("fleet_polling"),
        None,
    ] {
        match stored_mode {
            Some(mode) => as_account(&store, mode, "collecting"),
            None => {
                // No account row at all: delete the one an earlier pass wrote.
                store
                    .open()
                    .unwrap()
                    .execute("DELETE FROM collector_account_state", [])
                    .unwrap();
                assert!(
                    store
                        .open()
                        .unwrap()
                        .query_row("SELECT COUNT(*) FROM collector_account_state", [], |row| {
                            row.get::<_, i64>(0)
                        })
                        .unwrap()
                        == 0,
                    "the missing-row pass has no account row"
                );
            }
        }
        for loaded in [Legacy, Provider, FleetPolling] {
            let view = |ms: i64| as_view_in(&store, ms, loaded, None);
            // A waiting unit: attention only for Legacy and Provider, never Fleet.
            write(waiting.clone());
            let want =
                (loaded != FleetPolling).then_some(CollectionAttentionCode::CollectorAuthWaiting);
            assert_eq!(
                view(now).attention,
                want,
                "wait {loaded:?} stored {stored_mode:?}"
            );
            // A signed-out unit fails service for Legacy and Provider, never for Fleet.
            write(signed_out.clone());
            let want =
                (loaded != FleetPolling).then_some(ReadinessReasonCode::CollectorAuthTerminal);
            assert_eq!(
                as_service(&view(now)),
                want,
                "signed out {loaded:?} stored {stored_mode:?}"
            );
            // The refusal belongs to its own mode: it adds no C5 attention (C3
            // raises its own refusal code), but whether it applies differs per
            // loaded mode, in the effective status every surface shows.
            for refusal_mode in ["legacy", "provider"] {
                write(refused(refusal_mode));
                assert_eq!(
                    view(now).attention,
                    None,
                    "api_refused adds no C5 attention"
                );
                let effective = store
                    .effective_collector_auth(loaded, now)
                    .unwrap()
                    .map(|effective| effective.row.state);
                let want = match (loaded, refusal_mode) {
                    (FleetPolling, _) => None,
                    (Legacy, "legacy") | (Provider, "provider") => Some("api_refused"),
                    _ => Some("ready"),
                };
                assert_eq!(
                    effective.as_deref(),
                    want,
                    "refusal of {refusal_mode} under loaded {loaded:?}, stored {stored_mode:?}"
                );
            }
        }
    }
    // The context-free fallback reads the stored account mode instead: Fleet
    // ignores the retained status, Legacy, Provider and no row consult it.
    write(signed_out.clone());
    for (stored_mode, applies) in [
        (Some("legacy"), true),
        (Some("provider"), true),
        (Some("fleet_polling"), false),
    ] {
        as_account(&store, stored_mode.unwrap(), "collecting");
        let free = as_service(&as_view(&store, now));
        assert_eq!(
            free.is_some(),
            applies,
            "context-free, stored {stored_mode:?}"
        );
    }
    store
        .open()
        .unwrap()
        .execute("DELETE FROM collector_account_state", [])
        .unwrap();
    assert_eq!(
        as_readiness(&store, now),
        Some(ReadinessReasonCode::CollectorAuthTerminal),
        "no account row: the historical fallback consults the status"
    );
    // Even loaded Fleet reports a terminal lease: the lease is not the status.
    store
        .heartbeat_supervised_collector_lease(
            lease,
            SupervisedCollectorState::AuthenticationTerminal,
            now,
        )
        .unwrap();
    assert_eq!(
        as_service(&as_view_in(&store, now + 1, FleetPolling, None)),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
}

#[test]
fn cs_p1_signed_out_and_refusal_use_service_and_attention() {
    use crate::config::CollectorMode::Legacy;
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let generation = as_real_pair(&temporary, &store, "access", None);
    store
        .acquire_supervised_collector_lease(1_800_000_000_000)
        .unwrap();
    let now = 1_800_000_001_000;
    as_account(&store, "legacy", "owner_api_refused");
    store
        .write_collector_auth_row(
            &CollectorAuthRow {
                stopped_generation: Some(generation),
                ..as_row("signed_out", Some("access_token_rejected"), false)
            },
            now,
        )
        .unwrap();
    // A refusal beside a signed-out unit: terminal service, refusal attention.
    let view = as_view_in(&store, now, Legacy, None);
    assert_eq!(
        as_service(&view),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
    assert_eq!(
        view.attention,
        Some(CollectionAttentionCode::CollectorApiRefused)
    );
    // A usable sign-in beside the refusal: ready, and the refusal is the attention.
    store
        .write_collector_auth_row(
            &CollectorAuthRow {
                stopped_generation: Some(generation),
                ..as_row("sign_in_required", Some("refresh_token_rejected"), true)
            },
            now,
        )
        .unwrap();
    let view = as_view_in(&store, now, Legacy, None);
    assert_eq!(as_service(&view), None);
    assert_eq!(
        view.attention,
        Some(CollectionAttentionCode::CollectorApiRefused)
    );
}

#[test]
fn fleet_unknown_refresh_attention_has_fixed_combination_precedence() {
    use crate::config::CollectorMode::FleetPolling;
    use CollectionAttentionCode as Code;
    let (_temporary, store) = as_store();
    store.acquire_supervised_collector_lease(1_000).unwrap();
    let now = 1_001;
    // An identified unresolved Fleet refresh alone.
    as_account(&store, "fleet_polling", "collecting");
    assert_eq!(as_view_in(&store, now, FleetPolling, None).attention, None);
    as_unresolved_fleet(&store);
    let fleet = |cadence: Option<crate::config::CadenceViolation>| {
        as_view_in(&store, now, FleetPolling, cadence)
    };
    let alone = fleet(None);
    assert_eq!(
        alone.attention,
        Some(Code::CollectorFleetRefreshOutcomeUnknown)
    );
    assert_eq!(
        as_service(&alone),
        None,
        "attention is not a service failure"
    );
    assert!(
        alone.detail.iter().all(|word| !word.contains("refresh")),
        "no duplicate healthy detail"
    );
    // Unknown outranks cap, billing and Fleet access refusal.
    for state in [
        "cap_reached",
        "billing_suspended",
        "fleet_access_refused",
        "config_error",
        "account_locked",
    ] {
        as_account(&store, "fleet_polling", state);
        assert_eq!(
            fleet(None).attention,
            Some(Code::CollectorFleetRefreshOutcomeUnknown),
            "unknown over {state}"
        );
    }
    // Owner and provider refusal outrank unknown; cadence outranks them all.
    as_account(&store, "fleet_polling", "owner_api_refused");
    assert_eq!(fleet(None).attention, Some(Code::CollectorApiRefused));
    as_account(&store, "fleet_polling", "provider_refused");
    assert_eq!(fleet(None).attention, Some(Code::CollectorProviderRefused));
    assert_eq!(
        fleet(Some(as_cadence_fault())).attention,
        Some(Code::CollectorCadenceInvalid)
    );
    as_account(&store, "fleet_polling", "cap_reached");
    assert_eq!(
        fleet(Some(as_cadence_fault())).attention,
        Some(Code::CollectorCadenceInvalid)
    );
    // Service failures stay independent: a terminal lease still fails.
    let lease = store.supervised_collector_lease_status().unwrap().unwrap();
    assert_eq!(lease.started_at_ms, 1_000);
    assert_eq!(
        as_service(&as_view_in(
            &store,
            1_000 + SUPERVISED_COLLECTOR_LEASE_MS,
            FleetPolling,
            None
        )),
        Some(ReadinessReasonCode::CollectorStale)
    );
    // Only the identified condition counts: a started receipt bound to another
    // generation, or the same pair without its fence, is not it.
    let other = Uuid::from_u128(0xBEEF);
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE credential_refresh_attempts SET input_generation = ?1 WHERE slot = 'fleet_api'",
            params![other.to_string()],
        )
        .unwrap();
    assert!(!store.identified_unresolved_fleet_refresh().unwrap());
    assert!(
        s_test_with_credential_connection(&store, |connection| connection
            .has_unresolved_credential_refresh(CredentialSlot::FleetApi))
        .unwrap()
    );
    as_account(&store, "fleet_polling", "collecting");
    assert_eq!(fleet(None).attention, None);
}

#[test]
fn readiness_context_uses_loaded_mode_with_stale_or_missing_account_rows() {
    use crate::config::CollectorMode::{FleetPolling, Legacy};
    let temporary = crate::private_tempdir().expect("temporary store");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let generation = as_real_pair(&temporary, &store, "access", None);
    let lease = store
        .acquire_supervised_collector_lease(1_800_000_000_000)
        .unwrap();
    let now = 1_800_000_001_000;
    store
        .write_collector_auth_row(
            &CollectorAuthRow {
                stopped_generation: Some(generation),
                ..as_row("signed_out", Some("access_token_rejected"), false)
            },
            now,
        )
        .unwrap();
    // Fleet configured, the account row still says Legacy: Fleet governs.
    as_account(&store, "legacy", "collecting");
    assert_eq!(
        as_service(&as_view_in(&store, now, FleetPolling, None)),
        None
    );
    // Fleet configured, no account row at all: Fleet governs again, while the
    // context-free read keeps the historical fallback and consults the status.
    store
        .open()
        .unwrap()
        .execute("DELETE FROM collector_account_state", [])
        .unwrap();
    assert_eq!(
        as_service(&as_view_in(&store, now, FleetPolling, None)),
        None
    );
    assert_eq!(
        as_readiness(&store, now),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
    // The inverse: Legacy configured over a Fleet account row consults the status.
    as_account(&store, "fleet_polling", "collecting");
    assert_eq!(
        as_service(&as_view_in(&store, now, Legacy, None)),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
    assert_eq!(
        as_readiness(&store, now),
        None,
        "context-free: the stored mode is Fleet"
    );
    // Authentication time is dynamic: a sign-in whose token is usable at one
    // evaluation time is signed out at a later one, with no write between.
    let usable_until = 1_800_000_100;
    let g2 = as_real_pair(
        &temporary,
        &store,
        &as_jwt(usable_until + 300),
        Some((usable_until, usable_until - 500)),
    );
    store
        .write_collector_auth_row(
            &CollectorAuthRow {
                stopped_generation: Some(g2),
                ..as_row("sign_in_required", Some("refresh_token_rejected"), true)
            },
            now,
        )
        .unwrap();
    store
        .heartbeat_supervised_collector_lease(
            lease,
            SupervisedCollectorState::Active,
            usable_until * 1_000 - 5_000,
        )
        .unwrap();
    assert_eq!(
        as_service(&as_view_in(&store, usable_until * 1_000 - 1, Legacy, None)),
        None
    );
    assert_eq!(
        as_service(&as_view_in(&store, usable_until * 1_000, Legacy, None)),
        Some(ReadinessReasonCode::CollectorAuthTerminal)
    );
}

// ---------------------------------------------------------------------------
// Reads of the pair row and the fencing tables (R44a, R58, R62)
// ---------------------------------------------------------------------------

#[test]
fn the_stored_generation_is_read_without_decrypting_the_row() {
    let (_temporary, store) = as_store();
    assert_eq!(
        store.stored_legacy_credential_generation().unwrap(),
        StoredLegacyGeneration::NoRow
    );
    as_put_pair(&store, None);
    assert_eq!(
        store.stored_legacy_credential_generation().unwrap(),
        StoredLegacyGeneration::Unbound
    );
    as_put_pair(&store, Some(as_gen(4)));
    assert_eq!(
        store.stored_legacy_credential_generation().unwrap(),
        StoredLegacyGeneration::Bound(as_gen(4))
    );
}

#[test]
fn a_generation_is_a_refresh_output_only_when_a_completed_binding_produced_it() {
    let (_temporary, store) = as_store();
    let (g0, g1, unrelated) = (as_gen(1), as_gen(2), as_gen(3));
    as_put_pair(&store, Some(g0));
    assert!(
        !s_test_with_credential_connection(&store, |connection| connection
            .generation_is_refresh_output(CredentialSlot::OwnerApi, g0))
        .unwrap()
    );
    let receipt = s_test_with_credential_connection(&store, |connection| {
        let receipt = connection
            .begin_credential_refresh(CredentialSlot::OwnerApi, g0)
            .expect("begin output classification control");
        connection
            .credential_durability_barrier()
            .expect("output classification begin barrier");
        receipt
    });
    let started = s_test_with_credential_connection(&store, |connection| {
        let output: Option<String> = connection
            .test_connection()
            .query_row(
                "SELECT output_generation FROM credential_refresh_attempts WHERE receipt_id=?1",
                [receipt.0],
                |row| row.get(0),
            )
            .unwrap();
        (
            output,
            [g0, g1, unrelated].map(|generation| {
                connection
                    .generation_is_refresh_output(CredentialSlot::OwnerApi, generation)
                    .unwrap()
            }),
        )
    });
    assert_eq!(
        started,
        (None, [false, false, false]),
        "INTENT-OUTPUT-01 started attempt has no output or classified generation"
    );
    s_test_with_credential_connection(&store, |connection| {
        let refresh = format!("synthetic-refresh-{g1}");
        let successor = s_test_synthetic_pair(
            connection,
            CredentialSlot::OwnerApi,
            g1,
            b"synthetic-successor-access",
            refresh.as_bytes(),
            AS_T0 + 28_800,
            AS_T0 + 21_600,
        )
        .expect("output classification successor");
        connection
            .complete_credential_refresh(receipt, g0, &successor, &LegacyCandidates(Vec::new()))
            .expect("complete output classification control");
        connection
            .credential_durability_barrier()
            .expect("output classification completion barrier");
    });
    assert!(
        s_test_with_credential_connection(&store, |connection| connection
            .generation_is_refresh_output(CredentialSlot::OwnerApi, g1))
        .unwrap()
    );
    assert!(
        !s_test_with_credential_connection(&store, |connection| connection
            .generation_is_refresh_output(CredentialSlot::OwnerApi, g0))
        .unwrap(),
        "INTENT-OUTPUT-02 completed binding classifies its output and excludes its input"
    );
    assert!(
        !s_test_with_credential_connection(&store, |connection| connection
            .generation_is_refresh_output(CredentialSlot::OwnerApi, unrelated))
        .unwrap()
    );
}

#[test]
fn the_latest_token_receipt_start_is_read_whatever_its_outcome() {
    let (_temporary, store) = as_store();
    assert_eq!(store.latest_legacy_refresh_started_at_ms().unwrap(), None);
    let insert = |started: i64, transport: &str, operation: &str, outcome: &str| {
        let completed = (outcome != "started").then_some(started + 10);
        let duration = (outcome != "started").then_some(10_i64);
        store
            .open()
            .unwrap()
            .execute(
                "INSERT INTO outbound_request_receipts(
                    correlation_id, started_at_ms, completed_at_ms, duration_ms, vehicle_tesla_id,
                    transport, operation, safety_class, precondition, outcome)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, 'non_wake_endpoint', 'not_required', ?7)",
                params![
                    Uuid::new_v4().to_string(),
                    started,
                    completed,
                    duration,
                    transport,
                    operation,
                    outcome
                ],
            )
            .unwrap();
    };
    insert(5_000, "owner_api", "products", "success");
    assert_eq!(
        store.latest_legacy_refresh_started_at_ms().unwrap(),
        None,
        "not a token receipt"
    );
    insert(7_000, "legacy_auth", "token_refresh", "http_error");
    insert(6_000, "legacy_auth", "token_refresh", "success");
    assert_eq!(
        store.latest_legacy_refresh_started_at_ms().unwrap(),
        Some(7_000)
    );
    insert(8_000, "legacy_auth", "token_refresh", "started");
    insert(9_000, "stream", "stream_connect", "success");
    assert_eq!(
        store.latest_legacy_refresh_started_at_ms().unwrap(),
        Some(8_000)
    );
}

// ---------------------------------------------------------------------------
// The Owner-wire credential identity (B2 clarification of 2026-10-12)
// ---------------------------------------------------------------------------

#[test]
fn the_credential_identity_is_the_generation_of_the_last_external_write() {
    let (_temporary, store) = as_store();
    let identity = || {
        (
            store.owner_wire_credential_identity().unwrap(),
            store.collector_credential_identity(false).unwrap(),
            store.collector_credential_identity(true).unwrap(),
        )
    };
    assert_eq!(
        identity(),
        (String::new(), String::new(), String::new()),
        "INTENT-IDENTITY-01 empty Owner and both collector modes have empty identities"
    );
    let put_fleet = |generation: Uuid, client: &str, region: &str| {
        s_test_with_credential_connection(&store, |connection| {
            let refresh = format!("synthetic-fleet-refresh-{generation}");
            let mut pair = s_test_synthetic_pair(
                connection,
                CredentialSlot::FleetApi,
                generation,
                b"synthetic-fleet-access",
                refresh.as_bytes(),
                AS_T0 + 28_800,
                AS_T0 + 21_600,
            )
            .expect("identity Fleet external pair");
            pair.client_id = Some(client.to_string());
            pair.region = Some(region.to_string());
            connection
                .replace_credential_pair(
                    &pair,
                    &EntryProvenance::Supplied,
                    &LegacyCandidates(Vec::new()),
                    ReplacePolicy::ReplaceAny,
                )
                .expect("identity Fleet external write");
            connection
                .credential_durability_barrier()
                .expect("identity Fleet write barrier");
        });
    };
    let refresh_fleet = |input: Uuid, output: Uuid| {
        s_test_with_credential_connection(&store, |connection| {
            let current = connection
                .load_credential_pair(CredentialSlot::FleetApi)
                .unwrap()
                .unwrap();
            let refresh = format!("synthetic-fleet-refresh-{output}");
            let mut successor = s_test_synthetic_pair(
                connection,
                CredentialSlot::FleetApi,
                output,
                b"synthetic-fleet-successor-access",
                refresh.as_bytes(),
                AS_T0 + 57_600,
                AS_T0 + 50_400,
            )
            .expect("identity Fleet successor");
            successor.client_id = current.client_id;
            successor.region = current.region;
            let receipt = connection
                .begin_credential_refresh(CredentialSlot::FleetApi, input)
                .expect("identity Fleet begin");
            connection
                .credential_durability_barrier()
                .expect("identity Fleet begin barrier");
            connection
                .complete_credential_refresh(
                    receipt,
                    input,
                    &successor,
                    &LegacyCandidates(Vec::new()),
                )
                .expect("identity Fleet complete");
            connection
                .credential_durability_barrier()
                .expect("identity Fleet completion barrier");
        });
    };
    let (g0, g1, g2, g9, g10) = (as_gen(1), as_gen(2), as_gen(3), as_gen(9), as_gen(10));
    let (f0, f1, f2, f9, f10) = (as_gen(11), as_gen(12), as_gen(13), as_gen(19), as_gen(20));
    as_put_pair(&store, Some(g0));
    let (_owner_only_directory, owner_only) = as_store();
    let owner_external = as_gen(21);
    as_put_pair(&owner_only, Some(owner_external));
    as_refresh(&owner_only, owner_external, as_gen(22));
    assert_eq!(
        (
            owner_only.owner_wire_credential_identity().unwrap(),
            owner_only.collector_credential_identity(false).unwrap(),
            owner_only.collector_credential_identity(true).unwrap(),
        ),
        (
            owner_external.to_string(),
            owner_external.to_string(),
            String::new(),
        ),
        "INTENT2-IDENTITY-01 Owner-only credentials keep Owner identity while Fleet collector identity is empty"
    );
    put_fleet(f0, "Stored-App-A", "eu");
    assert_eq!(
        store.owner_wire_credential_identity().unwrap(),
        g0.to_string()
    );
    let mut snapshots = vec![("external", identity())];
    as_refresh(&store, g0, g1);
    refresh_fleet(f0, f1);
    assert_eq!(
        store.owner_wire_credential_identity().unwrap(),
        g0.to_string()
    );
    snapshots.push(("first refresh", identity()));
    as_refresh(&store, g1, g2);
    refresh_fleet(f1, f2);
    snapshots.push(("second refresh", identity()));
    as_put_pair(&store, Some(g9));
    put_fleet(f9, "Stored-App-B", "na");
    assert_eq!(
        store.owner_wire_credential_identity().unwrap(),
        g9.to_string()
    );
    snapshots.push(("later external", identity()));
    as_refresh(&store, g9, g10);
    refresh_fleet(f9, f10);
    snapshots.push(("later refresh", identity()));
    let unreadable = s_test_with_credential_connection(&store, |connection| {
        [CredentialSlot::OwnerApi, CredentialSlot::FleetApi].map(|slot| {
            let mut pair = connection.load_credential_pair(slot).unwrap().unwrap();
            // Deliberately unreadable synthetic envelopes preserve the clear
            // metadata and lengths; identity must not depend on their decoding.
            pair.access_envelope.fill(0);
            pair.refresh_envelope.fill(0);
            connection
                .test_connection()
                .execute(
                    "UPDATE credential_pairs SET access_envelope=?1,refresh_envelope=?2 WHERE slot=?3",
                    params![pair.access_envelope, pair.refresh_envelope, slot.as_str()],
                )
                .unwrap();
            crate::auth::credential_envelope::parse_header(
                pair.access_envelope[..39].try_into().unwrap(),
            )
            .is_err()
                && crate::auth::credential_envelope::parse_header(
                    pair.refresh_envelope[..39].try_into().unwrap(),
                )
                .is_err()
        })
    });
    snapshots.push(("unreadable ciphertext", identity()));
    let first = (
        g0.to_string(),
        g0.to_string(),
        "Stored-App-A|eu".to_string(),
    );
    let later = (
        g9.to_string(),
        g9.to_string(),
        "Stored-App-B|na".to_string(),
    );
    assert_eq!(
        (snapshots, unreadable),
        (
            vec![
                ("external", first.clone()),
                ("first refresh", first.clone()),
                ("second refresh", first),
                ("later external", later.clone()),
                ("later refresh", later.clone()),
                ("unreadable ciphertext", later),
            ],
            [true, true],
        ),
        "INTENT-IDENTITY-02 both modes preserve external identity through refresh chains and unreadable ciphertext"
    );
}

/// Make every receipt `days` old by the real clock, which retention reads
/// (`OUTBOUND_REQUEST_RETENTION_MS`), keeping the table's own checks true.
fn as_age_receipts(store: &HubStore, days: i64) {
    let now_ms = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let started = now_ms - days * 24 * 60 * 60 * 1_000;
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE outbound_request_receipts
                SET started_at_ms = ?1,
                    completed_at_ms = CASE WHEN outcome = 'started' THEN NULL ELSE ?1 + 5 END,
                    duration_ms = CASE WHEN outcome = 'started' THEN NULL ELSE 5 END",
            params![started],
        )
        .unwrap();
}

/// Run the retention step the way production reaches it: reserving the next
/// token attempt prunes expired receipts first, and cancelling it frees the
/// input again.
fn as_run_retention(store: &HubStore, current: Uuid) {
    s_test_with_credential_connection(store, |connection| {
        let receipt = connection
            .begin_credential_refresh(CredentialSlot::OwnerApi, current)
            .expect("begin refresh through real retention path");
        connection
            .credential_durability_barrier()
            .expect("begin durability barrier");
        connection
            .cancel_unsent_credential_refresh(receipt, current)
            .expect("cancel unsent retention control");
        connection
            .credential_durability_barrier()
            .expect("cancellation durability barrier");
    });
}

fn as_completed_token_receipts(store: &HubStore) -> i64 {
    store
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM credential_refresh_attempts
              WHERE slot = 'owner_api' AND output_generation IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn the_credential_identity_survives_the_retention_of_old_refresh_receipts() {
    // C5-B1: the identity is read by following completed bindings back from the
    // stored generation. A binding is deleted with its receipt, so receipts that
    // retention removed would make the identity jump to the first refresh that
    // survived, and C3's fingerprint would change about four times a day.
    let (_temporary, store) = as_store();
    let (g0, g1, g2, g3) = (as_gen(1), as_gen(2), as_gen(3), as_gen(4));
    as_put_pair(&store, Some(g0));
    as_refresh(&store, g0, g1);
    as_refresh(&store, g1, g2);
    assert_eq!(
        store.owner_wire_credential_identity().unwrap(),
        g0.to_string()
    );
    // Every receipt is now past retention (31 days against 30), and the next
    // reservation prunes. The chain behind the stored pair stays.
    as_age_receipts(&store, 31);
    as_run_retention(&store, g2);
    assert_eq!(
        as_completed_token_receipts(&store),
        2,
        "the chain behind the stored pair is kept"
    );
    assert_eq!(
        store.owner_wire_credential_identity().unwrap(),
        g0.to_string()
    );
    assert!(store.legacy_generation_is_refresh_output(g1).unwrap());
    assert!(store.legacy_generation_is_refresh_output(g2).unwrap());
    // The fuse restore and the adoption of a refresh written under the unit
    // (R57 step 7, R62) read the same bindings and still tell the unit's own
    // pair from an external one.
    assert!(!store.legacy_generation_is_refresh_output(g3).unwrap());
    // A later refresh extends the chain and the identity is still the first.
    as_refresh(&store, g2, g3);
    as_age_receipts(&store, 31);
    as_run_retention(&store, g3);
    assert_eq!(as_completed_token_receipts(&store), 3);
    assert_eq!(
        store.owner_wire_credential_identity().unwrap(),
        g0.to_string()
    );
}

#[test]
fn retention_still_removes_receipts_that_no_stored_pair_depends_on() {
    // The exemption is the live chain only: once an external write stores another
    // pair the old chain is unreachable, and it expires with every other receipt.
    let (_temporary, store) = as_store();
    let (g0, g1, g9) = (as_gen(1), as_gen(2), as_gen(9));
    as_put_pair(&store, Some(g0));
    as_refresh(&store, g0, g1);
    // An unrelated old receipt, and one that is not old.
    let connection = store.open().unwrap();
    for operation in ["products", "vehicle_probe"] {
        connection
            .execute(
                "INSERT INTO outbound_request_receipts(
                    correlation_id, started_at_ms, completed_at_ms, duration_ms, vehicle_tesla_id,
                    transport, operation, safety_class, precondition, outcome, http_status)
                 VALUES (?1, 1, 2, 1, NULL, 'owner_api', ?2,
                         'non_wake_endpoint', 'not_required', 'success', 200)",
                params![Uuid::new_v4().to_string(), operation],
            )
            .unwrap();
    }
    drop(connection);
    as_age_receipts(&store, 31);
    as_put_pair(&store, Some(g9));
    as_run_retention(&store, g9);
    assert_eq!(
        as_completed_token_receipts(&store),
        0,
        "the replaced chain expired"
    );
    let remaining: i64 = store
        .open()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM outbound_request_receipts",
            [],
            |row| row.get(0),
        )
        .unwrap();
    // The aborted reservation's own receipt is the only one left.
    assert_eq!(remaining, 1);
    assert_eq!(
        store.owner_wire_credential_identity().unwrap(),
        g9.to_string()
    );
}

#[test]
fn a_damaged_binding_chain_is_an_error_and_never_a_guess() {
    let (_temporary, store) = as_store();
    let (g0, g1, g2) = (as_gen(1), as_gen(2), as_gen(3));
    as_put_pair(&store, Some(g0));
    as_refresh(&store, g0, g1);
    as_refresh(&store, g1, g2);
    store
        .open()
        .unwrap()
        .execute(
            "UPDATE credential_refresh_attempts SET input_generation = ?1
              WHERE slot = 'owner_api' AND input_generation = ?2",
            params![g2.to_string(), g0.to_string()],
        )
        .unwrap();
    assert!(matches!(
        store.owner_wire_credential_identity(),
        Err(StoreError::CredentialPairInvalid)
    ));
}
