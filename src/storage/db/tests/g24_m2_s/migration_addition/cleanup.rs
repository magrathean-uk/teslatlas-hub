// SPDX-License-Identifier: AGPL-3.0-only
use super::*;

pub(super) fn run_cleanup_transitions() {
    same_value_marker_retry();
    below_rung_and_non_mutating_views();
    full_typed_state_views();
    state_enum_projection_bounds();
    exclusion_recorder();
    purge_and_markers();
    isolated_marker_prerequisites();
    failed_vacuum_does_not_authorise_purge();
    purge_commit_error_still_counts_callback();
    clean_live_state_never_reverses();
}

fn same_value_marker_retry() {
    for action in 0..4 {
        let mut f = migration_fixture(false, false, false);
        f.conn
            .commit_credential_migration(&empty_plan(T0_MS), &mut || {})
            .unwrap();
        if action != 0 {
            f.conn.vacuum_credential_catalogue().unwrap();
            f.conn.commit_credential_purge(&mut || {}).unwrap();
        }
        if action == 3 {
            f.conn.mark_credential_wal_scrubbed().unwrap();
            f.conn.mark_credential_files_removed().unwrap();
        }
        let record = |connection: &mut CredentialConnection| match action {
            0 => {
                connection.record_credential_secrets_exclusion(CredentialSecretsExclusion::Verified)
            }
            1 => connection.mark_credential_wal_scrubbed(),
            2 => connection.mark_credential_files_removed(),
            3 => connection.mark_credential_migration_complete(T0_MS + 5),
            _ => unreachable!(),
        };
        f.conn.set_test_barrier_failure(true);
        assert!(
            matches!(record(&mut f.conn), Err(StoreError::CatalogueDurability(_))),
            "C73-MARKER-FIRST-01 actual committed marker encounters a failed barrier: action={action}"
        );
        let state = f.conn.credential_format_state().unwrap();
        let committed = match action {
            0 => state.secrets_exclusion == Some(CredentialSecretsExclusion::Verified),
            1 => state.wal_scrubbed,
            2 => state.old_files_removed,
            3 => state.completed_at_ms == Some(T0_MS + 5),
            _ => unreachable!(),
        };
        assert!(
            committed,
            "C73-MARKER-PERSISTED-01 the first marker commit is present despite its failed barrier: action={action}"
        );
        f.conn.set_test_barrier_failure(false);
        let before = snapshot(&f);
        let changes = f.conn.test_connection().total_changes();
        assert!(
            matches!(record(&mut f.conn), Err(StoreError::CredentialPairInvalid)),
            "C73-MARKER-RETRY-01 same-value replay cannot acknowledge an unproven commit: action={action}"
        );
        assert_eq!(
            f.conn.test_connection().total_changes(),
            changes,
            "C73-MARKER-RETRY-02 poisoned same-value replay writes nothing: action={action}"
        );
        assert_eq!(
            snapshot(&f),
            before,
            "C73-MARKER-RETRY-03 poisoned same-value replay preserves persisted state: action={action}"
        );
    }
}

fn refuse_enum_state(f: &mut Fixture, context: &str) {
    let before = snapshot(f);
    let before_changes = f.conn.test_connection().total_changes();
    reset_test_credential_format_state_enum_bytes();
    reset_test_credential_format_state_queries();
    let result = f.conn.credential_format_state();
    assert_eq!(
        test_credential_format_state_enum_bytes(),
        0,
        "ENUM-BOUND-01 finite SQL enum projection returns no variable bytes before decoder refusal: {context}"
    );
    assert_eq!(
        test_credential_format_state_queries(),
        1,
        "ENUM-BOUND-02 malformed enum uses one bounded state SELECT: {context}"
    );
    assert!(
        matches!(result, Err(StoreError::CredentialPairInvalid)),
        "ENUM-REFUSE-01 malformed enum fails closed with the fixed validation error: {context}"
    );
    assert_eq!(
        snapshot(f),
        before,
        "ENUM-REFUSE-02 malformed enum is never repaired or written by a state read: {context}"
    );
    assert_eq!(
        f.conn.test_connection().total_changes(),
        before_changes,
        "ENUM-REFUSE-03 malformed state read issues no same-value or rolled-back SQL mutation: {context}"
    );
}

