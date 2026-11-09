// SPDX-License-Identifier: AGPL-3.0-only
use super::*;
use std::cell::Cell;

const PLAN_NOW: i64 = T0_MS + 50;

#[track_caller]
fn count(f: &Fixture, table: &str) -> i64 {
    f.conn
        .test_connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap_or_else(|error| panic!("count synthetic table {table}: {error}"))
}

fn total_changes(f: &Fixture) -> i64 {
    f.conn
        .test_connection()
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap()
}

fn seed_binding(
    f: &Fixture,
    slot: CredentialSlot,
    id: i64,
    attempt: Uuid,
    input: Uuid,
    output: Option<Uuid>,
    started: i64,
    completed: bool,
) -> CredentialMigrationAttempt {
    let fleet = slot == CredentialSlot::FleetApi;
    let db = f.conn.test_connection();
    db.execute(
        "INSERT INTO outbound_request_receipts(id,correlation_id,started_at_ms,
         completed_at_ms,duration_ms,transport,operation,safety_class,precondition,outcome)
         VALUES(?1,?2,?3,?4,?5,?6,'token_refresh','non_wake_endpoint','not_required',?7)",
        params![
            id,
            generation(40_000 + id as u128).to_string(),
            started,
            completed.then_some(started + 10),
            completed.then_some(10_i64),
            if fleet { "fleet_api" } else { "legacy_auth" },
            if completed { "success" } else { "started" }
        ],
    )
    .unwrap();
    let table = if fleet {
        "fleet_refresh_receipt_bindings"
    } else {
        "legacy_refresh_receipt_bindings"
    };
    db.execute(
        &format!(
            "INSERT INTO {table}(receipt_id,attempt_id,input_credential_generation,
             output_credential_generation) VALUES(?1,?2,?3,?4)"
        ),
        params![
            id,
            attempt.to_string(),
            input.to_string(),
            output.map(|value| value.to_string())
        ],
    )
    .unwrap();
    CredentialMigrationAttempt {
        receipt_id: OutboundRequestReceiptId(id),
        attempt_id: attempt,
        slot,
        input_generation: input,
        output_generation: output,
    }
}

fn source_fence(f: &Fixture, slot: CredentialSlot, id: Uuid, uppercase: bool) {
    let table = if slot == CredentialSlot::OwnerApi {
        "legacy_refresh_input_fences"
    } else {
        "fleet_refresh_input_fences"
    };
    let text = if uppercase {
        id.to_string().to_uppercase()
    } else {
        id.to_string()
    };
    f.conn
        .test_connection()
        .execute(
            &format!("INSERT INTO {table}(input_credential_generation) VALUES(?1)"),
            [text],
        )
        .unwrap();
}

fn initial_history(f: &Fixture) -> Vec<CredentialMigrationAttempt> {
    let shared = generation(8_001);
    let owner = seed_binding(
        f,
        CredentialSlot::OwnerApi,
        10,
        shared,
        generation(101),
        None,
        T0_MS - 200,
        false,
    );
    let mut fleet = seed_binding(
        f,
        CredentialSlot::FleetApi,
        20,
        shared,
        generation(101),
        Some(generation(202)),
        T0_MS - 100,
        true,
    );
    // R47 preserves the Fleet binding as a second row with a distinct v4 ID.
    fleet.attempt_id = Uuid::new_v4();
    let nullable_success = seed_binding(
        f,
        CredentialSlot::OwnerApi,
        30,
        generation(8_003),
        generation(202),
        None,
        T0_MS - 300,
        true,
    );
    for slot in [CredentialSlot::OwnerApi, CredentialSlot::FleetApi] {
        source_fence(f, slot, generation(101), false);
        source_fence(f, slot, generation(202), slot == CredentialSlot::OwnerApi);
    }
    vec![owner, fleet, nullable_success]
}

fn legacy_set(owner_anchored: bool, fleet_anchored: bool) -> Vec<CredentialMigrationLegacyFence> {
    vec![
        CredentialMigrationLegacyFence {
            generation: generation(101),
            identity: if owner_anchored {
                LegacyFenceIdentity::Anchored
            } else {
                LegacyFenceIdentity::Opaque
            },
            fenced_at_ms: T0_MS - 200,
        },
        CredentialMigrationLegacyFence {
            generation: generation(202),
            identity: if fleet_anchored {
                LegacyFenceIdentity::Anchored
            } else {
                LegacyFenceIdentity::Opaque
            },
            fenced_at_ms: T0_MS - 300,
        },
    ]
}

fn refresh_set(pairs: &[SealedCredentialPair]) -> Vec<CredentialMigrationRefreshFence> {
    let mut result = Vec::new();
    for pair in pairs {
        let fingerprint = pair.refresh_fingerprint.unwrap();
        if !result
            .iter()
            .any(|fence: &CredentialMigrationRefreshFence| fence.fingerprint == fingerprint)
        {
            result.push(CredentialMigrationRefreshFence {
                fingerprint,
                fenced_at_ms: PLAN_NOW,
            });
        }
    }
    result
}

fn plan<'a>(
    pairs: &'a [SealedCredentialPair],
    blocked: &'a [(CredentialSlot, CredentialMigrationBlockReason)],
    attempts: &'a [CredentialMigrationAttempt],
    legacy: &'a [CredentialMigrationLegacyFence],
    refresh: &'a [CredentialMigrationRefreshFence],
) -> CredentialMigrationPlan<'a> {
    CredentialMigrationPlan {
        pairs,
        blocked_slots: blocked,
        attempts,
        legacy_fences: legacy,
        refresh_fences: refresh,
        now_ms: PLAN_NOW,
    }
}

fn assert_counts(
    actual: &CredentialMigrationCounts,
    attempts: i64,
    fingerprints: i64,
    legacy: i64,
    case: &str,
) {
    assert_eq!(
        actual.attempts_migrated, attempts,
        "{case}: every binding counts"
    );
    assert_eq!(
        actual.fences_fingerprinted, fingerprints,
        "{case}: distinct migrated fingerprints count"
    );
    assert_eq!(
        actual.legacy_fences, legacy,
        "{case}: case-folded legacy count"
    );
}

fn refuse(f: &mut Fixture, candidate: &CredentialMigrationPlan<'_>, case: &str) {
    let before = snapshot(f);
    let changes_before = total_changes(f);
    let mut callbacks = 0;
    let result = f
        .conn
        .commit_credential_migration(candidate, &mut || callbacks += 1);
    assert!(
        matches!(result, Err(StoreError::CredentialPairInvalid)),
        "{case}: invalid plan is refused through the fixed error"
    );
    assert_eq!(callbacks, 0, "{case}: validation never invokes COMMIT hook");
    assert_eq!(
        total_changes(f),
        changes_before,
        "{case}: validation performs no transient SQL write"
    );
    assert_eq!(snapshot(f), before, "{case}: complete transaction rollback");
}

fn both_blocked() -> [(CredentialSlot, CredentialMigrationBlockReason); 2] {
    [
        (
            CredentialSlot::OwnerApi,
            CredentialMigrationBlockReason::OldKeyUnavailable,
        ),
        (
            CredentialSlot::FleetApi,
            CredentialMigrationBlockReason::OldPairInvalid,
        ),
    ]
}

pub(super) fn map_blocked(f: &mut Fixture) -> CredentialMigrationCounts {
    let attempts = initial_history(f);
    let legacy = legacy_set(false, false);
    let blocked = both_blocked();
    f.conn
        .commit_credential_migration(&plan(&[], &blocked, &attempts, &legacy, &[]), &mut || {})
        .unwrap()
}

