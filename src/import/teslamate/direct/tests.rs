// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    credentials::TeslaMatePostgresPassword,
    teslamate::ReadOnlySource,
    teslamate_projection_state::{
        PriorProjectionStateLookup, TeslaMateProjectionState, TeslaMateProjectionStateCursor,
        TeslaMateProjectionStateDigestPage, TeslaMateProjectionStateDigestRow,
        TeslaMateProjectionStateEntity,
    },
};

struct DirectTestPrior {
    rows: Vec<TeslaMateProjectionStateDigestRow>,
}

impl PriorProjectionStateLookup for DirectTestPrior {
    fn digest(
        &mut self,
        entity: TeslaMateProjectionStateEntity,
        id: i64,
    ) -> Result<Option<Sha256Digest>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self
            .rows
            .iter()
            .find(|row| row.entity == entity && row.id == id)
            .map(|row| row.digest))
    }

    fn page_after(
        &mut self,
        after: Option<TeslaMateProjectionStateCursor>,
        limit: u32,
    ) -> Result<TeslaMateProjectionStateDigestPage, Box<dyn std::error::Error + Send + Sync>> {
        let after = after.map(|cursor| (cursor.entity.ordinal(), cursor.id));
        let mut rows = self
            .rows
            .iter()
            .filter(|row| after.is_none_or(|after| (row.entity.ordinal(), row.id) > after))
            .cloned()
            .collect::<Vec<_>>();
        let limit = usize::try_from(limit).expect("page limit fits usize");
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeFixtureCounts {
    cars: u64,
    drives: u64,
    positions: u64,
    charging_processes: u64,
    charges: u64,
    schema_migrations: u64,
    private_tokens: u64,
}

fn configured_native_postgres_source(
    allow_local_fixture: bool,
) -> Option<(ReadOnlySource, TeslaMatePostgresPassword)> {
    let url = std::env::var("TESLATLAS_HUB_TEST_POSTGRES_URL")
        .ok()
        .or_else(|| {
            allow_local_fixture
                .then(|| std::env::var("TESLATLAS_LOCAL_TESLAMATE_URL").ok())
                .flatten()
        })?;
    let source = ReadOnlySource::parse(&url).expect("credential-free source URL");
    Some((
        source,
        configured_native_postgres_password(allow_local_fixture),
    ))
}

/// Read an explicitly supplied password file using the production parser.
fn configured_native_postgres_password(allow_local_fixture: bool) -> TeslaMatePostgresPassword {
    if let Some(credentials_directory) =
        std::env::var_os("TESLATLAS_HUB_TEST_POSTGRES_CREDENTIALS_DIRECTORY")
    {
        let path = PathBuf::from(credentials_directory).join("teslamate-postgres-password");
        return TeslaMatePostgresPassword::from_bytes(
            &normalize_private_postgres_password_file_bytes(
                fs::read(path).expect("test PostgreSQL credential"),
            )
            .expect("test PostgreSQL credential line"),
        )
        .expect("test PostgreSQL credential");
    }

    let password_file = std::env::var_os("TESLATLAS_HUB_TEST_POSTGRES_PASSWORD_FILE")
        .or_else(|| {
            allow_local_fixture
                .then(|| std::env::var_os("TESLATLAS_LOCAL_TESLAMATE_POSTGRES_PASSWORD_FILE"))
                .flatten()
        })
        .map(PathBuf::from)
        .expect("a private PostgreSQL password-file environment value");
    let metadata =
        fs::symlink_metadata(&password_file).expect("configured PostgreSQL password file metadata");
    assert!(
        metadata.file_type().is_file(),
        "configured PostgreSQL password must be a regular file"
    );
    assert_eq!(
        metadata.permissions().mode() & 0o077,
        0,
        "configured PostgreSQL password must not be group- or world-readable"
    );

    let password = normalize_private_postgres_password_file_bytes(
        fs::read(&password_file).expect("read private PostgreSQL password"),
    )
    .expect("configured PostgreSQL password must contain at most one terminal LF or CRLF");
    TeslaMatePostgresPassword::from_bytes(&password).expect("test PostgreSQL password credential")
}

fn normalize_private_postgres_password_file_bytes(
    mut bytes: Vec<u8>,
) -> Result<Vec<u8>, &'static str> {
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.pop();
    }
    if bytes.is_empty()
        || bytes
            .iter()
            .any(|byte| matches!(byte, b'\0' | b'\r' | b'\n'))
    {
        return Err("invalid PostgreSQL password line");
    }
    Ok(bytes)
}

#[test]
fn private_postgres_password_adapter_accepts_one_terminal_line_ending_only() {
    assert_eq!(
        normalize_private_postgres_password_file_bytes(b"private-password\n".to_vec())
            .expect("LF-terminated private password"),
        b"private-password"
    );
    assert_eq!(
        normalize_private_postgres_password_file_bytes(b"private-password\r\n".to_vec())
            .expect("CRLF-terminated private password"),
        b"private-password"
    );
    for invalid in [
        b"private\npassword".as_slice(),
        b"private\rpassword".as_slice(),
        b"private\n\n".as_slice(),
        b"\n".as_slice(),
        b"private\0password".as_slice(),
    ] {
        assert!(
            normalize_private_postgres_password_file_bytes(invalid.to_vec()).is_err(),
            "only one terminal LF or CRLF is accepted"
        );
    }
}