fn state_enum_projection_bounds() {
    for (column, valid) in STATE_ENUM_COLUMNS {
        let mut f = enum_state_fixture();
        f.conn
            .credential_format_state()
            .expect("ENUM-SETUP-OK the remaining published state is valid before corruption");
        set_state_text_for_refusal(&f, column, &"x".repeat(1_000_000));
        refuse_enum_state(&mut f, &format!("{column}: huge unknown TEXT"));

        let mut f = enum_state_fixture();
        set_state_text_for_refusal(&f, column, &valid.to_ascii_uppercase());
        refuse_enum_state(&mut f, &format!("{column}: wrong exact case"));

        for blob in [false, true] {
            let mut f = enum_state_fixture();
            allow_malformed_enum_fixture_values(&f);
            if blob {
                f.conn
                    .test_connection()
                    .execute(
                        &format!("UPDATE credential_format_state SET {column}=?1"),
                        [&[0x78u8; 64][..]],
                    )
                    .unwrap();
            } else {
                f.conn
                    .test_connection()
                    .execute(
                        &format!("UPDATE credential_format_state SET {column}=1"),
                        [],
                    )
                    .unwrap();
            }
            refuse_enum_state(&mut f, &format!("{column}: non-TEXT type, blob={blob}"));
        }
    }

    for column in ["owner_api_state", "fleet_api_state"] {
        // All non-TEXT values are invalid regardless of any internal enum
        // encoding. None/no-reason leaves no pending-row invariant to mask
        // a permissive integer projection.
        for value in [
            rusqlite::types::Value::Integer(0),
            rusqlite::types::Value::Integer(1),
            rusqlite::types::Value::Blob(vec![0x78; 64]),
        ] {
            let mut f = migration_fixture(false, false, false);
            allow_malformed_enum_fixture_values(&f);
            f.conn
                .test_connection()
                .execute(
                    &format!("UPDATE credential_format_state SET {column}=?1"),
                    [&value],
                )
                .unwrap();
            refuse_enum_state(
                &mut f,
                &format!("{column}: non-TEXT type on None/no-reason base: {value:?}"),
            );
        }

        let mut f = migration_fixture(false, false, false);
        allow_malformed_enum_fixture_values(&f);
        f.conn
            .test_connection()
            .execute(
                &format!("UPDATE credential_format_state SET {column}=NULL"),
                [],
            )
            .unwrap();
        refuse_enum_state(&mut f, &format!("{column}: required NULL is invalid"));
    }
    for column in [
        "owner_api_blocked",
        "fleet_api_blocked",
        "secrets_exclusion",
        "durability",
    ] {
        let mut f = enum_state_fixture();
        let suffix = if column == "durability" {
            ",durability_probed_at_ms=NULL"
        } else {
            ""
        };
        f.conn
            .test_connection()
            .execute(
                &format!("UPDATE credential_format_state SET {column}=NULL{suffix}"),
                [],
            )
            .unwrap();
        let before = snapshot(&f);
        reset_test_credential_format_state_enum_bytes();
        reset_test_credential_format_state_queries();
        let result = f.conn.credential_format_state();
        assert_eq!(
            test_credential_format_state_enum_bytes(),
            0,
            "ENUM-NULL-01 optional SQL NULL preserves a fixed-width projection: {column}"
        );
        assert_eq!(
            test_credential_format_state_queries(),
            1,
            "ENUM-NULL-02 optional NULL needs exactly one state read: {column}"
        );
        let state = result
            .expect("ENUM-NULL-OK optional enum NULL is valid, never an invalid-code sentinel");
        let absent = match column {
            "owner_api_blocked" => state.owner_api_blocked.is_none(),
            "fleet_api_blocked" => state.fleet_api_blocked.is_none(),
            "secrets_exclusion" => state.secrets_exclusion.is_none(),
            "durability" => state.durability.is_none(),
            _ => unreachable!(),
        };
        assert!(
            absent,
            "ENUM-NULL-03 optional NULL is retained as None: {column}"
        );
        if column == "durability" {
            assert_eq!(
                state.durability_probed_at_ms, None,
                "ENUM-NULL-PROBE-01 absent durability preserves an absent optional probe timestamp"
            );
        }
        assert_eq!(
            snapshot(&f),
            before,
            "ENUM-NULL-04 optional NULL view leaves every row unchanged: {column}"
        );
    }
}

