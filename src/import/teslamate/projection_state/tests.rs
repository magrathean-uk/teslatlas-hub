// SPDX-License-Identifier: AGPL-3.0-only

use std::collections::BTreeMap;

use super::*;
use crate::hub_pack::{ProjectionCarSettings, ProjectionDrive};

#[derive(Default)]
struct MemoryPrior {
    rows: BTreeMap<(u8, i64), TeslaMateProjectionStateDigestRow>,
}

impl PriorProjectionStateLookup for MemoryPrior {
    fn digest(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    ) -> Result<Option<Sha256Digest>, Box<dyn Error + Send + Sync>> {
        Ok(self.rows.get(&(entity.ordinal(), id)).map(|row| row.digest))
    }

    fn page_after(
        &mut self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateDigestPage, Box<dyn Error + Send + Sync>> {
        let (entity, id) = cursor_values(after);
        let mut rows = self
            .rows
            .values()
            .filter(|row| {
                i64::from(row.entity.ordinal()) > entity
                    || (i64::from(row.entity.ordinal()) == entity && row.id > id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let limit = usize::try_from(limit).expect("u32 fits usize");
        let next_after = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|row| TeslaMateProjectionStateCursor {
                entity: row.entity,
                id: row.id,
            })
        } else {
            None
        };
        Ok(TeslaMateProjectionStateDigestPage { rows, next_after })
    }
}

#[test]
fn write_batch_capacity_uses_a_small_fixed_margin_not_the_state_cap() {
    assert_eq!(
        write_batch_required_free_bytes(512 * 1024 * 1024).expect("bounded reserve"),
        544 * 1024 * 1024
    );
    assert!(matches!(
        write_batch_required_free_bytes(u64::MAX),
        Err(TeslaMateProjectionStateError::StateCapacityOverflow)
    ));
}

fn drive(id: i64, distance_km: Option<f64>) -> ProjectionDrive {
    ProjectionDrive {
        id,
        car_id: 1,
        optimized_at_ms: None,
        start_date_ms: 1,
        end_date_ms: 2,
        distance_km,
        duration_min: None,
        efficiency: None,
        outside_temp_avg: None,
        inside_temp_avg: None,
        speed_max: None,
        power_max: None,
        power_min: None,
        start_ideal_range_km: None,
        end_ideal_range_km: None,
        start_address: None,
        end_address: None,
        start_geofence: None,
        end_geofence: None,
        start_latitude: None,
        start_longitude: None,
        end_latitude: None,
        end_longitude: None,
        start_soc: None,
        end_soc: None,
        start_rated_range_km: None,
        end_rated_range_km: None,
        ascent: None,
        descent: None,
    }
}

#[test]
fn retains_only_changed_payloads_and_pages_digests() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    let original = drive(7, Some(10.0));
    let original_digest = state.record_drive(&original).expect("record original");
    let mut prior = MemoryPrior::default();
    prior.rows.insert(
        (TeslaMateProjectionStateEntity::Drive.ordinal(), 7),
        TeslaMateProjectionStateDigestRow {
            entity: TeslaMateProjectionStateEntity::Drive,
            id: 7,
            car_id: 1,
            digest: original_digest,
        },
    );
    let unchanged = state
        .record_if_changed(
            &mut prior,
            TeslaMateProjectionStateEntity::Position,
            8,
            1,
            &serde_json::json!({"id": 8}),
        )
        .expect("new row");
    assert_eq!(unchanged, TeslaMateProjectionStateChange::NewOrChanged);
    state.seal().expect("seal");
    assert_eq!(state.stats().row_count, 2);
    assert_eq!(state.stats().changed_row_count, 1);
    let current = state.page(None, 10).expect("current page");
    assert_eq!(current.rows.len(), 2);
    let changed = state.changed_page(None, 10).expect("changed page");
    assert_eq!(changed.rows.len(), 1);
    assert_eq!(changed.rows[0].state.id, 8);
}

#[test]
fn lookup_unchanged_omits_payload_and_missing_old_rows_become_tombstones() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut seed = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("seed state");
    let row = drive(7, Some(10.0));
    let digest = seed.record_drive(&row).expect("record seed");
    seed.seal().expect("seal seed");
    let mut prior = MemoryPrior::default();
    prior.rows.insert(
        (TeslaMateProjectionStateEntity::Drive.ordinal(), 7),
        TeslaMateProjectionStateDigestRow {
            entity: TeslaMateProjectionStateEntity::Drive,
            id: 7,
            car_id: 1,
            digest,
        },
    );
    prior.rows.insert(
        (TeslaMateProjectionStateEntity::Position.ordinal(), 8),
        TeslaMateProjectionStateDigestRow {
            entity: TeslaMateProjectionStateEntity::Position,
            id: 8,
            car_id: 1,
            digest: Sha256Digest::of_bytes(b"removed"),
        },
    );
    prior.rows.insert(
        (TeslaMateProjectionStateEntity::Car.ordinal(), 1),
        TeslaMateProjectionStateDigestRow {
            entity: TeslaMateProjectionStateEntity::Car,
            id: 1,
            car_id: 1,
            digest: Sha256Digest::of_bytes(b"car"),
        },
    );