fn validated_native_fixture_source_counts(
    path: &Path,
    expected_sha256: Option<String>,
) -> NativeFixtureCounts {
    let metadata = fs::symlink_metadata(path).expect("fixture counts metadata");
    assert!(
        metadata.file_type().is_file(),
        "fixture counts metadata must be a regular file"
    );
    let bytes = fs::read(path).expect("read fixture counts metadata");
    if let Some(expected_sha256) = expected_sha256 {
        assert_eq!(expected_sha256.len(), 64, "fixture counts digest length");
        assert!(
            expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "fixture counts digest must be hexadecimal"
        );
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            expected_sha256.to_ascii_lowercase(),
            "fixture counts digest"
        );
    }
    let counts: NativeFixtureCounts =
        serde_json::from_slice(&bytes).expect("valid fixture counts metadata");
    assert!(counts.cars >= 1, "fixture counts must include a car");
    assert!(
        counts.positions > 0,
        "fixture counts must include positions"
    );
    assert_eq!(
        counts.private_tokens, 0,
        "fixture counts must remain token-redacted"
    );
    let audited_rows = counts
        .cars
        .checked_add(counts.drives)
        .and_then(|value| value.checked_add(counts.positions))
        .and_then(|value| value.checked_add(counts.charging_processes))
        .and_then(|value| value.checked_add(counts.charges))
        .and_then(|value| value.checked_add(counts.schema_migrations))
        .expect("fixture counts total must fit u64");
    assert!(audited_rows >= counts.positions);
    counts
}

async fn native_ten_million_expected_source_counts(
    source: &ReadOnlySource,
    password: &TeslaMatePostgresPassword,
    limits: TeslaMateReadLimits,
    packs_dir: &Path,
) -> TeslaMateSourceCounts {
    let expected_positions = std::env::var("TESLATLAS_HUB_TEST_EXPECTED_POSITIONS")
        .ok()
        .map(|value| {
            let positions = value
                .parse::<u64>()
                .expect("TESLATLAS_HUB_TEST_EXPECTED_POSITIONS must be an unsigned integer");
            assert!(positions > 0, "expected source positions must be positive");
            positions
        });

    let counts = std::env::var_os("TESLATLAS_HUB_TEST_COUNTS_FILE")
        .map(|path| {
            (
                PathBuf::from(path),
                std::env::var("TESLATLAS_HUB_TEST_COUNTS_SHA256").ok(),
            )
        })
        .or_else(|| {
            std::env::var_os("TESLATLAS_LOCAL_TESLAMATE_COUNTS_FILE").map(|path| {
                (
                    PathBuf::from(path),
                    std::env::var("TESLATLAS_LOCAL_TESLAMATE_COUNTS_SHA256").ok(),
                )
            })
        });
    let fixture_counts = counts.map(|(counts_file, expected_sha256)| {
        validated_native_fixture_source_counts(&counts_file, expected_sha256)
    });
    if let Some(fixture_counts) = fixture_counts.as_ref()
        && let Some(expected_positions) = expected_positions
    {
        assert_eq!(
            fixture_counts.positions, expected_positions,
            "position environment override must match validated fixture metadata"
        );
    }

    let source_counts = preflight_teslamate_import(source, password, 1, limits, packs_dir)
        .await
        .expect("source-count preflight")
        .source_row_counts;
    if let Some(expected_positions) = expected_positions {
        assert_eq!(
            source_counts.positions, expected_positions,
            "position environment override must match source preflight"
        );
    }
    if let Some(fixture_counts) = fixture_counts {
        assert_eq!(
            source_counts.cars, fixture_counts.cars,
            "source preflight cars must match validated fixture metadata"
        );
        assert_eq!(
            source_counts.drives, fixture_counts.drives,
            "source preflight drives must match validated fixture metadata"
        );
        assert_eq!(
            source_counts.positions, fixture_counts.positions,
            "source preflight positions must match validated fixture metadata"
        );
        assert_eq!(
            source_counts.charging_processes, fixture_counts.charging_processes,
            "source preflight charging processes must match validated fixture metadata"
        );
        assert_eq!(
            source_counts.charges, fixture_counts.charges,
            "source preflight charges must match validated fixture metadata"
        );
    }
    source_counts
}

#[test]
fn preflight_report_is_bounded_and_redacted() {
    let report = TeslaMatePreflightReport {
        selected_car_id: 17,
        source_database_bytes: 123,
        schema: TeslaMateSchemaInfo {
            observed_migration_version: 20260808090000,
            observed_migration_count: 105,
            minimum_supported_migration_version: 20260808090000,
            maximum_validated_migration_version: 20260808090000,
            pinned_source_revision: "d6c43bc8c48784da8f0b701945b80b20911b3d1a",
            pinned_migration_set_sha256: "ea850d1b038c4af950db32e7a0939aa5ebe8f1dcefe5e56dcd592f3451038868",
            fingerprint: "abc".to_owned(),
        },
        source_row_counts: TeslaMateSourceCounts {
            cars: 1,
            drives: 2,
            positions: 3,
            charging_processes: 4,
            charges: 5,
            states: 6,
            addresses: 7,
            geofences: 8,
            updates: 9,
        },
        target_available_bytes: 456,
        estimated_target_output_bytes: 100,
        projection_state_maximum_bytes: 200,
        active_pack_transient_bytes: 300,
        target_required_bytes: 1_112,
        configured_maximum_rows: 20_000_000,
        configured_staging_limit_bytes: 4 * 1024 * 1024 * 1024,
        configured_staging_reserve_bytes: 512 * 1024 * 1024,
        admission: TeslaMatePreflightAdmission {
            passed: true,
            reason: None,
        },
    };
    let value = serde_json::to_value(report).expect("preflight JSON");
    assert_eq!(value["selectedCarId"], 17);
    assert_eq!(value["sourceRowCounts"]["positions"], 3);
    assert_eq!(value["sourceRowCounts"]["addresses"], 7);
    assert_eq!(value["sourceRowCounts"]["geofences"], 8);
    assert_eq!(value["sourceRowCounts"]["updates"], 9);
    assert_eq!(value["estimatedTargetOutputBytes"], 100);
    assert_eq!(value["projectionStateMaximumBytes"], 200);
    assert_eq!(value["activePackTransientBytes"], 300);
    assert_eq!(value["targetRequiredBytes"], 1_112);
    assert_eq!(value["admission"]["passed"], true);
    assert!(value.get("password").is_none());
    assert!(value.get("sourceUrl").is_none());
    assert!(value.get("snapshotId").is_none());
}