fn isolated_marker_prerequisites() {
    let mut f = migration_fixture(false, false, false);
    f.conn
        .commit_credential_migration(&empty_plan(T0_MS), &mut || {})
        .unwrap();
    let before = snapshot(&f);
    assert!(
        matches!(
            f.conn.mark_credential_wal_scrubbed(),
            Err(StoreError::CredentialPairInvalid)
        ),
        "MARKER-ISOLATED-01 converted but purge-required source refuses WAL marker before attempting scrub"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "MARKER-ISOLATED-02 isolated purge prerequisite refusal writes no state"
    );
    f.conn.vacuum_credential_catalogue().unwrap();
    f.conn.commit_credential_purge(&mut || {}).unwrap();
    f.conn.mark_credential_wal_scrubbed().unwrap();
    let state = f.conn.credential_format_state().unwrap();
    assert!(
        state.converted_at_ms.is_some()
            && !state.purge_required
            && state.wal_scrubbed
            && !state.old_files_removed
            && state.completed_at_ms.is_none(),
        "MARKER-ISOLATED-03 only the files marker is missing after real conversion/purge/scrub"
    );
    let before = snapshot(&f);
    assert!(
        matches!(
            f.conn.mark_credential_migration_complete(T0_MS + 1),
            Err(StoreError::CredentialPairInvalid)
        ),
        "MARKER-ISOLATED-04 clean scrubbed source still cannot complete without file removal"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "MARKER-ISOLATED-05 missing-files completion refusal leaves all state unchanged"
    );
    f.conn.mark_credential_files_removed().unwrap();
    f.conn
        .mark_credential_migration_complete(T0_MS + 2)
        .unwrap();
    assert_eq!(
        f.conn.credential_format_state().unwrap().completed_at_ms,
        Some(T0_MS + 2),
        "MARKER-ISOLATED-06 completing the missing prerequisite admits completion"
    );
}

fn below_rung_and_non_mutating_views() {
    let f = fixture_with_memory_temp_store();
    f.conn
        .test_connection()
        .pragma_update(None, "user_version", 78)
        .unwrap();
    reset_test_credential_format_state_queries();
    let result = f.store.credential_format_state();
    assert_eq!(
        test_credential_format_state_queries(),
        0,
        "VIEW-BELOW-PRESENT-01 below-rung status never queries even a present valid C10 state table"
    );
    assert!(
        matches!(result, Ok(None)),
        "VIEW-BELOW-PRESENT-02 below-rung status ignores a present converted C10 row"
    );

    let mut f = fixture_with_memory_temp_store();
    drop_c10_for_floor(&f);
    let before = snapshot(&f);
    assert!(
        f.store.credential_format_state().unwrap().is_none(),
        "VIEW-BELOW-01 below-rung view returns None even when C10 state table is absent"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "VIEW-BELOW-02 below-rung view performs no migration, probe, barrier or write"
    );
    assert!(
        matches!(
            f.conn.credential_format_state(),
            Err(StoreError::UnsupportedSchema(78))
        ),
        "VIEW-BELOW-03 writable state API refuses below rung before querying C10"
    );

    let f = migration_fixture(true, false, true);
    let before = snapshot(&f);
    let (observer, version) = f.observe();
    configure_test_memory_temp_store(&observer);
    let state = f.store.credential_format_state().unwrap().unwrap();
    assert!(
        matches!(state.owner_api_state, CredentialSlotFormatState::Pending),
        "VIEW-AT-01 at-rung HubStore view reports persisted Pending slot"
    );
    assert!(
        state.fences_pending && !state.old_tables_dropped && state.purge_required,
        "VIEW-AT-02 state view preserves independent history/drop/purge flags"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "VIEW-AT-03 at-rung state view leaves every synthetic row unchanged"
    );
    assert_eq!(
        data_version(&observer),
        version,
        "VIEW-AT-04 at-rung view performs no state write or barrier COMMIT"
    );
}

