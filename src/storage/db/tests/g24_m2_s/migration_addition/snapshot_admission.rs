// SPDX-License-Identifier: AGPL-3.0-only
use super::*;

pub(super) fn run_snapshot_admission() {
    below_rung_pending_and_read_error();
    malformed_state_has_no_handle();
    pending_and_blocked_refuse_before_destination();
    cleanup_shortfall_and_cached_pending();
    clean_through_later_copy();
}

fn malformed_state_has_no_handle() {
    for (column, _) in STATE_ENUM_COLUMNS {
        let f = enum_state_fixture();
        set_state_text_for_refusal(&f, column, &"x".repeat(1_000_000));
        f.conn
            .test_connection()
            .execute_batch(
                "CREATE TABLE synthetic_format_view_audit (
                     singleton_id INTEGER PRIMARY KEY CHECK(singleton_id=1),
                     update_count INTEGER NOT NULL CHECK(update_count>=0)
                 ) STRICT;
                 INSERT INTO synthetic_format_view_audit VALUES(1,0);
                 CREATE TRIGGER synthetic_format_view_audit_update
                 AFTER UPDATE ON credential_format_state BEGIN
                     UPDATE synthetic_format_view_audit
                     SET update_count=update_count+1 WHERE singleton_id=1;
                 END;",
            )
            .expect("synthetic malformed-state audit observer setup");
        let before = snapshot(&f);
        let (observer, before_version) = f.observe();
        configure_test_memory_temp_store(&observer);
        reset_test_credential_format_state_enum_bytes();
        reset_test_credential_format_state_queries();
        let result = f.store.begin_backup_snapshot();
        assert_eq!(
            test_credential_format_state_enum_bytes(),
            0,
            "ADMISSION-ENUM-BOUND-01 malformed capture returns no variable enum bytes before refusal: {column}"
        );
        assert!(
            result.is_err(),
            "ADMISSION-ENUM-REFUSE-01 malformed enum returns no snapshot handle: {column}"
        );
        assert!(
            test_credential_format_state_queries() <= 1,
            "ADMISSION-ENUM-BOUND-02 an erroneous capture never repeats the bounded state read: {column}"
        );
        assert_eq!(
            snapshot(&f),
            before,
            "ADMISSION-ENUM-REFUSE-02 malformed admission performs no state repair or write: {column}"
        );
        assert_eq!(
            data_version(&observer),
            before_version,
            "ADMISSION-ENUM-REFUSE-03 malformed admission makes no same-value committed write on its separate connection: {column}"
        );
    }
}

fn below_rung_pending_and_read_error() {
    let f = fixture_with_memory_temp_store();
    f.conn
        .test_connection()
        .pragma_update(None, "user_version", 78)
        .unwrap();
    reset_test_credential_format_state_queries();
    let handle = f.store.begin_backup_snapshot().expect(
        "ADMISSION-BELOW-PRESENT-OK below-rung capture with C10 tables still returns a handle",
    );
    assert_eq!(
        f.conn
            .test_connection()
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        i64::from(MIN_STORE_VERSION),
        "ADMISSION-BELOW-VERSION-01 below-rung snapshot admission preserves the supported floor user_version"
    );
    assert_eq!(
        test_credential_format_state_queries(),
        0,
        "ADMISSION-BELOW-PRESENT-01 below-rung capture never queries even a present valid C10 state table"
    );
    assert_eq!(
        handle.credential_backup_admission(),
        CredentialBackupAdmission::Pending,
        "ADMISSION-BELOW-PRESENT-02 version-first admission ignores a present converted C10 row"
    );
    drop(handle);

    let f = fixture_with_memory_temp_store();
    drop_c10_for_floor(&f);
    let before = snapshot(&f);
    reset_test_credential_format_state_queries();
    let handle = f.store.begin_backup_snapshot().unwrap();
    assert_eq!(
        test_credential_format_state_queries(),
        0,
        "ADMISSION-CAPTURE-01 below-rung capture executes no C10 state SELECT"
    );
    assert_eq!(
        handle.credential_backup_admission(),
        CredentialBackupAdmission::Pending,
        "ADMISSION-BELOW-01 version-first capture returns Pending with no C10 table"
    );
    let destination = f._temporary.path().join("below-rung-copy");
    assert!(
        matches!(
            handle.copy_to(&destination),
            Err(StoreError::BackupWaitsForCredentialCleanup)
        ),
        "ADMISSION-BELOW-02 below-rung copy refuses before destination creation"
    );
    assert!(
        !destination.exists(),
        "ADMISSION-BELOW-03 refused copy creates no destination"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "ADMISSION-BELOW-04 snapshot admission does not migrate or write source"
    );
    drop(handle);

    let f = fixture_with_memory_temp_store();
    f.conn
        .test_connection()
        .execute_batch("DROP TABLE credential_format_state")
        .unwrap();
    assert!(
        f.store.begin_backup_snapshot().is_err(),
        "ADMISSION-ERROR-01 at-rung read error returns no snapshot handle"
    );
}