fn direct_retention_test_counts() -> TeslaMateSourceCounts {
    TeslaMateSourceCounts {
        cars: 1,
        drives: 0,
        positions: 0,
        charging_processes: 0,
        charges: 0,
        states: 0,
        addresses: 0,
        geofences: 0,
        updates: 0,
    }
}

#[test]
fn direct_geofence_count_joins_one_related_id_union() {
    let after_addresses = direct_source_count_sql()
        .split_once(") AS \"addresses\",")
        .expect("address count boundary")
        .1;
    let geofence_count = after_addresses
        .split_once(") AS \"geofences\",")
        .expect("geofence count boundary")
        .0;

    assert!(geofence_count.contains("JOIN ("));
    assert_eq!(geofence_count.matches("UNION").count(), 2);
    assert!(geofence_count.contains("\"drive\".\"start_geofence_id\" AS \"id\""));
    assert!(geofence_count.contains("\"drive\".\"end_geofence_id\" AS \"id\""));
    assert!(geofence_count.contains("\"process\".\"geofence_id\" AS \"id\""));
    assert!(geofence_count.contains("ON \"related\".\"id\" = \"source\".\"id\""));
    assert!(!geofence_count.contains("EXISTS"));
}

#[test]
fn successor_position_capture_bypasses_fragment_work_but_keeps_state_and_fingerprint() {
    let temporary = crate::private_tempdir().expect("temporary directory");
    let writer = ProjectionPackWriter::new(temporary.path());
    let car = crate::hub_pack::ProjectionCar {
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
        settings: Default::default(),
    };
    let position = crate::hub_pack::ProjectionPosition {
        id: 30,
        drive_id: Some(20),
        car_id: 1,
        date_ms: 1_700_000_030_000,
        latitude: 51.505,
        longitude: -0.105,
        speed: Some(40),
        power: Some(3.0),
        battery_level: Some(78),
        usable_battery_level: Some(77),
        elevation: Some(25),
        odometer: Some(10_000.5),
        ideal_battery_range_km: Some(390.0),
        est_battery_range_km: Some(385.0),
        rated_battery_range_km: Some(388.0),
        fan_status: None,
        driver_temp_setting: None,
        passenger_temp_setting: None,
        is_climate_on: None,
        is_rear_defroster_on: None,
        is_front_defroster_on: None,
        inside_temp: None,
        outside_temp: None,
        battery_heater: None,
        battery_heater_on: None,
        battery_heater_no_power: None,
        tpms_pressure_fl: None,
        tpms_pressure_fr: None,
        tpms_pressure_rl: None,
        tpms_pressure_rr: None,
    };
    let drive = ProjectionDrive {
        id: 20,
        car_id: 1,
        optimized_at_ms: None,
        start_date_ms: 1_700_000_000_000,
        end_date_ms: 1_700_000_060_000,
        distance_km: Some(12.5),
        duration_min: Some(10),
        efficiency: Some(145.0),
        outside_temp_avg: Some(18.5),
        inside_temp_avg: Some(20.0),
        speed_max: Some(80),
        power_max: Some(36.0),
        power_min: Some(-7.0),
        start_ideal_range_km: Some(390.0),
        end_ideal_range_km: Some(385.0),
        start_address: None,
        end_address: None,
        start_geofence: None,
        end_geofence: None,
        start_latitude: None,
        start_longitude: None,
        end_latitude: None,
        end_longitude: None,
        start_soc: Some(80),
        end_soc: Some(75),
        start_rated_range_km: Some(400.0),
        end_rated_range_km: Some(375.0),
        ascent: Some(60),
        descent: Some(30),
    };
    let mut removed_position = position.clone();
    removed_position.id += 1;
    let state_limits = TeslaMateProjectionStateLimits {
        max_rows: 4,
        max_state_bytes: 128 * 1024,
        max_changed_payload_bytes: 128 * 1024,
        minimum_free_bytes: 0,
    };
    let mut prior_state =
        TeslaMateProjectionState::create(temporary.path(), state_limits).expect("prior state");
    prior_state
        .record_position(&position)
        .expect("current prior position");
    prior_state
        .record_position(&removed_position)
        .expect("removed prior position");
    prior_state.seal().expect("seal prior state");
    let prior_rows = prior_state.page(None, 10).expect("prior rows").rows;
    drop(prior_state);
    let state =
        TeslaMateProjectionState::create(temporary.path(), state_limits).expect("successor state");
    let mut sink = PackSink::new_with_schema_2_1(
        &writer,
        ProjectionBinding {
            installation_id: Uuid::from_u128(1),
            account_id: Uuid::from_u128(2),
            vehicle_id: Uuid::from_u128(3),
            generation: 1,
            selected_car_id: 1,
        },
        Uuid::from_u128(47),
        SequenceRange {
            from_exclusive: 1,
            to_inclusive: 2,
        },
        Vec::new(),
        true,
    )
    .capture_state_only()
    .with_projection_state_capture(TeslaMateProjectionStateCapture::for_successor(
        state,
        Box::new(DirectTestPrior { rows: prior_rows }),
    ));
    let mut fragments =
        direct_position_fragment_accumulator(&car, TeslaMateFragmentLimits::default(), &sink)
            .expect("position capture");
    assert!(
        fragments.is_none(),
        "state-only successors must not create a position fragment accumulator"
    );
    let mut logical_fingerprint = DirectProjectionFingerprint::new();
    let mut expected_fingerprint = DirectProjectionFingerprint::new();
    expected_fingerprint
        .record(DirectProjectionFingerprintFact::Position, &position)
        .expect("expected fingerprint");
    let mut report = ProjectionReport::default();

    append_direct_position(
        position.clone(),
        Some(&drive),
        &mut fragments,
        &mut sink,
        &mut logical_fingerprint,
        &mut report,
    )
    .expect("capture state-only position");

    assert_eq!(report.projected_positions, 1);
    assert_eq!(
        logical_fingerprint.finish(),
        expected_fingerprint.finish(),
        "state-only capture keeps the exact logical position fingerprint"
    );
    assert!(
        !sink.has_written_fragments(),
        "direct state capture must not submit even a disposable fragment"
    );
    let (chunks, capture, selected_car) = sink.into_parts();
    assert!(chunks.is_empty());
    assert!(
        selected_car.is_none(),
        "position-only state capture must not clone a car through a snapshot"
    );
    let mut capture = capture.expect("successor capture");
    capture.seal().expect("seal successor capture");
    let current = capture.page(None, 10).expect("current page");
    assert_eq!(current.rows.len(), 1);
    assert_eq!(current.rows[0].id, position.id);
    let changed = capture.changed_page(None, 10).expect("changed page");
    assert!(
        changed.rows.is_empty(),
        "the prior digest still suppresses an unchanged position payload"
    );
    let (tombstones, next_after) = capture.tombstone_page(None, 10).expect("tombstone page");
    assert!(next_after.is_none());
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].id, removed_position.id);
    assert_eq!(
        tombstones[0].entity,
        crate::hub_pack::ProjectionDeltaEntity::Position
    );
}