fn assert_full_typed_state(state: CredentialFormatState, view: &str) {
    assert!(
        matches!(state.owner_api_state, CredentialSlotFormatState::Pending),
        "STATE-FIELD-01 Owner slot state is Pending through {view}"
    );
    assert!(
        matches!(state.fleet_api_state, CredentialSlotFormatState::Pending),
        "STATE-FIELD-02 Fleet slot state is Pending through {view}"
    );
    assert_eq!(
        state.owner_api_blocked,
        Some(CredentialMigrationBlockReason::OldKeyUnavailable),
        "STATE-FIELD-03 Owner block reason is preserved through {view}"
    );
    assert_eq!(
        state.fleet_api_blocked,
        Some(CredentialMigrationBlockReason::OldPairInvalid),
        "STATE-FIELD-04 Fleet block reason is preserved through {view}"
    );
    assert!(
        !state.fences_pending,
        "STATE-FIELD-05 ordinary mapped history is no longer pending through {view}"
    );
    assert!(
        !state.old_tables_dropped,
        "STATE-FIELD-06 blocked predecessor rows keep their tables through {view}"
    );
    assert!(
        state.purge_required,
        "STATE-FIELD-07 mapped blocked store still requires purge through {view}"
    );
    assert!(
        !state.wal_scrubbed,
        "STATE-FIELD-08 no WAL scrub is invented through {view}"
    );
    assert!(
        !state.old_files_removed,
        "STATE-FIELD-09 no file removal is invented through {view}"
    );
    assert_eq!(
        state.attempts_migrated, 3,
        "STATE-FIELD-10 ordinary mapping counts every binding through {view}"
    );
    assert_eq!(
        state.fences_fingerprinted, 0,
        "STATE-FIELD-11 no pair means no migrated fingerprint through {view}"
    );
    assert_eq!(
        state.legacy_fences, 2,
        "STATE-FIELD-12 ordinary mapping retains both legacy generations through {view}"
    );
    assert_eq!(
        state.purge_needed_bytes, None,
        "STATE-FIELD-13 no purge deferral invents a shortfall through {view}"
    );
    assert!(
        matches!(
            state.secrets_exclusion,
            Some(CredentialSecretsExclusion::NotApplicable)
        ),
        "STATE-FIELD-14 recorded exclusion decodes exactly through {view}"
    );
    assert!(
        matches!(
            state.durability,
            Some(CredentialDurabilityState::Unavailable)
        ),
        "STATE-FIELD-15 unavailable durability decodes exactly through {view}"
    );
    assert_eq!(
        state.durability_probed_at_ms,
        Some(T0_MS + 20),
        "STATE-FIELD-16 distinct probe timestamp is preserved through {view}"
    );
    assert_eq!(
        state.started_at_ms,
        Some(T0_MS + 10),
        "STATE-FIELD-17 distinct start timestamp is preserved through {view}"
    );
    assert_eq!(
        state.converted_at_ms, None,
        "STATE-FIELD-18 blocked slots prevent conversion through {view}"
    );
    assert_eq!(
        state.completed_at_ms, None,
        "STATE-FIELD-19 blocked slots prevent completion through {view}"
    );
}

fn full_typed_state_views() {
    let mut f = migration_fixture(true, true, true);
    for (view, state) in [
        (
            "HubStore",
            f.store
                .credential_format_state()
                .expect("STATE-PREPHASE-HUBSTORE-OK valid pre-Phase-B state is readable")
                .expect("STATE-PREPHASE-HUBSTORE-PRESENT-OK at-rung pre-Phase-B state is present"),
        ),
        (
            "CredentialConnection",
            f.conn
                .credential_format_state()
                .expect("STATE-PREPHASE-CONNECTION-OK valid pre-Phase-B state is readable"),
        ),
    ] {
        assert_eq!(
            state.started_at_ms, None,
            "STATE-OPTIONAL-TIME-01 explicitly seeded pre-Phase-B start time remains absent through {view}"
        );
        assert_eq!(
            state.durability_probed_at_ms, None,
            "STATE-OPTIONAL-TIME-02 explicitly seeded pre-probe time remains absent through {view}"
        );
    }
    // R47's ordinary transaction supplies the block reasons, history and
    // counts. Only published diagnostic metadata is seeded synthetically.
    atomic::map_blocked(&mut f);
    f.conn
        .record_credential_secrets_exclusion(CredentialSecretsExclusion::NotApplicable)
        .expect("STATE-EXCLUSION-OK R18 recorder accepts the ordinarily mapped blocked store");
    f.conn
        .test_connection()
        .execute(
            "UPDATE credential_format_state SET durability='unavailable',
             durability_probed_at_ms=?1,started_at_ms=?2",
            params![T0_MS + 20, T0_MS + 10],
        )
        .unwrap();
    let before = snapshot(&f);
    let before_changes = f.conn.test_connection().total_changes();
    let (observer, version) = f.observe();
    configure_test_memory_temp_store(&observer);
    assert_full_typed_state(
        f.store
            .credential_format_state()
            .expect("STATE-HUBSTORE-OK valid mapped state is readable through HubStore")
            .expect("STATE-HUBSTORE-PRESENT-OK at-rung state is present"),
        "HubStore",
    );
    assert_full_typed_state(
        f.conn.credential_format_state().expect(
            "STATE-CONNECTION-OK valid mapped state is readable through CredentialConnection",
        ),
        "CredentialConnection",
    );
    assert_eq!(
        snapshot(&f),
        before,
        "STATE-VIEW-NOWRITE-01 both complete projections leave all persisted values unchanged"
    );
    assert_eq!(
        f.conn.test_connection().total_changes(),
        before_changes,
        "STATE-VIEW-NOWRITE-02 complete CredentialConnection projection issues no SQL mutation"
    );
    assert_eq!(
        data_version(&observer),
        version,
        "STATE-VIEW-NOWRITE-03 complete HubStore projection commits no same-value write"
    );

    for durability in ["ok", "unavailable"] {
        f.conn
            .test_connection()
            .execute(
                "UPDATE credential_format_state SET durability=?1",
                [durability],
            )
            .unwrap();
        for (view, state) in [
            (
                "HubStore",
                f.store
                    .credential_format_state()
                    .expect("STATE-DURABILITY-HUBSTORE-OK valid diagnostic durability is readable")
                    .expect(
                        "STATE-DURABILITY-HUBSTORE-PRESENT-OK at-rung diagnostic state is present",
                    ),
            ),
            (
                "CredentialConnection",
                f.conn.credential_format_state().expect(
                    "STATE-DURABILITY-CONNECTION-OK valid diagnostic durability is readable",
                ),
            ),
        ] {
            assert!(
                matches!(
                    (durability, state.durability),
                    ("ok", Some(CredentialDurabilityState::Ok))
                        | ("unavailable", Some(CredentialDurabilityState::Unavailable))
                ),
                "STATE-DURABILITY-01 both valid durability values decode exactly through {view}: {durability}"
            );
            assert_eq!(
                state.durability_probed_at_ms,
                Some(T0_MS + 20),
                "STATE-DURABILITY-02 durability decoding preserves its distinct probe time through {view}"
            );
        }
    }
}

