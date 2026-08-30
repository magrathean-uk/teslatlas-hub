// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::{
    teslamate_projection::TeslaMateCar,
    teslamate_stage::{TeslaMateStageLimits, TeslaMateStageTable},
};

#[test]
fn teslamate_check_snapshot_json_covers_connection_and_redacts_vin() {
    let snapshot = TeslaMateCheckSnapshot {
        schema: TeslaMateSchemaInfo {
            observed_migration_version: 105,
            observed_migration_count: 105,
            minimum_supported_migration_version: 105,
            maximum_validated_migration_version: 105,
            pinned_source_revision: "d6c43bc8",
            pinned_migration_set_sha256: "abc",
            fingerprint: "fp".to_owned(),
        },
        connection: TeslaMateConnectionDiagnostics {
            current_user: "reader".to_owned(),
            database: "teslamate".to_owned(),
            server_address: "127.0.0.1".to_owned(),
            server_port: 5432,
            postmaster_start_epoch_seconds: 1,
            transaction_read_only: true,
            private_schema_usage: false,
        },
        selected_car: TeslaMateSelectedCarDiagnostics {
            id: 1,
            name: Some("Athena".to_owned()),
            model: Some("3".to_owned()),
            vin_present: true,
        },
        open_sessions: TeslaMateOpenSessionCounts {
            drives: 0,
            charging_processes: 1,
            states: 1,
        },
        selected_car_counts: TeslaMateSelectedCarCounts {
            drives: 10,
            positions: 1_000,
            charging_processes: 4,
            charges: 40,
            states: 8,
            updates: 2,
        },
        source_totals: TeslaMateSourceTotals {
            cars: 1,
            drives: 10,
            positions: 1_000,
            charging_processes: 4,
            charges: 40,
            states: 8,
            updates: 2,
            schema_migrations: 105,
        },
        source_tokens_relation_present: true,
        legacy_token_pair: TeslaMateLegacyTokenPairDiagnostics {
            relation: "private.tokens".to_owned(),
            access_ciphertext_bytes: 128,
            refresh_ciphertext_bytes: 160,
        },
    };
    let value = serde_json::to_value(&snapshot).expect("JSON");
    assert_eq!(value["connection"]["transactionReadOnly"], true);
    assert_eq!(value["connection"]["database"], "teslamate");
    assert_eq!(value["selectedCar"]["vinPresent"], true);
    assert!(value["selectedCar"].get("vin").is_none());
    assert_eq!(value["openSessions"]["chargingProcesses"], 1);
    assert_eq!(value["selectedCarCounts"]["positions"], 1_000);
    assert_eq!(value["sourceTotals"]["schemaMigrations"], 105);
    assert_eq!(value["sourceTokensRelationPresent"], true);
    assert_eq!(value["legacyTokenPair"]["relation"], "private.tokens");
}

#[test]
fn postgres_transport_uses_plaintext_only_for_literal_loopback() {
    for source in [
        "postgresql://reader@127.0.0.1/db",
        "postgresql://reader@[::1]/db",
    ] {
        let source = ReadOnlySource::parse(source).unwrap();
        assert_eq!(
            source_transport(&source),
            SourceTransport::PlaintextLoopback
        );
        assert!(!source.connection_host().contains(['[', ']']));
    }
    for source in [
        "postgresql://reader@192.168.1.2/db",
        "postgresql://reader@db.example/db",
    ] {
        let source = ReadOnlySource::parse(source).unwrap();
        assert_eq!(source_transport(&source), SourceTransport::Rustls);
    }
}

#[test]
fn live_source_witness_is_fixed_read_only_and_never_reads_private_tokens() {
    assert!(LIVE_SOURCE_WITNESS_SQL.contains("current_setting('transaction_read_only')"));
    assert!(LIVE_SOURCE_WITNESS_SQL.contains("pg_postmaster_start_time()"));
    assert!(LIVE_SOURCE_WITNESS_SQL.contains("host(pg_catalog.inet_server_addr())"));
    assert!(!LIVE_SOURCE_WITNESS_SQL.contains("current_setting('data_directory')"));
    assert!(
        LIVE_SOURCE_WITNESS_SQL.contains("has_schema_privilege(current_user, 'private', 'USAGE')")
    );
    assert!(!LIVE_SOURCE_WITNESS_SQL.contains("private\".\"tokens"));
    assert!(!LIVE_SOURCE_WITNESS_SQL.contains("private.tokens"));
    for relation in [
        "cars",
        "drives",
        "positions",
        "charging_processes",
        "charges",
        "states",
        "updates",
        "schema_migrations",
    ] {
        assert!(LIVE_SOURCE_WITNESS_SQL.contains(&format!("\"public\".\"{relation}\"")));
    }
}

