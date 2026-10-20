// SPDX-License-Identifier: AGPL-3.0-only

//! C1-C storage-level spec tests (C1 section 8a, C1 R94 with C1 R89: C1 ST104
//! (g) and (h)), through `HubStore`: the car-row version of an import is seeded
//! with the time the import recorded for its `updates` watermark, which is the
//! latest end over the imported updates, counting an update without an end from
//! its start. The import writer is not under test: the `updates` watermark is
//! written as the writer will write it, with the time computed by
//! `updates_watermark_time_ms`, and both seeding paths read it back. The unit
//! half (ST113) is in `src/runtime/lifecycle/b3_c1_c_tests.rs`. Synthetic data
//! only.

use super::*;
use crate::lifecycle::updates_watermark_time_ms;

/// The `updates` watermark rows of the vehicle as `(max_source_row_id,
/// max_timestamp_ms)`.
fn stored_updates_watermarks(store: &HubStore, vehicle: Uuid) -> Vec<(Option<i64>, Option<i64>)> {
    let connection = store.open().expect("connection");
    let mut statement = connection
        .prepare(
            "SELECT max_source_row_id, max_timestamp_ms FROM lifecycle_source_watermarks
             WHERE vehicle_id = ?1 AND domain = 'updates'",
        )
        .expect("prepare");
    statement
        .query_map(params![vehicle.to_string()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows")
}

/// ST104's setup (car row "2024.14.3 aa", no materialised update, one drive
/// ending at t0-1d) with the vehicle's imported updates `updates`, none
/// materialised. The session's `updates` watermark is `{max_id: 12,
/// max_timestamp_ms: updates_watermark_time_ms(updates)}`. For each seeding
/// path it asserts the stored watermark row, the seeded version and its time,
/// and the row of the update a live observation then records at t0.
fn seed_imported_updates(
    label: &str,
    updates: [(Option<i64>, Option<i64>); 2],
    expected_time: i64,
) {
    assert_eq!(
        updates_watermark_time_ms(updates),
        Some(expected_time),
        "{label}: the watermark time"
    );
    for path in ["seed", "promote"] {
        let (_directory, store, source, vehicle) = st104_store(label, Some(COST_T0 - R89_DAY));
        let mut session = st93_session(COST_T0 - COST_HOUR);
        session.watermarks.updates.max_id = Some(12);
        session.watermarks.updates.max_timestamp_ms = updates_watermark_time_ms(updates);
        seed_by(
            path,
            &store,
            &source,
            &vehicle,
            &session,
            COST_T0 - COST_HOUR,
        );
        assert_eq!(
            stored_updates_watermarks(&store, vehicle.vehicle_id),
            vec![(Some(12), Some(expected_time))],
            "{label}, {path}: the watermark row"
        );
        assert_eq!(
            stored_version(&store, vehicle.vehicle_id),
            (Some("2024.14.3 aa".to_owned()), Some(expected_time)),
            "{label}, {path}: the seeded version and its time"
        );
        let (rows, _) = live_version(&store, vehicle.vehicle_id, "2024.20.1 bb");
        assert_eq!(
            rows,
            vec![(expected_time, COST_T0, "2024.20.1 bb".to_owned())],
            "{label}, {path}: the missed update's row"
        );
    }
}

#[test]
fn c1_st104_updates_end_time_seed() {
    // (g) Under a latest-start rule the time would be t0-3d.
    seed_imported_updates(
        "st104-g",
        [
            (Some(COST_T0 - 5 * R89_DAY), Some(COST_T0 - 4 * R89_DAY)),
            (Some(COST_T0 - 3 * R89_DAY), Some(COST_T0 - 2 * R89_DAY)),
        ],
        COST_T0 - 2 * R89_DAY,
    );
}

#[test]
fn c1_st104_open_update_start_seed() {
    // (h) The newest update is still open: it counts from its start.
    seed_imported_updates(
        "st104-h",
        [
            (Some(COST_T0 - 3 * R89_DAY), Some(COST_T0 - 2 * R89_DAY)),
            (Some(COST_T0 - R89_DAY), None),
        ],
        COST_T0 - R89_DAY,
    );
}