#[test]
fn direct_retention_admission_admits_the_canonical_large_position_shape() {
    let admitted = admit_direct_retention(
        TeslaMateSourceCounts {
            cars: 1,
            drives: 3_151,
            positions: 10_402_457,
            charging_processes: 770,
            charges: 292_548,
            states: 5_164,
            addresses: 942,
            geofences: 19_038,
            updates: 39,
        },
        TeslaMateReadLimits::default(),
    )
    .expect("canonical 10M-position source remains direct-memory admissible");

    assert_eq!(admitted.related_position_cache_ids, 7_072);
    assert_eq!(admitted.retained_row_units, 93_662);
    assert!(admitted.retained_row_units < DIRECT_MAX_RETAINED_ROW_UNITS);
}

#[test]
fn direct_retention_admission_rejects_each_history_sized_retained_relation() {
    let cases = [
        (
            "addresses",
            TeslaMateSourceCounts {
                addresses: DIRECT_MAX_RETAINED_ADDRESS_ROWS + 1,
                ..direct_retention_test_counts()
            },
        ),
        (
            "geofences",
            TeslaMateSourceCounts {
                geofences: DIRECT_MAX_RETAINED_GEOFENCE_ROWS + 1,
                ..direct_retention_test_counts()
            },
        ),
        (
            "states",
            TeslaMateSourceCounts {
                states: DIRECT_MAX_RETAINED_STATE_ROWS + 1,
                ..direct_retention_test_counts()
            },
        ),
        (
            "drives",
            TeslaMateSourceCounts {
                drives: DIRECT_MAX_RETAINED_DRIVE_ROWS + 1,
                ..direct_retention_test_counts()
            },
        ),
        (
            "charging_processes",
            TeslaMateSourceCounts {
                charging_processes: DIRECT_MAX_RETAINED_CHARGING_PROCESS_ROWS + 1,
                ..direct_retention_test_counts()
            },
        ),
        (
            "updates_schema_2_2",
            TeslaMateSourceCounts {
                updates: DIRECT_MAX_RETAINED_SCHEMA_22_UPDATE_ROWS + 1,
                ..direct_retention_test_counts()
            },
        ),
        (
            "related_positions",
            TeslaMateSourceCounts {
                drives: DIRECT_MAX_RETAINED_DRIVE_ROWS,
                charging_processes: 1,
                ..direct_retention_test_counts()
            },
        ),
    ];

    for (table, counts) in cases {
        let error = admit_direct_retention(counts, TeslaMateReadLimits::default())
            .expect_err("retained relation must be admitted before collection allocation");
        assert!(matches!(
            error,
            TeslaMateDirectError::DirectRetainedTableLimitExceeded {
                table: actual_table,
                ..
            } if actual_table == table
        ));
    }
}