#[test]
fn exact_token_reader_requires_the_private_relation() {
    assert_eq!(
        exact_legacy_token_queries(true).expect("private token relation"),
        (
            PRIVATE_LEGACY_TOKEN_LENGTHS_SQL,
            PRIVATE_LEGACY_TOKENS_SQL,
            "private.tokens"
        )
    );
    assert!(matches!(
        exact_legacy_token_queries(false),
        Err(TeslaMateReaderError::LegacyTokenPairMissing)
    ));
    assert!(PRIVATE_LEGACY_TOKENS_EXISTS_SQL.contains("pg_catalog.to_regclass"));
    assert!(PRIVATE_LEGACY_TOKENS_EXISTS_SQL.contains("'private.tokens'"));
    assert!(!PRIVATE_LEGACY_TOKENS_EXISTS_SQL.contains(';'));
}

#[test]
fn private_token_reader_query_is_bounded_and_fixed() {
    assert!(PRIVATE_LEGACY_TOKENS_SQL.contains("FROM \"private\".\"tokens\""));
    assert!(PRIVATE_LEGACY_TOKENS_SQL.contains("\"access\" AS \"access\""));
    assert!(PRIVATE_LEGACY_TOKENS_SQL.contains("\"refresh\" AS \"refresh\""));
    assert!(PRIVATE_LEGACY_TOKENS_SQL.ends_with("LIMIT 2"));
    assert!(!PRIVATE_LEGACY_TOKENS_SQL.contains("WHERE"));
    assert!(!PRIVATE_LEGACY_TOKENS_SQL.contains(';'));
    assert!(PRIVATE_LEGACY_TOKEN_LENGTHS_SQL.contains("pg_catalog.octet_length"));
    assert!(PRIVATE_LEGACY_TOKEN_LENGTHS_SQL.ends_with("LIMIT 2"));
    assert!(!PRIVATE_LEGACY_TOKEN_LENGTHS_SQL.contains(';'));
}

#[test]
fn ciphertext_lengths_allow_the_limit_and_reject_the_next_byte() {
    assert!(
        validate_legacy_ciphertext_length(
            "private.tokens",
            "access",
            MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES_I64
        )
        .is_ok()
    );
    match validate_legacy_ciphertext_length(
        "private.tokens",
        "access",
        MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES_I64 + 1,
    ) {
        Err(TeslaMateReaderError::LegacyTokenCiphertextTooLarge {
            relation,
            column,
            maximum,
            actual,
        }) => {
            assert_eq!(relation, "private.tokens");
            assert_eq!(column, "access");
            assert_eq!(maximum, MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES_I64);
            assert_eq!(actual, MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES_I64 + 1);
        }
        other => panic!("expected length error, got {other:?}"),
    }
}

#[test]
fn compatibility_requires_one_nonempty_bounded_token_pair() {
    assert!(matches!(
        validate_legacy_token_pair_lengths("private.tokens", &[]),
        Err(TeslaMateReaderError::LegacyTokenPairMissing)
    ));
    assert!(matches!(
        validate_legacy_token_pair_lengths("private.tokens", &[(1, 1), (2, 2)]),
        Err(TeslaMateReaderError::LegacyTokenPairAmbiguous)
    ));
    assert!(matches!(
        validate_legacy_token_pair_lengths("private.tokens", &[(0, 1)]),
        Err(TeslaMateReaderError::LegacyTokenPairEmpty)
    ));
    let valid = validate_legacy_token_pair_lengths(
        "private.tokens",
        &[(MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES_I64, 1)],
    )
    .expect("bounded pair");
    assert_eq!(
        valid.access_ciphertext_bytes,
        u64::try_from(MAX_LEGACY_TOKEN_CIPHERTEXT_BYTES_I64).unwrap()
    );
}

