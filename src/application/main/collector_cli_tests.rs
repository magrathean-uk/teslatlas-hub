// SPDX-License-Identifier: AGPL-3.0-only

//! The collector actions of the control CLI (C3 R65, R66, section 8 item 5):
//! `collector retry`, `collector resume` and `collector gaps acknowledge`.

use std::{fs, os::unix::fs::PermissionsExt};

use clap::Parser;
use teslatlas_hub::db::HubStore;
use uuid::Uuid;

use crate::{Cli, CollectorCommand, CollectorGapsCommand, Command, run};

#[test]
fn collector_actions_parse_with_their_arguments() {
    let retry = Cli::try_parse_from(["teslatlas-hub", "collector", "retry"]).expect("retry CLI");
    assert!(matches!(
        retry.command,
        Command::Collector {
            command: CollectorCommand::Retry
        }
    ));
    let resume = Cli::try_parse_from([
        "teslatlas-hub",
        "collector",
        "resume",
        "00000000-0000-0000-0000-000000000001",
    ])
    .expect("resume CLI");
    assert!(matches!(
        resume.command,
        Command::Collector {
            command: CollectorCommand::Resume { .. }
        }
    ));
    let acknowledge = Cli::try_parse_from(["teslatlas-hub", "collector", "gaps", "acknowledge"])
        .expect("acknowledge CLI");
    assert!(matches!(
        acknowledge.command,
        Command::Collector {
            command: CollectorCommand::Gaps {
                command: CollectorGapsCommand::Acknowledge
            }
        }
    ));
    // A car is named by its Hub UUID, and the action needs one.
    assert!(Cli::try_parse_from(["teslatlas-hub", "collector", "resume"]).is_err());
    assert!(Cli::try_parse_from(["teslatlas-hub", "collector", "resume", "not-a-uuid"]).is_err());
    assert!(Cli::try_parse_from(["teslatlas-hub", "collector", "gaps"]).is_err());
}

#[tokio::test]
async fn collector_actions_record_a_request_and_acknowledge_alert_gaps() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
        .expect("protect temporary directory");
    let store = HubStore::initialize(temporary.path()).expect("store");
    let config_path = temporary.path().join("config.toml");
    fs::write(
        &config_path,
        format!(
            "data_dir = {:?}\nbind = '127.0.0.1:18443'\n",
            temporary.path()
        ),
    )
    .expect("write config");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).expect("private config");
    let invoke = |command: CollectorCommand| {
        let config_path = config_path.clone();
        async move {
            run(Cli {
                config: Some(config_path),
                command: Command::Collector { command },
            })
            .await
        }
    };

    // `collector retry` records one `retry_collection` row for the collector to apply.
    invoke(CollectorCommand::Retry).await.expect("retry");
    let rows: Vec<(String, Option<String>, Option<i64>)> = store
        .open()
        .expect("connection")
        .prepare("SELECT action, vehicle_id, applied_at_ms FROM collector_owner_actions")
        .expect("statement")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(rows, vec![("retry_collection".to_owned(), None, None)]);

    // `collector resume` names a configured car; any other UUID is refused before a
    // row is written.
    let error = invoke(CollectorCommand::Resume {
        vehicle: Uuid::new_v4(),
    })
    .await
    .expect_err("an unknown car");
    assert!(
        error
            .to_string()
            .contains("does not identify a configured car")
    );
    let count: i64 = store
        .open()
        .expect("connection")
        .query_row("SELECT COUNT(*) FROM collector_owner_actions", [], |row| {
            row.get(0)
        })
        .expect("count");
    assert_eq!(count, 1);

    // `collector gaps acknowledge` marks every unnotified alert gap and no other.
    let connection = store.open().expect("connection");
    for (kind, alert) in [("hub_down", 1), ("host_suspended", 0)] {
        connection
            .execute(
                "INSERT INTO collection_gaps(kind, started_at_ms, ended_at_ms, alert, created_at_ms)
                 VALUES (?1, 1000, 2000, ?2, 2000)",
                rusqlite::params![kind, alert],
            )
            .expect("gap");
    }
    invoke(CollectorCommand::Gaps {
        command: CollectorGapsCommand::Acknowledge,
    })
    .await
    .expect("acknowledge");
    let notified: Vec<(String, Option<i64>)> = connection
        .prepare("SELECT kind, notified_at_ms FROM collection_gaps ORDER BY gap_id")
        .expect("statement")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows");
    assert_eq!(notified.len(), 2);
    assert!(notified[0].1.is_some(), "the alert gap is acknowledged");
    assert_eq!(
        notified[1].1, None,
        "a gap that raised no alert is left alone"
    );
}
