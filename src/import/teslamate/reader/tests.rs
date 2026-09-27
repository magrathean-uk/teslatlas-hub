// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use crate::{
    teslamate_projection::{
        TeslaMateAddressPhysicalV2_2, TeslaMateCar, TeslaMateCarPhysicalV2_2,
        TeslaMateCarSettingsPhysicalV2_2, TeslaMateChargePhysicalV2_2,
        TeslaMateChargingProcessPhysicalV2_2, TeslaMateDrivePhysicalV2_2,
        TeslaMateGeofencePhysicalV2_2, TeslaMatePositionPhysicalV2_2,
        TeslaMateSettingsPhysicalV2_2, TeslaMateStatePhysicalV2_2, TeslaMateUpdatePhysicalV2_2,
    },
    teslamate_stage::{
        TeslaMateStageFormat, TeslaMateStageLimits, TeslaMateStageState, TeslaMateStageTable,
    },
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
fn related_position_copy_statement_filters_before_the_reviewed_ordering() {
    let sql = related_positions_binary_copy_sql(7, &[3, 11]);
    let filter = sql
        .find("\"source\".\"id\" = ANY(ARRAY[3,11]::int4[])")
        .expect("related position filter");
    let ordering = sql
        .find("ORDER BY \"source\".\"id\" ASC")
        .expect("canonical position ordering");
    assert!(sql.starts_with("COPY (\nSELECT"));
    assert!(sql.contains("FROM \"public\".\"positions\" AS \"source\""));
    assert!(sql.contains("\"source\".\"car_id\" = 7"));
    assert!(filter < ordering, "ID predicate must run before ORDER BY");
    assert_eq!(sql.matches("ORDER BY \"source\".\"id\" ASC").count(), 1);
    assert!(!sql.contains("SELECT \"related\".* FROM ("));
    assert!(!sql.contains("\"related\"."));
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
    assert!(GEOFENCE_GEOMETRY_SQL.contains("UNION"));
    assert!(GEOFENCE_GEOMETRY_SQL.contains("start_geofence_id"));
    assert!(GEOFENCE_GEOMETRY_SQL.contains("end_geofence_id"));
    assert!(GEOFENCE_GEOMETRY_SQL.contains("process.geofence_id"));
    assert!(!GEOFENCE_GEOMETRY_SQL.contains("EXISTS"));
}