fn exclusion_recorder() {
    let mut f = fixture_with_memory_temp_store();
    let clean_at = f.conn.credential_format_state().unwrap().converted_at_ms;
    for (value, expected) in [
        (CredentialSecretsExclusion::Verified, "verified"),
        (CredentialSecretsExclusion::Failed, "failed"),
        (CredentialSecretsExclusion::NotApplicable, "not_applicable"),
    ] {
        f.conn
            .record_credential_secrets_exclusion(value)
            .expect("EXCLUSION-RECORD-OK R18 valid result records successfully on a clean source");
        let stored: String = f
            .conn
            .test_connection()
            .query_row(
                "SELECT secrets_exclusion FROM credential_format_state WHERE singleton_id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            stored, expected,
            "EXCLUSION-01 R18 recorder stores each exact admitted exclusion result"
        );
        let state = f.conn.credential_format_state().unwrap();
        assert!(
            matches!(
                (expected, state.secrets_exclusion),
                ("verified", Some(CredentialSecretsExclusion::Verified))
                    | ("failed", Some(CredentialSecretsExclusion::Failed))
                    | (
                        "not_applicable",
                        Some(CredentialSecretsExclusion::NotApplicable)
                    )
            ),
            "EXCLUSION-DECODE-01 R18 recorded result decodes to the exact typed exclusion variant"
        );
        assert_eq!(
            state.converted_at_ms, clean_at,
            "EXCLUSION-02 exclusion diagnostic cannot clear live conversion"
        );
        assert!(
            !state.purge_required,
            "EXCLUSION-03 exclusion diagnostic cannot reintroduce live purge"
        );
    }
    f.conn.set_test_barrier_failure(true);
    let result = f
        .conn
        .record_credential_secrets_exclusion(CredentialSecretsExclusion::Verified);
    assert!(
        result.is_err(),
        "EXCLUSION-04 exclusion state write cannot succeed after failed durability barrier"
    );
    f.conn.set_test_barrier_failure(false);
    // R73's barrier follows the first commit; its persisted result is not
    // asserted to roll back. The failed barrier must poison later writes.
    let before = snapshot(&f);
    let before_changes = f.conn.test_connection().total_changes();
    assert!(
        matches!(
            f.conn
                .record_credential_secrets_exclusion(CredentialSecretsExclusion::Failed),
            Err(StoreError::CredentialPairInvalid)
        ),
        "EXCLUSION-POISON-01 resetting the injected barrier fault does not authorise a later state write"
    );
    assert_eq!(
        f.conn.test_connection().total_changes(),
        before_changes,
        "EXCLUSION-POISON-02 refused post-barrier-failure write issues no SQL mutation"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "EXCLUSION-POISON-03 refused post-barrier-failure write preserves the persisted first result"
    );
}