fn initial_map() {
    let mut f = migration_fixture(true, true, true);
    let attempts = initial_history(&f);
    let pairs = [
        migrated_pair(
            &mut f,
            CredentialSlot::OwnerApi,
            101,
            "migration-owner-refresh",
        ),
        migrated_pair(
            &mut f,
            CredentialSlot::FleetApi,
            202,
            "migration-fleet-refresh",
        ),
    ];
    let legacy = legacy_set(true, true);
    let refresh = refresh_set(&pairs);
    let observer = f.store.open().unwrap();
    configure_test_memory_temp_store(&observer);
    let calls = Cell::new(0);
    let counts = f
        .conn
        .commit_credential_migration(
            &plan(&pairs, &[], &attempts, &legacy, &refresh),
            &mut || {
                calls.set(calls.get() + 1);
                let converted: Option<i64> = observer
                    .query_row(
                        "SELECT converted_at_ms FROM credential_format_state",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(
                    converted.is_none(),
                    "initial_map: hook precedes explicit COMMIT"
                );
            },
        )
        .expect("initial_map: a complete valid plan commits");
    assert_eq!(
        calls.get(),
        1,
        "initial_map: exactly one counted explicit COMMIT"
    );
    assert_counts(&counts, 3, 2, 2, "initial_map");
    assert_eq!(
        count(&f, "credential_pairs"),
        2,
        "initial_map: both pairs installed"
    );
    assert_eq!(
        count(&f, "credential_refresh_attempts"),
        3,
        "shared_attempt_id_two_rows"
    );
    for (id, expected_attempt, slot, input, output) in [
        (
            10_i64,
            attempts[0].attempt_id,
            "owner_api",
            generation(101),
            None,
        ),
        (
            20,
            attempts[1].attempt_id,
            "fleet_api",
            generation(101),
            Some(generation(202)),
        ),
        (
            30,
            attempts[2].attempt_id,
            "owner_api",
            generation(202),
            None,
        ),
    ] {
        let row: (String, String, String, Option<String>, i64, Option<i64>) = f
            .conn
            .test_connection()
            .query_row(
                "SELECT attempt_id,slot,input_generation,output_generation,migrated,fence_id
                 FROM credential_refresh_attempts WHERE receipt_id=?1",
                [id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            row.0,
            expected_attempt.to_string(),
            "initial_map: attempt correspondence"
        );
        assert_eq!(row.1, slot, "initial_map: slot correspondence");
        assert_eq!(
            row.2,
            input.to_string(),
            "initial_map: input correspondence"
        );
        assert_eq!(
            row.3,
            output.map(|v| v.to_string()),
            "initial_map: nullable output correspondence"
        );
        assert_eq!(row.4, 1, "initial_map: migrated attempt marker");
        assert_eq!(
            row.5, None,
            "initial_map: migration adds no attempt-kind fence"
        );
    }
    assert_ne!(
        attempts[1].attempt_id, attempts[0].attempt_id,
        "shared_attempt_id_two_rows: Fleet reassigned"
    );
    assert_eq!(
        attempts[1].attempt_id.get_version_num(),
        4,
        "shared_attempt_id_two_rows: Fleet v4"
    );
    let legacy_rows: Vec<(String, String, i64)> = f
        .conn
        .test_connection()
        .prepare("SELECT legacy_generation,identity,fenced_at_ms FROM credential_legacy_fences ORDER BY legacy_generation")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        legacy_rows,
        vec![
            (
                generation(101).to_string(),
                "anchored".to_owned(),
                T0_MS - 200
            ),
            (
                generation(202).to_string(),
                "anchored".to_owned(),
                T0_MS - 300
            ),
        ],
        "initial_map: case-folded union preserves earliest receipt time"
    );
    for fence in &refresh {
        let actual: (String, i64) = f
            .conn
            .test_connection()
            .query_row(
                "SELECT kind,fenced_at_ms FROM credential_refresh_fences WHERE fingerprint=?1",
                [fence.fingerprint.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            actual,
            ("migrated".to_owned(), PLAN_NOW),
            "initial_fingerprint_set"
        );
    }
    let state = f.conn.credential_format_state().expect(
        "initial_map: readable converted state requires every predecessor table to be dropped",
    );
    assert!(matches!(
        state.owner_api_state,
        CredentialSlotFormatState::Migrated
    ));
    assert!(matches!(
        state.fleet_api_state,
        CredentialSlotFormatState::Migrated
    ));
    assert!(!state.fences_pending && state.old_tables_dropped);
    assert_eq!(
        state.converted_at_ms,
        Some(PLAN_NOW),
        "initial_map: conversion timestamp"
    );
    assert!(
        state.purge_required,
        "initial_map: conversion does not skip purge"
    );
    assert!(!state.wal_scrubbed && !state.old_files_removed && state.completed_at_ms.is_none());
    assert_eq!(state.attempts_migrated, 3);
    assert_eq!(state.fences_fingerprinted, 2);
    assert_eq!(state.legacy_fences, 2);
    let before = snapshot(&f);
    let data_before = data_version(&observer);
    let replay_calls = Cell::new(0);
    let replay = f
        .conn
        .commit_credential_migration(&empty_plan(PLAN_NOW + 1), &mut || {
            replay_calls.set(replay_calls.get() + 1)
        })
        .unwrap();
    assert_counts(&replay, 3, 2, 2, "empty_finalize replay");
    assert_eq!(
        replay_calls.get(),
        0,
        "empty_finalize: converted replay has no COMMIT"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "empty_finalize: converted replay writes nothing"
    );
    assert_eq!(
        data_version(&observer),
        data_before,
        "empty_finalize: no replay write transaction"
    );
}

fn initial_validation() {
    let mut f = migration_fixture(true, true, true);
    let mut attempts = initial_history(&f);
    let pairs = [
        migrated_pair(
            &mut f,
            CredentialSlot::OwnerApi,
            101,
            "initial-validation-owner",
        ),
        migrated_pair(
            &mut f,
            CredentialSlot::FleetApi,
            202,
            "initial-validation-fleet",
        ),
    ];
    let mut legacy = legacy_set(true, true);
    let mut refresh = refresh_set(&pairs);
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts[..2], &legacy, &refresh),
        "initial_map missing binding",
    );
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy[..1], &refresh),
        "initial_identity_validation missing legacy generation",
    );
    legacy.push(CredentialMigrationLegacyFence {
        generation: generation(9_999),
        identity: LegacyFenceIdentity::Opaque,
        fenced_at_ms: PLAN_NOW,
    });
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &refresh),
        "initial_identity_validation unrelated extra legacy generation",
    );
    legacy.pop();
    legacy.push(CredentialMigrationLegacyFence {
        generation: generation(101),
        identity: LegacyFenceIdentity::Anchored,
        fenced_at_ms: T0_MS - 200,
    });
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &refresh),
        "initial_identity_validation duplicate casefold legacy generation",
    );
    legacy.pop();
    let fleet_original = attempts[1].attempt_id;
    attempts[1].attempt_id = attempts[0].attempt_id;
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &refresh),
        "shared_attempt_id_two_rows original Fleet id refused",
    );
    attempts[1].attempt_id = fleet_original;
    // The reassignment contract requires an RFC UUID v4, not only a v4 nibble.
    // Byte 6 has version 4; byte 8 deliberately selects the NCS variant.
    attempts[1].attempt_id = Uuid::from_bytes([
        0xf1, 0x82, 0x91, 0x43, 0x10, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x01,
    ]);
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &refresh),
        "shared_attempt_id_two_rows non-RFC reassigned Fleet id",
    );
    attempts[1].attempt_id = fleet_original;
    let saved_output = attempts[1].output_generation;
    attempts[1].output_generation = None;
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &refresh),
        "initial_map output correspondence",
    );
    attempts[1].output_generation = saved_output;
    legacy[0].identity = LegacyFenceIdentity::Opaque;
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &refresh),
        "initial_identity_validation false Opaque",
    );
    legacy[0].identity = LegacyFenceIdentity::Anchored;
    legacy[0].fenced_at_ms += 1;
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &refresh),
        "initial_identity_validation earliest time",
    );
    legacy[0].fenced_at_ms -= 1;
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &refresh[..1]),
        "initial_fingerprint_set missing fingerprint",
    );
    refresh.push(CredentialMigrationRefreshFence {
        fingerprint: [0x93; 32],
        fenced_at_ms: PLAN_NOW,
    });
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &refresh),
        "initial_fingerprint_set unrelated fingerprint",
    );
    refresh.pop();
    refresh[0].fenced_at_ms -= 1;
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &refresh),
        "initial_fingerprint_set timestamp",
    );
    refresh[0].fenced_at_ms += 1;
    let blocked = [(
        CredentialSlot::FleetApi,
        CredentialMigrationBlockReason::OldPairInvalid,
    )];
    let owner_refresh = refresh_set(&pairs[..1]);
    refuse(
        &mut f,
        &plan(&pairs[..1], &blocked, &attempts, &legacy, &owner_refresh),
        "initial_identity_validation false Anchored without pair",
    );
    legacy[1].identity = LegacyFenceIdentity::Opaque;
    refuse(
        &mut f,
        &plan(&pairs[..1], &[], &attempts, &legacy, &owner_refresh),
        "initial_identity_validation unresolved slot lacks reason",
    );
    let valid = f
        .conn
        .commit_credential_migration(
            &plan(&pairs[..1], &blocked, &attempts, &legacy, &owner_refresh),
            &mut || {},
        )
        .unwrap();
    assert_counts(
        &valid,
        3,
        1,
        2,
        "initial_identity_validation accepted control",
    );
    assert_eq!(
        count(&f, "legacy_owner_tokens"),
        0,
        "initial_map deletes converted old row only"
    );
    assert_eq!(
        count(&f, "fleet_tokens"),
        1,
        "initial_map preserves blocked old row"
    );
}

fn remove_second_source_fence(f: &Fixture) {
    for table in ["legacy_refresh_input_fences", "fleet_refresh_input_fences"] {
        f.conn
            .test_connection()
            .execute(
                &format!("DELETE FROM {table} WHERE input_credential_generation=?1"),
                [generation(202).to_string()],
            )
            .unwrap();
    }
    // A released unsent receipt may retain its binding without a permanent fence.
    f.conn
        .test_connection()
        .execute(
            "UPDATE outbound_request_receipts SET outcome='cancelled' WHERE id=30",
            [],
        )
        .unwrap();
}