    let mut state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    assert_eq!(
        state
            .record_if_changed(
                &mut prior,
                TeslaMateProjectionStateEntity::Drive,
                row.id,
                row.car_id,
                &row,
            )
            .expect("unchanged drive"),
        TeslaMateProjectionStateChange::Unchanged
    );
    state.seal().expect("seal");
    assert!(
        state
            .changed_page(None, 10)
            .expect("changed")
            .rows
            .is_empty()
    );
    let (tombstones, next) = state
        .tombstone_page(&mut prior, None, 10)
        .expect("tombstones");
    assert!(next.is_none());
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].id, 8);
    assert_eq!(
        tombstones[0].entity,
        crate::hub_pack::ProjectionDeltaEntity::Position
    );
}

#[test]
fn initial_base_capture_keeps_only_digests_even_for_repeated_fragment_rows() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 1,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    let row = drive(7, Some(10.0));
    let mut capture = TeslaMateProjectionStateCapture::for_initial_base(state);
    assert_eq!(
        capture.record_drive(&row).expect("capture row"),
        TeslaMateProjectionStateChange::CapturedDigestOnly
    );
    assert_eq!(
        capture
            .record_drive(&row)
            .expect("deduplicate fragment repeat"),
        TeslaMateProjectionStateChange::CapturedDigestOnly
    );
    capture.seal().expect("seal");
    assert_eq!(
        capture.mode(),
        TeslaMateProjectionStateCaptureMode::InitialBase
    );
    assert_eq!(capture.stats().row_count, 1);
    assert_eq!(capture.stats().changed_row_count, 0);
    assert_eq!(capture.stats().changed_payload_bytes, 0);
}

#[test]
fn canonicalizes_nested_object_keys_before_digesting_or_spooling_payload() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let limits = TeslaMateProjectionStateLimits {
        max_rows: 10,
        max_state_bytes: 128 * 1024,
        max_changed_payload_bytes: 128 * 1024,
        minimum_free_bytes: 0,
    };
    let mut nested_one = serde_json::Map::new();
    nested_one.insert("z".into(), serde_json::json!(2));
    nested_one.insert("a".into(), serde_json::json!(1));
    let mut object_one = serde_json::Map::new();
    object_one.insert("z".into(), serde_json::json!(3));
    object_one.insert("a".into(), serde_json::Value::Object(nested_one));

    let mut nested_two = serde_json::Map::new();
    nested_two.insert("a".into(), serde_json::json!(1));
    nested_two.insert("z".into(), serde_json::json!(2));
    let mut object_two = serde_json::Map::new();
    object_two.insert("a".into(), serde_json::Value::Object(nested_two));
    object_two.insert("z".into(), serde_json::json!(3));

    let mut first = TeslaMateProjectionState::create(temporary.path(), limits).expect("first");
    let digest = first
        .record_changed(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::Value::Object(object_one),
        )
        .expect("record changed");
    first.seal().expect("seal first");
    assert_eq!(
        first.changed_page(None, 10).expect("changed page").rows[0].canonical_payload,
        br#"{"a":{"a":1,"z":2},"z":3}"#
    );

    let mut second = TeslaMateProjectionState::create(temporary.path(), limits).expect("second");
    let equivalent_digest = second
        .record(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::Value::Object(object_two),
        )
        .expect("record equivalent");
    assert_eq!(digest, equivalent_digest);
}