fn pending_and_blocked_refuse_before_destination() {
    let f = migration_fixture(true, true, true);
    reset_test_credential_format_state_queries();
    let handle = f.store.begin_backup_snapshot().unwrap();
    assert_eq!(
        test_credential_format_state_queries(),
        1,
        "ADMISSION-CAPTURE-02 at-rung snapshot captures exactly one bounded state SELECT"
    );
    assert_eq!(
        handle.credential_backup_admission(),
        CredentialBackupAdmission::Pending,
        "ADMISSION-PENDING-01 unconverted/no-reason source remains Pending"
    );
    handle
        .copy_admission()
        .expect("ADMISSION-CAPTURE-SETUP retained capacity computation remains available");
    assert_eq!(
        test_credential_format_state_queries(),
        1,
        "ADMISSION-CAPTURE-03 cache getter and retained capacity computation perform no fresh state read"
    );
    let destination = f._temporary.path().join("pending-copy");
    assert!(
        matches!(
            handle.copy_to(&destination),
            Err(StoreError::BackupWaitsForCredentialCleanup)
        ),
        "ADMISSION-PENDING-02 pending source is refused before copy"
    );
    assert!(
        !destination.exists(),
        "ADMISSION-PENDING-03 pending refusal has no destination write"
    );
    drop(handle);
    let destination = f._temporary.path().join("pending-backup");
    assert!(
        matches!(
            f.store.backup_to(&destination),
            Err(StoreError::BackupWaitsForCredentialCleanup)
        ),
        "ADMISSION-PENDING-04 backup_to retains the copy_to credential guard"
    );
    assert!(
        !destination.exists(),
        "ADMISSION-PENDING-05 backup_to refusal precedes destination creation"
    );

    for (owner, fleet) in [
        (
            Some(CredentialMigrationBlockReason::OldKeyUnavailable),
            None,
        ),
        (None, Some(CredentialMigrationBlockReason::OldPairInvalid)),
        (
            Some(CredentialMigrationBlockReason::OldKeyUnavailable),
            Some(CredentialMigrationBlockReason::OldPairInvalid),
        ),
    ] {
        let f = migration_fixture(true, true, false);
        f.conn
            .test_connection()
            .execute(
                "UPDATE credential_format_state SET owner_api_blocked=?1,fleet_api_blocked=?2",
                params![
                    owner.map(|_| "old_key_unavailable"),
                    fleet.map(|_| "old_pair_invalid")
                ],
            )
            .unwrap();
        let handle = f.store.begin_backup_snapshot().unwrap();
        assert_eq!(
            handle.credential_backup_admission(),
            CredentialBackupAdmission::Blocked {
                owner_api: owner,
                fleet_api: fleet,
            },
            "ADMISSION-BLOCKED-01 cache preserves both distinct slot block reasons"
        );
        let destination = f._temporary.path().join("blocked-copy");
        assert!(
            matches!(
                handle.copy_to(&destination),
                Err(StoreError::BackupWaitsForCredentialCleanup)
            ),
            "ADMISSION-BLOCKED-02 either block reason refuses copying"
        );
        assert!(
            !destination.exists(),
            "ADMISSION-BLOCKED-03 blocked refusal is pre-publication"
        );
    }
}