fn only_fenced_pairs_produce_fingerprints() {
    let mut f = migration_fixture(true, true, true);
    let attempts = initial_history(&f);
    remove_second_source_fence(&f);
    let pairs = [
        migrated_pair(
            &mut f,
            CredentialSlot::OwnerApi,
            101,
            "initial-fenced-refresh",
        ),
        migrated_pair(
            &mut f,
            CredentialSlot::FleetApi,
            202,
            "initial-unfenced-refresh",
        ),
    ];
    let legacy = [CredentialMigrationLegacyFence {
        generation: generation(101),
        identity: LegacyFenceIdentity::Anchored,
        fenced_at_ms: T0_MS - 200,
    }];
    let all = refresh_set(&pairs);
    refuse(
        &mut f,
        &plan(&pairs, &[], &attempts, &legacy, &all),
        "initial_fingerprint_set excludes a converting unfenced pair",
    );
    let expected = refresh_set(&pairs[..1]);
    let counts = f
        .conn
        .commit_credential_migration(
            &plan(&pairs, &[], &attempts, &legacy, &expected),
            &mut || {},
        )
        .expect("initial_fingerprint_set accepts exactly the generation-filtered fingerprint set");
    assert_counts(
        &counts,
        3,
        1,
        1,
        "initial_fingerprint_set generation filter",
    );
    let unfenced_exists: bool = f
        .conn
        .test_connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_refresh_fences WHERE fingerprint=?1)",
            [pairs[1].refresh_fingerprint.unwrap().as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !unfenced_exists,
        "initial_fingerprint_set does not invent a fence for an unspent pair"
    );

    let mut f = migration_fixture(true, true, true);
    let attempts = initial_history(&f);
    remove_second_source_fence(&f);
    let blocked = both_blocked();
    let legacy = [CredentialMigrationLegacyFence {
        generation: generation(101),
        identity: LegacyFenceIdentity::Opaque,
        fenced_at_ms: T0_MS - 200,
    }];
    f.conn
        .commit_credential_migration(&plan(&[], &blocked, &attempts, &legacy, &[]), &mut || {})
        .unwrap();
    let fleet = [migrated_pair(
        &mut f,
        CredentialSlot::FleetApi,
        202,
        "late-unfenced-refresh",
    )];
    let owner_blocked = [(
        CredentialSlot::OwnerApi,
        CredentialMigrationBlockReason::OldKeyUnavailable,
    )];
    let arbitrary = refresh_set(&fleet);
    refuse(
        &mut f,
        &plan(&fleet, &owner_blocked, &[], &[], &arbitrary),
        "late_vectors_rejected no independent fingerprint insertion for an unfenced pair",
    );
    let counts = f
        .conn
        .commit_credential_migration(&plan(&fleet, &owner_blocked, &[], &[], &[]), &mut || {})
        .expect("late_anchor accepts an unfenced pair without deriving a fingerprint");
    assert_counts(&counts, 3, 0, 1, "late_anchor unfenced generation");
    assert_eq!(
        count(&f, "credential_refresh_fences"),
        0,
        "late_anchor derives no unrelated migrated fingerprint"
    );
    let identity: String = f
        .conn
        .test_connection()
        .query_row("SELECT identity FROM credential_legacy_fences", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        identity, "opaque",
        "late_anchor leaves unrelated legacy identity opaque"
    );
}