#[test]
fn changed_page_rejects_a_same_length_payload_tampered_after_capture() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    let original = br#"{"payload":"a"}"#;
    let tampered = br#"{"payload":"b"}"#;
    assert_eq!(
        original.len(),
        tampered.len(),
        "regression requires same-size tampering"
    );
    state
        .record_changed(
            TeslaMateProjectionStateEntity::Position,
            7,
            1,
            &serde_json::json!({"payload": "a"}),
        )
        .expect("record changed payload");
    state.seal().expect("seal");

    // The API cannot create this condition. Mutate the private spool only
    // in the regression to prove byte accounting alone is insufficient.
    state
        .connection
        .execute(
            "UPDATE changed_rows SET payload_json = ?1 \
             WHERE entity_ordinal = ?2 AND entity_id = ?3",
            params![
                std::str::from_utf8(tampered).expect("test JSON is UTF-8"),
                i64::from(TeslaMateProjectionStateEntity::Position.ordinal()),
                7_i64,
            ],
        )
        .expect("same-length tamper");

    assert!(matches!(
        state.changed_page(None, 10),
        Err(TeslaMateProjectionStateError::StoredChangedPayloadDigestMismatch)
    ));
}

#[test]
fn deduplicates_exact_rows_rejects_conflicts_and_cleans_up_private_file() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 2,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    let path = state.path.clone();
    assert_eq!(
        fs::metadata(path.parent().expect("state parent"))
            .expect("state parent permissions")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let car = ProjectionCar {
        id: 1,
        name: "Road car".into(),
        model: "3".into(),
        vin: None,
        source_eid: None,
        source_vid: None,
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        firmware_version: None,
        efficiency_wh_per_km: None,
        settings: ProjectionCarSettings::default(),
    };
    state.record_car(&car).expect("car");
    state.record_car(&car).expect("exact repeat is a no-op");
    assert_eq!(state.stats().row_count, 1);
    let conflicting = ProjectionCar {
        name: "Other car".into(),
        ..car.clone()
    };
    assert!(matches!(
        state.record_car(&conflicting),
        Err(TeslaMateProjectionStateError::ConflictingRow { .. })
    ));
    let changed = serde_json::json!({"id": 2, "value": "new"});
    state
        .record_changed(TeslaMateProjectionStateEntity::Position, 2, 1, &changed)
        .expect("changed row");
    state
        .record_car(&car)
        .expect("exact current repeat is permitted at row capacity");
    let accounting = state.stats();
    state
        .record_changed(TeslaMateProjectionStateEntity::Position, 2, 1, &changed)
        .expect("exact changed repeat is a no-op");
    assert_eq!(state.stats(), accounting);
    assert!(matches!(
        state.page(None, 1),
        Err(TeslaMateProjectionStateError::StateNotSealed)
    ));
    state.discard().expect("discard");
    assert!(
        matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    );
}

#[test]
fn targeted_current_upsert_does_not_ignore_non_uniqueness_errors() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 2,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    state
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_current_rows \
             BEFORE INSERT ON current_rows \
             BEGIN SELECT RAISE(FAIL, 'injected current-row failure'); END;",
        )
        .expect("install failure trigger");

    assert!(matches!(
        state.record(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"id": 1}),
        ),
        Err(TeslaMateProjectionStateError::Sqlite(_))
    ));
    assert_eq!(state.stats().row_count, 0);
    assert!(state.connection.is_autocommit());

    state
        .connection
        .execute_batch("DROP TRIGGER reject_current_rows")
        .expect("remove failure trigger");
    state
        .record(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"id": 1}),
        )
        .expect("state remains usable after a non-uniqueness error");
    state.seal().expect("seal state");
}

#[test]
fn targeted_current_upsert_error_mid_batch_preserves_prior_pending_rows() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    state
        .record(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"id": 1}),
        )
        .expect("first pending row");
    assert_eq!(state.pending_write_rows, 1);
    assert!(!state.connection.is_autocommit());

    state
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_second_current_row \
             BEFORE INSERT ON current_rows \
             WHEN NEW.entity_id = 2 \
             BEGIN SELECT RAISE(ABORT, 'injected current-row failure'); END;",
        )
        .expect("install failure trigger");
    assert!(matches!(
        state.record(
            TeslaMateProjectionStateEntity::Position,
            2,
            1,
            &serde_json::json!({"id": 2}),
        ),
        Err(TeslaMateProjectionStateError::Sqlite(_))
    ));
    assert_eq!(state.stats().row_count, 1);
    assert_eq!(state.pending_write_rows, 1);
    assert!(!state.connection.is_autocommit());

    state
        .connection
        .execute_batch("DROP TRIGGER reject_second_current_row")
        .expect("remove failure trigger");
    state
        .record(
            TeslaMateProjectionStateEntity::Position,
            3,
            1,
            &serde_json::json!({"id": 3}),
        )
        .expect("state remains usable after a mid-batch fast-path error");
    state.seal().expect("seal recovered state");
    assert_eq!(
        state
            .page(None, 10)
            .expect("sealed rows")
            .rows
            .iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
}