#[test]
fn geofence_page_projection_joins_one_related_id_union() {
    let sql = projection(SourceTable::Geofences).sql;
    assert!(sql.contains("JOIN ("));
    assert_eq!(sql.matches("\n  UNION\n").count(), 2);
    assert!(sql.contains("\"drive\".\"start_geofence_id\" AS \"id\""));
    assert!(sql.contains("\"drive\".\"end_geofence_id\" AS \"id\""));
    assert!(sql.contains("\"process\".\"geofence_id\" AS \"id\""));
    assert!(sql.contains("ON \"related\".\"id\" = \"source\".\"id\""));
    assert!(!sql.contains("EXISTS"));
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
fn physical_v3_root_queries_keep_car_and_car_settings_separate() {
    assert!(CAR_V2_2_SQL.contains("FROM public.cars AS source"));
    assert!(CAR_V2_2_SQL.contains("WHERE source.id = $1"));
    assert!(!CAR_V2_2_SQL.contains("JOIN"));
    assert!(!CAR_V2_2_SQL.contains("public.car_settings"));
    assert!(!CAR_V2_2_SQL.contains("public.settings"));

    assert!(CAR_SETTINGS_V2_2_SQL.contains("FROM public.car_settings AS source"));
    assert!(CAR_SETTINGS_V2_2_SQL.contains("WHERE source.id = $1"));
    assert!(!CAR_SETTINGS_V2_2_SQL.contains("JOIN"));
    assert!(!CAR_SETTINGS_V2_2_SQL.contains("public.cars"));
    assert!(!CAR_SETTINGS_V2_2_SQL.contains("public.settings"));
}

#[test]
fn physical_relation_queries_use_nullable_initial_cursors() {
    for (table, sql, car_scope) in [
        ("drives", DRIVES_V2_2_SQL, "source.car_id = $3"),
        ("positions", POSITIONS_V2_2_SQL, "source.car_id = $3"),
        (
            "charging_processes",
            CHARGING_PROCESSES_V2_2_SQL,
            "source.car_id = $3",
        ),
        ("charges", CHARGES_V2_2_SQL, "process.car_id = $3"),
        ("addresses", ADDRESSES_V2_2_SQL, "drive.car_id = $3"),
        ("geofences", GEOFENCES_V2_2_SQL, "drive.car_id = $3"),
        ("states", STATES_V2_2_SQL, "source.car_id = $3"),
        ("updates", UPDATES_V2_2_SQL, "source.car_id = $3"),
    ] {
        assert!(sql.contains("$1::integer IS NULL OR source.id > $1"));
        assert!(sql.contains(car_scope));
        assert!(sql.contains("ORDER BY source.id ASC"));
        assert!(sql.contains("LIMIT $2"));
        assert!(sql.contains(&format!("FROM public.{table} AS source")));
        assert!(!sql.contains("private.tokens"));
    }
    assert!(CHARGES_V2_2_SQL.contains("INNER JOIN public.charging_processes AS process"));
    for sql in [ADDRESSES_V2_2_SQL, GEOFENCES_V2_2_SQL] {
        assert!(sql.contains("INNER JOIN ("));
        assert_eq!(sql.matches("\n  UNION\n").count(), 2);
        assert!(sql.contains("process.car_id = $3"));
        assert!(!sql.contains("source.raw"));
    }
    assert!(STATES_V2_2_SQL.contains("source.state::text AS state"));
    for sql in [
        DRIVES_V2_2_SQL,
        POSITIONS_V2_2_SQL,
        CHARGING_PROCESSES_V2_2_SQL,
        CHARGES_V2_2_SQL,
        ADDRESSES_V2_2_SQL,
        GEOFENCES_V2_2_SQL,
    ] {
        assert!(sql.contains("::text AS"));
        assert!(!sql.contains("::double precision"));
    }

    assert_eq!(
        advance_signed_v2_2_cursor(None, -7, "states").expect("nullable first cursor"),
        Some(-7)
    );
    assert!(matches!(
        require_positive_physical_id("states", -7),
        Err(TeslaMateReaderError::PhysicalSourceIdNotPositive {
            table: "states",
            id: -7
        })
    ));
}

#[test]
fn physical_fixed_numeric_and_float_decoders_preserve_source_values() {
    for (value, scale, expected) in [
        ("1.234567", 6, ProjectionFixedNumericV2_2::Finite(1_234_567)),
        ("-0.1", 1, ProjectionFixedNumericV2_2::Finite(-1)),
        ("42", 2, ProjectionFixedNumericV2_2::Finite(4_200)),
        (
            "999999999999.99",
            2,
            ProjectionFixedNumericV2_2::Finite(99_999_999_999_999),
        ),
        (
            "-999999999999.99",
            2,
            ProjectionFixedNumericV2_2::Finite(-99_999_999_999_999),
        ),
        ("NaN", 6, ProjectionFixedNumericV2_2::NaN),
    ] {
        assert_eq!(
            parse_fixed_numeric_v2_2(value, scale, "fixture", "numeric").unwrap(),
            expected
        );
    }
    assert!(matches!(
        parse_fixed_numeric_v2_2("0.001", 2, "fixture", "numeric"),
        Err(TeslaMateReaderError::DecimalFixedScale { .. })
    ));
    assert_eq!(
        ProjectionFloat64BitsV2_2::from_f64(-0.0).0,
        (-0.0_f64).to_bits()
    );
}

#[test]
fn physical_car_efficiency_is_bit_exact_or_rejected_before_staging() {
    for value in [0.0, -0.0, 0.1, -12.5, f64::MIN_POSITIVE, f64::MAX] {
        validate_stage_efficiency(Some(value)).expect("stageable finite FLOAT8");
        let encoded = serde_json::to_string(&value).expect("finite JSON");
        let decoded: f64 = serde_json::from_str(&encoded).expect("finite JSON round trip");
        assert_eq!(decoded.to_bits(), value.to_bits());
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            validate_stage_efficiency(Some(value)),
            Err(TeslaMateReaderError::PhysicalFloatNotStageable {
                table: "cars",
                column: "efficiency"
            })
        ));
    }
    validate_stage_efficiency(None).expect("nullable efficiency");
}