fn late_conversion() {
    let mut f = migration_fixture(true, true, true);
    let initial = map_blocked(&mut f);
    assert_counts(&initial, 3, 0, 2, "late_anchor initial mapping");
    let pairs = [migrated_pair(
        &mut f,
        CredentialSlot::OwnerApi,
        101,
        "late-owner-refresh",
    )];
    let blocked = [(
        CredentialSlot::FleetApi,
        CredentialMigrationBlockReason::OldPairInvalid,
    )];
    let replay_attempt = CredentialMigrationAttempt {
        receipt_id: OutboundRequestReceiptId(10),
        attempt_id: generation(8_001),
        slot: CredentialSlot::OwnerApi,
        input_generation: generation(101),
        output_generation: None,
    };
    refuse(
        &mut f,
        &plan(&pairs, &blocked, &[replay_attempt], &[], &[]),
        "late_vectors_rejected attempt replay",
    );
    let replay_legacy = legacy_set(false, false);
    refuse(
        &mut f,
        &plan(&pairs, &blocked, &[], &replay_legacy, &[]),
        "late_vectors_rejected legacy replay",
    );
    let unrelated = [CredentialMigrationRefreshFence {
        fingerprint: [0x94; 32],
        fenced_at_ms: PLAN_NOW,
    }];
    refuse(
        &mut f,
        &plan(&pairs, &blocked, &[], &[], &unrelated),
        "late_vectors_rejected arbitrary fingerprint",
    );
    let fingerprint = pairs[0].refresh_fingerprint.unwrap();
    f.conn.test_connection().execute("INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms) VALUES(?1,'attempt',?2)", params![fingerprint.as_slice(),T0_MS]).unwrap();
    refuse(
        &mut f,
        &plan(&pairs, &blocked, &[], &[], &[]),
        "late_fingerprint_attempt_collision",
    );
    let kind: String = f
        .conn
        .test_connection()
        .query_row(
            "SELECT kind FROM credential_refresh_fences WHERE fingerprint=?1",
            [fingerprint.as_slice()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        kind, "attempt",
        "late_fingerprint_attempt_collision never relabels or releases"
    );
    f.conn
        .test_connection()
        .execute(
            "DELETE FROM credential_refresh_fences WHERE fingerprint=?1",
            [fingerprint.as_slice()],
        )
        .unwrap();
    let result = f
        .conn
        .commit_credential_migration(&plan(&pairs, &blocked, &[], &[], &[]), &mut || {})
        .unwrap();
    assert_counts(&result, 3, 1, 2, "late_anchor");
    let identity: (String, i64) = f
        .conn
        .test_connection()
        .query_row(
            "SELECT identity,fenced_at_ms FROM credential_legacy_fences WHERE legacy_generation=?1",
            [generation(101).to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        identity,
        ("anchored".to_owned(), T0_MS - 200),
        "late_anchor derives promotion from pair generation"
    );
    assert_eq!(
        count(&f, "credential_legacy_fences"),
        2,
        "late_anchor preserves permanent fences"
    );
    assert_eq!(count(&f, "legacy_owner_tokens"), 0);
    assert_eq!(count(&f, "fleet_tokens"), 1);
    let state = f.conn.credential_format_state().unwrap();
    assert!(matches!(
        state.owner_api_state,
        CredentialSlotFormatState::Migrated
    ));
    assert_eq!(state.owner_api_blocked, None);
    assert_eq!(
        state.fleet_api_blocked,
        Some(CredentialMigrationBlockReason::OldPairInvalid)
    );
    assert!(
        state.converted_at_ms.is_none() && !state.old_tables_dropped,
        "late_anchor leaves unresolved slot pending"
    );
}

fn late_migrated_dedup() {
    let mut f = migration_fixture(true, true, true);
    let attempts = initial_history(&f);
    let fleet = [migrated_pair(
        &mut f,
        CredentialSlot::FleetApi,
        202,
        "shared-migrated-refresh",
    )];
    let blocked = [(
        CredentialSlot::OwnerApi,
        CredentialMigrationBlockReason::OldKeyUnavailable,
    )];
    let legacy = legacy_set(false, true);
    let refresh = refresh_set(&fleet);
    let initial = f
        .conn
        .commit_credential_migration(
            &plan(&fleet, &blocked, &attempts, &legacy, &refresh),
            &mut || {},
        )
        .unwrap();
    assert_counts(&initial, 3, 1, 2, "late_migrated_dedup initial");
    let owner = [migrated_pair(
        &mut f,
        CredentialSlot::OwnerApi,
        101,
        "shared-migrated-refresh",
    )];
    let mut exact = refresh_set(&owner);
    exact[0].fenced_at_ms = PLAN_NOW + 100;
    let before_fence: (i64, String, i64) = f
        .conn
        .test_connection()
        .query_row(
            "SELECT fence_id,kind,fenced_at_ms FROM credential_refresh_fences",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let mut late_plan = plan(&owner, &[], &[], &[], &exact);
    late_plan.now_ms = PLAN_NOW + 100;
    let result = f
        .conn
        .commit_credential_migration(&late_plan, &mut || {})
        .unwrap();
    assert_counts(&result, 3, 1, 2, "late_migrated_dedup");
    assert_eq!(
        count(&f, "credential_refresh_fences"),
        1,
        "late_migrated_dedup adds zero rows"
    );
    let after_fence: (i64, String, i64) = f
        .conn
        .test_connection()
        .query_row(
            "SELECT fence_id,kind,fenced_at_ms FROM credential_refresh_fences",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        after_fence, before_fence,
        "late_migrated_dedup leaves existing migrated row untouched"
    );
    let state = f.conn.credential_format_state().unwrap();
    assert!(state.old_tables_dropped && state.converted_at_ms == Some(PLAN_NOW + 100));
}

fn empty_cases() {
    let mut f = migration_fixture(false, false, false);
    let untouched = f.conn.credential_format_state().unwrap();
    assert!(
        matches!(untouched.owner_api_state, CredentialSlotFormatState::None),
        "empty_finalize: untouched Owner predecessor slot decodes as None"
    );
    assert!(
        matches!(untouched.fleet_api_state, CredentialSlotFormatState::None),
        "empty_finalize: untouched Fleet predecessor slot decodes as None"
    );
    let mut calls = 0;
    let result = f
        .conn
        .commit_credential_migration(&empty_plan(PLAN_NOW), &mut || calls += 1)
        .unwrap();
    assert_counts(&result, 0, 0, 0, "empty_finalize");
    assert_eq!(
        calls, 1,
        "empty_finalize has one explicit conversion COMMIT"
    );
    for table in PREDECESSOR_TABLES {
        let exists: bool = f
            .conn
            .test_connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!exists, "empty_finalize drops every predecessor table");
    }
    let state = f.conn.credential_format_state().unwrap();
    assert_eq!(state.converted_at_ms, Some(PLAN_NOW));
    assert!(state.old_tables_dropped && !state.fences_pending);
    let mut f = migration_fixture(false, false, true);
    source_fence(&f, CredentialSlot::OwnerApi, generation(303), false);
    refuse(
        &mut f,
        &empty_plan(PLAN_NOW),
        "empty_unmapped_history_rejected",
    );
    let legacy = [CredentialMigrationLegacyFence {
        generation: generation(303),
        identity: LegacyFenceIdentity::Opaque,
        fenced_at_ms: PLAN_NOW,
    }];
    let mapped = f
        .conn
        .commit_credential_migration(&plan(&[], &[], &[], &legacy, &[]), &mut || {})
        .unwrap();
    assert_counts(
        &mapped,
        0,
        0,
        1,
        "empty_unmapped_history initial mapping control",
    );
    let fallback_time: i64 = f
        .conn
        .test_connection()
        .query_row(
            "SELECT fenced_at_ms FROM credential_legacy_fences WHERE legacy_generation=?1",
            [generation(303).to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        fallback_time, PLAN_NOW,
        "initial_map uses plan time only when no receipt start exists"
    );
    let mut f = migration_fixture(false, false, false);
    source_fence(&f, CredentialSlot::OwnerApi, generation(304), false);
    refuse(
        &mut f,
        &empty_plan(PLAN_NOW),
        "empty_unmapped_history_rejected false mapped flag",
    );
}

fn compound_recovery_preserves_other_new_pair() {
    struct SyntheticVerifiedExclusion;
    impl crate::auth::credential_keys::BackupExclusion for SyntheticVerifiedExclusion {
        fn ensure_excluded(
            &self,
            _path: &std::path::Path,
        ) -> Result<
            crate::auth::credential_keys::ExclusionState,
            crate::auth::credential_keys::CredentialKeyError,
        > {
            Ok(crate::auth::credential_keys::ExclusionState::Verified)
        }
    }

    let mut f = fixture_with_memory_temp_store();
    let key_directory_path = f._temporary.path().join("secrets");
    let key_directory =
        crate::platform::private_directory::open_validated(&key_directory_path, true)
            .expect("compound_other_new_pair synthetic key directory");
    f.keys = crate::auth::credential_keys::create_keyring(
        &key_directory,
        &SyntheticVerifiedExclusion,
        (T0_MS / 1_000) as u64,
    )
    .expect("compound_other_new_pair creates only an own-temp keyring with fake exclusion");
    f.nonces = FixedNonces::new(
        f.keys.keyring().current.id,
        (0..128_u64).map(|draw| {
            let mut nonce = [0_u8; 12];
            nonce[..8].copy_from_slice(&draw.to_be_bytes());
            nonce[8..].copy_from_slice(b"Sadd");
            nonce
        }),
    );
    let fleet = f.pair(CredentialSlot::FleetApi, 9_006, "other-new-fleet-refresh");
    f.put(&fleet, ReplacePolicy::OnlyIfEmpty)
        .expect("compound_other_new_pair installs Fleet through S while clean");

    // Prepare only synthetic S binding/provenance while clean. This component
    // case neither redeems an issuer grant nor publishes a replacement keyring.
    let secret = Zeroizing::new([0xaa; 32]);
    let sign_in_id = f
        .conn
        .insert_sign_in_binding(CredentialSlot::OwnerApi, binding(&secret), T0_MS)
        .expect("compound_other_new_pair prepares one valid synthetic Owner grant binding");
    let owner_grant = f.pair_with(
        CredentialSlot::OwnerApi,
        9_007,
        CredentialOrigin::HubLogin,
        RefreshOwner::Hub,
        b"other-new-pair-owner-access",
        b"other-new-pair-owner-refresh",
    );
    let provenance = EntryProvenance::HubGrant {
        sign_in_id,
        grant_secret: &secret,
    };
    std::fs::remove_file(key_directory_path.join("credential-keys"))
        .expect("compound_other_new_pair removes only its own known synthetic keyring");
    assert!(
        crate::auth::credential_keys::check_keyring(&key_directory)
            .expect("compound_other_new_pair nonmutating own-temp missing-keyring check")
            .is_none(),
        "compound_keyring_missing_recovery other_new_pair keyring is actually missing"
    );
    assert_eq!(
        count(&f, "credential_pairs"),
        1,
        "compound_other_new_pair loss fixture retains an existing new Fleet row"
    );

    // Construct a distinct predecessor-input fixture, not a live S state action.
    // The existing new Fleet row is deliberately untouched; its old slot is None.
    create_predecessors(f.conn.test_connection(), 63, false);
    seed_predecessor_row(&f, CredentialSlot::OwnerApi, generation(101));
    f.conn
        .test_connection()
        .execute_batch(
            "UPDATE credential_format_state SET owner_api_state='pending',
         fleet_api_state='none',owner_api_blocked=NULL,fleet_api_blocked=NULL,
         fences_pending=1,old_tables_dropped=0,purge_required=1,wal_scrubbed=0,
         old_files_removed=0,attempts_migrated=0,fences_fingerprinted=0,
         legacy_fences=0,purge_needed_bytes=NULL,secrets_exclusion=NULL,
         durability=NULL,durability_probed_at_ms=NULL,started_at_ms=NULL,
         converted_at_ms=NULL,completed_at_ms=NULL;",
        )
        .unwrap();
    let attempt = seed_binding(
        &f,
        CredentialSlot::OwnerApi,
        501,
        generation(8_501),
        generation(101),
        None,
        T0_MS - 200,
        false,
    );
    source_fence(&f, CredentialSlot::OwnerApi, generation(101), false);
    let before = snapshot(&f);
    let changes = total_changes(&f);
    assert!(
        matches!(
            f.conn.replace_credential_pair(
                &owner_grant,
                &provenance,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny
            ),
            Err(StoreError::CredentialPairInvalid)
        ),
        "compound_other_new_pair pending-no-reason refuses direct R28"
    );
    assert_eq!(
        total_changes(&f),
        changes,
        "compound_other_new_pair direct R28 has no transient SQL write"
    );
    assert!(
        matches!(
            f.conn.clear_credential_pair(CredentialSlot::OwnerApi),
            Err(StoreError::CredentialPairInvalid)
        ),
        "compound_other_new_pair pending-no-reason refuses R35"
    );
    assert_eq!(
        total_changes(&f),
        changes,
        "compound_other_new_pair direct R35 has no transient SQL write"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "compound_other_new_pair refusals preserve both stores and the same grant binding"
    );

    let blocked = [(
        CredentialSlot::OwnerApi,
        CredentialMigrationBlockReason::OldKeyUnavailable,
    )];
    let legacy = [CredentialMigrationLegacyFence {
        generation: generation(101),
        identity: LegacyFenceIdentity::Opaque,
        fenced_at_ms: T0_MS - 200,
    }];
    let counts = f
        .conn
        .commit_credential_migration(&plan(&[], &blocked, &[attempt], &legacy, &[]), &mut || {})
        .expect("compound_other_new_pair ordinary full Owner history mapping passes its barrier");
    assert_counts(
        &counts,
        1,
        0,
        1,
        "compound_other_new_pair full initial mapping",
    );
    f.conn
        .replace_credential_pair(
            &owner_grant,
            &provenance,
            &LegacyCandidates(Vec::new()),
            ReplacePolicy::ReplaceAny,
        )
        .expect("compound_other_new_pair mapped R48 permits the same valid bound grant");
    f.conn
        .credential_durability_barrier()
        .expect("compound_other_new_pair accepted replacement is durable");
    let stored_fleet = f
        .conn
        .load_credential_pair(CredentialSlot::FleetApi)
        .expect("compound_other_new_pair loads the unrelated new Fleet pair")
        .expect("compound_other_new_pair retains the unrelated new Fleet pair");
    assert_pair(&stored_fleet, &fleet);
    let state = f.conn.credential_format_state().unwrap();
    assert!(
        matches!(state.owner_api_state, CredentialSlotFormatState::Discarded),
        "compound_other_new_pair Owner is discarded by R48"
    );
    assert!(
        matches!(state.fleet_api_state, CredentialSlotFormatState::None),
        "compound_other_new_pair never invents a migrated Fleet predecessor"
    );
    assert_eq!(state.owner_api_blocked, None);
    assert_eq!(state.fleet_api_blocked, None);
    assert_eq!(state.attempts_migrated, 1);
    assert_eq!(state.fences_fingerprinted, 0);
    assert_eq!(state.legacy_fences, 1);
    let opaque: (String, i64) = f
        .conn
        .test_connection()
        .query_row(
            "SELECT identity,fenced_at_ms FROM credential_legacy_fences WHERE legacy_generation=?1",
            [generation(101).to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        opaque,
        ("opaque".to_owned(), T0_MS - 200),
        "compound_other_new_pair opaque Owner history remains permanent and unchanged"
    );
    assert_eq!(
        count(&f, "fleet_refresh_receipt_bindings"),
        0,
        "compound_other_new_pair invents no old Fleet binding"
    );
    assert_eq!(
        count(&f, "fleet_refresh_input_fences"),
        0,
        "compound_other_new_pair invents no old Fleet fence"
    );
}

fn pending_and_discard() {
    let mut f = migration_fixture(true, true, true);
    let replacement = f.pair(CredentialSlot::OwnerApi, 9_001, "premature-owner-refresh");
    let before = snapshot(&f);
    let before_replace_changes = total_changes(&f);
    let result = f.put(&replacement, ReplacePolicy::ReplaceAny);
    assert!(
        matches!(result, Err(StoreError::CredentialPairInvalid)),
        "pending_unblocked_replace_refused"
    );
    assert_eq!(
        total_changes(&f),
        before_replace_changes,
        "pending_unblocked_replace_refused performs no transient SQL write before validation"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "pending_unblocked_replace_refused has no write"
    );
    let before_clear_changes = total_changes(&f);
    let result = f.conn.clear_credential_pair(CredentialSlot::FleetApi);
    assert!(
        matches!(result, Err(StoreError::CredentialPairInvalid)),
        "pending_unblocked_clear_refused"
    );
    assert_eq!(
        total_changes(&f),
        before_clear_changes,
        "pending_unblocked_clear_refused performs no transient SQL write before validation"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "pending_unblocked_clear_refused has no write"
    );
    let directory = crate::platform::private_directory::open_validated(
        &f._temporary.path().join("secrets"),
        true,
    )
    .expect("compound_keyring_missing_recovery synthetic admitted key directory");
    assert!(
        crate::auth::credential_keys::check_keyring(&directory)
            .expect("compound_keyring_missing_recovery checks only synthetic keyring absence")
            .is_none(),
        "compound_keyring_missing_recovery uses no persisted keyring"
    );
    let mapped = map_blocked(&mut f);
    assert_counts(
        &mapped,
        3,
        0,
        2,
        "compound_keyring_missing_recovery full ordinary mapping",
    );
    // This is the S binding/provenance transition after normal mapping. Real
    // issuer redemption and replacement-keyring publication remain V1/V2 proof.
    let grant = f.pair_with(
        CredentialSlot::OwnerApi,
        9_003,
        CredentialOrigin::HubLogin,
        RefreshOwner::Hub,
        b"mapped-grant-access",
        b"mapped-grant-refresh",
    );
    f.put(&grant, ReplacePolicy::ReplaceAny).expect(
        "compound_keyring_missing_recovery permits a valid bound grant after ordinary mapping",
    );
    assert_eq!(
        count(&f, "legacy_owner_tokens"),
        0,
        "blocked_discard replacement removes only blocked old Owner row"
    );
    assert_eq!(
        count(&f, "fleet_tokens"),
        1,
        "blocked_discard preserves other pending old row"
    );
    let state = f.conn.credential_format_state().unwrap();
    assert!(
        matches!(state.owner_api_state, CredentialSlotFormatState::Discarded),
        "blocked_discard marks Owner discarded atomically"
    );
    assert_eq!(state.owner_api_blocked, None);
    assert_eq!(
        state.fleet_api_blocked,
        Some(CredentialMigrationBlockReason::OldPairInvalid)
    );
    assert_eq!(count(&f, "credential_pairs"), 1);
    f.conn
        .clear_credential_pair(CredentialSlot::FleetApi)
        .expect(
            "blocked_discard preserves the ordinary mapped capability across Owner replacement",
        );
    let state = f.conn.credential_format_state().unwrap();
    assert!(
        matches!(state.fleet_api_state, CredentialSlotFormatState::Discarded),
        "blocked_discard clear marks Fleet discarded atomically"
    );
    assert_eq!(state.fleet_api_blocked, None);
    for table in PREDECESSOR_TABLES {
        let exists: bool = f
            .conn
            .test_connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            exists,
            "blocked_discard retains every predecessor table for ordinary Phase B finalisation"
        );
    }
    assert_eq!(
        count(&f, "fleet_tokens"),
        0,
        "blocked_discard removes the resolved Fleet row without dropping its table"
    );
    assert!(
        !state.old_tables_dropped,
        "blocked_discard leaves table drop to ordinary Phase B"
    );
    assert!(
        state.converted_at_ms.is_none(),
        "blocked_discard leaves conversion marker to ordinary Phase B"
    );
    assert_eq!(
        count(&f, "credential_refresh_attempts"),
        3,
        "blocked_discard keeps mapped attempts"
    );
    assert_eq!(
        count(&f, "credential_legacy_fences"),
        2,
        "blocked_discard keeps permanent legacy fences"
    );
    assert!(!state.fences_pending);
    f.conn
        .credential_durability_barrier()
        .expect("blocked_discard clear is durable before dependent Phase B");
    let mut finalise_hooks = 0;
    let finalised = f
        .conn
        .commit_credential_migration(&empty_plan(PLAN_NOW + 1), &mut || finalise_hooks += 1)
        .unwrap();
    assert_counts(
        &finalised,
        3,
        0,
        2,
        "blocked_discard ordinary Phase B finalisation preserves totals",
    );
    assert_eq!(
        finalise_hooks, 1,
        "blocked_discard ordinary finalisation has one counted COMMIT"
    );
    let converted = f.conn.credential_format_state().unwrap();
    assert!(
        converted.old_tables_dropped,
        "blocked_discard ordinary Phase B records table drop"
    );
    assert_eq!(
        converted.converted_at_ms,
        Some(PLAN_NOW + 1),
        "blocked_discard ordinary Phase B records conversion"
    );
    for table in PREDECESSOR_TABLES {
        let exists: bool = f
            .conn
            .test_connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name=?1)",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !exists,
            "blocked_discard ordinary Phase B drops every predecessor table"
        );
    }
    let replay_before = snapshot(&f);
    let mut replay_hooks = 0;
    let final_counts = f
        .conn
        .commit_credential_migration(&empty_plan(PLAN_NOW + 2), &mut || replay_hooks += 1)
        .unwrap();
    assert_counts(
        &final_counts,
        3,
        0,
        2,
        "blocked_discard empty finalisation preserves totals",
    );
    assert_eq!(
        replay_hooks, 0,
        "blocked_discard finalisation replay has no COMMIT hook"
    );
    assert_eq!(
        snapshot(&f),
        replay_before,
        "blocked_discard finalisation replay writes nothing"
    );
    compound_recovery_preserves_other_new_pair();
}

fn refuse_pair_writes_after_failed_barrier(
    f: &mut Fixture,
    replacement: &SealedCredentialPair,
    provenance: &EntryProvenance<'_>,
    clear_slot: CredentialSlot,
    case: &str,
) {
    let before = snapshot(f);
    let changes = total_changes(f);
    assert!(
        matches!(
            f.conn.replace_credential_pair(
                replacement,
                provenance,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny,
            ),
            Err(StoreError::CredentialPairInvalid)
        ),
        "{case}: failed conversion barrier refuses R28"
    );
    assert_eq!(
        total_changes(f),
        changes,
        "{case}: refused R28 performs no transient SQL write"
    );
    assert_eq!(snapshot(f), before, "{case}: refused R28 stores no grant");
    assert!(
        matches!(
            f.conn.clear_credential_pair(clear_slot),
            Err(StoreError::CredentialPairInvalid)
        ),
        "{case}: failed conversion barrier refuses R35"
    );
    assert_eq!(
        total_changes(f),
        changes,
        "{case}: refused R35 performs no transient SQL write"
    );
    assert_eq!(
        snapshot(f),
        before,
        "{case}: refused R35 preserves the stored input"
    );
}

fn commit_error_and_barrier() {
    let mut f = migration_fixture(false, false, false);
    f.conn.test_connection().execute_batch(
        "CREATE TABLE migration_commit_parent(id INTEGER PRIMARY KEY) STRICT;
         CREATE TABLE migration_commit_child(id INTEGER REFERENCES migration_commit_parent(id) DEFERRABLE INITIALLY DEFERRED) STRICT;
         CREATE TRIGGER migration_commit_refusal AFTER UPDATE OF converted_at_ms ON credential_format_state
         WHEN NEW.converted_at_ms IS NOT NULL BEGIN INSERT INTO migration_commit_child VALUES(1); END;"
    ).unwrap();
    let before = snapshot(&f);
    let mut calls = 0;
    let result = f
        .conn
        .commit_credential_migration(&empty_plan(PLAN_NOW), &mut || calls += 1);
    assert!(
        result.is_err(),
        "commit_error: deferred constraint refuses explicit COMMIT"
    );
    assert_eq!(
        calls, 1,
        "commit_error: R55 hook runs even when COMMIT errors"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "commit_error: failed COMMIT rolls back state and drops"
    );

    let mut f = migration_fixture(false, false, false);
    f.conn
        .test_connection()
        .execute_batch(
            "CREATE TRIGGER migration_before_commit_refusal
             BEFORE UPDATE OF converted_at_ms ON credential_format_state
             WHEN NEW.converted_at_ms IS NOT NULL
             BEGIN SELECT RAISE(ABORT,'synthetic before-COMMIT refusal'); END;",
        )
        .unwrap();
    let before = snapshot(&f);
    let mut calls = 0;
    let result = f
        .conn
        .commit_credential_migration(&empty_plan(PLAN_NOW), &mut || calls += 1);
    assert!(
        matches!(
            &result,
            Err(StoreError::CredentialPairStore(rusqlite::Error::SqliteFailure(error, Some(message))))
                if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER
                    && message == "synthetic before-COMMIT refusal"
        ),
        "before_commit_sql_error: fixture reaches the actual synthetic SQL statement failure"
    );
    assert_eq!(
        calls, 0,
        "before_commit_sql_error: no hook before a COMMIT attempt"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "before_commit_sql_error: statement failure rolls back all state and drops"
    );

    let mut f = migration_fixture(true, true, true);
    let attempts = initial_history(&f);
    let legacy = legacy_set(false, false);
    let blocked = both_blocked();
    let grant_secret = Zeroizing::new([0xa8; 32]);
    let grant_sign_in = f
        .conn
        .insert_sign_in_binding(CredentialSlot::OwnerApi, binding(&grant_secret), T0_MS)
        .expect("blocked_map_poison prepares one valid synthetic grant binding");
    let grant = f.pair_with(
        CredentialSlot::OwnerApi,
        9_002,
        CredentialOrigin::HubLogin,
        RefreshOwner::Hub,
        b"barrier-failed-grant-access",
        b"barrier-failed-grant-refresh",
    );
    let grant_provenance = EntryProvenance::HubGrant {
        sign_in_id: grant_sign_in,
        grant_secret: &grant_secret,
    };
    f.conn.set_test_barrier_failure(true);
    let result = f
        .conn
        .commit_credential_migration(&plan(&[], &blocked, &attempts, &legacy, &[]), &mut || {});
    assert!(
        result.is_err(),
        "compound_keyring_missing_recovery barrier failure is returned"
    );
    f.conn.set_test_barrier_failure(false);
    refuse_pair_writes_after_failed_barrier(
        &mut f,
        &grant,
        &grant_provenance,
        CredentialSlot::FleetApi,
        "blocked_map_poison_after_physical_barrier_recovers",
    );
    assert!(
        f.conn.vacuum_credential_catalogue().is_err(),
        "failed_conversion_barrier forbids VACUUM"
    );
    assert!(
        f.conn.commit_credential_purge(&mut || {}).is_err(),
        "failed_conversion_barrier forbids purge state commit"
    );
    assert!(
        f.conn.mark_credential_files_removed().is_err(),
        "failed_conversion_barrier forbids removal marker"
    );
    assert!(
        f.conn
            .replace_credential_pair(
                &grant,
                &grant_provenance,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny
            )
            .is_err(),
        "compound_keyring_missing_recovery barrier failure stores no grant"
    );
    assert_eq!(
        count(&f, "credential_pairs"),
        0,
        "compound_keyring_missing_recovery no grant on retry failure"
    );

    let mut f = migration_fixture(false, false, false);
    f.conn.set_test_barrier_failure(true);
    let result = f
        .conn
        .commit_credential_migration(&empty_plan(PLAN_NOW), &mut || {});
    assert!(
        result.is_err(),
        "failed_conversion_barrier returns failure after a real conversion COMMIT"
    );
    let persisted_conversion: bool = f
        .conn
        .test_connection()
        .query_row(
            "SELECT converted_at_ms IS NOT NULL FROM credential_format_state",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        persisted_conversion,
        "failed_conversion_barrier control reaches converted state"
    );
    f.conn.set_test_barrier_failure(false);
    assert!(
        f.conn.vacuum_credential_catalogue().is_err(),
        "failed_conversion_barrier cannot VACUUM converted input"
    );
    assert!(
        f.conn.commit_credential_purge(&mut || {}).is_err(),
        "failed_conversion_barrier cannot clear converted-store purge"
    );
    assert!(
        f.conn.mark_credential_files_removed().is_err(),
        "failed_conversion_barrier cannot mark converted-store removal"
    );

    let mut f = migration_fixture(true, false, false);
    let pairs = [migrated_pair(
        &mut f,
        CredentialSlot::OwnerApi,
        101,
        "failed-resolved-refresh",
    )];
    let resolved_secret = Zeroizing::new([0xa9; 32]);
    let resolved_sign_in = f
        .conn
        .insert_sign_in_binding(CredentialSlot::OwnerApi, binding(&resolved_secret), T0_MS)
        .expect("resolved_map_poison prepares one valid synthetic grant binding");
    let replacement = f.pair_with(
        CredentialSlot::OwnerApi,
        9_004,
        CredentialOrigin::HubLogin,
        RefreshOwner::Hub,
        b"failed-resolved-grant-access",
        b"failed-resolved-grant-refresh",
    );
    let resolved_provenance = EntryProvenance::HubGrant {
        sign_in_id: resolved_sign_in,
        grant_secret: &resolved_secret,
    };
    f.conn.set_test_barrier_failure(true);
    let result = f
        .conn
        .commit_credential_migration(&plan(&pairs, &[], &[], &[], &[]), &mut || {});
    assert!(
        result.is_err(),
        "resolved_map_poison reports conversion barrier failure"
    );
    f.conn.set_test_barrier_failure(false);
    let persisted: bool = f
        .conn
        .test_connection()
        .query_row(
            "SELECT converted_at_ms IS NOT NULL FROM credential_format_state",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        persisted,
        "resolved_map_poison reaches a resolved converted slot"
    );
    refuse_pair_writes_after_failed_barrier(
        &mut f,
        &replacement,
        &resolved_provenance,
        CredentialSlot::OwnerApi,
        "resolved_map_poison_after_physical_barrier_recovers",
    );
    let generation_text: String = f
        .conn
        .test_connection()
        .query_row(
            "SELECT generation FROM credential_pairs WHERE slot='owner_api'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        generation_text,
        generation(101).to_string(),
        "resolved_map_poison leaves the migrated pair rather than a new grant"
    );
}

fn mapped_no_work_replay_barrier_failure() {
    let mut f = migration_fixture(true, true, true);
    let initial = map_blocked(&mut f);
    assert_counts(
        &initial,
        3,
        0,
        2,
        "mapped_no_work_replay_barrier_failure baseline",
    );
    let state_projection = |fixture: &Fixture| {
        fixture
            .conn
            .test_connection()
            .query_row(
                "SELECT owner_api_state,fleet_api_state,owner_api_blocked,
             fleet_api_blocked,attempts_migrated,fences_fingerprinted,
             legacy_fences FROM credential_format_state",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .unwrap()
    };
    let before = state_projection(&f);
    f.conn.set_test_barrier_failure(true);
    let blocked = both_blocked();
    let mut hooks = 0;
    let result = f
        .conn
        .commit_credential_migration(&plan(&[], &blocked, &[], &[], &[]), &mut || hooks += 1);
    assert!(
        result.is_err(),
        "mapped_no_work_replay_barrier_failure: ordinary no-work replay returns its barrier error"
    );
    assert_eq!(
        hooks, 0,
        "mapped_no_work_replay_barrier_failure: no counted COMMIT on replay failure"
    );
    assert_eq!(
        state_projection(&f),
        before,
        "mapped_no_work_replay_barrier_failure: slots reasons and historical totals are preserved"
    );
    assert_eq!(
        count(&f, "credential_refresh_attempts"),
        3,
        "mapped_no_work_replay_barrier_failure: actual mapped attempts remain"
    );
    assert_eq!(
        count(&f, "credential_legacy_fences"),
        2,
        "mapped_no_work_replay_barrier_failure: actual permanent legacy fences remain"
    );
    assert_eq!(
        count(&f, "credential_refresh_fences"),
        0,
        "mapped_no_work_replay_barrier_failure: replay invents no fingerprint"
    );
    f.conn.set_test_barrier_failure(false);
    // R73 may update durability diagnostics, intentionally excluded above.
    // The existing blocked/resolved failure cases exercise shared poisoned-write
    // guards; this case isolates no-work barrier-error propagation and history.
}

fn reopened_mapping_requires_ordinary_replay() {
    let mut f = migration_fixture(true, true, true);
    map_blocked(&mut f);
    let grant_secret = Zeroizing::new([0xa7; 32]);
    let sign_in_id = f
        .conn
        .insert_sign_in_binding(CredentialSlot::OwnerApi, binding(&grant_secret), T0_MS)
        .expect("reopened_mapping prepares one valid synthetic grant binding before close");
    let replacement = f.pair_with(
        CredentialSlot::OwnerApi,
        9_005,
        CredentialOrigin::HubLogin,
        RefreshOwner::Hub,
        b"reopened-mapped-grant-access",
        b"reopened-mapped-grant-refresh",
    );
    let provenance = EntryProvenance::HubGrant {
        sign_in_id,
        grant_secret: &grant_secret,
    };
    let Fixture {
        _temporary,
        store,
        conn,
        keys,
        nonces,
        sign_in_sequence,
    } = f;
    drop(conn);
    drop(store);
    let store = HubStore::initialize(_temporary.path())
        .expect("reopened_mapping synthetic writable reopen");
    let mut conn = store
        .take_credential_connection()
        .expect("reopened_mapping takes once on the new store");
    configure_test_memory_temp_store(conn.test_connection());
    conn.set_test_now_ms(T0_MS);
    let mut f = Fixture {
        _temporary,
        store,
        conn,
        keys,
        nonces,
        sign_in_sequence,
    };
    let before = snapshot(&f);
    let changes = total_changes(&f);
    assert!(
        matches!(
            f.conn.replace_credential_pair(
                &replacement,
                &provenance,
                &LegacyCandidates(Vec::new()),
                ReplacePolicy::ReplaceAny
            ),
            Err(StoreError::CredentialPairInvalid)
        ),
        "reopened_mapping refuses R28 before ordinary replay barrier"
    );
    assert_eq!(
        total_changes(&f),
        changes,
        "reopened_mapping premature R28 has no transient SQL write"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "reopened_mapping premature R28 stores no grant"
    );
    let binding_retained: bool = f
        .conn
        .test_connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_sign_ins WHERE sign_in_id=?1)",
            [sign_in_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        binding_retained,
        "reopened_mapping premature R28 retains the same valid grant binding"
    );
    let blocked = both_blocked();
    let mut hooks = 0;
    let counts = f
        .conn
        .commit_credential_migration(&plan(&[], &blocked, &[], &[], &[]), &mut || hooks += 1)
        .unwrap();
    assert_counts(&counts, 3, 0, 2, "reopened_mapping ordinary mapped replay");
    assert_eq!(
        hooks, 0,
        "reopened_mapping no-work replay has no counted COMMIT"
    );
    assert_eq!(
        total_changes(&f),
        changes,
        "reopened_mapping barrier replay performs no SQL write"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "reopened_mapping barrier replay preserves rows and markers"
    );
    f.conn
        .replace_credential_pair(
            &replacement,
            &provenance,
            &LegacyCandidates(Vec::new()),
            ReplacePolicy::ReplaceAny,
        )
        .expect("reopened_mapping ordinary replay barrier permits R28");
    f.conn
        .credential_durability_barrier()
        .expect("reopened_mapping accepted replacement is durable");
    let binding_remaining: bool = f
        .conn
        .test_connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_sign_ins WHERE sign_in_id=?1)",
            [sign_in_id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !binding_remaining,
        "reopened_mapping successful R28 consumes that same grant binding"
    );
    let generation_text: String = f
        .conn
        .test_connection()
        .query_row(
            "SELECT generation FROM credential_pairs WHERE slot='owner_api'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        generation_text,
        generation(9_005).to_string(),
        "reopened_mapping stores exactly the accepted replacement"
    );
    assert_eq!(
        count(&f, "legacy_owner_tokens"),
        0,
        "reopened_mapping discards only resolved Owner input"
    );
    assert_eq!(
        count(&f, "fleet_tokens"),
        1,
        "reopened_mapping preserves other blocked input"
    );
}