#[test]
fn fixed_write_batch_commits_at_the_boundary_and_seal_flushes_the_tail() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: u64::from(WRITE_BATCH_ROWS) + 1,
            max_state_bytes: 4 * 1024 * 1024,
            max_changed_payload_bytes: 4 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    state
        .record_changed(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"id": 1}),
        )
        .expect("first pending changed row");
    for id in 2..i64::from(WRITE_BATCH_ROWS) {
        state
            .record(
                TeslaMateProjectionStateEntity::Position,
                id,
                1,
                &serde_json::json!({"id": id}),
            )
            .expect("record pending row");
    }
    assert_eq!(state.pending_write_rows, WRITE_BATCH_ROWS - 1);
    assert!(!state.connection.is_autocommit());
    assert_eq!(state.stats().changed_row_count, 1);
    state
        .record(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"id": 1}),
        )
        .expect("deduplicate within unflushed batch");
    assert!(matches!(
        state.record(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"id": 1, "different": true}),
        ),
        Err(TeslaMateProjectionStateError::ConflictingRow { .. })
    ));
    assert_eq!(state.pending_write_rows, WRITE_BATCH_ROWS - 1);
    assert!(!state.connection.is_autocommit());
    assert_eq!(
        state.stats().row_count,
        u64::from(WRITE_BATCH_ROWS - 1),
        "accepted rows remain visible in the state accounting before commit"
    );

    state
        .record(
            TeslaMateProjectionStateEntity::Position,
            i64::from(WRITE_BATCH_ROWS),
            1,
            &serde_json::json!({"id": WRITE_BATCH_ROWS}),
        )
        .expect("record boundary row");
    assert_eq!(state.pending_write_rows, 0);
    assert!(state.connection.is_autocommit());

    state
        .record(
            TeslaMateProjectionStateEntity::Position,
            i64::from(WRITE_BATCH_ROWS) + 1,
            1,
            &serde_json::json!({"id": WRITE_BATCH_ROWS + 1}),
        )
        .expect("record tail row");
    assert_eq!(state.pending_write_rows, 1);
    assert!(!state.connection.is_autocommit());

    let sealed = state.seal().expect("seal flushes tail row");
    assert_eq!(sealed.row_count, u64::from(WRITE_BATCH_ROWS) + 1);
    assert_eq!(sealed.changed_row_count, 1);
    assert!(state.connection.is_autocommit());
    assert_eq!(
        state
            .page(None, WRITE_BATCH_ROWS + 1)
            .expect("read after sealed flush")
            .rows
            .len(),
        usize::try_from(WRITE_BATCH_ROWS + 1).expect("batch size fits usize")
    );
    assert_eq!(
        state
            .changed_page(None, 10)
            .expect("changed row survives shared batch")
            .rows
            .len(),
        1
    );
}

#[test]
fn changed_pair_failure_rolls_back_only_the_unpublished_row() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    state
        .record(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"id": 1}),
        )
        .expect("first pending row");
    state
        .connection
        .execute_batch(
            "CREATE TRIGGER reject_changed_rows \
             BEFORE INSERT ON changed_rows \
             BEGIN SELECT RAISE(FAIL, 'injected changed-row failure'); END;",
        )
        .expect("install failure trigger");

    assert!(
        state
            .record_changed(
                TeslaMateProjectionStateEntity::Position,
                2,
                1,
                &serde_json::json!({"id": 2}),
            )
            .is_err()
    );
    assert_eq!(state.stats().row_count, 1);
    assert_eq!(state.stats().changed_row_count, 0);
    assert_eq!(
        state
            .connection
            .query_row(
                "SELECT COUNT(*) FROM current_rows WHERE entity_ordinal = ?1 AND entity_id = ?2",
                params![
                    i64::from(TeslaMateProjectionStateEntity::Position.ordinal()),
                    2_i64
                ],
                |row| row.get::<_, i64>(0),
            )
            .expect("inspect current row"),
        0
    );
    assert!(!state.connection.is_autocommit());

    state.seal().expect("remaining batch stays usable");
    let rows = state.page(None, 10).expect("sealed page").rows;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, 1);
}