fn cleanup_shortfall_and_cached_pending() {
    let mut f = migration_fixture(false, false, false);
    f.conn
        .commit_credential_migration(&empty_plan(T0_MS), &mut || {})
        .unwrap();
    f.conn.defer_credential_purge(987_654).unwrap();
    let handle = f.store.begin_backup_snapshot().unwrap();
    assert_eq!(
        handle.credential_backup_admission(),
        CredentialBackupAdmission::CleanupPending {
            needed_bytes: Some(987_654),
        },
        "ADMISSION-CLEANUP-01 conversion with purge shortfall is CleanupPending"
    );
    let destination = f._temporary.path().join("cleanup-copy");
    assert!(
        matches!(
            handle.copy_to(&destination),
            Err(StoreError::BackupWaitsForCredentialCleanup)
        ),
        "ADMISSION-CLEANUP-02 cleanup shortfall prevents publication"
    );
    assert!(
        !destination.exists(),
        "ADMISSION-CLEANUP-03 shortfall refusal creates nothing"
    );
    drop(handle);
    let before = snapshot(&f);
    assert!(
        matches!(
            f.conn.defer_credential_purge(u64::MAX),
            Err(StoreError::CredentialPairInvalid)
        ),
        "ADMISSION-CLEANUP-04 unrepresentable shortfall refuses without truncation"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "ADMISSION-CLEANUP-05 invalid shortfall has no partial diagnostic write"
    );

    let mut f = migration_fixture(false, false, false);
    let handle = f.store.begin_backup_snapshot().unwrap();
    assert_eq!(
        handle.credential_backup_admission(),
        CredentialBackupAdmission::Pending,
        "ADMISSION-CACHED-01 snapshot initially caches Pending"
    );
    f.conn
        .commit_credential_migration(&empty_plan(T0_MS), &mut || {})
        .unwrap();
    f.conn.vacuum_credential_catalogue().unwrap();
    f.conn.commit_credential_purge(&mut || {}).unwrap();
    let clean = f.conn.credential_format_state().unwrap();
    assert!(
        clean.converted_at_ms.is_some() && !clean.purge_required,
        "ADMISSION-CACHED-02 source really becomes clean after the older handle"
    );
    assert!(
        !clean.wal_scrubbed && clean.completed_at_ms.is_none(),
        "ADMISSION-CACHED-03 clean admission does not require scrub or completion"
    );
    let queries_before_cached_read = test_credential_format_state_queries();
    assert_eq!(
        handle.credential_backup_admission(),
        CredentialBackupAdmission::Pending,
        "ADMISSION-CACHED-04 cache reader does not re-query the now-clean source"
    );
    assert_eq!(
        test_credential_format_state_queries(),
        queries_before_cached_read,
        "ADMISSION-CAPTURE-04 cached Pending getter performs no state SELECT after source becomes clean"
    );
    let destination = f._temporary.path().join("stale-pending-copy");
    assert!(
        matches!(
            handle.copy_to(&destination),
            Err(StoreError::BackupWaitsForCredentialCleanup)
        ),
        "ADMISSION-CACHED-05 cached Pending remains refused after source purge"
    );
    assert!(
        !destination.exists(),
        "ADMISSION-CACHED-06 stale handle still writes no destination"
    );
    drop(handle);
    reset_test_credential_format_state_queries();
    let handle = f.store.begin_backup_snapshot().unwrap();
    assert_eq!(
        test_credential_format_state_queries(),
        1,
        "ADMISSION-CAPTURE-05 new handle takes one fresh admission read"
    );
    assert_eq!(
        handle.credential_backup_admission(),
        CredentialBackupAdmission::Clean,
        "ADMISSION-CACHED-07 a new snapshot handle admits the newly clean source"
    );
}

fn clean_through_later_copy() {
    let mut f = fixture_with_memory_temp_store();
    f.conn
        .test_connection()
        .execute_batch(
            "UPDATE credential_format_state SET wal_scrubbed=0,old_files_removed=0,
         completed_at_ms=NULL",
        )
        .unwrap();
    let handle = f.store.begin_backup_snapshot().unwrap();
    assert_eq!(
        handle.credential_backup_admission(),
        CredentialBackupAdmission::Clean,
        "ADMISSION-CLEAN-01 exactly converted plus purge-cleared admits without completion"
    );
    let converted = f.conn.credential_format_state().unwrap().converted_at_ms;
    f.conn
        .record_credential_secrets_exclusion(CredentialSecretsExclusion::Verified)
        .unwrap();
    f.conn.defer_credential_purge(555).unwrap();
    f.conn.mark_credential_files_removed().unwrap();
    f.conn.mark_credential_wal_scrubbed().unwrap();
    f.conn.mark_credential_migration_complete(T0_MS).unwrap();
    let mut callbacks = 0;
    f.conn
        .commit_credential_migration(&empty_plan(T0_MS), &mut || callbacks += 1)
        .unwrap();
    f.conn
        .commit_credential_purge(&mut || callbacks += 1)
        .unwrap();
    let state = f.conn.credential_format_state().unwrap();
    assert_eq!(
        state.converted_at_ms, converted,
        "ADMISSION-CLEAN-02 later live actions never clear conversion after clean capture"
    );
    assert!(
        !state.purge_required && callbacks == 0,
        "ADMISSION-CLEAN-03 later live actions never reintroduce purge or replay COMMIT"
    );
    assert_eq!(
        handle.credential_backup_admission(),
        CredentialBackupAdmission::Clean,
        "ADMISSION-CLEAN-04 clean cache remains valid across later source actions"
    );
    let destination = f._temporary.path().join("clean-copy");
    handle
        .copy_to(&destination)
        .expect("ADMISSION-CLEAN-COPY-OK a cached Clean source permits the retained online copy");
    let copied = Connection::open_with_flags(
        // The retained snapshot copy helper places the catalogue at the
        // destination root; data-backup container layout is a later layer.
        destination.join("hub.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("ADMISSION-CLEAN-COPY-01 open the retained snapshot catalogue destination");
    configure_test_memory_temp_store(&copied);
    let copied_clean: (Option<i64>, bool) = copied.query_row(
        "SELECT converted_at_ms,purge_required FROM credential_format_state WHERE singleton_id=1",
        [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(
        copied_clean,
        (converted, false),
        "ADMISSION-CLEAN-05 later online-copy snapshot is still credential-clean"
    );

    // Deliberately break only this synthetic source after the copy. A field-only
    // reader must remain usable; this is not a permitted live S/M transition.
    f.conn
        .test_connection()
        .execute_batch("DROP TABLE credential_format_state")
        .unwrap();
    assert_eq!(
        handle.credential_backup_admission(),
        CredentialBackupAdmission::Clean,
        "ADMISSION-CACHE-READ-01 getter returns the captured field without any fresh query"
    );
}