fn bulk_source_fences(
    f: &Fixture,
    slot: CredentialSlot,
    domain: u32,
    rows: usize,
) -> Vec<CredentialMigrationLegacyFence> {
    let table = if slot == CredentialSlot::OwnerApi {
        "legacy_refresh_input_fences"
    } else {
        "fleet_refresh_input_fences"
    };
    let transaction = f.conn.test_connection().unchecked_transaction().unwrap();
    let mut result = Vec::with_capacity(rows);
    {
        let mut insert = transaction
            .prepare(&format!(
                "INSERT INTO {table}(input_credential_generation) VALUES(?1)"
            ))
            .unwrap();
        for index in 0..rows {
            let text = format!("{domain:08x}-0000-4000-8000-{index:012x}");
            insert.execute([&text]).unwrap();
            result.push(CredentialMigrationLegacyFence {
                generation: Uuid::parse_str(&text).unwrap(),
                identity: LegacyFenceIdentity::Opaque,
                fenced_at_ms: PLAN_NOW,
            });
        }
    }
    transaction.commit().unwrap();
    result
}

fn destination_attempt_fences(f: &Fixture, rows: usize) {
    let transaction = f.conn.test_connection().unchecked_transaction().unwrap();
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO credential_refresh_fences(fingerprint,kind,fenced_at_ms)
                 VALUES(?1,'attempt',?2)",
            )
            .unwrap();
        for index in 0..rows {
            let mut fingerprint = [0x72_u8; 32];
            fingerprint[..8].copy_from_slice(&(index as u64).to_be_bytes());
            insert
                .execute(params![fingerprint.as_slice(), T0_MS])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
}