#[test]
fn failed_physical_capture_discards_its_private_open_stage() {
    let temporary = tempfile::tempdir().expect("temporary stage directory");
    let stage = TeslaMateStage::create_physical_v3(
        temporary.path().join("imports"),
        TeslaMateStageLimits {
            max_rows: 4,
            max_stage_bytes: 64 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("physical stage");
    let path = stage.path().to_path_buf();
    let error = discard_stage_after_error(stage, TeslaMateReaderError::SettingsSingletonMissing);
    assert!(matches!(
        error,
        TeslaMateReaderError::SettingsSingletonMissing
    ));
    assert!(!path.exists());
}

#[tokio::test]
async fn physical_v3_capture_uses_one_exported_snapshot_and_discards_hook_failures_when_configured()
{
    let (Ok(source_url), Ok(admin_url)) = (
        std::env::var("TESLATLAS_HUB_PHYSICAL_CAPTURE_TEST_POSTGRES_URL"),
        std::env::var("TESLATLAS_HUB_PHYSICAL_CAPTURE_TEST_POSTGRES_ADMIN_URL"),
    ) else {
        return;
    };
    let source = ReadOnlySource::parse(&source_url).expect("credential-free fixture source");
    let password = TeslaMatePostgresPassword::from_bytes(b"fixture-password")
        .expect("synthetic fixture password");
    let admin_config = admin_url
        .parse::<tokio_postgres::Config>()
        .expect("credential-free fixture admin URL");
    let (admin, connection) = admin_config
        .connect(NoTls)
        .await
        .expect("fixture admin connection");
    let connection_task = tokio::spawn(async move {
        connection.await.expect("fixture admin connection task");
    });

    for version in crate::teslamate_schema::tests::PINNED_MIGRATION_VERSIONS {
        admin
            .execute(
                "INSERT INTO public.schema_migrations(version) VALUES($1)",
                &[&version],
            )
            .await
            .expect("synthetic migration row");
    }
    admin
        .batch_execute(
            "INSERT INTO public.settings(
                 id, inserted_at, updated_at, unit_of_length,
                 unit_of_temperature, preferred_range, base_url, grafana_url,
                 language, unit_of_pressure, theme_mode
             ) VALUES (
                 100, TIMESTAMP '2000-01-01 00:00:01',
                 TIMESTAMP '2000-01-01 00:00:02', 'km', 'C', 'rated',
                 NULL, NULL, 'en', 'bar', 'system'
             );
             INSERT INTO public.car_settings(
                 id, suspend_min, suspend_after_idle_min, req_not_unlocked,
                 free_supercharging, use_streaming_api, enabled, lfp_battery
             ) VALUES (200, 21, 15, false, false, true, true, false);
             INSERT INTO public.cars(
                 id, eid, vid, model, efficiency, inserted_at, updated_at,
                 vin, name, trim_badging, settings_id, exterior_color,
                 spoiler_type, wheel_type, display_priority, marketing_name
             ) VALUES (
                 1, 1001, 2001, '3', '-0'::double precision,
                 TIMESTAMP '2000-01-01 00:00:03',
                 TIMESTAMP '2000-01-01 00:00:04',
                 'SYNTHETIC-VIN', 'Fixture', 'LR', 200, 'white',
                 'none', 'fixture-wheel', 1, 'Fixture 3'
             );
             INSERT INTO public.addresses(
                 id, display_name, latitude, longitude, name, house_number,
                 road, neighbourhood, city, county, postcode, state,
                 state_district, country, raw, inserted_at, updated_at,
                 osm_id, osm_type
             ) VALUES
                 (70, '', 'NaN'::numeric, -0.000001, 'Synthetic', NULL,
                  '', NULL, 'Test City', NULL, '', NULL, NULL, 'GB',
                  '{}'::jsonb, '-infinity'::timestamp,
                  'infinity'::timestamp, -9223372036854775808, ''),
                 (71, 'unrelated', 1.000001, 2.000002, NULL, NULL,
                  NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
                  NULL, TIMESTAMP '2000-01-01 00:00:05',
                  TIMESTAMP '2000-01-01 00:00:06', NULL, NULL);
             INSERT INTO public.geofences(
                 id, name, latitude, longitude, radius, billing_type,
                 cost_per_unit, session_fee, inserted_at, updated_at
             ) VALUES
                 (80, 'Synthetic geofence', 'NaN'::numeric, -0.000001,
                  -32768, 'per_kwh', 99999.9999, -999999999999.99,
                  '-infinity'::timestamp, 'infinity'::timestamp),
                 (81, 'Unrelated geofence', 1.000001, 2.000002,
                  1, 'per_minute', NULL, NULL,
                  TIMESTAMP '2000-01-01 00:00:05',
                  TIMESTAMP '2000-01-01 00:00:06');
             INSERT INTO public.updates(id, start_date, end_date, version, car_id)
             VALUES (
                 10, TIMESTAMP '2000-01-01 00:00:00.123456', NULL,
                 'pre-export', 1
             );
             INSERT INTO public.states(id, car_id, state, start_date, end_date)
             VALUES (
                 20, 1, 'online', TIMESTAMP '2000-01-01 00:00:00.234567',
                 'infinity'::timestamp
             );
             INSERT INTO public.drives(
                 id, car_id, start_date, end_date, start_address_id,
                 start_geofence_id, outside_temp_avg, start_km
             ) VALUES (
                 30, 1, TIMESTAMP '2000-01-01 00:00:00.345678',
                 'infinity'::timestamp, 70, 80,
                 'NaN'::numeric, '-0'::double precision
             );
             INSERT INTO public.positions(
                 id, car_id, drive_id, date, latitude, longitude, odometer
             ) VALUES (
                 40, 1, 30, TIMESTAMP '2000-01-01 00:00:00.456789',
                 'NaN'::numeric, 1.234567, '-0'::double precision
             );
             INSERT INTO public.charging_processes(
                 id, car_id, position_id, address_id, geofence_id,
                 start_date, end_date,
                 charge_energy_added, outside_temp_avg, cost
             ) VALUES (
                 50, 1, 40, 70, 80,
                 TIMESTAMP '2000-01-01 00:00:00.567890',
                 'infinity'::timestamp, 'NaN'::numeric, -0.1,
                 999999999999.99
             );
             INSERT INTO public.charges(
                 id, charging_process_id, date,
                 battery_heater, battery_heater_on, battery_heater_no_power,
                 charge_energy_added, charger_power, conn_charge_cable,
                 fast_charger_present, ideal_battery_range_km,
                 rated_battery_range_km, outside_temp
             ) VALUES (
                 60, 50, '-infinity'::timestamp,
                 NULL, false, true, 'NaN'::numeric, -32768, '',
                 NULL, -9999.99, 'NaN'::numeric, -0.1
             );",
        )
        .await
        .expect("synthetic physical roots and pre-export relations");

    let temporary = tempfile::tempdir().expect("private capture root");
    let imports_dir = temporary.path().join("success");
    let limits = TeslaMateReadLimits {
        page_size: 1,
        maximum_rows: 11,
        maximum_stage_bytes: 256 * 1024,
        minimum_free_bytes: 0,
        parallel_copy_lanes: 1,
        ..TeslaMateReadLimits::default()
    };
    let stage = capture_physical_v3_to_stage_with_post_export(
        &source,
        &password,
        1,
        limits,
        &imports_dir,
        || async {
            admin
                .batch_execute(
                    "INSERT INTO public.addresses(
                         id, inserted_at, updated_at
                     ) VALUES (
                         72, TIMESTAMP '2000-01-01 00:00:07',
                         TIMESTAMP '2000-01-01 00:00:08'
                     );
                     INSERT INTO public.geofences(
                         id, name, latitude, longitude, radius, billing_type,
                         inserted_at, updated_at
                     ) VALUES (
                         82, 'post-export', 3.000003, 4.000004, 2,
                         'per_minute', TIMESTAMP '2000-01-01 00:00:07',
                         TIMESTAMP '2000-01-01 00:00:08'
                     );
                     INSERT INTO public.updates(
                         id, start_date, end_date, version, car_id
                     ) VALUES (
                         11, TIMESTAMP '2000-01-01 00:00:00.654321', NULL,
                         'post-export', 1
                     );
                     INSERT INTO public.states(id, car_id, state, start_date, end_date)
                     VALUES (
                         21, 1, 'offline',
                         TIMESTAMP '2000-01-01 00:00:00.765432', NULL
                     );
                     INSERT INTO public.drives(
                         id, car_id, start_address_id, start_geofence_id,
                         start_date
                     ) VALUES (
                         31, 1, 72, 82,
                         TIMESTAMP '2000-01-01 00:00:00.876543'
                     );
                     INSERT INTO public.positions(
                         id, car_id, drive_id, date, latitude, longitude
                     ) VALUES (
                         41, 1, 31, TIMESTAMP '2000-01-01 00:00:00.987654',
                         2.345678, 3.456789
                     );
                     INSERT INTO public.charging_processes(
                         id, car_id, position_id, address_id, geofence_id,
                         start_date, cost
                     ) VALUES (
                         51, 1, 41, 72, 82,
                         TIMESTAMP '2000-01-01 00:00:00.998765',
                         -999999999999.99
                     );
                     INSERT INTO public.charges(
                         id, charging_process_id, date, charge_energy_added,
                         charger_power, ideal_battery_range_km
                     ) VALUES (
                         61, 51, TIMESTAMP '2000-01-01 00:00:00.999876',
                         1.23, 11, 123.45
                     );",
                )
                .await?;
            Ok(())
        },
    )
    .await
    .expect("sealed physical V3 capture");

    assert_eq!(
        stage.format().expect("stage format"),
        TeslaMateStageFormat::PhysicalV3
    );
    let stats = stage.stats().expect("stage stats");
    assert_eq!(stats.state, TeslaMateStageState::Sealed);
    assert_eq!(stats.row_count, 11);
    let settings = stage
        .get::<TeslaMateSettingsPhysicalV2_2>(TeslaMateStageTable::GlobalSettings, 100)
        .expect("settings lookup")
        .expect("settings row");
    assert_eq!(settings.inserted_at_pg_us, 1_000_000);
    assert_eq!(settings.updated_at_pg_us, 2_000_000);
    let car = stage
        .get::<TeslaMateCarPhysicalV2_2>(TeslaMateStageTable::Cars, 1)
        .expect("car lookup")
        .expect("car row");
    assert_eq!(
        car.efficiency.expect("efficiency").to_bits(),
        (-0.0_f64).to_bits()
    );
    assert_eq!(car.inserted_at_pg_us, 3_000_000);
    let car_settings = stage
        .get::<TeslaMateCarSettingsPhysicalV2_2>(TeslaMateStageTable::CarSettings, 200)
        .expect("car settings lookup")
        .expect("car settings row");
    assert_eq!(car_settings.id, car.settings_id);
    let updates = stage
        .page::<TeslaMateUpdatePhysicalV2_2>(TeslaMateStageTable::Updates, 0, 10)
        .expect("updates page");
    assert_eq!(updates.rows.len(), 1);
    assert_eq!(updates.rows[0].source_id, 10);
    assert_eq!(updates.rows[0].value.version.as_deref(), Some("pre-export"));
    assert_eq!(updates.rows[0].value.start_date_pg_us, 123_456);
    let states = stage
        .page::<TeslaMateStatePhysicalV2_2>(TeslaMateStageTable::States, 0, 10)
        .expect("states page");
    assert_eq!(states.rows.len(), 1);
    assert_eq!(states.rows[0].source_id, 20);
    assert_eq!(
        states.rows[0].value.state,
        ProjectionStateStatusV2_2::Online
    );
    assert_eq!(states.rows[0].value.start_date_pg_us, 234_567);
    assert_eq!(states.rows[0].value.end_date_pg_us, Some(i64::MAX));
    let drives = stage
        .page::<TeslaMateDrivePhysicalV2_2>(TeslaMateStageTable::Drives, 0, 10)
        .expect("drives page");
    assert_eq!(drives.rows.len(), 1);
    assert_eq!(drives.rows[0].source_id, 30);
    assert_eq!(drives.rows[0].value.start_date_pg_us, 345_678);
    assert_eq!(drives.rows[0].value.end_date_pg_us, Some(i64::MAX));
    assert_eq!(
        drives.rows[0].value.outside_temp_avg_e1,
        Some(ProjectionFixedNumericV2_2::NaN)
    );
    assert_eq!(
        drives.rows[0].value.start_km,
        Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits()))
    );
    let positions = stage
        .page::<TeslaMatePositionPhysicalV2_2>(TeslaMateStageTable::Positions, 0, 10)
        .expect("positions page");
    assert_eq!(positions.rows.len(), 1);
    assert_eq!(positions.rows[0].source_id, 40);
    assert_eq!(positions.rows[0].value.drive_id, Some(30));
    assert_eq!(positions.rows[0].value.date_pg_us, 456_789);
    assert_eq!(
        positions.rows[0].value.latitude_e6,
        ProjectionFixedNumericV2_2::NaN
    );
    assert_eq!(
        positions.rows[0].value.longitude_e6,
        ProjectionFixedNumericV2_2::Finite(1_234_567)
    );
    assert_eq!(
        positions.rows[0].value.odometer,
        Some(ProjectionFloat64BitsV2_2((-0.0_f64).to_bits()))
    );
    let charging_processes = stage
        .page::<TeslaMateChargingProcessPhysicalV2_2>(TeslaMateStageTable::ChargingProcesses, 0, 10)
        .expect("charging processes page");
    assert_eq!(charging_processes.rows.len(), 1);
    assert_eq!(charging_processes.rows[0].source_id, 50);
    assert_eq!(charging_processes.rows[0].value.position_id, 40);
    assert_eq!(charging_processes.rows[0].value.start_date_pg_us, 567_890);
    assert_eq!(
        charging_processes.rows[0].value.end_date_pg_us,
        Some(i64::MAX)
    );
    assert_eq!(
        charging_processes.rows[0].value.charge_energy_added_e2,
        Some(ProjectionFixedNumericV2_2::NaN)
    );
    assert_eq!(
        charging_processes.rows[0].value.outside_temp_avg_e1,
        Some(ProjectionFixedNumericV2_2::Finite(-1))
    );
    assert_eq!(
        charging_processes.rows[0].value.cost_e2,
        Some(ProjectionFixedNumericV2_2::Finite(99_999_999_999_999))
    );
    let charges = stage
        .page::<TeslaMateChargePhysicalV2_2>(TeslaMateStageTable::Charges, 0, 10)
        .expect("charges page");
    assert_eq!(charges.rows.len(), 1);
    assert_eq!(charges.rows[0].source_id, 60);
    assert_eq!(charges.rows[0].value.charging_process_id, 50);
    assert_eq!(charges.rows[0].value.date_pg_us, i64::MIN);
    assert_eq!(charges.rows[0].value.battery_heater, None);
    assert_eq!(charges.rows[0].value.battery_heater_on, Some(false));
    assert_eq!(charges.rows[0].value.battery_heater_no_power, Some(true));
    assert_eq!(
        charges.rows[0].value.charge_energy_added_e2,
        ProjectionFixedNumericV2_2::NaN
    );
    assert_eq!(charges.rows[0].value.charger_power, i16::MIN);
    assert_eq!(charges.rows[0].value.conn_charge_cable.as_deref(), Some(""));
    assert_eq!(charges.rows[0].value.fast_charger_present, None);
    assert_eq!(
        charges.rows[0].value.ideal_battery_range_km_e2,
        ProjectionFixedNumericV2_2::Finite(-999_999)
    );
    assert_eq!(
        charges.rows[0].value.rated_battery_range_km_e2,
        Some(ProjectionFixedNumericV2_2::NaN)
    );
    assert_eq!(
        charges.rows[0].value.outside_temp_e1,
        Some(ProjectionFixedNumericV2_2::Finite(-1))
    );
    let addresses = stage
        .page::<TeslaMateAddressPhysicalV2_2>(TeslaMateStageTable::Addresses, 0, 10)
        .expect("addresses page");
    assert_eq!(addresses.rows.len(), 1);
    assert_eq!(addresses.rows[0].source_id, 70);
    assert_eq!(addresses.rows[0].value.display_name.as_deref(), Some(""));
    assert_eq!(
        addresses.rows[0].value.latitude_e6,
        Some(ProjectionFixedNumericV2_2::NaN)
    );
    assert_eq!(
        addresses.rows[0].value.longitude_e6,
        Some(ProjectionFixedNumericV2_2::Finite(-1))
    );
    assert_eq!(addresses.rows[0].value.inserted_at_pg_us, i64::MIN);
    assert_eq!(addresses.rows[0].value.updated_at_pg_us, i64::MAX);
    assert_eq!(addresses.rows[0].value.osm_id, Some(i64::MIN));
    assert_eq!(addresses.rows[0].value.osm_type.as_deref(), Some(""));
    let geofences = stage
        .page::<TeslaMateGeofencePhysicalV2_2>(TeslaMateStageTable::Geofences, 0, 10)
        .expect("geofences page");
    assert_eq!(geofences.rows.len(), 1);
    assert_eq!(geofences.rows[0].source_id, 80);
    assert_eq!(
        geofences.rows[0].value.latitude_e6,
        ProjectionFixedNumericV2_2::NaN
    );
    assert_eq!(
        geofences.rows[0].value.longitude_e6,
        ProjectionFixedNumericV2_2::Finite(-1)
    );
    assert_eq!(geofences.rows[0].value.radius, i16::MIN);
    assert_eq!(
        geofences.rows[0].value.billing_type,
        GeofenceBillingType::PerKwh
    );
    assert_eq!(
        geofences.rows[0].value.cost_per_unit_e4,
        Some(ProjectionFixedNumericV2_2::Finite(999_999_999))
    );
    assert_eq!(
        geofences.rows[0].value.session_fee_e2,
        Some(ProjectionFixedNumericV2_2::Finite(-99_999_999_999_999))
    );
    assert_eq!(geofences.rows[0].value.inserted_at_pg_us, i64::MIN);
    assert_eq!(geofences.rows[0].value.updated_at_pg_us, i64::MAX);
    assert_eq!(
        admin
            .query_one("SELECT COUNT(*)::bigint AS count FROM public.updates", &[])
            .await
            .expect("source update count")
            .try_get::<_, i64>("count")
            .expect("source update count value"),
        2
    );
    for table in ["drives", "positions", "charging_processes", "charges"] {
        assert_eq!(
            admin
                .query_one(
                    &format!("SELECT COUNT(*)::bigint AS count FROM public.{table}"),
                    &[]
                )
                .await
                .expect("source relation count")
                .try_get::<_, i64>("count")
                .expect("source relation count value"),
            2
        );
    }
    for table in ["addresses", "geofences"] {
        assert_eq!(
            admin
                .query_one(
                    &format!("SELECT COUNT(*)::bigint AS count FROM public.{table}"),
                    &[]
                )
                .await
                .expect("source location relation count")
                .try_get::<_, i64>("count")
                .expect("source location relation count value"),
            3
        );
    }
    assert_eq!(
        admin
            .query_one("SELECT COUNT(*)::bigint AS count FROM public.states", &[])
            .await
            .expect("source state count")
            .try_get::<_, i64>("count")
            .expect("source state count value"),
        2
    );
    let stage_path = stage.path().to_path_buf();
    stage.discard().expect("discard successful test stage");
    assert!(!stage_path.exists());

    let cap_dir = temporary.path().join("cap-failure");
    let cap_failure = capture_physical_v3_to_stage(
        &source,
        &password,
        1,
        TeslaMateReadLimits {
            maximum_rows: 10,
            ..limits
        },
        &cap_dir,
    )
    .await;
    assert!(matches!(
        cap_failure,
        Err(TeslaMateReaderError::MaximumRowsExceeded { maximum: 10 })
    ));
    let capped_stages = std::fs::read_dir(cap_dir.join(".staging"))
        .expect("cap failure staging directory")
        .map(|entry| entry.expect("cap failure staging entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "sqlite")
        })
        .collect::<Vec<_>>();
    assert!(
        capped_stages.is_empty(),
        "retained stages: {capped_stages:?}"
    );

    let failure_dir = temporary.path().join("failure");
    let failure = capture_physical_v3_to_stage_with_post_export(
        &source,
        &password,
        1,
        limits,
        &failure_dir,
        || async { Err(TeslaMateReaderError::InvalidExportedSnapshot) },
    )
    .await;
    assert!(matches!(
        failure,
        Err(TeslaMateReaderError::InvalidExportedSnapshot)
    ));
    let staged_files = std::fs::read_dir(failure_dir.join(".staging"))
        .expect("failure staging directory")
        .map(|entry| entry.expect("failure staging entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "sqlite")
        })
        .collect::<Vec<_>>();
    assert!(staged_files.is_empty(), "retained stages: {staged_files:?}");

    drop(admin);
    connection_task.await.expect("fixture admin join");
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