#[test]
fn changed_payload_byte_cap_flushes_before_the_row_cap() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut state = TeslaMateProjectionState::create_with_changed_payload_row_limit(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 32 * 1024 * 1024,
            max_changed_payload_bytes: 16 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
        16 * 1024 * 1024,
    )
    .expect("state");
    let payload = "x".repeat(
        usize::try_from(WRITE_BATCH_CHANGED_PAYLOAD_BYTES).expect("payload batch limit fits usize"),
    );
    state
        .record_changed(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"payload": payload}),
        )
        .expect("changed row reaches byte cap");

    assert_eq!(state.pending_write_rows, 0);
    assert!(state.connection.is_autocommit());
    assert_eq!(state.stats().changed_row_count, 1);
    assert!(
        state.stats().changed_payload_bytes >= WRITE_BATCH_CHANGED_PAYLOAD_BYTES,
        "the commit was driven by payload bytes, not the 1,024-row cap"
    );
    state.seal().expect("already-flushed state seals cleanly");
}

#[test]
fn changed_payload_row_cap_rejects_before_any_durable_retention() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let maximum_payload_bytes = 1024_u64;
    let mut state = TeslaMateProjectionState::create_with_changed_payload_row_limit(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
        maximum_payload_bytes,
    )
    .expect("state");

    let error = state
        .record_changed(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"payload": "x".repeat(1024)}),
        )
        .expect_err("canonical JSON overhead makes this source row exceed the configured cap");
    assert!(matches!(
        error,
        TeslaMateProjectionStateError::ChangedPayloadRowLimitExceeded {
            maximum,
            payload_bytes,
        } if maximum == maximum_payload_bytes && payload_bytes > maximum_payload_bytes
    ));
    assert_eq!(state.stats().row_count, 0);
    assert_eq!(state.stats().changed_row_count, 0);
    assert_eq!(state.pending_write_rows, 0);
    assert_eq!(
        state
            .connection
            .query_row("SELECT COUNT(*) FROM current_rows", [], |row| row
                .get::<_, i64>(0))
            .expect("inspect current rows"),
        0,
        "a rejected payload must not leave a current-row-only orphan"
    );
    assert_eq!(
        state
            .connection
            .query_row("SELECT COUNT(*) FROM changed_rows", [], |row| row
                .get::<_, i64>(0))
            .expect("inspect changed rows"),
        0
    );
    state
        .seal()
        .expect("empty state remains sealable after rejection");
}

#[test]
fn changed_page_payload_cap_preserves_order_and_cursor_without_loading_an_over_cap_page() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let page_cap = 8 * 1024_u64;
    let mut state = TeslaMateProjectionState::create_with_changed_payload_row_limit(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 64 * 1024,
            minimum_free_bytes: 0,
        },
        16 * 1024,
    )
    .expect("state");
    for id in 1..=3_i64 {
        state
            .record_changed(
                TeslaMateProjectionStateEntity::Position,
                id,
                1,
                &serde_json::json!({"id": id, "payload": "x".repeat(3 * 1024)}),
            )
            .expect("bounded changed row");
    }
    state.seal().expect("seal");

    let first = state
        .changed_page_with_payload_limit(None, 10, page_cap)
        .expect("first byte-bounded page");
    assert_eq!(
        first
            .rows
            .iter()
            .map(|row| row.state.id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(
        first
            .rows
            .iter()
            .map(|row| u64::try_from(row.canonical_payload.len()).expect("usize fits u64"))
            .sum::<u64>()
            <= page_cap
    );
    assert_eq!(
        first.next_after,
        Some(TeslaMateProjectionStateCursor {
            entity: TeslaMateProjectionStateEntity::Position,
            id: 2,
        })
    );

    let second = state
        .changed_page_with_payload_limit(first.next_after, 10, page_cap)
        .expect("second byte-bounded page");
    assert_eq!(
        second
            .rows
            .iter()
            .map(|row| row.state.id)
            .collect::<Vec<_>>(),
        vec![3]
    );
    assert!(second.next_after.is_none());
}

#[test]
fn changed_page_rejects_an_individual_row_before_decoding_it() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let configured_row_cap = 16 * 1024_u64;
    let requested_page_cap = 8 * 1024_u64;
    let mut state = TeslaMateProjectionState::create_with_changed_payload_row_limit(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 64 * 1024,
            minimum_free_bytes: 0,
        },
        configured_row_cap,
    )
    .expect("state");
    state
        .record_changed(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"payload": "x".repeat(9 * 1024)}),
        )
        .expect("row fits the durable cap");
    state.seal().expect("seal");

    let error = state
        .changed_page_with_payload_limit(None, 10, requested_page_cap)
        .expect_err("metadata must reject an oversized row before its JSON is fetched");
    assert!(matches!(
        error,
        TeslaMateProjectionStateError::ChangedPayloadRowLimitExceeded {
            maximum,
            payload_bytes,
        } if maximum == requested_page_cap && payload_bytes > requested_page_cap
    ));
}