#[test]
fn direct_retention_admission_has_an_aggregate_cap_and_bounds_schema_22_updates() {
    let aggregate = TeslaMateSourceCounts {
        addresses: DIRECT_MAX_RETAINED_ADDRESS_ROWS,
        geofences: DIRECT_MAX_RETAINED_GEOFENCE_ROWS,
        states: DIRECT_MAX_RETAINED_STATE_ROWS,
        drives: 16_384,
        charging_processes: DIRECT_MAX_RETAINED_CHARGING_PROCESS_ROWS,
        ..direct_retention_test_counts()
    };
    let error = admit_direct_retention(aggregate, TeslaMateReadLimits::default())
        .expect_err("separate retained caps must not add up to an unbounded heap");
    assert!(matches!(
        error,
        TeslaMateDirectError::DirectRetainedAggregateLimitExceeded {
            requested: 622_593,
            maximum: DIRECT_MAX_RETAINED_ROW_UNITS,
        }
    ));

    let update_heavy = TeslaMateSourceCounts {
        updates: 100_000,
        ..direct_retention_test_counts()
    };
    assert!(matches!(
        admit_direct_retention(update_heavy, TeslaMateReadLimits::default()),
        Err(TeslaMateDirectError::DirectRetainedTableLimitExceeded {
            table: "updates_schema_2_2",
            requested: 100_000,
            maximum: DIRECT_MAX_RETAINED_SCHEMA_22_UPDATE_ROWS,
        })
    ));

    let source_ceiling = TeslaMateSourceCounts {
        positions: u64::try_from(TeslaMateReadLimits::default().maximum_rows)
            .expect("configured source ceiling fits u64"),
        updates: 1,
        ..direct_retention_test_counts()
    };
    assert!(matches!(
        admit_direct_retention(source_ceiling, TeslaMateReadLimits::default()),
        Err(TeslaMateDirectError::MaximumRowsExceeded {
            maximum: 20_000_000,
        })
    ));
}

#[test]
fn bounded_direct_maps_reject_growth_and_duplicate_ids() {
    let mut rows = HashMap::new();
    insert_direct_bounded_map("projected_drives", &mut rows, 1, "one", 1)
        .expect("first bounded row");
    assert!(matches!(
        insert_direct_bounded_map("projected_drives", &mut rows, 2, "two", 1),
        Err(TeslaMateDirectError::DirectRetainedTableLimitExceeded {
            table: "projected_drives",
            requested: 2,
            maximum: 1,
        })
    ));
    assert!(matches!(
        insert_direct_bounded_map("projected_drives", &mut rows, 1, "duplicate", 1),
        Err(TeslaMateDirectError::DuplicateDirectRetainedId {
            table: "projected_drives",
            id: 1,
        })
    ));
}

#[test]
fn direct_count_gate_accepts_named_projection_and_skip_reasons() {
    let report = ProjectionReport {
        completed_drives: 1,
        skipped_open_drives: 2,
        skipped_unattached_positions: 3,
        projected_positions: 4,
        projected_charges: 5,
        projected_charge_samples: 6,
        projected_states: 7,
        projected_updates: 8,
        skipped_incomplete_updates: 9,
    };
    assert!(
        validate_direct_source_counts(
            TeslaMateSourceCounts {
                cars: 1,
                drives: 3,
                positions: 7,
                charging_processes: 5,
                charges: 6,
                states: 7,
                addresses: 0,
                geofences: 0,
                updates: 17,
            },
            report,
            17,
        )
        .is_ok()
    );
}

#[test]
fn direct_count_gate_rejects_unexplained_loss() {
    let error = validate_direct_source_counts(
        TeslaMateSourceCounts {
            cars: 1,
            drives: 0,
            positions: 1,
            charging_processes: 0,
            charges: 0,
            states: 0,
            addresses: 0,
            geofences: 0,
            updates: 0,
        },
        ProjectionReport::default(),
        0,
    )
    .expect_err("position must be accounted for");
    assert!(matches!(
        error,
        TeslaMateDirectError::UnexplainedSourceRows {
            table: "positions",
            source_rows: 1,
            accounted: 0,
        }
    ));
}

#[test]
fn direct_metadata_and_update_reconciliation_rejects_count_drift() {
    for table in ["addresses", "geofences", "updates"] {
        assert!(matches!(
            validate_direct_count(table, 1, 0),
            Err(TeslaMateDirectError::UnexplainedSourceRows {
                table: actual_table,
                source_rows: 1,
                accounted: 0,
            }) if actual_table == table
        ));
    }
}

#[test]
fn direct_count_gate_rejects_dropped_update_history() {
    let error = validate_direct_source_counts(
        TeslaMateSourceCounts {
            cars: 1,
            drives: 0,
            positions: 0,
            charging_processes: 0,
            charges: 0,
            states: 0,
            addresses: 0,
            geofences: 0,
            updates: 1,
        },
        ProjectionReport::default(),
        0,
    )
    .expect_err("a direct source update cannot disappear before publication");
    assert!(matches!(
        error,
        TeslaMateDirectError::UnexplainedSourceRows {
            table: "updates",
            source_rows: 1,
            accounted: 0,
        }
    ));
}

#[test]
fn direct_count_gate_rejects_missing_or_extra_state_rows() {
    let missing = validate_direct_source_counts(
        TeslaMateSourceCounts {
            cars: 1,
            drives: 0,
            positions: 0,
            charging_processes: 0,
            charges: 0,
            states: 1,
            addresses: 0,
            geofences: 0,
            updates: 0,
        },
        ProjectionReport::default(),
        0,
    )
    .expect_err("a source state cannot disappear from a direct snapshot");
    assert!(matches!(
        missing,
        TeslaMateDirectError::UnexplainedSourceRows {
            table: "states",
            source_rows: 1,
            accounted: 0,
        }
    ));

    let extra = validate_direct_source_counts(
        TeslaMateSourceCounts {
            cars: 1,
            drives: 0,
            positions: 0,
            charging_processes: 0,
            charges: 0,
            states: 0,
            addresses: 0,
            geofences: 0,
            updates: 0,
        },
        ProjectionReport {
            projected_states: 1,
            ..ProjectionReport::default()
        },
        0,
    )
    .expect_err("a direct snapshot cannot invent a state row");
    assert!(matches!(
        extra,
        TeslaMateDirectError::UnexplainedSourceRows {
            table: "states",
            source_rows: 0,
            accounted: 1,
        }
    ));
}