fn initial_capacity_case(pair_count: usize, overflow: bool) {
    let mut f = migration_fixture(true, pair_count == 2, true);
    let mut pairs = vec![migrated_pair(
        &mut f,
        CredentialSlot::OwnerApi,
        101,
        "capacity-owner-refresh",
    )];
    if pair_count == 2 {
        pairs.push(migrated_pair(
            &mut f,
            CredentialSlot::FleetApi,
            202,
            "capacity-fleet-refresh",
        ));
    }
    let legacy_count = 100_000 - pair_count + usize::from(overflow);
    let mut legacy = Vec::new();
    for (slot, id) in [
        (CredentialSlot::OwnerApi, generation(101)),
        (CredentialSlot::FleetApi, generation(202)),
    ]
    .into_iter()
    .take(pair_count)
    {
        source_fence(&f, slot, id, false);
        legacy.push(CredentialMigrationLegacyFence {
            generation: id,
            identity: LegacyFenceIdentity::Anchored,
            fenced_at_ms: PLAN_NOW,
        });
    }
    legacy.extend(bulk_source_fences(
        &f,
        CredentialSlot::OwnerApi,
        0x1100_0001,
        legacy_count - pair_count,
    ));
    let refresh = refresh_set(&pairs);
    assert_eq!(
        refresh.len(),
        pair_count,
        "migration_capacity_exact distinct fingerprints"
    );
    let candidate = plan(&pairs, &[], &[], &legacy, &refresh);
    if overflow {
        refuse(&mut f, &candidate, "migration_capacity_plus_one");
        assert_eq!(
            count(&f, "credential_pairs"),
            0,
            "migration_capacity_plus_one converts no slot"
        );
    } else {
        let counts = f
            .conn
            .commit_credential_migration(&candidate, &mut || {})
            .unwrap();
        assert_counts(
            &counts,
            0,
            pair_count as i64,
            legacy_count as i64,
            "migration_capacity_exact",
        );
        assert_eq!(
            count(&f, "credential_legacy_fences") + count(&f, "credential_refresh_fences"),
            100_000,
            "migration_capacity_exact admits the exact shared limit"
        );
    }
}