#[test]
fn legacy_token_ciphertexts_are_redacted_and_zeroizable() {
    let mut ciphertexts = TeslaMateLegacyTokenCiphertexts {
        access: b"access-ciphertext-marker".to_vec(),
        refresh: b"refresh-ciphertext-marker".to_vec(),
    };
    let debug = format!("{ciphertexts:?}");
    assert!(!debug.contains("access-ciphertext-marker"));
    assert!(!debug.contains("refresh-ciphertext-marker"));
    ciphertexts.zeroize();
    assert!(ciphertexts.access.iter().all(|byte| *byte == 0));
    assert!(ciphertexts.refresh.iter().all(|byte| *byte == 0));
}

#[test]
fn cleanup_failure_is_typed_and_keeps_the_primary_reader_error() {
    let temporary = tempfile::tempdir().expect("temporary stage directory");
    let stage = TeslaMateStage::create(
        temporary.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 1,
            max_stage_bytes: 64 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("stage");
    let path = stage.path().to_path_buf();
    std::fs::remove_file(path).expect("remove stage before cleanup");
    assert!(matches!(
        discard_stage_after_error(stage, TeslaMateReaderError::InvalidSelectedCarId),
        TeslaMateReaderError::StageCleanupFailure { primary, cleanup }
            if matches!(*primary, TeslaMateReaderError::InvalidSelectedCarId)
                && cleanup == TeslaMateStageCleanupFailureKind::MissingOrChanged
    ));
}

#[test]
fn same_snapshot_token_companion_keeps_exact_private_contract() {
    assert!(
        snapshot_import_sql("000003A0-1")
            .expect("validated snapshot")
            .starts_with("SET TRANSACTION SNAPSHOT '")
    );
    assert_eq!(
        exact_legacy_token_queries(true)
            .expect("exact private relation")
            .2,
        "private.tokens"
    );
    assert!(exact_legacy_token_queries(false).is_err());
    assert!(PRIVATE_LEGACY_TOKENS_SQL.ends_with("LIMIT 2"));
}

#[test]
fn import_limits_reject_unbounded_or_oversized_pages() {
    assert!(matches!(
        TeslaMateReadLimits {
            page_size: 0,
            ..TeslaMateReadLimits::default()
        }
        .validate(),
        Err(TeslaMateReaderError::InvalidPageSize)
    ));
    assert!(matches!(
        TeslaMateReadLimits {
            maximum_rows: 0,
            ..TeslaMateReadLimits::default()
        }
        .validate(),
        Err(TeslaMateReaderError::InvalidMaximumRows)
    ));
    assert!(matches!(
        TeslaMateReadLimits {
            parallel_copy_lanes: 0,
            ..TeslaMateReadLimits::default()
        }
        .validate(),
        Err(TeslaMateReaderError::InvalidParallelCopyLanes)
    ));
}

#[test]
fn capture_jobs_are_bounded_and_distributed_across_lanes() {
    let lanes = distribute_capture_jobs(4, 100, 10);
    assert_eq!(lanes.len(), 4);
    assert_eq!(lanes.iter().map(Vec::len).sum::<usize>(), 15);
    assert!(lanes.iter().all(|lane| lane.len() <= 4));
    assert_eq!(distribute_capture_jobs(1, 0, 0)[0].len(), 7);
}

#[test]
fn large_table_shards_are_contiguous_and_cover_each_id_once() {
    let jobs = shard_id_ranges(TeslaMateStageTable::Positions, 10, 4);
    let ranges = jobs
        .into_iter()
        .map(|job| match job {
            CaptureJob::IdRange {
                start_id, end_id, ..
            } => (start_id, end_id),
            CaptureJob::Table(_) => panic!("expected range"),
        })
        .collect::<Vec<_>>();
    assert_eq!(ranges, vec![(1, 2), (3, 5), (6, 7), (8, 10)]);
}

#[test]
fn row_budget_is_hard_before_retention() {
    let mut total = 2;
    assert!(matches!(
        retain_row(&mut total, 2),
        Err(TeslaMateReaderError::MaximumRowsExceeded { maximum: 2 })
    ));
    assert_eq!(total, 3);
}

#[test]
fn stale_open_parents_are_not_guessed_as_live() {
    assert_eq!(unique_open_parent::<u8>(vec![]), None);
    assert_eq!(unique_open_parent(vec![7]), Some(7));
    assert_eq!(unique_open_parent(vec![7, 8]), None);
}

#[test]
fn position_materialization_queries_are_finite_and_cap_plus_one() {
    assert_eq!(
        validate_materialized_history_position_count(100, 100).unwrap(),
        100
    );
    assert!(matches!(
        validate_materialized_history_position_count(101, 100),
        Err(
            TeslaMateReaderError::MaterializedHistoryPositionLimitExceeded {
                maximum: 100,
                count: 101
            }
        )
    ));
    let history = bounded_position_binary_copy_sql(7, 101);
    assert!(history.contains("\"source\".\"car_id\" = 7"));
    assert!(history.contains("LIMIT 101"));
    assert!(!history.contains("LIMIT ALL"));
    let count = materialized_position_count_sql(100);
    assert!(count.contains("COUNT(*)::bigint"));
    assert!(count.contains("WHERE \"car_id\" = $1"));
    assert!(count.contains("LIMIT 101"));

    let open = open_position_branch_copy_sql(7, OpenPositionBranch::Standalone, 101);
    assert!(open.contains("LIMIT 101"));
    assert!(!open.contains("LIMIT ALL"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_task_shutdown_is_bounded_and_abort_on_drop_is_exact() {
    struct DropWitness(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for DropWitness {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    let mut completed = Some(tokio::spawn(async {}));
    assert!(
        finish_connection_task(&mut completed, Duration::from_millis(50))
            .await
            .is_ok()
    );
    assert!(completed.is_none());

    let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
    let mut cancelled_finish = Some(tokio::spawn(async move {
        let _witness = DropWitness(Some(cancelled_tx));
        std::future::pending::<()>().await;
    }));
    tokio::task::yield_now().await;
    {
        let finish = finish_connection_task(&mut cancelled_finish, Duration::from_secs(1));
        tokio::pin!(finish);
        assert!(
            timeout(Duration::from_millis(20), &mut finish)
                .await
                .is_err()
        );
    }
    assert!(
        cancelled_finish.is_some(),
        "a cancelled finish future must leave the task owned for session Drop"
    );
    abort_connection_task(&mut cancelled_finish);
    timeout(Duration::from_secs(1), cancelled_rx)
        .await
        .expect("cancelled finish remains abortable")
        .expect("cancelled-finish drop witness");

    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let mut pending = Some(tokio::spawn(async move {
        let _witness = DropWitness(Some(dropped_tx));
        std::future::pending::<()>().await;
    }));
    tokio::task::yield_now().await;
    assert!(matches!(
        finish_connection_task(&mut pending, Duration::from_millis(20)).await,
        Err(TeslaMateReaderError::SnapshotConnectionShutdownTimedOut)
    ));
    timeout(Duration::from_secs(1), dropped_rx)
        .await
        .expect("aborted task drops its witness")
        .expect("drop witness sender");
    assert!(pending.is_none());

    let (drop_tx, drop_rx) = tokio::sync::oneshot::channel();
    let (drop_started_tx, drop_started_rx) = tokio::sync::oneshot::channel();
    let mut unfinished = Some(tokio::spawn(async move {
        let _witness = DropWitness(Some(drop_tx));
        let _ = drop_started_tx.send(());
        std::future::pending::<()>().await;
    }));
    drop_started_rx.await.expect("drop task started");
    abort_connection_task(&mut unfinished);
    timeout(Duration::from_secs(1), drop_rx)
        .await
        .expect("drop aborts task")
        .expect("drop witness sender");
    assert!(unfinished.is_none());

    let (session_drop_tx, session_drop_rx) = tokio::sync::oneshot::channel();
    let (session_started_tx, session_started_rx) = tokio::sync::oneshot::channel();
    let session_task = tokio::spawn(async move {
        let _witness = DropWitness(Some(session_drop_tx));
        let _ = session_started_tx.send(());
        std::future::pending::<()>().await;
    });
    session_started_rx.await.expect("session task started");
    drop(TeslaMateSnapshotSession::for_connection_task(session_task));
    timeout(Duration::from_secs(1), session_drop_rx)
        .await
        .expect("snapshot session Drop aborts its task")
        .expect("snapshot session drop witness");

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    let (cancel_started_tx, cancel_started_rx) = tokio::sync::oneshot::channel();
    let session_task = tokio::spawn(async move {
        let _witness = DropWitness(Some(cancel_tx));
        let _ = cancel_started_tx.send(());
        std::future::pending::<()>().await;
    });
    cancel_started_rx
        .await
        .expect("cancelled session task started");
    let session = TeslaMateSnapshotSession::for_connection_task(session_task);
    let finish = tokio::spawn(session.finish());
    tokio::task::yield_now().await;
    finish.abort();
    let _ = finish.await;
    timeout(Duration::from_secs(1), cancel_rx)
        .await
        .expect("cancelling snapshot session finish still aborts its task")
        .expect("cancelled snapshot session finish drop witness");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn non_cooperative_connection_abort_is_owned_until_drained() {
    struct BlockingDrop {
        release: std::sync::mpsc::Receiver<()>,
        dropped: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl Drop for BlockingDrop {
        fn drop(&mut self) {
            let _ = self.release.recv();
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _blocking_drop = BlockingDrop {
            release: release_rx,
            dropped: Some(dropped_tx),
        };
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    });
    started_rx.await.expect("non-cooperative task started");
    let mut task = Some(task);

    assert!(matches!(
        finish_connection_task(&mut task, Duration::from_millis(20)).await,
        Err(TeslaMateReaderError::SnapshotConnectionAbortTimedOut)
    ));
    assert!(task.is_none(), "the runtime drain task owns the JoinHandle");
    release_tx
        .send(())
        .expect("release blocking task destructor");
    timeout(Duration::from_secs(1), dropped_rx)
        .await
        .expect("owned connection task eventually drains")
        .expect("blocking drop witness");
}

#[tokio::test]
async fn first_parallel_lane_error_aborts_and_drains_siblings() {
    struct DropWitness(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for DropWitness {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    let temporary = tempfile::tempdir().expect("temporary stage directory");
    let mut stage = TeslaMateStage::create(
        temporary.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 10,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("stage");
    let (sender, mut receiver) = mpsc::channel(1);
    drop(sender);
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let mut lanes = JoinSet::new();
    lanes.spawn(async { Err(TeslaMateReaderError::InvalidSelectedCarId) });
    lanes.spawn(async move {
        let _witness = DropWitness(Some(dropped_tx));
        std::future::pending::<Result<(), TeslaMateReaderError>>().await
    });
    tokio::task::yield_now().await;

    assert!(matches!(
        coordinate_parallel_capture(&mut stage, &mut receiver, &mut lanes).await,
        Err(TeslaMateReaderError::InvalidSelectedCarId)
    ));
    timeout(Duration::from_secs(1), dropped_rx)
        .await
        .expect("sibling aborted")
        .expect("sibling drop witness");
    assert!(lanes.is_empty());
}

#[test]
fn selected_car_id_must_fit_the_source_smallint_domain() {
    assert!(matches!(
        selected_source_car_id(i64::from(i16::MAX)),
        Ok(value) if value == i16::MAX
    ));
    assert!(matches!(
        selected_source_car_id(i64::from(i16::MAX) + 1),
        Err(TeslaMateReaderError::SelectedCarIdOutOfRange)
    ));
}

#[test]
fn exported_snapshot_ids_are_strictly_safe_for_future_lane_sql() {
    assert_eq!(
        validate_exported_snapshot_id("000003A0-1".to_owned()).expect("snapshot ID"),
        "000003A0-1"
    );
    assert!(validate_exported_snapshot_id("000003A0-1-2".to_owned()).is_ok());
    for invalid in ["", "000003A0", "000003A0-'; SELECT 1", "-1"] {
        assert!(matches!(
            validate_exported_snapshot_id(invalid.to_owned()),
            Err(TeslaMateReaderError::InvalidExportedSnapshot)
        ));
    }
}

#[test]
fn capture_lane_sql_accepts_only_validated_postgres_snapshot_ids() {
    assert_eq!(
        snapshot_import_sql("000003A0-1").expect("snapshot SQL"),
        "SET TRANSACTION SNAPSHOT '000003A0-1'"
    );
    assert!(matches!(
        snapshot_import_sql("000003A0-1'; SELECT 1"),
        Err(TeslaMateReaderError::InvalidExportedSnapshot)
    ));
}

#[test]
fn binary_copy_statements_are_fixed_streaming_projection_queries() {
    for table in SourceTable::ALL {
        let sql = binary_copy_sql(table, 17);
        assert!(sql.starts_with("COPY ("));
        assert!(sql.ends_with("TO STDOUT WITH (FORMAT BINARY)"));
        assert!(sql.contains("17"));
        assert!(sql.contains("LIMIT ALL"));
        assert!(!sql.contains('$'));
        assert!(!sql.contains(';'));
    }
}

#[test]
fn related_position_copy_statement_wraps_the_reviewed_positions_projection() {
    let sql = related_positions_binary_copy_sql(7, &[3, 11]);
    assert!(sql.starts_with("COPY (SELECT \"related\".* FROM (\nSELECT"));
    assert!(sql.contains("FROM \"public\".\"positions\" AS \"source\""));
    assert!(sql.contains("\"source\".\"car_id\" = 7"));
    assert!(sql.contains("\"related\".\"id\" = ANY(ARRAY[3,11]::int4[])"));
    assert!(sql.ends_with("TO STDOUT WITH (FORMAT BINARY)"));
    assert!(!sql.contains('$'));
    assert!(!sql.contains(';'));
}

#[test]
fn open_position_copy_branches_are_fixed_and_do_not_use_exists() {
    let standalone = open_position_branch_copy_sql(7, OpenPositionBranch::Standalone, 101);
    assert!(standalone.contains(
        "WHERE \"source\".\"id\" > 0\n  AND \"source\".\"car_id\" = 7\n  \
         AND \"source\".\"drive_id\" IS NULL\nORDER BY \"source\".\"id\" ASC\nLIMIT 101"
    ));
    assert!(!standalone.contains("FROM (\nSELECT"));
    assert!(!standalone.contains("\"branch\""));
    assert!(!standalone.contains("OR EXISTS"));
    assert!(standalone.ends_with("TO STDOUT WITH (FORMAT BINARY)"));
    assert!(!standalone.contains('$'));
    assert!(!standalone.contains(';'));

    let active = open_position_branch_copy_sql(7, OpenPositionBranch::ActiveDrive(42), 101);
    assert!(active.contains(
        "WHERE \"source\".\"id\" > 0\n  AND \"source\".\"car_id\" = 7\n  \
         AND \"source\".\"drive_id\" = 42\nORDER BY \"source\".\"id\" ASC\nLIMIT 101"
    ));
    assert!(!active.contains("FROM (\nSELECT"));
    assert!(!active.contains("\"branch\""));
    assert!(!active.contains("OR EXISTS"));
    assert!(!active.contains('$'));
    assert!(!active.contains(';'));
}

#[test]
fn open_queries_are_scoped_to_active_rows_and_keep_standalone_positions() {
    let drives = open_rows_sql(SourceTable::Drives, "\"source\".\"end_date\" IS NULL");
    let charges = open_rows_sql(SourceTable::Charges, "\"process\".\"end_date\" IS NULL");
    let states = open_rows_sql(SourceTable::States, "\"source\".\"end_date\" IS NULL");
    for sql in [&drives, &charges, &states] {
        assert!(sql.contains("\"public\""));
        assert!(sql.contains("\"source\".\"id\" > $1"));
        assert!(
            sql.contains("\"source\".\"car_id\" = $3")
                || sql.contains("\"process\".\"car_id\" = $3")
        );
        assert!(sql.contains("ORDER BY \"source\".\"id\" ASC"));
    }
    let standalone = open_position_branch_copy_sql(7, OpenPositionBranch::Standalone, 101);
    assert!(standalone.contains("\"source\".\"drive_id\" IS NULL"));
    assert!(!standalone.contains("OR EXISTS"));
}

#[test]
fn car_binary_copy_types_match_the_reviewed_projection_width() {
    assert_eq!(
        car_copy_types().len(),
        projection(SourceTable::Cars).columns.len()
    );
    assert_eq!(car_copy_types()[0], Type::INT2);
    assert_eq!(car_copy_types()[1], Type::INT8);
    assert_eq!(car_copy_types()[6], Type::FLOAT8);
    assert_eq!(car_copy_types()[7], Type::INT4);
    assert_eq!(car_copy_types()[8], Type::INT4);
}

#[test]
fn legacy_car_settings_integer_values_are_range_checked_before_narrowing() {
    assert_eq!(
        narrow_smallint(i16::MIN as i32, "car_settings", "suspend_min")
            .expect("i16 minimum is representable"),
        i16::MIN
    );
    assert_eq!(
        narrow_smallint(i16::MAX as i32, "car_settings", "suspend_min")
            .expect("i16 maximum is representable"),
        i16::MAX
    );
    assert!(matches!(
        narrow_smallint(i32::from(i16::MAX) + 1, "car_settings", "suspend_min"),
        Err(TeslaMateReaderError::IntegerOutOfRange {
            table: "car_settings",
            column: "suspend_min",
        })
    ));
}

#[test]
fn drive_binary_copy_types_match_the_reviewed_projection_width() {
    assert_eq!(
        drive_copy_types().len(),
        projection(SourceTable::Drives).columns.len()
    );
    assert_eq!(drive_copy_types()[0], Type::INT4);
    assert_eq!(drive_copy_types()[10], Type::NUMERIC);
    assert_eq!(drive_copy_types()[19], Type::FLOAT8);
}

#[test]
fn position_binary_copy_types_match_the_reviewed_projection_width() {
    assert_eq!(
        position_copy_types().len(),
        projection(SourceTable::Positions).columns.len()
    );
    assert_eq!(position_copy_types()[3], Type::TIMESTAMP);
    assert_eq!(position_copy_types()[4], Type::NUMERIC);
    assert_eq!(position_copy_types()[2], Type::INT8);
    assert_eq!(position_copy_types()[6], Type::INT8);
    assert_eq!(position_copy_types()[7], Type::INT8);
    assert_eq!(position_copy_types()[8], Type::FLOAT8);
    assert_eq!(position_copy_types()[9], Type::FLOAT8);
    assert_eq!(position_copy_types()[13], Type::INT8);
    assert_eq!(position_copy_types()[14], Type::INT8);
    assert_eq!(position_copy_types()[20], Type::INT8);
    assert_eq!(position_copy_types()[23], Type::BOOL);
}

#[test]
fn charging_process_binary_copy_types_match_the_reviewed_projection_width() {
    assert_eq!(
        charging_process_copy_types().len(),
        projection(SourceTable::ChargingProcesses).columns.len()
    );
    assert_eq!(charging_process_copy_types()[5], Type::TIMESTAMP);
    assert_eq!(charging_process_copy_types()[7], Type::NUMERIC);
}

#[test]
fn charge_binary_copy_types_match_the_reviewed_projection_width() {
    assert_eq!(
        charge_copy_types().len(),
        projection(SourceTable::Charges).columns.len()
    );
    assert_eq!(charge_copy_types()[2], Type::TIMESTAMP);
    assert_eq!(charge_copy_types()[8], Type::NUMERIC);
    assert_eq!(charge_copy_types()[14], Type::TEXT);
}

#[test]
fn address_binary_copy_types_match_the_reviewed_projection_width() {
    assert_eq!(
        address_copy_types().len(),
        projection(SourceTable::Addresses).columns.len()
    );
    assert_eq!(address_copy_types(), &[Type::INT4, Type::TEXT, Type::TEXT]);
}

#[test]
fn geofence_geometry_projection_contains_required_columns() {
    assert!(GEOFENCE_GEOMETRY_SQL.contains("latitude"));
    assert!(GEOFENCE_GEOMETRY_SQL.contains("longitude"));
    assert!(GEOFENCE_GEOMETRY_SQL.contains("radius_m"));
}

#[test]
fn settings_v2_2_singleton_query_preserves_all_physical_values() {
    let select = SETTINGS_V2_2_SQL
        .split("FROM public.settings")
        .next()
        .expect("select clause");
    assert_eq!(select.matches("source.").count(), 11);
    for column in [
        "id",
        "unit_of_length",
        "unit_of_temperature",
        "unit_of_pressure",
        "preferred_range",
        "base_url",
        "grafana_url",
        "language",
        "theme_mode",
        "inserted_at",
        "updated_at",
    ] {
        assert!(select.contains(column), "missing settings column {column}");
    }
    assert_eq!(select.matches("::text").count(), 4);
    for cast in [
        "source.unit_of_length::text",
        "source.unit_of_temperature::text",
        "source.unit_of_pressure::text",
        "source.preferred_range::text",
    ] {
        assert!(select.contains(cast), "missing reviewed enum cast {cast}");
    }
    for forbidden in ["WHERE", "$1", "$2", "$3", "COALESCE", "CASE"] {
        assert!(
            !SETTINGS_V2_2_SQL.contains(forbidden),
            "settings singleton query must not add {forbidden}"
        );
    }
    assert!(SETTINGS_V2_2_SQL.contains("ORDER BY source.id ASC"));
    assert!(SETTINGS_V2_2_SQL.contains("LIMIT 2"));

    assert_eq!(
        "km".parse::<ProjectionUnitOfLengthV2_2>(),
        Ok(ProjectionUnitOfLengthV2_2::Kilometers)
    );
    assert_eq!(
        "F".parse::<ProjectionUnitOfTemperatureV2_2>(),
        Ok(ProjectionUnitOfTemperatureV2_2::Fahrenheit)
    );
    assert_eq!(
        "psi".parse::<ProjectionUnitOfPressureV2_2>(),
        Ok(ProjectionUnitOfPressureV2_2::Psi)
    );
    assert_eq!(
        "ideal".parse::<ProjectionPreferredRangeV2_2>(),
        Ok(ProjectionPreferredRangeV2_2::Ideal)
    );
    assert!("kpa".parse::<ProjectionUnitOfPressureV2_2>().is_err());
    for value in [i64::MIN, 0, i64::MAX] {
        validate_timestamp_0_pg_us(value, "settings", "inserted_at").unwrap();
    }
}

#[test]
fn cars_and_car_settings_v2_2_production_query_is_exact_and_physical() {
    let select = CARS_AND_CAR_SETTINGS_V2_2_SQL
        .split("FROM public.cars")
        .next()
        .expect("select clause");
    assert_eq!(select.matches("source.").count(), 16);
    assert_eq!(select.matches("car_settings.").count(), 8);
    for column in [
        "id",
        "eid",
        "vid",
        "vin",
        "name",
        "model",
        "efficiency",
        "trim_badging",
        "marketing_name",
        "exterior_color",
        "wheel_type",
        "spoiler_type",
        "display_priority",
        "inserted_at",
        "updated_at",
        "settings_id",
    ] {
        assert!(select.contains(column), "missing cars column {column}");
    }
    for column in [
        "id AS car_settings_row_id",
        "suspend_min",
        "suspend_after_idle_min",
        "req_not_unlocked",
        "free_supercharging",
        "use_streaming_api",
        "enabled",
        "lfp_battery",
    ] {
        assert!(
            select.contains(column),
            "missing car_settings column {column}"
        );
    }
    for forbidden in [
        "public.settings",
        "efficiency_wh_per_km",
        "firmware_version",
        "::",
    ] {
        assert!(
            !CARS_AND_CAR_SETTINGS_V2_2_SQL.contains(forbidden),
            "physical local candidate must not contain {forbidden}"
        );
    }
    for clause in [
        "INNER JOIN public.car_settings AS car_settings ON car_settings.id = source.settings_id",
        "WHERE source.id = $1",
        "ORDER BY source.id ASC",
        "LIMIT 1",
    ] {
        assert!(
            CARS_AND_CAR_SETTINGS_V2_2_SQL.contains(clause),
            "missing {clause}"
        );
    }
}

#[test]
fn update_binary_copy_types_match_the_reviewed_projection_width() {
    assert_eq!(
        update_copy_types().len(),
        projection(SourceTable::Updates).columns.len()
    );
    assert_eq!(
        update_copy_types(),
        &[
            Type::INT4,
            Type::INT2,
            Type::TIMESTAMP,
            Type::TIMESTAMP,
            Type::TEXT
        ]
    );
}

#[test]
fn sealed_stage_round_trips_the_small_snapshot_reader_contract() {
    let temporary = tempfile::tempdir().expect("temporary stage directory");
    let mut stage = TeslaMateStage::create(
        temporary.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 10,
            max_stage_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("stage");
    let car = TeslaMateCar {
        id: 1,
        eid: 88,
        vid: Some(99),
        vin: Some("5YJTESTVIN1234567".to_owned()),
        name: Some("Road car".to_owned()),
        model: Some("Model 3".to_owned()),
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        efficiency_wh_per_km: Some(0.145),
        settings: Default::default(),
    };
    stage
        .insert(TeslaMateStageTable::Cars, car.id, &car)
        .expect("stage car");
    stage.seal().expect("sealed");

    let history = materialize_small_staged_history(&stage, 10).expect("history");
    assert_eq!(history.cars, vec![car]);
    assert!(history.drives.is_empty());
    assert!(history.positions.is_empty());
    assert!(matches!(
        materialize_small_staged_history(&stage, 0),
        Err(TeslaMateReaderError::MaximumRowsExceeded { maximum: 0 })
    ));
}