fn fingerprint_test_car() -> crate::hub_pack::ProjectionCar {
    crate::hub_pack::ProjectionCar {
        id: 1,
        name: "Fingerprint test car".to_owned(),
        model: "3".to_owned(),
        vin: Some("5YJ3E1EA7KF000001".to_owned()),
        source_eid: Some(1),
        source_vid: Some(2),
        trim_badging: None,
        marketing_name: None,
        exterior_color: None,
        wheel_type: None,
        spoiler_type: None,
        firmware_version: Some("2026.1".to_owned()),
        efficiency_wh_per_km: Some(150.0),
        settings: crate::hub_pack::ProjectionCarSettings::default(),
    }
}

fn fingerprint_test_charge(id: i64, text: &str) -> ProjectionCharge {
    ProjectionCharge {
        id,
        car_id: 1,
        start_date_ms: 1_700_000_000_000 + id,
        end_date_ms: Some(1_700_000_000_001 + id),
        charge_energy_added: Some(1.0),
        charge_energy_used_kwh: Some(1.0),
        start_ideal_range_km: Some(100.0),
        end_ideal_range_km: Some(101.0),
        cost: None,
        fast_charger_type: Some(text.to_owned()),
        billing_type: None,
        cost_per_unit: None,
        session_fee: None,
        start_latitude: Some(51.5),
        start_longitude: Some(-0.1),
        start_battery_level: Some(50),
        end_battery_level: Some(51),
        duration_min: Some(1),
        address: Some(text.to_owned()),
        location_name: Some(text.to_owned()),
        geofence: Some(text.to_owned()),
        is_dc: Some(false),
        charge_rate_km_per_hour: Some(1.0),
        max_charger_power_kw: Some(1.0),
        outside_temp_avg: Some(20.0),
        start_rated_range_km: Some(100.0),
        end_rated_range_km: Some(101.0),
    }
}

/// Mirrors the empty-charge fragment layout in `write_charges` without
/// building packs. The returned count proves the exact limits chose a
/// different physical layout; it is intentionally not an input to the
/// direct logical digest.
fn logical_fingerprint_for_empty_charge_fragments(
    limits: TeslaMateFragmentLimits,
) -> (Sha256Digest, u64) {
    const CHARGE_COUNT: i64 = 300;

    let car = fingerprint_test_car();
    let state = crate::hub_pack::ProjectionState {
        id: 1,
        car_id: 1,
        state: "online".to_owned(),
        start_date_ms: 1_700_000_000_000,
        end_date_ms: None,
    };
    let mut fingerprint = DirectProjectionFingerprint::new();
    fingerprint
        .record(DirectProjectionFingerprintFact::Car, &car)
        .expect("serialize test car");
    fingerprint
        .record(DirectProjectionFingerprintFact::State, &state)
        .expect("serialize test state");

    let car_bytes = serialized_bytes(&car).expect("size test car");
    let mut fragment_rows = 1_u64;
    let mut fragment_bytes = car_bytes;
    let mut fragments = 0_u64;
    let text = "x".repeat(crate::hub_pack::MAX_TEXT_BYTES);
    for id in 1..=CHARGE_COUNT {
        let charge = fingerprint_test_charge(id, &text);
        let charge_bytes = serialized_bytes(&charge).expect("size test charge");
        let would_exceed = fragment_rows.checked_add(1).expect("test row count")
            > limits.max_rows_per_fragment
            || fragment_bytes
                .checked_add(charge_bytes)
                .expect("test byte count")
                > limits.max_projected_json_bytes;
        if would_exceed && fragment_rows > 1 {
            fragments = fragments.checked_add(1).expect("test fragment count");
            fragment_rows = 1;
            fragment_bytes = car_bytes;
        }
        assert!(
            fragment_rows.checked_add(1).expect("test row count") <= limits.max_rows_per_fragment
                && fragment_bytes
                    .checked_add(charge_bytes)
                    .expect("test byte count")
                    <= limits.max_projected_json_bytes,
            "one test charge fits after a fragment reset"
        );
        fragment_rows = fragment_rows.checked_add(1).expect("test row count");
        fragment_bytes = fragment_bytes
            .checked_add(charge_bytes)
            .expect("test byte count");
        fingerprint
            .record(DirectProjectionFingerprintFact::Charge, &charge)
            .expect("serialize test charge");
    }
    if fragment_rows > 1 {
        fragments = fragments.checked_add(1).expect("test fragment count");
    }

    (fingerprint.finish(), fragments)
}

#[test]
fn direct_logical_fingerprint_binds_fact_type_and_order() {
    let mut state_fact = DirectProjectionFingerprint::new();
    state_fact
        .record(DirectProjectionFingerprintFact::State, &"same payload")
        .expect("record state fact");
    let mut charge_fact = DirectProjectionFingerprint::new();
    charge_fact
        .record(DirectProjectionFingerprintFact::Charge, &"same payload")
        .expect("record charge fact");
    assert_ne!(
        state_fact.finish(),
        charge_fact.finish(),
        "entity tags keep equal JSON payloads typed"
    );

    let mut source_order = DirectProjectionFingerprint::new();
    source_order
        .record(DirectProjectionFingerprintFact::Charge, &"first")
        .expect("record first fact");
    source_order
        .record(DirectProjectionFingerprintFact::Charge, &"second")
        .expect("record second fact");
    let mut reversed_order = DirectProjectionFingerprint::new();
    reversed_order
        .record(DirectProjectionFingerprintFact::Charge, &"second")
        .expect("record second fact");
    reversed_order
        .record(DirectProjectionFingerprintFact::Charge, &"first")
        .expect("record first fact");
    assert_ne!(
        source_order.finish(),
        reversed_order.finish(),
        "logical source order participates in duplicate suppression"
    );
}