fn purge_and_markers() {
    let mut f = migration_fixture(false, false, false);
    let before = snapshot(&f);
    assert!(
        matches!(
            f.conn.mark_credential_files_removed(),
            Err(StoreError::CredentialPairInvalid)
        ),
        "MARKER-01 removal marker requires conversion and its passed barrier"
    );
    assert!(
        matches!(
            f.conn.mark_credential_wal_scrubbed(),
            Err(StoreError::CredentialPairInvalid)
        ),
        "MARKER-02 scrub marker cannot precede conversion and purge"
    );
    assert!(
        matches!(
            f.conn.mark_credential_migration_complete(T0_MS),
            Err(StoreError::CredentialPairInvalid)
        ),
        "MARKER-03 completion cannot precede conversion"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "MARKER-04 marker prerequisite refusals write nothing"
    );

    let mut callbacks = 0;
    let counts = f
        .conn
        .commit_credential_migration(&empty_plan(T0_MS), &mut || callbacks += 1)
        .unwrap();
    assert_eq!(
        callbacks, 1,
        "PURGE-01 initial empty conversion has exactly one explicit-COMMIT callback"
    );
    assert_eq!(
        (
            counts.attempts_migrated,
            counts.fences_fingerprinted,
            counts.legacy_fences
        ),
        (0, 0, 0),
        "PURGE-02 empty conversion computes zero historical totals"
    );
    f.conn.defer_credential_purge(123_456).unwrap();
    let state = f.conn.credential_format_state().unwrap();
    assert_eq!(
        state.purge_needed_bytes,
        Some(123_456),
        "PURGE-03 capacity deferral records the exact shortfall"
    );
    assert_eq!(
        f.store
            .credential_format_state()
            .expect("PURGE-VIEW-OK actual deferred purge state is readable through HubStore")
            .expect("PURGE-VIEW-PRESENT-OK at-rung deferred purge state is present")
            .purge_needed_bytes,
        Some(123_456),
        "PURGE-VIEW-SHORTFALL-01 HubStore preserves the shortfall recorded by the real S deferral action"
    );
    assert!(
        state.purge_required && state.converted_at_ms == Some(T0_MS),
        "PURGE-04 deferral retains conversion and required purge"
    );

    // R44 step 8 says WHEN CONVERTED; purge and scrub may still be pending.
    f.conn.mark_credential_files_removed().unwrap();
    assert!(
        f.conn.credential_format_state().unwrap().old_files_removed,
        "MARKER-05 converted/barriered source may record file removal before purge"
    );
    let mut purge_callbacks = 0;
    assert!(
        matches!(
            f.conn.commit_credential_purge(&mut || purge_callbacks += 1),
            Err(StoreError::CredentialPairInvalid)
        ),
        "PURGE-05 state clear requires a successful VACUUM on this connection"
    );
    assert_eq!(
        purge_callbacks, 0,
        "PURGE-06 refused purge has no explicit-COMMIT callback"
    );
    assert!(
        matches!(
            f.conn.mark_credential_migration_complete(T0_MS + 1),
            Err(StoreError::CredentialPairInvalid)
        ),
        "MARKER-06 removal alone cannot complete while purge remains required"
    );

    f.conn.vacuum_credential_catalogue().unwrap();
    assert!(
        f.conn.credential_format_state().unwrap().purge_required,
        "PURGE-07 successful VACUUM does not itself clear the durable purge marker"
    );
    let purge_observer = f
        .store
        .open()
        .expect("PURGE-CALLBACK-OBSERVER-OK a separate fixture-owned purge observer opens");
    configure_test_memory_temp_store(&purge_observer);
    f.conn
        .commit_credential_purge(&mut || {
            purge_callbacks += 1;
            let persisted_required: bool = purge_observer
                .query_row(
                    "SELECT purge_required FROM credential_format_state WHERE singleton_id=1",
                    [],
                    |row| row.get(0),
                )
                .expect("PURGE-CALLBACK-READ-OK the observer reads committed purge state in the hook");
            assert!(
                persisted_required,
                "PURGE-COMMIT-BEFORE-01 the counted hook runs while the persisted purge marker is still required"
            );
        })
        .unwrap();
    assert_eq!(
        purge_callbacks, 1,
        "PURGE-08 VACUUM internal commit is not an extra counted explicit COMMIT"
    );
    let persisted_required: bool = purge_observer
        .query_row(
            "SELECT purge_required FROM credential_format_state WHERE singleton_id=1",
            [],
            |row| row.get(0),
        )
        .expect("PURGE-CALLBACK-AFTER-READ-OK the observer reads the successful purge commit");
    assert!(
        !persisted_required,
        "PURGE-COMMIT-AFTER-01 the same observer sees the purge marker cleared after successful COMMIT"
    );
    drop(purge_observer);
    let state = f.conn.credential_format_state().unwrap();
    assert!(
        !state.purge_required && state.purge_needed_bytes.is_none(),
        "PURGE-09 only committed post-VACUUM purge clears required/shortfall"
    );
    assert!(
        !state.wal_scrubbed,
        "PURGE-10 purge state clear resets WAL scrub evidence"
    );
    assert!(
        matches!(
            f.conn.mark_credential_migration_complete(T0_MS + 2),
            Err(StoreError::CredentialPairInvalid)
        ),
        "MARKER-07 post-purge state still needs actual WAL scrub"
    );

    f.conn.mark_credential_wal_scrubbed().unwrap();
    f.conn
        .mark_credential_migration_complete(T0_MS + 3)
        .unwrap();
    let state = f.conn.credential_format_state().unwrap();
    assert!(
        state.wal_scrubbed && state.old_files_removed,
        "MARKER-08 completion retains scrub and removal evidence"
    );
    assert_eq!(
        state.completed_at_ms,
        Some(T0_MS + 3),
        "MARKER-09 completion records the supplied nonnegative timestamp"
    );
    let (observer, version) = f.observe();
    configure_test_memory_temp_store(&observer);
    let before = snapshot(&f);
    let changes: i64 = f
        .conn
        .test_connection()
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    f.conn.mark_credential_wal_scrubbed().unwrap();
    f.conn.mark_credential_files_removed().unwrap();
    f.conn
        .mark_credential_migration_complete(T0_MS + 99)
        .unwrap();
    let counts = f
        .conn
        .commit_credential_migration(&empty_plan(T0_MS + 100), &mut || callbacks += 1)
        .unwrap();
    assert_eq!(
        callbacks, 1,
        "REPLAY-01 already-converted replay never invokes the counted COMMIT hook"
    );
    assert_eq!(
        (
            counts.attempts_migrated,
            counts.fences_fingerprinted,
            counts.legacy_fences
        ),
        (0, 0, 0),
        "REPLAY-02 replay returns the persisted historical totals"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "REPLAY-03 marker and completed-plan replay preserves all persisted values"
    );
    assert_eq!(
        data_version(&observer),
        version,
        "REPLAY-04 idempotent marker and conversion replay makes no database write"
    );
    let after_changes: i64 = f
        .conn
        .test_connection()
        .query_row("SELECT total_changes()", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        after_changes, changes,
        "REPLAY-05 idempotent replay does not issue even a same-value or rolled-back SQL mutation"
    );
}

