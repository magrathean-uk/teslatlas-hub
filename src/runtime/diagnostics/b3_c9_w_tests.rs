// SPDX-License-Identifier: AGPL-3.0-only

use super::{doctor_config, *};
use crate::db::{LifecycleCommit, SourceDescriptor, VehicleDescriptor};
use crate::hub_pack::{ProjectionDrive, ProjectionPosition};
use serde_json::json;

fn assert_store_status(store: &HubStore, report: &HubDoctorReport) {
    let expected = store
        .terrain_backlog(diagnostic_epoch_ms(), crate::db::K_STATUS_COUNT_CAP)
        .unwrap();
    let progress = store.terrain_tidy_progress().unwrap();
    let d = &report.collector.terrain;
    assert_eq!(
        (d.rows_unexamined, d.retries_due, d.retries_waiting),
        (
            Some(expected.rows_unexamined),
            Some(expected.retries_due),
            Some(expected.retries_waiting)
        )
    );
    assert_eq!(d.error_code, None);
    assert_eq!(
        d.tidy_progress,
        Some(TerrainTidyDiagnostics {
            run: progress.run,
            examined: progress.examined,
            cleared: progress.cleared,
            requeued: progress.requeued,
            removed: progress.removed,
            kept: progress.kept,
            drives_changed: progress.drives_changed,
            armed_at_ms: progress.armed_at_ms,
            completed_at_ms: progress.completed_at_ms,
        })
    );
    let value = serde_json::to_value(report).unwrap();
    assert_eq!(
        value["collector"]["terrain"],
        serde_json::to_value(d).unwrap()
    );
}

#[tokio::test]
async fn c9_st77_store_status() {
    let dir = crate::private_tempdir().unwrap();
    let store = HubStore::initialize(dir.path()).unwrap();
    let config = doctor_config(dir.path(), "interval_seconds = 0", "");
    let t0 = 1_760_000_000_000_i64;
    let source = store
        .register_source(&SourceDescriptor::new("owner_api_compat", "c9-doctor"), t0)
        .unwrap();
    let v = store
        .register_vehicle(&VehicleDescriptor::new(source.source_id, "synthetic"), t0)
        .unwrap()
        .vehicle_id;
    let drive: ProjectionDrive = serde_json::from_value(
        json!({"id":1,"car_id":1,"start_date_ms":t0,"end_date_ms":t0+10000}),
    )
    .unwrap();
    let positions:Vec<ProjectionPosition>=(1..=4).map(|id|serde_json::from_value(json!({"id":id,"drive_id":1,"car_id":1,"date_ms":t0+id*1000,"latitude":10.5,"longitude":20.5})).unwrap()).collect();
    let state = crate::lifecycle::OpenSessionState::new().encode().unwrap();
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: v,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 0,
            quarantined: false,
            updated_at_ms: t0 + 10000,
            delta: &crate::lifecycle::LifecycleDelta {
                drives: vec![drive],
                positions,
                ..Default::default()
            },
        })
        .unwrap();
    store.open().unwrap().execute("INSERT INTO terrain_elevation_provenance(vehicle_id,position_id,drive_id,latitude,longitude,status,error_code,attempts,attempted_at_ms,retry_after_ms) VALUES(?1,4,1,10.5,20.5,'failed','timeout',1,?2,?2)",rusqlite::params![v.to_string(),t0]).unwrap();
    let plain = inspect_hub(&store, &config).unwrap();
    assert!(plain.is_ok());
    let connection = store.open().unwrap();
    let version = || {
        connection
            .query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let before = version();
    let pending = inspect_hub(&store, &config).unwrap();
    assert_store_status(&store, &pending);
    assert_eq!(version(), before);
    assert_eq!(
        (
            pending.collector.terrain.rows_unexamined,
            pending.collector.terrain.retries_due
        ),
        (Some(4), Some(1))
    );
    assert_eq!(
        pending
            .collector
            .terrain
            .tidy_progress
            .as_ref()
            .unwrap()
            .run,
        1
    );
    assert!(
        pending
            .collector
            .terrain
            .tidy_progress
            .as_ref()
            .unwrap()
            .completed_at_ms
            .is_none()
    );
    // C9 ST77: three fresh qualifying rows plus one due retry are filled through P.
    crate::collector::fill_diagnostic_fixture(&store).await;
    let before = version();
    let complete = inspect_hub(&store, &config).unwrap();
    assert_store_status(&store, &complete);
    assert_eq!(version(), before);
    assert_eq!(
        (
            complete.collector.terrain.rows_unexamined,
            complete.collector.terrain.retries_due
        ),
        (Some(0), Some(0))
    );
    assert!(
        complete
            .collector
            .terrain
            .tidy_progress
            .as_ref()
            .unwrap()
            .completed_at_ms
            .is_some()
    );
    for report in [&pending, &complete] {
        assert!(report.is_ok());
        assert_eq!(report.status, plain.status);
        assert_eq!(report.checks, plain.checks);
        assert_eq!(report.collector.readiness, plain.collector.readiness);
        assert_eq!(
            report.collector.readiness_detail,
            plain.collector.readiness_detail
        );
        assert_eq!(
            report.collector.collection_attention,
            plain.collector.collection_attention
        );
        assert_eq!(
            report.collector.terrain_enabled,
            plain.collector.terrain_enabled
        );
    }
    // C9 R63: a missing optional progress row cannot turn a healthy doctor into failure.
    store
        .open()
        .unwrap()
        .execute_batch("DELETE FROM terrain_tidy_state;")
        .unwrap();
    let fault = inspect_hub(&store, &config).unwrap();
    assert!(fault.is_ok());
    assert_eq!(fault.collector.terrain.error_code, Some("store"));
    assert_eq!(fault.collector.terrain.tidy_progress, None);
    assert_eq!(fault.status, plain.status);
    assert_eq!(fault.checks, plain.checks);
    assert_eq!(fault.collector.readiness, plain.collector.readiness);
}