#[test]
fn direct_logical_fingerprint_binds_preprojection_source_evidence() {
    let first_evidence = Sha256Digest::of_bytes(b"drive endpoints first");
    let second_evidence = Sha256Digest::of_bytes(b"drive endpoints second");
    let mut first = DirectProjectionFingerprint::new();
    first.bind_source_evidence(&first_evidence);
    let mut repeated = DirectProjectionFingerprint::new();
    repeated.bind_source_evidence(&first_evidence);
    let mut changed = DirectProjectionFingerprint::new();
    changed.bind_source_evidence(&second_evidence);

    let first = first.finish();
    let repeated = repeated.finish();
    let changed = changed.finish();
    assert_eq!(first, repeated);
    assert_ne!(
        repeated, changed,
        "a source-only fact change must invalidate direct duplicate suppression"
    );
}

#[test]
fn direct_logical_fingerprint_ignores_fragment_limit_retry_layout() {
    let default_limits = TeslaMateFragmentLimits::default();
    let retry_limits = next_fragment_limits(default_limits)
        .expect("the default direct target has the dense retry target");
    assert_eq!(retry_limits, DENSE_DIRECT_FRAGMENT_LIMITS);

    let (default_fingerprint, default_fragments) =
        logical_fingerprint_for_empty_charge_fragments(default_limits);
    let (retry_fingerprint, retry_fragments) =
        logical_fingerprint_for_empty_charge_fragments(retry_limits);

    assert_ne!(
        default_fragments, retry_fragments,
        "the fixture must exercise distinct 50k/8MiB and 100k/16MiB layouts"
    );
    assert_eq!(
        default_fingerprint, retry_fingerprint,
        "one projected history must keep its duplicate fingerprint across a retry"
    );
}

#[test]
fn direct_fragment_target_keeps_the_default_below_the_dense_boundary() {
    let limits = initial_direct_fragment_limits(TeslaMateSourceCounts {
        cars: 1,
        drives: 1,
        positions: DENSE_DIRECT_POSITION_THRESHOLD - 1,
        charging_processes: 1,
        charges: 1,
        states: 1,
        addresses: 0,
        geofences: 0,
        updates: 0,
    });

    assert_eq!(limits, TeslaMateFragmentLimits::default());
}

#[test]
fn direct_fragment_target_selects_dense_boundary_and_retries_upward() {
    let selected = initial_direct_fragment_limits(TeslaMateSourceCounts {
        cars: 1,
        drives: 3_151,
        positions: DENSE_DIRECT_POSITION_THRESHOLD,
        charging_processes: 770,
        charges: 292_548,
        states: 1,
        addresses: 0,
        geofences: 0,
        updates: 0,
    });
    let protocol = crate::protocol::ProtocolLimits::default();

    assert_eq!(selected, DENSE_DIRECT_FRAGMENT_LIMITS);
    assert!(selected.max_rows_per_fragment <= protocol.max_rows_per_pack);
    assert!(selected.max_projected_json_bytes <= protocol.max_uncompressed_pack_bytes);

    let retry = next_fragment_limits(selected).expect("dense target has a bounded retry");
    assert_eq!(
        retry,
        TeslaMateFragmentLimits {
            max_rows_per_fragment: 200_000,
            max_projected_json_bytes: 32 * 1024 * 1024,
        }
    );
    assert!(retry.max_rows_per_fragment <= protocol.max_rows_per_pack);
    assert!(retry.max_projected_json_bytes <= protocol.max_uncompressed_pack_bytes);
}

#[test]
fn incremental_capture_capacity_rejects_overflow_before_filesystem_access() {
    let temporary = tempfile::tempdir().expect("pack directory");
    let error =
        ensure_direct_capture_capacity(&ProjectionPackWriter::new(temporary.path()), u64::MAX)
            .expect_err("reserve addition must not wrap");
    assert!(matches!(
        error,
        TeslaMateDirectError::Pack(ProjectionPackError::CapacityOverflow)
    ));
}

#[test]
fn projection_state_admission_uses_exact_rows_and_bounds_successor_payload() {
    let counts = TeslaMateSourceCounts {
        cars: 1,
        drives: 3_200,
        positions: 10_782_430,
        charging_processes: 800,
        charges: 292_600,
        states: 1,
        addresses: 0,
        geofences: 0,
        updates: 32,
    };
    let limits = TeslaMateReadLimits::default();
    let initial = direct_projection_state_limits(counts, limits, DirectCaptureMode::PublishPacks)
        .expect("measured-size initial spool");
    let successor =
        direct_projection_state_limits(counts, limits, DirectCaptureMode::SuccessorDiff)
            .expect("measured-size successor spool");

    assert_eq!(initial.max_rows, 11_079_064);
    assert!(initial.max_state_bytes < 2 * 1024 * 1024 * 1024);
    assert_eq!(initial.max_changed_payload_bytes, 1);
    assert!(successor.max_state_bytes < 3 * 1024 * 1024 * 1024);
    assert_eq!(
        successor.max_changed_payload_bytes,
        DIRECT_SUCCESSOR_CHANGED_PAYLOAD_BYTES
    );
    assert!(successor.max_state_bytes > initial.max_state_bytes);
    let projected_output =
        direct_projected_output_estimate(counts).expect("selected-car output estimate");
    assert!(projected_output > 1024 * 1024 * 1024);
    assert!(projected_output < 2 * 1024 * 1024 * 1024);
}