fn failed_vacuum_does_not_authorise_purge() {
    let mut f = migration_fixture(false, false, false);
    f.conn
        .commit_credential_migration(&empty_plan(T0_MS), &mut || {})
        .unwrap();
    let blocker = f.store.open().unwrap();
    configure_test_memory_temp_store(&blocker);
    blocker
        .execute_batch("BEGIN IMMEDIATE")
        .expect("VACUUM-SETUP-01 a separate own-temp connection holds the write lock");
    f.conn
        .test_connection()
        .busy_timeout(std::time::Duration::ZERO)
        .unwrap();
    let error = f
        .conn
        .vacuum_credential_catalogue()
        .expect_err("VACUUM-01 the real VACUUM cannot acquire the other connection's write lock");
    assert!(
        matches!(error, StoreError::CredentialPairStore(rusqlite::Error::SqliteFailure(ref code, _))
            if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)),
        "VACUUM-SQL-01 failure is the actual VACUUM SQL lock refusal, not nested BEGIN or state validation"
    );
    blocker.execute_batch("ROLLBACK").unwrap();
    let before = snapshot(&f);
    let mut callbacks = 0;
    assert!(
        matches!(
            f.conn.commit_credential_purge(&mut || callbacks += 1),
            Err(StoreError::CredentialPairInvalid)
        ),
        "VACUUM-02 failed VACUUM never grants purge state-clear capability"
    );
    assert_eq!(
        callbacks, 0,
        "VACUUM-03 refused post-error purge has no COMMIT hook"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "VACUUM-04 failed VACUUM leaves purge/shortfall and all state untouched"
    );
}

