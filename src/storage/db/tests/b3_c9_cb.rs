// SPDX-License-Identifier: AGPL-3.0-only

use super::*;
use serde_json::json;

const T0: i64 = 1_760_000_000_000;
const NOW: i64 = T0 + 60_000_000;
const FILL_AT: i64 = NOW - 1_000;

fn fixture() -> (tempfile::TempDir, HubStore, Uuid) {
    let (dir, store) = b3_c8_sa::c8_store();
    let vehicle = b3_c8_sa::c8_vehicle(&store, "terrain-rewrite");
    (dir, store, vehicle)
}

fn position(id: i64, drive: Option<i64>, elevation: Option<i64>) -> ProjectionPosition {
    let mut p = position_row(id, drive, T0 + (id - 1) * 60_000);
    p.latitude = 10.5;
    p.longitude = 20.0 + (id - 1) as f64 * 6.0 / 1200.0;
    p.elevation = elevation;
    p
}

fn drive(id: i64, ascent: Option<i64>, descent: Option<i64>) -> ProjectionDrive {
    let mut d = drive_row(id, NOW);
    d.start_date_ms = T0 - 1;
    d.ascent = ascent;
    d.descent = descent;
    d
}

fn commit(store: &HubStore, vehicle: Uuid, delta: &crate::lifecycle::LifecycleDelta) {
    let state = crate::lifecycle::OpenSessionState::new().encode().unwrap();
    store
        .commit_lifecycle_delta(&LifecycleCommit {
            vehicle_id: vehicle,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 0,
            quarantined: false,
            updated_at_ms: NOW,
            delta,
        })
        .unwrap();
}

fn fill(store: &HubStore, vehicle: Uuid, values: &[(i64, i16)]) {
    fill_at(store, vehicle, values, NOW);
}

fn fill_at(store: &HubStore, vehicle: Uuid, values: &[(i64, i16)], committed_at_ms: i64) {
    let connection = store.open().unwrap();
    let outcomes: Vec<_> = values.iter().map(|&(id, elevation_m)| {
        let position = {
            let text: String = connection.query_row(
                "SELECT position_json FROM materialised_positions WHERE vehicle_id = ?1 AND position_id = ?2",
                params![vehicle.to_string(), id], |r| r.get(0),
            ).unwrap();
            serde_json::from_str::<ProjectionPosition>(&text).unwrap()
        };
        let forward: bool = connection.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM terrain_elevation_provenance WHERE vehicle_id = ?1 AND position_id = ?2)",
            params![vehicle.to_string(), id], |r| r.get(0),
        ).unwrap();
        TerrainOutcome {
            candidate: TerrainCandidate { vehicle_id: vehicle, position }, forward,
            kind: TerrainOutcomeKind::Filled {
                elevation_m, tile: TileProvenance { tile_name: "N10E020".into(),
                    tile_hash: "synthetic-digest".into(), dataset_source: "aws".into(),
                    dataset_version: "synthetic-v1".into() },
            },
        }
    }).collect();
    assert_eq!(
        store
            .apply_terrain_batch(&TerrainBatch {
                vehicle_id: vehicle,
                outcomes: &outcomes,
                retire: &[],
                cursor_to: None,
                committed_at_ms,
            })
            .unwrap()
            .filled as usize,
        values.len()
    );
}