#[test]
fn projection_state_admission_fails_before_capture_when_exact_rows_do_not_fit() {
    let error = direct_projection_state_limits(
        TeslaMateSourceCounts {
            cars: 1,
            drives: 0,
            positions: 1_000,
            charging_processes: 0,
            charges: 0,
            states: 0,
            addresses: 0,
            geofences: 0,
            updates: 0,
        },
        TeslaMateReadLimits {
            maximum_stage_bytes: 64 * 1024,
            ..TeslaMateReadLimits::default()
        },
        DirectCaptureMode::PublishPacks,
    )
    .expect_err("exact state estimate exceeds the configured capacity");
    assert!(matches!(
        error,
        TeslaMateDirectError::ProjectionStateCapacityExceeded {
            maximum: 65_536,
            ..
        }
    ));
}

#[test]
fn selected_car_output_admission_rejects_overflow() {
    assert!(matches!(
        direct_projected_output_estimate(TeslaMateSourceCounts {
            positions: u64::MAX,
            ..direct_retention_test_counts()
        }),
        Err(TeslaMateDirectError::TargetCapacityOverflow)
    ));
}

#[tokio::test]
async fn native_complete_corpus_direct_import_projects_every_kind_when_configured() {
    let Some((source, password)) = configured_native_postgres_source(false) else {
        return;
    };
    let packs = tempfile::tempdir().expect("pack directory");
    let result = write_direct_full_snapshot(
        &source,
        &password,
        1,
        TeslaMateReadLimits {
            maximum_rows: 32,
            parallel_copy_lanes: 3,
            ..TeslaMateReadLimits::default()
        },
        &ProjectionPackWriter::new(packs.path()),
        ProjectionBinding {
            installation_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            vehicle_id: Uuid::new_v4(),
            generation: 1,
            selected_car_id: 1,
        },
        Uuid::new_v4(),
        SequenceRange {
            from_exclusive: 0,
            to_inclusive: 1,
        },
    )
    .await
    .expect("complete direct import");

    assert!(!result.chunks.is_empty());
    assert_eq!(result.report.completed_drives, 1);
    assert_eq!(result.report.projected_positions, 2);
    assert_eq!(result.report.skipped_unattached_positions, 1);
    assert_eq!(result.report.projected_charges, 1);
    assert_eq!(result.report.projected_charge_samples, 1);
    assert_eq!(result.report.projected_states, 1);
}

#[tokio::test]
async fn native_ten_million_corpus_direct_import_meets_target_when_enabled() {
    if std::env::var("TESLATLAS_HUB_RUN_10M").as_deref() != Ok("1") {
        return;
    }
    let (source, password) = configured_native_postgres_source(true)
        .expect("10m test source URL or local TeslaMate fixture URL");
    let packs = tempfile::tempdir().expect("pack directory");
    let limits = TeslaMateReadLimits {
        // The canonical source contains 10.4M positions before drives,
        // charges, state rows, and metadata are counted. Keep the direct
        // capture bounded while leaving a proven ceiling for that corpus.
        maximum_rows: 20_000_000,
        parallel_copy_lanes: 3,
        ..TeslaMateReadLimits::default()
    };
    let expected_source_counts =
        native_ten_million_expected_source_counts(&source, &password, limits, packs.path()).await;
    let writer = ProjectionPackWriter::new(packs.path());
    let mut phase_trace = native_ten_million_phase_trace::enabled_from_environment();
    let started = Instant::now();
    let import = tokio::time::timeout(
        Duration::from_secs(600),
        write_direct_full_snapshot_with_projection_state(
            &source,
            &password,
            1,
            limits,
            &writer,
            ProjectionBinding {
                installation_id: Uuid::new_v4(),
                account_id: Uuid::new_v4(),
                vehicle_id: Uuid::new_v4(),
                generation: 1,
                selected_car_id: 1,
            },
            Uuid::new_v4(),
            SequenceRange {
                from_exclusive: 0,
                to_inclusive: 1,
            },
            false,
            TeslaMateMigrationProgressReporter::default(),
            |state_limits| {
                TeslaMateProjectionState::create(packs.path(), state_limits)
                    .map(TeslaMateProjectionStateCapture::for_initial_base)
                    .map_err(Into::into)
            },
        ),
    )
    .await;
    match &import {
        Err(_) => {
            if let Some(trace) = phase_trace.as_mut() {
                trace.report("timeout");
            }
        }
        Ok(Err(_)) => {
            if let Some(trace) = phase_trace.as_mut() {
                trace.report("import_error");
            }
        }
        Ok(Ok(_)) => {}
    }
    let mut result = import
        .expect("ten-million direct import timed out")
        .expect("ten-million direct import")
        .packs;
    if let Some(trace) = phase_trace.as_mut() {
        trace.report("completed");
    }

    assert!(started.elapsed() < Duration::from_secs(600));
    let updates_accounted = result
        .report
        .projected_updates
        .checked_add(result.report.skipped_incomplete_updates)
        .expect("update counts do not overflow");
    validate_direct_source_counts(expected_source_counts, result.report, updates_accounted)
        .expect("direct report must account for every validated source row");
    let state = result
        .projection_state
        .as_mut()
        .expect("direct stateful capture must retain its projection state");
    let state_stats = state.seal().expect("seal direct projection state");
    assert!(state_stats.sealed);
    assert_eq!(state_stats.changed_row_count, 0);
    assert_eq!(state_stats.changed_payload_bytes, 0);
    assert!(
        state_stats.row_count >= result.report.logical_row_count().expect("row count"),
        "state capture must cover every projected typed row"
    );
}