fn capacity_cases() {
    for pair_count in [1, 2] {
        initial_capacity_case(pair_count, false);
        initial_capacity_case(pair_count, true);
    }

    let mut f = migration_fixture(false, false, true);
    let mut legacy = bulk_source_fences(&f, CredentialSlot::OwnerApi, 0x2200_0001, 100_000);
    legacy.extend(bulk_source_fences(
        &f,
        CredentialSlot::FleetApi,
        0x2200_0002,
        100_000,
    ));
    assert_eq!(
        f.conn
            .read_pre_upgrade_refresh_fences(CredentialSlot::OwnerApi)
            .unwrap()
            .len(),
        100_000,
        "source_union_200000_refused_atomically reads complete Owner source"
    );
    assert_eq!(
        f.conn
            .read_pre_upgrade_refresh_fences(CredentialSlot::FleetApi)
            .unwrap()
            .len(),
        100_000,
        "source_union_200000_refused_atomically reads complete Fleet source"
    );
    refuse(
        &mut f,
        &plan(&[], &[], &[], &legacy, &[]),
        "source_union_200000_refused_atomically",
    );

    let mut f = migration_fixture(true, true, true);
    let attempts = initial_history(&f);
    let pairs = [
        migrated_pair(
            &mut f,
            CredentialSlot::OwnerApi,
            101,
            "casefold-shared-refresh",
        ),
        migrated_pair(
            &mut f,
            CredentialSlot::FleetApi,
            202,
            "casefold-shared-refresh",
        ),
    ];
    let legacy = legacy_set(true, true);
    let refresh = refresh_set(&pairs);
    assert_eq!(
        refresh.len(),
        1,
        "casefold_union_and_fingerprint_dedup derives distinct set"
    );
    let counts = f
        .conn
        .commit_credential_migration(&plan(&pairs, &[], &attempts, &legacy, &refresh), &mut || {})
        .unwrap();
    assert_counts(&counts, 3, 1, 2, "casefold_union_and_fingerprint_dedup");
    assert_eq!(count(&f, "credential_refresh_fences"), 1);

    let mut f = migration_fixture(true, false, true);
    source_fence(&f, CredentialSlot::OwnerApi, generation(101), false);
    let unrelated = generation(7_777);
    f.conn.test_connection().execute("INSERT INTO credential_legacy_fences(legacy_generation,identity,fenced_at_ms) VALUES(?1,'opaque',?2)", params![unrelated.to_string(),T0_MS]).unwrap();
    destination_attempt_fences(&f, 99_998);
    let pairs = [migrated_pair(
        &mut f,
        CredentialSlot::OwnerApi,
        101,
        "existing-fence-capacity",
    )];
    let legacy = [CredentialMigrationLegacyFence {
        generation: generation(101),
        identity: LegacyFenceIdentity::Anchored,
        fenced_at_ms: PLAN_NOW,
    }];
    let refresh = refresh_set(&pairs);
    refuse(
        &mut f,
        &plan(&pairs, &[], &[], &legacy, &refresh),
        "existing_fences_counted",
    );

    let mut f = migration_fixture(true, true, true);
    map_blocked(&mut f);
    destination_attempt_fences(&f, 99_998);
    let pairs = [migrated_pair(
        &mut f,
        CredentialSlot::OwnerApi,
        101,
        "late-capacity-refresh",
    )];
    let blocked = [(
        CredentialSlot::FleetApi,
        CredentialMigrationBlockReason::OldPairInvalid,
    )];
    refuse(
        &mut f,
        &plan(&pairs, &blocked, &[], &[], &[]),
        "late_anchor_capacity_refused",
    );
    assert_eq!(
        count(&f, "legacy_owner_tokens"),
        1,
        "late_anchor_capacity_refused keeps pending input"
    );
    assert_eq!(count(&f, "credential_pairs"), 0);
    assert_eq!(
        count(&f, "credential_refresh_fences"),
        99_998,
        "late_anchor_capacity_refused neither prunes nor inserts"
    );
}