fn purge_commit_error_still_counts_callback() {
    let mut f = migration_fixture(false, false, false);
    f.conn
        .commit_credential_migration(&empty_plan(T0_MS), &mut || {})
        .unwrap();
    f.conn.vacuum_credential_catalogue().unwrap();
    f.conn
        .test_connection()
        .execute_batch(
            "CREATE TABLE synthetic_purge_commit_fault(
           receipt_id INTEGER REFERENCES outbound_request_receipts(id)
             DEFERRABLE INITIALLY DEFERRED) STRICT;
         CREATE TRIGGER synthetic_purge_commit_error AFTER UPDATE OF purge_required
         ON credential_format_state WHEN NEW.purge_required=0 BEGIN
         INSERT INTO synthetic_purge_commit_fault VALUES(9223372036854770000); END;",
        )
        .unwrap();
    let error_probe = f
        .store
        .open()
        .expect("SETUP-PURGE-ERROR-PROBE-OK a separate fixture-owned write-lock probe opens");
    configure_test_memory_temp_store(&error_probe);
    error_probe.busy_timeout(std::time::Duration::ZERO).unwrap();
    error_probe
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK")
        .expect("SETUP-PURGE-ERROR-PROBE-CALIBRATED the observer can acquire and release the unlocked writer reservation");
    let before = snapshot(&f);
    let mut callbacks = 0;
    assert!(
        f.conn
            .commit_credential_purge(&mut || {
                callbacks += 1;
                let acquisition = error_probe.execute_batch("BEGIN IMMEDIATE");
                if acquisition.is_ok() {
                    error_probe.execute_batch("ROLLBACK").expect(
                        "SETUP-PURGE-ERROR-PROBE-RELEASE an unexpectedly acquired probe lock is released before refusal",
                    );
                }
                assert!(
                    matches!(
                        acquisition,
                        Err(rusqlite::Error::SqliteFailure(error, _))
                            if matches!(
                                error.code,
                                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                            )
                    ),
                    "PURGE-ERROR-COMMIT-BEFORE-01 the counted hook still runs while the failing COMMIT's writer transaction owns its lock"
                );
            })
            .is_err(),
        "COMMIT-PURGE-01 a deferred foreign-key error refuses actual COMMIT"
    );
    assert_eq!(
        callbacks, 1,
        "COMMIT-PURGE-02 callback fires immediately before even a failing COMMIT attempt"
    );
    let acquisition = error_probe.execute_batch("BEGIN IMMEDIATE");
    if acquisition.is_ok() {
        error_probe
            .execute_batch("ROLLBACK")
            .expect("SETUP-PURGE-ERROR-PROBE-AFTER-RELEASE the post-error calibration releases its own lock");
    }
    assert!(
        acquisition.is_ok(),
        "PURGE-ERROR-COMMIT-AFTER-01 the same probe acquires its writer reservation after failed COMMIT rollback"
    );
    drop(error_probe);
    assert_eq!(
        snapshot(&f),
        before,
        "COMMIT-PURGE-03 failing COMMIT rolls back state clear and trigger insertion"
    );
}

fn clean_live_state_never_reverses() {
    let mut f = fixture_with_memory_temp_store();
    let converted = f.conn.credential_format_state().unwrap().converted_at_ms;
    assert!(
        converted.is_some(),
        "MONOTONIC-01 fresh live store starts converted"
    );
    f.conn.defer_credential_purge(999).unwrap();
    f.conn
        .record_credential_secrets_exclusion(CredentialSecretsExclusion::Verified)
        .unwrap();
    f.conn.mark_credential_files_removed().unwrap();
    f.conn.mark_credential_wal_scrubbed().unwrap();
    f.conn
        .mark_credential_migration_complete(T0_MS + 1)
        .unwrap();
    let mut callbacks = 0;
    f.conn
        .commit_credential_purge(&mut || callbacks += 1)
        .unwrap();
    f.conn
        .commit_credential_migration(&empty_plan(T0_MS + 2), &mut || callbacks += 1)
        .unwrap();
    let state = f.conn.credential_format_state().unwrap();
    assert_eq!(
        state.converted_at_ms, converted,
        "MONOTONIC-02 every permitted S live action preserves conversion timestamp"
    );
    assert!(
        !state.purge_required,
        "MONOTONIC-03 every permitted S live action preserves cleared purge"
    );
    assert_eq!(
        callbacks, 0,
        "MONOTONIC-04 clean-state conversion/purge replays do not create counted commits"
    );
}