#[test]
fn changed_payload_boundary_commits_the_prior_batch_before_the_next_row() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 32 * 1024 * 1024,
            max_changed_payload_bytes: 16 * 1024 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    let first_payload = "x".repeat(
        usize::try_from(WRITE_BATCH_CHANGED_PAYLOAD_BYTES - 1024 * 1024)
            .expect("payload batch limit fits usize"),
    );
    let second_payload = "y".repeat(2 * 1024 * 1024);
    state
        .record_changed(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"payload": first_payload}),
        )
        .expect("first changed row stays pending");
    assert_eq!(state.pending_write_rows, 1);
    assert!(!state.connection.is_autocommit());

    state
        .record_changed(
            TeslaMateProjectionStateEntity::Position,
            2,
            1,
            &serde_json::json!({"payload": second_payload}),
        )
        .expect("second changed row crosses byte boundary");
    assert_eq!(state.pending_write_rows, 1);
    assert!(!state.connection.is_autocommit());
    assert_eq!(state.stats().changed_row_count, 2);

    state.seal().expect("flush second bounded batch");
    assert_eq!(state.stats().changed_row_count, 2);
}

#[test]
fn failed_batch_flush_rolls_back_pending_rows_and_poisons_the_capture() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let mut state = TeslaMateProjectionState::create(
        temporary.path(),
        TeslaMateProjectionStateLimits {
            max_rows: 10,
            max_state_bytes: 128 * 1024,
            max_changed_payload_bytes: 128 * 1024,
            minimum_free_bytes: 0,
        },
    )
    .expect("state");
    state
        .record(
            TeslaMateProjectionStateEntity::Position,
            1,
            1,
            &serde_json::json!({"id": 1}),
        )
        .expect("pending row");
    // The API never writes an invalid changed row. Inject one through the
    // private connection solely to make COMMIT fail after a valid pending
    // write, then verify accounting and visibility reset together.
    state
        .connection
        .execute_batch("PRAGMA defer_foreign_keys = ON")
        .expect("defer foreign key enforcement until commit");
    state
        .connection
        .execute(
            "INSERT INTO changed_rows( \
                entity_ordinal, entity_id, car_id, projection_sha256, payload_json, payload_bytes \
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![0_i64, 99_i64, 1_i64, vec![0_u8; 32], "{}", 2_i64],
        )
        .expect("inject deferred foreign-key violation");

    assert!(matches!(
        state.seal(),
        Err(TeslaMateProjectionStateError::Sqlite(_))
    ));
    assert!(state.write_failed);
    assert!(state.connection.is_autocommit());
    assert_eq!(state.stats().row_count, 0);
    assert_eq!(state.stats().changed_row_count, 0);
    assert_eq!(
        state
            .connection
            .query_row("SELECT COUNT(*) FROM current_rows", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("pending rows rolled back"),
        0
    );
    assert!(matches!(
        state.record(
            TeslaMateProjectionStateEntity::Position,
            2,
            1,
            &serde_json::json!({"id": 2}),
        ),
        Err(TeslaMateProjectionStateError::WriteBatchFailed)
    ));
    let mut prior = MemoryPrior::default();
    assert!(matches!(
        state.record_if_changed(
            &mut prior,
            TeslaMateProjectionStateEntity::Position,
            2,
            1,
            &serde_json::json!({"id": 2}),
        ),
        Err(TeslaMateProjectionStateError::WriteBatchFailed)
    ));
}