fn historical_counts() {
    let mut f = migration_fixture(true, true, true);
    map_blocked(&mut f);
    // Put only the two terminal synthetic receipts outside the retention window.
    // Calling the retained pruning function then exercises its actual FK cascade.
    f.conn.test_connection().execute("UPDATE outbound_request_receipts SET started_at_ms=1,completed_at_ms=2,duration_ms=1 WHERE id IN (20,30)", []).unwrap();
    {
        let transaction = f.conn.test_connection().unchecked_transaction().unwrap();
        prune_expired_outbound_request_receipts(&transaction).unwrap();
        transaction.commit().unwrap();
    }
    assert_eq!(
        count(&f, "outbound_request_receipts"),
        1,
        "s_computes_counts retained pruning actually removed terminal receipts"
    );
    assert_eq!(
        count(&f, "credential_refresh_attempts"),
        1,
        "s_computes_counts runtime receipts cascade from mapped rows"
    );
    let before = f.conn.credential_format_state().unwrap();
    assert_eq!(
        before.attempts_migrated, 3,
        "s_computes_counts historical total survives pruning"
    );
    assert_eq!(
        before.legacy_fences, 2,
        "s_computes_counts permanent history total survives pruning"
    );
    let pairs = [migrated_pair(
        &mut f,
        CredentialSlot::OwnerApi,
        101,
        "historical-owner-refresh",
    )];
    let blocked = [(
        CredentialSlot::FleetApi,
        CredentialMigrationBlockReason::OldPairInvalid,
    )];
    let result = f
        .conn
        .commit_credential_migration(&plan(&pairs, &blocked, &[], &[], &[]), &mut || {})
        .unwrap();
    assert_counts(
        &result,
        3,
        1,
        2,
        "s_computes_counts late conversion preserves historical totals",
    );
    let after = f.conn.credential_format_state().unwrap();
    assert_eq!(after.attempts_migrated, 3);
    assert_eq!(after.fences_fingerprinted, 1);
    assert_eq!(after.legacy_fences, 2);
}

fn refresh_owner_contract() {
    fn seal_case(f: &mut Fixture, pair: &mut SealedCredentialPair, refresh: &str) {
        f.conn
            .reserve_and_seal_credential_pair(
                pair,
                &f.keys,
                b"synthetic-migration-access",
                refresh.as_bytes(),
                &mut f.nonces,
            )
            .expect(
                "C73-RESEAL-OK negative and restored owners have matching authenticated envelopes",
            );
    }
    let mut f = migration_fixture(false, true, false);
    let mut pair = migrated_pair(&mut f, CredentialSlot::FleetApi, 202, "synthetic-c73-fleet");
    assert_eq!(
        pair.refresh_owner,
        RefreshOwner::Hub,
        "C73-FLEET-BUILDER-01 Fleet is Hub without any output binding"
    );
    pair.origin = Some(CredentialOrigin::ProviderEntry);
    refuse(
        &mut f,
        &plan(std::slice::from_ref(&pair), &[], &[], &[], &[]),
        "C73-FLEET-ORIGIN-01 Fleet requires fleet_oauth",
    );
    pair.origin = Some(CredentialOrigin::FleetOauth);
    pair.refresh_owner = RefreshOwner::External;
    seal_case(&mut f, &mut pair, "synthetic-c73-fleet");
    refuse(
        &mut f,
        &plan(std::slice::from_ref(&pair), &[], &[], &[], &[]),
        "C73-FLEET-EXTERNAL-01 Fleet never becomes External",
    );
    pair.refresh_owner = RefreshOwner::Hub;
    seal_case(&mut f, &mut pair, "synthetic-c73-fleet");
    let mut commits = 0;
    let result = f
        .conn
        .commit_credential_migration(&plan(&[pair], &[], &[], &[], &[]), &mut || commits += 1);
    assert!(
        result.is_ok(),
        "C73-FLEET-HUB-01 Hub-owned Fleet without a matching output binding converts"
    );
    assert_eq!(
        commits, 1,
        "C73-FLEET-COMMIT-01 valid Fleet conversion commits once"
    );
    let state = f.conn.credential_format_state().unwrap();
    assert_eq!(
        state.fleet_api_state,
        CredentialSlotFormatState::Migrated,
        "C73-FLEET-STATE-01 accepted Fleet resolves its predecessor"
    );

    for output_matches in [true, false] {
        for outcome in [
            None,
            Some(OutboundRequestOutcome::Success),
            Some(OutboundRequestOutcome::HttpError),
            Some(OutboundRequestOutcome::Timeout),
            Some(OutboundRequestOutcome::TransportError),
            Some(OutboundRequestOutcome::AuthenticationRejected),
            Some(OutboundRequestOutcome::ProtocolError),
            Some(OutboundRequestOutcome::ResponseTooLarge),
            Some(OutboundRequestOutcome::Cancelled),
        ] {
            let mut f = migration_fixture(true, false, true);
            let binding = seed_binding(
                &f,
                CredentialSlot::OwnerApi,
                10,
                generation(8_100),
                generation(303),
                Some(generation(if output_matches { 101 } else { 404 })),
                T0_MS - 20,
                outcome.is_some(),
            );
            let expected =
                if output_matches && matches!(outcome, Some(OutboundRequestOutcome::Success)) {
                    RefreshOwner::Hub
                } else {
                    RefreshOwner::External
                };
            if let Some(outcome) = outcome {
                f.conn
                    .test_connection()
                    .execute(
                        "UPDATE outbound_request_receipts SET outcome=?1 WHERE id=10",
                        [outcome.as_str()],
                    )
                    .unwrap();
            }
            let mut pair =
                migrated_pair(&mut f, CredentialSlot::OwnerApi, 101, "synthetic-c73-owner");
            assert_eq!(
                pair.refresh_owner, expected,
                "C73-OWNER-BUILDER-01 only a matching completed successful receipt gives Hub ownership"
            );
            pair.refresh_owner = if expected == RefreshOwner::Hub {
                RefreshOwner::External
            } else {
                RefreshOwner::Hub
            };
            seal_case(&mut f, &mut pair, "synthetic-c73-owner");
            refuse(
                &mut f,
                &plan(
                    std::slice::from_ref(&pair),
                    &[],
                    std::slice::from_ref(&binding),
                    &[],
                    &[],
                ),
                "C73-OWNER-INVALID-01 reject the opposite R46 owner for each receipt state/output",
            );
            pair.refresh_owner = expected;
            seal_case(&mut f, &mut pair, "synthetic-c73-owner");
            let mut commits = 0;
            let result = f.conn.commit_credential_migration(
                &plan(&[pair], &[], &[binding], &[], &[]),
                &mut || commits += 1,
            );
            assert!(
                result.is_ok(),
                "C73-OWNER-VALID-01 every R46 receipt/output case accepts its exact owner"
            );
            assert_eq!(
                commits, 1,
                "C73-OWNER-COMMIT-01 valid ownership commits exactly once"
            );
            assert_counts(
                &result.unwrap(),
                1,
                0,
                0,
                "C73-OWNER-COUNTS-01 ownership preserves every mapped binding",
            );
        }
    }
}

pub(super) fn run_atomic_conversion() {
    refresh_owner_contract();
    initial_map();
    initial_validation();
    only_fenced_pairs_produce_fingerprints();
    late_conversion();
    late_migrated_dedup();
    empty_cases();
    pending_and_discard();
    commit_error_and_barrier();
    mapped_no_work_replay_barrier_failure();
    reopened_mapping_requires_ordinary_replay();
    capacity_cases();
    historical_counts();
}