fn seed(store: &HubStore, vehicle: Uuid, rows: &[(i64, i64, i16)]) {
    let mut ids = Vec::new();
    for &(_, id, _) in rows {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    commit(
        store,
        vehicle,
        &crate::lifecycle::LifecycleDelta {
            drives: ids.into_iter().map(|id| drive(id, None, None)).collect(),
            positions: rows
                .iter()
                .map(|&(id, d, _)| position(id, Some(d), None))
                .collect(),
            ..Default::default()
        },
    );
    fill_at(
        store,
        vehicle,
        &rows
            .iter()
            .map(|&(id, _, value)| (id, value))
            .collect::<Vec<_>>(),
        FILL_AT,
    );
    clear_mutations(store);
}

fn clear_mutations(store: &HubStore) {
    store
        .open()
        .unwrap()
        .execute("DELETE FROM sync_mutations", [])
        .unwrap();
}

fn pair(connection: &Connection, vehicle: Uuid, id: i64) -> (Option<i64>, Option<i64>) {
    connection.query_row("SELECT ascent, descent FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = ?2",
        params![vehicle.to_string(), id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
}

fn stored_position(connection: &Connection, vehicle: Uuid, id: i64) -> Value {
    let json: String = connection.query_row("SELECT position_json FROM materialised_positions WHERE vehicle_id = ?1 AND position_id = ?2",
        params![vehicle.to_string(), id], |r| r.get(0)).unwrap();
    serde_json::from_str(&json).unwrap()
}

fn provenance(connection: &Connection, vehicle: Uuid, id: i64) -> Option<Value> {
    let json: Option<String> = connection.query_row(
        "SELECT json_object('drive', drive_id, 'latitude', latitude, 'longitude', longitude,
            'elevation', elevation_m, 'tile', tile_name, 'hash', tile_hash,
            'source', dataset_source, 'version', dataset_version, 'status', status,
            'error', error_code, 'attempts', attempts, 'at', attempted_at_ms, 'retry', retry_after_ms)
         FROM terrain_elevation_provenance WHERE vehicle_id = ?1 AND position_id = ?2",
        params![vehicle.to_string(), id], |r| r.get(0),
    ).optional().unwrap();
    json.map(|j| serde_json::from_str(&j).unwrap())
}

fn mutations(connection: &Connection, vehicle: Uuid) -> Vec<(String, i64, Value)> {
    connection.prepare("SELECT entity, entity_id, payload_json FROM sync_mutations WHERE vehicle_id = ?1 ORDER BY revision").unwrap()
        .query_map([vehicle.to_string()], |r| { let j: String = r.get(2)?;
            Ok((r.get(0)?, r.get(1)?, serde_json::from_str(&j).unwrap()))
        }).unwrap().map(Result::unwrap).collect()
}

fn order(connection: &Connection, vehicle: Uuid) -> Vec<(String, i64)> {
    mutations(connection, vehicle)
        .into_iter()
        .map(|(e, id, _)| (e, id))
        .collect()
}

fn observe_calls() {
    C9_RECOMPUTE_CALLS.with(|c| *c.borrow_mut() = Some(Vec::new()));
}
fn calls() -> Vec<(i64, i64)> {
    C9_RECOMPUTE_CALLS.with(|c| c.borrow().clone().unwrap())
}
fn stop_observing() {
    C9_RECOMPUTE_CALLS.with(|c| *c.borrow_mut() = None);
}

fn record_position(tx: &Transaction<'_>, vehicle: Uuid, id: i64) {
    let json = stored_position(tx, vehicle, id).to_string();
    record_sync_mutation_in_transaction(tx, vehicle, "position", id, 1, "upsert", &json).unwrap();
}

fn requeued(connection: &Connection, vehicle: Uuid, id: i64, d: i64, lat: f64, lon: f64) {
    assert_eq!(
        provenance(connection, vehicle, id),
        Some(json!({
            "drive": d, "latitude": lat, "longitude": lon, "elevation": null,
            "tile": null, "hash": null, "source": null, "version": null,
            "status": "failed", "error": "position_rewritten", "attempts": 1,
            "at": FILL_AT, "retry": NOW,
        }))
    );
}

#[test]
fn c9_st56_rewrite() {
    // C9 ST56: the corrected coordinate, null, car, missing member and deletion cases.
    for mode in [
        "coordinate",
        "latitude",
        "null",
        "car",
        "missing-member",
        "delete",
        "same",
        "detach",
        "missing-drive",
    ] {
        let (_dir, store, vehicle) = fixture();
        seed(
            &store,
            vehicle,
            &[(1, 1, 100), (2, 1, 103), (3, 1, 101), (4, 1, 106)],
        );
        let mut connection = store.open().unwrap();
        let untouched = stored_position(&connection, vehicle, 1);
        let untouched_provenance = provenance(&connection, vehicle, 1);
        // C9 R38: unknown JSON and separate columns survive terrain correction.
        connection.execute("UPDATE materialised_positions SET position_json = json_set(position_json, '$.future', 77), speed = 123 WHERE position_id = 2", []).unwrap();
        let before = provenance(&connection, vehicle, 2);
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let sql = match mode {
            "coordinate" => {
                "UPDATE materialised_positions SET position_json = json_set(position_json, '$.longitude', 20.0025) WHERE position_id = 2"
            }
            "latitude" => {
                "UPDATE materialised_positions SET position_json = json_set(position_json, '$.latitude', 10.6) WHERE position_id = 2"
            }
            "null" => {
                "UPDATE materialised_positions SET position_json = json_set(position_json, '$.elevation', NULL) WHERE position_id = 2"
            }
            "car" => {
                "UPDATE materialised_positions SET position_json = json_set(position_json, '$.elevation', 90) WHERE position_id = 2"
            }
            "missing-member" => {
                "UPDATE materialised_positions SET position_json = json_remove(position_json, '$.elevation') WHERE position_id = 2"
            }
            "delete" => "DELETE FROM materialised_positions WHERE position_id = 2",
            "detach" => {
                "UPDATE materialised_positions SET drive_id = NULL, position_json = json_set(position_json, '$.drive_id', NULL) WHERE position_id = 2"
            }
            "missing-drive" => {
                "UPDATE materialised_positions SET drive_id = 99, position_json = json_set(position_json, '$.drive_id', 99) WHERE position_id = 2"
            }
            _ => "UPDATE materialised_positions SET speed = 123 WHERE position_id = 2",
        };
        tx.execute(sql, []).unwrap();
        observe_calls();
        let changes_before = tx.total_changes();
        let applied = terrain_after_position_write_in_transaction(&tx, vehicle, 2, NOW).unwrap();
        let expected = match mode {
            "same" => TerrainRewrite::Unchanged,
            "coordinate" | "latitude" | "null" | "missing-member" => TerrainRewrite::Requeued,
            _ => TerrainRewrite::Removed,
        };
        assert_eq!(applied.outcome, expected, "{mode}");
        assert_eq!(
            applied.recompute_drive_ids,
            if mode == "same" { vec![] } else { vec![1] },
            "{mode}"
        );
        assert!(calls().is_empty(), "C9 ST56: no inline recompute");
        assert_eq!(pair(&tx, vehicle, 1), (Some(8), Some(2)));
        if mode == "same" {
            assert_eq!(tx.total_changes(), changes_before);
            assert_eq!(provenance(&tx, vehicle, 2), before);
        }
        if expected == TerrainRewrite::Requeued {
            requeued(
                &tx,
                vehicle,
                2,
                1,
                if mode == "latitude" { 10.6 } else { 10.5 },
                if mode == "coordinate" {
                    20.0025
                } else {
                    20.005
                },
            );
        } else if expected == TerrainRewrite::Removed {
            assert!(provenance(&tx, vehicle, 2).is_none());
        }
        if mode != "delete" {
            let value = stored_position(&tx, vehicle, 2);
            assert_eq!(value["future"], 77);
            assert_eq!(
                value["elevation"],
                match mode {
                    "car" => json!(90),
                    "same" => json!(103),
                    _ => Value::Null,
                }
            );
            let speed: i64 = tx
                .query_row(
                    "SELECT speed FROM materialised_positions WHERE position_id = 2",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(speed, 123);
            record_position(&tx, vehicle, 2);
        } else {
            record_sync_mutation_in_transaction(&tx, vehicle, "position", 2, 1, "tombstone", "{}")
                .unwrap();
        }
        finalise_position_drives_in_transaction(
            &tx,
            vehicle,
            &[],
            &[],
            &applied.recompute_drive_ids,
        )
        .unwrap();
        let expected_pair = match mode {
            "same" => (Some(8), Some(2)),
            "car" => (Some(16), Some(10)),
            _ => (Some(6), Some(0)),
        };
        assert_eq!(pair(&tx, vehicle, 1), expected_pair, "{mode}");
        assert_eq!(calls(), if mode == "same" { vec![] } else { vec![(1, 1)] });
        for (entity, _, payload) in mutations(&tx, vehicle) {
            if entity == "drive" {
                assert_eq!(
                    (payload["ascent"].as_i64(), payload["descent"].as_i64()),
                    expected_pair
                );
            }
        }
        tx.commit().unwrap();
        stop_observing();
        assert_eq!(stored_position(&connection, vehicle, 1), untouched);
        assert_eq!(provenance(&connection, vehicle, 1), untouched_provenance);
        assert_eq!(
            order(&connection, vehicle),
            if mode == "same" {
                vec![("position".into(), 2)]
            } else {
                vec![("position".into(), 2), ("drive".into(), 1)]
            }
        );
        if mode == "coordinate" {
            let retries = store.terrain_due_retries(NOW, 250).unwrap();
            assert_eq!(retries.len(), 1);
            assert_eq!(
                retries[0].candidate.as_ref().unwrap().position.longitude,
                20.0025
            );
            fill(&store, vehicle, &[(2, 150)]);
            assert_eq!(pair(&connection, vehicle, 1), (Some(55), Some(49)));
            let p = provenance(&connection, vehicle, 2).unwrap();
            assert_eq!(p["status"], "success");
            assert_eq!(p["attempts"], 2);
        }
    }
    // C9 R54: drive-link precedence applies to success, failed and void alike.
    for status in ["success", "failed", "void"] {
        for (value, latitude, longitude, expected) in [
            (None, 10.5, 20.005, TerrainRewrite::Requeued),
            (Some(140), 10.5, 20.005, TerrainRewrite::Removed),
            (None, 91.0, 20.005, TerrainRewrite::Removed),
            (None, 10.5, 181.0, TerrainRewrite::Removed),
        ] {
            let (_dir, store, vehicle) = fixture();
            seed(
                &store,
                vehicle,
                &[(1, 1, 100), (2, 1, 110), (3, 2, 120), (4, 2, 130)],
            );
            let mut connection = store.open().unwrap();
            connection
                .execute(
                    "UPDATE terrain_elevation_provenance SET status = ?1 WHERE position_id = 2",
                    [status],
                )
                .unwrap();
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            tx.execute("UPDATE materialised_positions SET drive_id = 2, position_json = json_set(position_json, '$.drive_id', 2, '$.elevation', ?1, '$.latitude', ?2, '$.longitude', ?3) WHERE position_id = 2",params![value,latitude,longitude]).unwrap();
            let r = terrain_after_position_write_in_transaction(&tx, vehicle, 2, NOW).unwrap();
            assert_eq!(r.recompute_drive_ids, [1, 2]);
            assert_eq!(r.outcome, expected);
            if expected == TerrainRewrite::Requeued {
                requeued(&tx, vehicle, 2, 2, latitude, longitude);
            } else {
                assert!(provenance(&tx, vehicle, 2).is_none());
                assert_eq!(stored_position(&tx, vehicle, 2)["elevation"], json!(value));
            }
            record_position(&tx, vehicle, 2);
            finalise_position_drives_in_transaction(&tx, vehicle, &[], &[], &r.recompute_drive_ids)
                .unwrap();
            tx.commit().unwrap();
        }
        // C9 R54: unchanged failed/void rows and changed coordinates request no totals.
        if status != "success" {
            for mode in ["same", "coordinate", "car", "car-coordinate"] {
                let (_dir, store, vehicle) = fixture();
                seed(&store, vehicle, &[(1, 1, 100), (2, 1, 110)]);
                let mut connection = store.open().unwrap();
                connection
                    .execute(
                        "UPDATE terrain_elevation_provenance SET status = ?1 WHERE position_id = 2",
                        [status],
                    )
                    .unwrap();
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                tx.execute("UPDATE materialised_positions SET position_json = json_set(position_json, '$.elevation', ?1, '$.longitude', ?2) WHERE position_id = 2",params![if mode.starts_with("car") {Some(80)} else {None},if mode.contains("coordinate") {21.0} else {20.005}]).unwrap();
                let prior = provenance(&tx, vehicle, 2);
                let changes = tx.total_changes();
                let r = terrain_after_position_write_in_transaction(&tx, vehicle, 2, NOW).unwrap();
                assert!(r.recompute_drive_ids.is_empty());
                assert_eq!(
                    r.outcome,
                    match mode {
                        "same" => TerrainRewrite::Unchanged,
                        "coordinate" => TerrainRewrite::Requeued,
                        _ => TerrainRewrite::Removed,
                    }
                );
                if mode == "same" {
                    assert_eq!(tx.total_changes(), changes);
                    assert_eq!(provenance(&tx, vehicle, 2), prior);
                } else if mode == "coordinate" {
                    requeued(&tx, vehicle, 2, 1, 10.5, 21.0);
                } else {
                    assert!(provenance(&tx, vehicle, 2).is_none());
                    assert_eq!(stored_position(&tx, vehicle, 2)["elevation"], 80);
                }
                record_position(&tx, vehicle, 2);
                finalise_position_drives_in_transaction(
                    &tx,
                    vehicle,
                    &[],
                    &[],
                    &r.recompute_drive_ids,
                )
                .unwrap();
                tx.commit().unwrap();
            }
        }
    }
}

fn direct_write(store: &HubStore, vehicle: Uuid, p: &ProjectionPosition) -> TerrainRewriteApplied {
    let mut connection = store.open().unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let state = crate::lifecycle::OpenSessionState::new().encode().unwrap();
    let delta = crate::lifecycle::LifecycleDelta::default();
    let commit = LifecycleCommit {
        vehicle_id: vehicle,
        car_id: 1,
        open_session_json: &state,
        last_observation_id: 0,
        quarantined: false,
        updated_at_ms: NOW,
        delta: &delta,
    };
    observe_calls();
    let r = HubStore::write_materialised_position(&tx, &commit, p).unwrap();
    assert!(
        calls().is_empty(),
        "C9 ST76: position writes never recompute"
    );
    finalise_position_drives_in_transaction(&tx, vehicle, &[], &[], &r.recompute_drive_ids)
        .unwrap();
    tx.commit().unwrap();
    stop_observing();
    r
}

fn assert_unchanged_position_avoids_payload_read() {
    // C9 R64: a row without provenance needs no position payload read.
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    let (_dir, store, vehicle) = fixture();
    let mut connection = store.open().unwrap();
    let reads = Arc::new(AtomicUsize::new(0));
    let observed = reads.clone();
    connection
        .authorizer(Some(move |context: AuthContext<'_>| {
            if matches!(
                context.action,
                AuthAction::Read {
                    table_name: "materialised_positions",
                    column_name: "position_json"
                }
            ) {
                observed.fetch_add(1, Ordering::Relaxed);
            }
            Authorization::Allow
        }))
        .unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let state = crate::lifecycle::OpenSessionState::new().encode().unwrap();
    let delta = crate::lifecycle::LifecycleDelta::default();
    let commit = LifecycleCommit {
        vehicle_id: vehicle,
        car_id: 1,
        open_session_json: &state,
        last_observation_id: 0,
        quarantined: false,
        updated_at_ms: NOW,
        delta: &delta,
    };
    let position = position(1, None, Some(123));
    let result = HubStore::write_materialised_position(&tx, &commit, &position).unwrap();
    assert_eq!(result.outcome, TerrainRewrite::Unchanged);
    assert!(result.recompute_drive_ids.is_empty());
    assert_eq!(
        reads.load(Ordering::Relaxed),
        0,
        "C9 R64: unchanged payload is reused"
    );
    let log = mutations(&tx, vehicle);
    assert_eq!(log.len(), 1);
    assert_eq!((log[0].0.as_str(), log[0].1), ("position", 1));
    assert_eq!(log[0].2, serde_json::to_value(&position).unwrap());
    tx.commit().unwrap();
}

#[test]
fn c9_st76_lifecycle_rewrite() {
    assert_unchanged_position_avoids_payload_read();
    // C9 ST76 (a), (b), (c): null replay, no-provenance insertion and car replacement.
    let (_dir, store, vehicle) = fixture();
    seed(&store, vehicle, &[(1, 1, 100), (2, 1, 110)]);
    let connection = store.open().unwrap();
    let p2 = stored_position(&connection, vehicle, 2);
    let p2_prov = provenance(&connection, vehicle, 2);
    let r = direct_write(&store, vehicle, &position(1, Some(1), None));
    assert_eq!(
        r,
        TerrainRewriteApplied {
            outcome: TerrainRewrite::Requeued,
            recompute_drive_ids: vec![1]
        }
    );
    requeued(&connection, vehicle, 1, 1, 10.5, 20.0);
    assert_eq!(pair(&connection, vehicle, 1), (None, None));
    assert_eq!(stored_position(&connection, vehicle, 2), p2);
    assert_eq!(provenance(&connection, vehicle, 2), p2_prov);
    assert_eq!(
        order(&connection, vehicle),
        [("position".into(), 1), ("drive".into(), 1)]
    );
    assert!(mutations(&connection, vehicle)[0].2["elevation"].is_null());
    fill(&store, vehicle, &[(1, 100)]);
    assert_eq!(pair(&connection, vehicle, 1), (Some(10), Some(0)));
    assert_eq!(provenance(&connection, vehicle, 1).unwrap()["attempts"], 2);
    commit(
        &store,
        vehicle,
        &crate::lifecycle::LifecycleDelta {
            drives: vec![drive(2, None, None)],
            ..Default::default()
        },
    );
    clear_mutations(&store);
    let count = || {
        connection
            .query_row(
                "SELECT count(*) FROM terrain_elevation_provenance",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    };
    let before = count();
    let r = direct_write(&store, vehicle, &position(3, Some(2), None));
    assert_eq!(
        r,
        TerrainRewriteApplied {
            outcome: TerrainRewrite::Unchanged,
            recompute_drive_ids: vec![]
        }
    );
    assert_eq!(count(), before);
    let r = direct_write(&store, vehicle, &position(2, Some(1), Some(140)));
    assert_eq!(
        r,
        TerrainRewriteApplied {
            outcome: TerrainRewrite::Removed,
            recompute_drive_ids: vec![1]
        }
    );
    assert!(provenance(&connection, vehicle, 2).is_none());
    let page = store
        .drive_positions_window_page(vehicle, 1, 0, i64::MAX, None, 20)
        .unwrap();
    assert_eq!(page[1].position.elevation, Some(140));
    assert!(page[1].terrain.is_none());

    // C9 ST76 (d), (e): detached and relocated filled values, then car relocation.
    for mode in ["detach", "move", "car"] {
        let (_dir, store, vehicle) = fixture();
        seed(
            &store,
            vehicle,
            &[(1, 1, 100), (2, 1, 110), (3, 2, 120), (4, 2, 130)],
        );
        let mut connection = store.open().unwrap();
        let p1 = stored_position(&connection, vehicle, 1);
        let prov1 = provenance(&connection, vehicle, 1);
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let state = crate::lifecycle::OpenSessionState::new().encode().unwrap();
        let delta = crate::lifecycle::LifecycleDelta::default();
        let commit = LifecycleCommit {
            vehicle_id: vehicle,
            car_id: 1,
            open_session_json: &state,
            last_observation_id: 0,
            quarantined: false,
            updated_at_ms: NOW,
            delta: &delta,
        };
        let p = position(
            2,
            if mode == "detach" { None } else { Some(2) },
            Some(if mode == "car" { 140 } else { 110 }),
        );
        observe_calls();
        let r = HubStore::write_materialised_position(&tx, &commit, &p).unwrap();
        assert_eq!(
            r.outcome,
            if mode == "move" {
                TerrainRewrite::Requeued
            } else {
                TerrainRewrite::Removed
            }
        );
        assert_eq!(
            r.recompute_drive_ids,
            if mode == "detach" {
                vec![1]
            } else {
                vec![1, 2]
            }
        );
        assert_eq!(pair(&tx, vehicle, 1), (Some(10), Some(0)));
        assert_eq!(pair(&tx, vehicle, 2), (Some(10), Some(0)));
        assert!(calls().is_empty());
        assert_eq!(order(&tx, vehicle), [("position".into(), 2)]);
        let payload = &mutations(&tx, vehicle)[0].2;
        assert_eq!(
            payload["drive_id"],
            if mode == "detach" {
                Value::Null
            } else {
                json!(2)
            }
        );
        assert_eq!(
            payload["elevation"],
            if mode == "car" {
                json!(140)
            } else {
                Value::Null
            }
        );
        if mode == "move" {
            requeued(&tx, vehicle, 2, 2, 10.5, 20.005);
        } else {
            assert!(provenance(&tx, vehicle, 2).is_none());
        }
        finalise_position_drives_in_transaction(&tx, vehicle, &[], &[], &r.recompute_drive_ids)
            .unwrap();
        assert_eq!(
            calls(),
            if mode == "detach" {
                vec![(1, 1)]
            } else {
                vec![(1, 1), (2, 1)]
            }
        );
        assert_eq!(pair(&tx, vehicle, 1), (None, None));
        assert_eq!(
            pair(&tx, vehicle, 2),
            if mode == "car" {
                (Some(10), Some(20))
            } else {
                (Some(10), Some(0))
            }
        );
        assert_eq!(stored_position(&tx, vehicle, 1), p1);
        assert_eq!(provenance(&tx, vehicle, 1), prov1);
        let expected = if mode == "car" {
            vec![
                ("position".into(), 2),
                ("drive".into(), 1),
                ("drive".into(), 2),
            ]
        } else {
            vec![("position".into(), 2), ("drive".into(), 1)]
        };
        assert_eq!(order(&tx, vehicle), expected);
        for (entity, id, payload) in mutations(&tx, vehicle) {
            if entity == "drive" {
                assert_eq!(
                    (payload["ascent"].as_i64(), payload["descent"].as_i64()),
                    if id == 1 {
                        (None, None)
                    } else {
                        (Some(10), Some(20))
                    }
                );
            }
        }
        tx.commit().unwrap();
        stop_observing();
        if mode == "move" {
            let due = store.terrain_due_retries(NOW, 250).unwrap();
            assert_eq!(due.len(), 1);
            assert_eq!(
                due[0].candidate.as_ref().unwrap().position.drive_id,
                Some(2)
            );
            clear_mutations(&store);
            fill(&store, vehicle, &[(2, 110)]);
            assert_eq!(pair(&connection, vehicle, 2), (Some(20), Some(0)));
            assert_eq!(pair(&connection, vehicle, 1), (None, None));
            assert_eq!(stored_position(&connection, vehicle, 1), p1);
            assert_eq!(provenance(&connection, vehicle, 1), prov1);
            assert_eq!(
                order(&connection, vehicle),
                [("position".into(), 2), ("drive".into(), 2)]
            );
        }
    }
    // C9 ST76 (f): the final phase sees later close pages and precedes charge close.
    for (closing_descent, inherited) in [(20, false), (0, false), (0, true)] {
        let (_dir, store, vehicle) = fixture();
        seed(&store, vehicle, &[(1, 1, 100), (2, 1, 110)]);
        commit(
            &store,
            vehicle,
            &crate::lifecycle::LifecycleDelta {
                open_drive_positions: vec![
                    position(3, Some(2), Some(120)),
                    position(4, Some(2), Some(130)),
                ],
                ..Default::default()
            },
        );
        clear_mutations(&store);
        let mut connection = store.open().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM materialised_positions WHERE position_id IN (3,4)",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        let state = crate::lifecycle::OpenSessionState::new();
        assert!(state.open_drive.is_none());
        let encoded = state.encode().unwrap();
        let delta = crate::lifecycle::LifecycleDelta {
            drives: vec![drive(2, Some(10), Some(closing_descent))],
            positions: vec![position(2, Some(2), Some(140))],
            recompute_drive_ids: if inherited { vec![2] } else { vec![] },
            charges: vec![r88_charge_row(7, T0, NOW)],
            ..Default::default()
        };
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        observe_calls();
        let mut stats = ClosePageStats::default();
        let result = HubStore::commit_lifecycle_delta_paged(
            &tx,
            &LifecycleCommit {
                vehicle_id: vehicle,
                car_id: 1,
                open_session_json: &encoded,
                last_observation_id: 0,
                quarantined: false,
                updated_at_ms: NOW,
                delta: &delta,
            },
            1,
            &mut stats,
        )
        .unwrap();
        assert_eq!(result.0, 3);
        assert_eq!(
            calls(),
            if inherited {
                vec![(2, 3), (1, 3)]
            } else {
                vec![(1, 3), (2, 3)]
            }
        );
        assert_eq!(pair(&tx, vehicle, 1), (None, None));
        assert_eq!(pair(&tx, vehicle, 2), (Some(10), Some(20)));
        let mut expected = vec![
            ("drive".into(), 2),
            ("position".into(), 2),
            ("position".into(), 3),
            ("position".into(), 4),
        ];
        if inherited {
            expected.push(("drive".into(), 2));
        }
        expected.push(("drive".into(), 1));
        if !inherited && closing_descent == 0 {
            expected.push(("drive".into(), 2));
        }
        expected.push(("charge".into(), 7));
        assert_eq!(order(&tx, vehicle), expected);
        for (index, (entity, id, payload)) in mutations(&tx, vehicle).into_iter().enumerate() {
            if entity == "position" {
                assert_eq!(
                    payload,
                    serde_json::to_value(position(
                        id,
                        Some(2),
                        Some(match id {
                            2 => 140,
                            3 => 120,
                            _ => 130,
                        })
                    ))
                    .unwrap()
                );
                assert!(provenance(&tx, vehicle, id).is_none());
            }
            if entity == "drive" {
                if id == 1 {
                    assert!(payload["ascent"].is_null());
                    assert!(payload["descent"].is_null());
                } else {
                    assert_eq!(payload["ascent"], 10);
                    assert_eq!(
                        payload["descent"],
                        if index == 0 {
                            json!(closing_descent)
                        } else {
                            json!(20)
                        }
                    );
                }
            }
        }
        tx.commit().unwrap();
        stop_observing();
    }
    // C9 R64: closing and discarded child pages also propagate corrections.
    for mode in ["closing", "outside", "discarded"] {
        let discard = mode == "discarded";
        let (_dir, store, vehicle) = fixture();
        seed(&store, vehicle, &[(1, 1, 100), (2, 1, 110), (3, 1, 120)]);
        let mut changed = position(2, Some(1), Some(110));
        changed.longitude = 21.0;
        let mut third = position(3, Some(1), Some(120));
        if mode == "outside" {
            third.date_ms = NOW + 1;
        }
        commit(
            &store,
            vehicle,
            &crate::lifecycle::LifecycleDelta {
                open_drive_positions: vec![changed, third],
                ..Default::default()
            },
        );
        clear_mutations(&store);
        observe_calls();
        let delta = crate::lifecycle::LifecycleDelta {
            drives: if discard {
                vec![]
            } else {
                vec![drive(1, Some(20), Some(0))]
            },
            discarded_drive_ids: if discard { vec![1] } else { vec![] },
            ..Default::default()
        };
        let state = crate::lifecycle::OpenSessionState::new().encode().unwrap();
        let mut connection = store.open().unwrap();
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let mut stats = ClosePageStats::default();
        let result = HubStore::commit_lifecycle_delta_paged(
            &tx,
            &LifecycleCommit {
                vehicle_id: vehicle,
                car_id: 1,
                open_session_json: &state,
                last_observation_id: 0,
                quarantined: false,
                updated_at_ms: NOW,
                delta: &delta,
            },
            1,
            &mut stats,
        )
        .unwrap();
        assert_eq!(result.0, 2);
        assert_eq!((stats.pages, stats.largest_page), (2, 1));
        tx.commit().unwrap();
        assert_eq!(calls(), [(1, 2)]);
        stop_observing();
        if discard {
            for id in [2, 3] {
                assert!(stored_position(&connection, vehicle, id)["drive_id"].is_null());
                assert!(stored_position(&connection, vehicle, id)["elevation"].is_null());
                assert!(provenance(&connection, vehicle, id).is_none());
            }
            assert_eq!(pair(&connection, vehicle, 1), (None, None));
            assert_eq!(
                order(&connection, vehicle),
                [
                    ("position".into(), 2),
                    ("position".into(), 3),
                    ("drive".into(), 1)
                ]
            );
        } else if mode == "outside" {
            requeued(&connection, vehicle, 2, 1, 10.5, 21.0);
            let third = stored_position(&connection, vehicle, 3);
            assert!(third["drive_id"].is_null());
            assert!(third["elevation"].is_null());
            assert!(provenance(&connection, vehicle, 3).is_none());
            assert_eq!(pair(&connection, vehicle, 1), (None, None));
            assert_eq!(
                order(&connection, vehicle),
                [
                    ("drive".into(), 1),
                    ("position".into(), 2),
                    ("position".into(), 3),
                    ("drive".into(), 1),
                ]
            );
        } else {
            requeued(&connection, vehicle, 2, 1, 10.5, 21.0);
            assert_eq!(pair(&connection, vehicle, 1), (Some(20), Some(0)));
            assert_eq!(
                order(&connection, vehicle),
                [
                    ("drive".into(), 1),
                    ("position".into(), 2),
                    ("position".into(), 3)
                ]
            );
        }
        for (entity, id, payload) in mutations(&connection, vehicle) {
            if entity == "position" {
                assert_eq!(payload, stored_position(&connection, vehicle, id));
            }
        }
    }
}

#[test]
fn c9_st76_deletion_finalises_survivors() {
    // C9 ST76 (g): all three C9 R52 methods, with surviving and deleted drives.
    for way in ["a", "b", "c", "last-position"] {
        let (_dir, store, vehicle) = fixture();
        seed(&store, vehicle, &[(1, 1, 100), (2, 1, 110)]);
        let mut connection = store.open().unwrap();
        let p1 = stored_position(&connection, vehicle, 1);
        let prov1 = provenance(&connection, vehicle, 1);
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        observe_calls();
        let mut pending = TerrainRecomputeRequests::default();
        if way == "b" {
            // C9 R52 (b): capture all old ids in one statement before either deletion.
            let ids:Vec<i64>=tx.prepare("SELECT DISTINCT drive_id FROM terrain_elevation_provenance WHERE vehicle_id = ?1 AND position_id = 2 AND drive_id IS NOT NULL ORDER BY drive_id").unwrap()
                .query_map([vehicle.to_string()],|r|r.get(0)).unwrap().map(Result::unwrap).collect();
            assert_eq!(ids, [1]);
            pending.extend(ids);
            tx.execute(
                "DELETE FROM terrain_elevation_provenance WHERE position_id = 2",
                [],
            )
            .unwrap();
            tx.execute(
                "DELETE FROM materialised_positions WHERE position_id = 2",
                [],
            )
            .unwrap();
            let gone = terrain_after_position_write_in_transaction(&tx, vehicle, 2, NOW).unwrap();
            assert_eq!(gone.outcome, TerrainRewrite::Unchanged);
            assert!(gone.recompute_drive_ids.is_empty());
            pending.extend(gone.recompute_drive_ids);
        } else if way == "c" {
            tx.execute("DELETE FROM materialised_drives", []).unwrap();
            tx.execute("DELETE FROM terrain_elevation_provenance", [])
                .unwrap();
            tx.execute("DELETE FROM materialised_positions", [])
                .unwrap();
        } else {
            for id in if way == "last-position" {
                vec![1, 2]
            } else {
                vec![2]
            } {
                tx.execute(
                    "DELETE FROM materialised_positions WHERE position_id = ?1",
                    [id],
                )
                .unwrap();
                let r = terrain_after_position_write_in_transaction(&tx, vehicle, id, NOW).unwrap();
                assert_eq!(r.outcome, TerrainRewrite::Removed);
                assert_eq!(r.recompute_drive_ids, [1]);
                pending.extend(r.recompute_drive_ids);
            }
        }
        let deleted_ids: &[i64] = if matches!(way, "c" | "last-position") {
            &[1, 2]
        } else {
            &[2]
        };
        for &id in deleted_ids {
            record_sync_mutation_in_transaction(&tx, vehicle, "position", id, 1, "tombstone", "{}")
                .unwrap();
        }
        assert!(calls().is_empty());
        if way != "c" {
            assert_eq!(pair(&tx, vehicle, 1), (Some(10), Some(0)));
        }
        finalise_position_drives_in_transaction(&tx, vehicle, &[], &[], &pending.ids).unwrap();
        if way != "c" {
            assert_eq!(calls(), [(1, deleted_ids.len() as i64)]);
            assert_eq!(pair(&tx, vehicle, 1), (None, None));
            let mut expected: Vec<_> = deleted_ids
                .iter()
                .map(|&id| ("position".to_owned(), id))
                .collect();
            expected.push(("drive".into(), 1));
            assert_eq!(order(&tx, vehicle), expected);
            let payload = &mutations(&tx, vehicle)[deleted_ids.len()].2;
            assert!(payload["ascent"].is_null());
            assert!(payload["descent"].is_null());
            if way != "last-position" {
                assert_eq!(stored_position(&tx, vehicle, 1), p1);
                assert_eq!(provenance(&tx, vehicle, 1), prov1);
            }
        } else {
            assert!(calls().is_empty());
            assert_eq!(
                order(&tx, vehicle),
                [("position".into(), 1), ("position".into(), 2)]
            );
        }
        assert!(provenance(&tx, vehicle, 2).is_none());
        tx.commit().unwrap();
        stop_observing();
    }
}

#[test]
fn c9_c1_shared_recompute_phase() {
    assert_imported_closed_exclusion();
    // C9 R64: overlap, first-request order and C1-only written-drive admission.
    let (_dir, store, _) = fixture();
    let (vehicle, _, lineage) = imported_v2_base(&store);
    let vehicle = vehicle.vehicle_id;
    let source = store
        .register_source(
            &SourceDescriptor::new(IMPORT_SOURCE_KIND, "written-inventory"),
            NOW,
        )
        .unwrap();
    seed(
        &store,
        vehicle,
        &[
            (1, 1, 100),
            (2, 1, 110),
            (3, 2, 120),
            (4, 2, 130),
            (5, 3, 140),
            (6, 3, 150),
            (7, 4, 160),
            (8, 4, 170),
        ],
    );
    let mut connection = store.open().unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    tx.execute(
        "INSERT INTO teslamate_import_projection_heads(
            vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence)
         VALUES (?1, ?2, ?3, 1, 0)",
        params![
            vehicle.to_string(),
            source.source_id.to_string(),
            lineage.base.snapshot_id.to_string()
        ],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO teslamate_import_projection_rows VALUES (?1, 'drive', 2), (?1, 'drive', 3)",
        [vehicle.to_string()],
    )
    .unwrap();
    tx.execute("UPDATE materialised_drives SET ascent = 99, descent = 99, drive_json = json_set(drive_json, '$.ascent', 99, '$.descent', 99)",[]).unwrap();
    for id in [1, 2, 3, 4, 5, 6, 7, 8] {
        record_position(&tx, vehicle, id);
    }
    observe_calls();
    let mut requests = TerrainRecomputeRequests::default();
    requests.extend([3, 2, 1, 3, 999]);
    assert_eq!(requests.ids, [3, 2, 1, 999]);
    finalise_position_drives_in_transaction(
        &tx,
        vehicle,
        &[drive(2, None, None), drive(3, None, None)],
        &[4, 2, 2],
        &requests.ids,
    )
    .unwrap();
    assert_eq!(calls(), [(2, 8), (3, 8), (1, 8)]);
    assert_eq!(pair(&tx, vehicle, 4), (Some(99), Some(99)));
    assert_eq!(
        order(&tx, vehicle)[8..],
        [
            ("drive".into(), 2),
            ("drive".into(), 3),
            ("drive".into(), 1)
        ]
    );
    for id in [1, 2, 3] {
        assert_eq!(pair(&tx, vehicle, id), (Some(10), Some(0)));
    }
    assert_eq!(mutations(&tx, vehicle)[8].2["ascent"], 10);
    let corrected = &mutations(&tx, vehicle)[8];
    assert_eq!((corrected.0.as_str(), corrected.1), ("drive", 2));
    assert_eq!(corrected.2["descent"], 0);
    let stored: String = tx
        .query_row(
            "SELECT drive_json FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = 2",
            [vehicle.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(corrected.2, serde_json::from_str::<Value>(&stored).unwrap());
    tx.commit().unwrap();
    stop_observing();
}

fn assert_imported_closed_exclusion() {
    // C9 R40: ownership, rather than an empty position set, excludes imports.
    for inventory in ["legacy", "durable", "legacy-precedence"] {
        let (_dir, store, _) = fixture();
        let (vehicle, _, lineage) = imported_v2_base(&store);
        let vehicle = vehicle.vehicle_id;
        let source = store
            .register_source(
                &SourceDescriptor::new(IMPORT_SOURCE_KIND, "terrain-inventory"),
                NOW,
            )
            .unwrap();
        commit(
            &store,
            vehicle,
            &crate::lifecycle::LifecycleDelta {
                drives: vec![drive(9, Some(55), Some(40))],
                ..Default::default()
            },
        );
        clear_mutations(&store);
        let mut connection = store.open().unwrap();
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        if inventory != "durable" {
            tx.execute("INSERT INTO teslamate_import_projection_heads(vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence)
                VALUES (?1, ?2, ?3, 1, 0)", params![vehicle.to_string(), source.source_id.to_string(), lineage.base.snapshot_id.to_string()]).unwrap();
            if inventory == "legacy" {
                tx.execute(
                    "INSERT INTO teslamate_import_projection_rows VALUES (?1, 'drive', 9)",
                    [vehicle.to_string()],
                )
                .unwrap();
            }
        }
        if inventory != "legacy" {
            tx.execute("INSERT INTO teslamate_import_projection_state_heads(vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence)
                VALUES (?1, ?2, ?3, 1, 0)", params![vehicle.to_string(), source.source_id.to_string(), lineage.base.snapshot_id.to_string()]).unwrap();
            tx.execute("INSERT INTO teslamate_import_projection_state_rows VALUES (?1, 'drive', 1, 9, 1, zeroblob(32))", [vehicle.to_string()]).unwrap();
        }
        tx.execute("INSERT INTO terrain_elevation_provenance(vehicle_id, position_id, drive_id, latitude, longitude, elevation_m, status, attempts, attempted_at_ms, retry_after_ms)
            VALUES (?1, 99, 9, 10.5, 20.0, 100, 'success', 1, ?2, 0)", params![vehicle.to_string(), NOW]).unwrap();
        observe_calls();
        let r = terrain_after_position_write_in_transaction(&tx, vehicle, 99, NOW).unwrap();
        assert_eq!(r.outcome, TerrainRewrite::Removed);
        assert_eq!(r.recompute_drive_ids, [9]);
        finalise_position_drives_in_transaction(&tx, vehicle, &[], &[], &r.recompute_drive_ids)
            .unwrap();
        if inventory == "legacy-precedence" {
            assert_eq!(pair(&tx, vehicle, 9), (None, None));
            assert_eq!(calls(), [(9, 0)]);
            assert_eq!(order(&tx, vehicle), [("drive".into(), 9)]);
        } else {
            assert_eq!(pair(&tx, vehicle, 9), (Some(55), Some(40)));
            assert!(calls().is_empty());
            assert!(mutations(&tx, vehicle).is_empty());
        }
        tx.commit().unwrap();
        stop_observing();
    }
}

fn assert_catalogue_requeue(stale_json_id: bool) {
    // C9 R54: the keyed row and its current catalogue link identify the requeue.
    let (_dir, store, vehicle) = fixture();
    seed(
        &store,
        vehicle,
        &[(1, 1, 100), (2, 1, 110), (3, 2, 120), (4, 2, 130)],
    );
    let mut connection = store.open().unwrap();
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let untouched = provenance(&tx, vehicle, 3);
    tx.execute(
        "UPDATE materialised_positions SET drive_id = 2,
                position_json = json_set(position_json, '$.id', ?1, '$.drive_id', ?3)
             WHERE vehicle_id = ?2 AND position_id = 2",
        params![
            if stale_json_id { 3 } else { 2 },
            vehicle.to_string(),
            if stale_json_id { 2 } else { 1 }
        ],
    )
    .unwrap();
    observe_calls();
    let applied = terrain_after_position_write_in_transaction(&tx, vehicle, 2, NOW).unwrap();
    assert_eq!(applied.outcome, TerrainRewrite::Requeued);
    assert_eq!(applied.recompute_drive_ids, [1, 2]);
    requeued(&tx, vehicle, 2, 2, 10.5, 20.005);
    assert_eq!(provenance(&tx, vehicle, 3), untouched);
    assert!(stored_position(&tx, vehicle, 2)["elevation"].is_null());
    assert!(calls().is_empty());
    assert!(mutations(&tx, vehicle).is_empty());
    record_position(&tx, vehicle, 2);
    finalise_position_drives_in_transaction(&tx, vehicle, &[], &[], &applied.recompute_drive_ids)
        .unwrap();
    assert_eq!(calls(), [(1, 1), (2, 1)]);
    assert_eq!(pair(&tx, vehicle, 1), (None, None));
    assert_eq!(pair(&tx, vehicle, 2), (Some(10), Some(0)));
    tx.commit().unwrap();
    stop_observing();
}

#[test]
fn c9_r54_catalogue_drive_controls_requeue() {
    assert_catalogue_requeue(false);
}

#[test]
fn c9_r54_catalogue_position_key_controls_requeue() {
    assert_catalogue_requeue(true);
}

#[test]
fn c9_r54_status_link_precedence_and_requeue_metadata() {
    // C9 R54: link removal/relocation precedes every status and elevation case.
    for status in ["success", "failed", "void"] {
        for link in [None, Some(99), Some(2)] {
            for elevation in [None, Some(110), Some(140)] {
                let (_dir, store, vehicle) = fixture();
                seed(
                    &store,
                    vehicle,
                    &[(1, 1, 100), (2, 1, 110), (3, 2, 120), (4, 2, 130)],
                );
                let mut connection = store.open().unwrap();
                let tx = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                tx.execute(
                    "UPDATE terrain_elevation_provenance SET status = ?1,
                        elevation_m = CASE WHEN ?1 = 'success' THEN 110 ELSE NULL END,
                        attempts = 7, attempted_at_ms = ?2 WHERE position_id = 2",
                    params![status, FILL_AT - 10],
                )
                .unwrap();
                tx.execute(
                    "UPDATE materialised_positions SET drive_id = ?1, speed = 123,
                        position_json = json_set(position_json, '$.drive_id', ?1,
                            '$.elevation', ?2, '$.future', 77)
                     WHERE position_id = 2",
                    params![link, elevation],
                )
                .unwrap();
                let mut expected_position = stored_position(&tx, vehicle, 2);
                let clear = status == "success" && elevation == Some(110);
                if clear {
                    expected_position["elevation"] = Value::Null;
                }
                let requeue = link == Some(2) && (clear || elevation.is_none());
                observe_calls();
                let applied =
                    terrain_after_position_write_in_transaction(&tx, vehicle, 2, NOW).unwrap();
                assert_eq!(
                    applied.outcome,
                    if requeue {
                        TerrainRewrite::Requeued
                    } else {
                        TerrainRewrite::Removed
                    },
                    "{status} {link:?} {elevation:?}"
                );
                assert_eq!(
                    applied.recompute_drive_ids,
                    if link == Some(2) { vec![1, 2] } else { vec![1] }
                );
                assert_eq!(stored_position(&tx, vehicle, 2), expected_position);
                let speed: i64 = tx
                    .query_row(
                        "SELECT speed FROM materialised_positions WHERE position_id = 2",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(speed, 123);
                if requeue {
                    assert_eq!(
                        provenance(&tx, vehicle, 2),
                        Some(json!({
                            "drive": 2, "latitude": 10.5, "longitude": 20.005,
                            "elevation": null, "tile": null, "hash": null,
                            "source": null, "version": null, "status": "failed",
                            "error": "position_rewritten", "attempts": 7,
                            "at": FILL_AT - 10, "retry": NOW,
                        }))
                    );
                } else {
                    assert!(provenance(&tx, vehicle, 2).is_none());
                }
                assert!(calls().is_empty());
                assert!(mutations(&tx, vehicle).is_empty());
                assert_eq!(pair(&tx, vehicle, 1), (Some(10), Some(0)));
                assert_eq!(pair(&tx, vehicle, 2), (Some(10), Some(0)));
                let changes = tx.total_changes();
                let absent =
                    terrain_after_position_write_in_transaction(&tx, vehicle, 999, NOW).unwrap();
                assert_eq!(absent.outcome, TerrainRewrite::Unchanged);
                assert!(absent.recompute_drive_ids.is_empty());
                assert_eq!(tx.total_changes(), changes);
                record_position(&tx, vehicle, 2);
                finalise_position_drives_in_transaction(
                    &tx,
                    vehicle,
                    &[],
                    &[],
                    &applied.recompute_drive_ids,
                )
                .unwrap();
                assert_eq!(
                    calls(),
                    if link == Some(2) {
                        vec![(1, 1), (2, 1)]
                    } else {
                        vec![(1, 1)]
                    }
                );
                tx.commit().unwrap();
                stop_observing();
            }
        }
    }
}

#[test]
fn c9_r40_relocated_retry_preserves_imported_closed_totals() {
    // C9 R40, C9 R54: relocation and its later refill preserve copied drive totals.
    for inventory in ["legacy", "durable"] {
        let (_dir, store, _) = fixture();
        let (vehicle, _, lineage) = imported_v2_base(&store);
        let vehicle = vehicle.vehicle_id;
        seed(&store, vehicle, &[(1, 1, 100), (2, 1, 110)]);
        commit(
            &store,
            vehicle,
            &crate::lifecycle::LifecycleDelta {
                drives: vec![drive(9, Some(55), Some(40))],
                ..Default::default()
            },
        );
        let source = store
            .register_source(
                &SourceDescriptor::new(IMPORT_SOURCE_KIND, "relocation-inventory"),
                NOW,
            )
            .unwrap();
        let connection = store.open().unwrap();
        if inventory == "legacy" {
            connection
                .execute(
                    "INSERT INTO teslamate_import_projection_heads(
                vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence)
                VALUES (?1, ?2, ?3, 1, 0)",
                    params![
                        vehicle.to_string(),
                        source.source_id.to_string(),
                        lineage.base.snapshot_id.to_string()
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO teslamate_import_projection_rows VALUES (?1, 'drive', 9)",
                    [vehicle.to_string()],
                )
                .unwrap();
        } else {
            connection
                .execute(
                    "INSERT INTO teslamate_import_projection_state_heads(
                vehicle_id, source_id, base_snapshot_id, selected_car_id, head_sequence)
                VALUES (?1, ?2, ?3, 1, 0)",
                    params![
                        vehicle.to_string(),
                        source.source_id.to_string(),
                        lineage.base.snapshot_id.to_string()
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO teslamate_import_projection_state_rows
                VALUES (?1, 'drive', 1, 9, 1, zeroblob(32))",
                    [vehicle.to_string()],
                )
                .unwrap();
        }
        clear_mutations(&store);
        observe_calls();
        commit(
            &store,
            vehicle,
            &crate::lifecycle::LifecycleDelta {
                positions: vec![position(2, Some(9), Some(110))],
                ..Default::default()
            },
        );
        requeued(&connection, vehicle, 2, 9, 10.5, 20.005);
        assert_eq!(calls(), [(1, 1)]);
        assert_eq!(pair(&connection, vehicle, 1), (None, None));
        assert_eq!(pair(&connection, vehicle, 9), (Some(55), Some(40)));
        let due = store.terrain_due_retries(NOW, 250).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(
            due[0].candidate.as_ref().unwrap().position.drive_id,
            Some(9)
        );
        clear_mutations(&store);
        observe_calls();
        fill(&store, vehicle, &[(2, 110)]);
        assert_eq!(
            pair(&connection, vehicle, 9),
            (Some(55), Some(40)),
            "{inventory}"
        );
        assert!(
            calls().is_empty(),
            "C9 R40: imported closed drive never recomputed"
        );
        assert_eq!(order(&connection, vehicle), [("position".into(), 2)]);
        assert_eq!(provenance(&connection, vehicle, 2).unwrap()["attempts"], 2);
        stop_observing();
    }
}
