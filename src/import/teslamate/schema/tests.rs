// SPDX-License-Identifier: AGPL-3.0-only

use super::*;

const PINNED_MIGRATION_VERSIONS: [i64; TESLAMATE_V4_MIGRATION_COUNT] = [
    20190330150000,
    20190330160000,
    20190330170000,
    20190330180000,
    20190330190000,
    20190330200000,
    20190408203117,
    20190415103933,
    20190415115227,
    20190415130006,
    20190415130705,
    20190415192200,
    20190416125429,
    20190525125700,
    20190717184003,
    20190729142656,
    20190729181314,
    20190730101523,
    20190731154452,
    20190805092941,
    20190810105216,
    20190810131321,
    20190810151901,
    20190812191616,
    20190813184320,
    20190814152810,
    20190816165713,
    20190816200723,
    20190821143938,
    20190821155748,
    20190823173437,
    20190826142828,
    20190828094708,
    20190828104902,
    20190828122529,
    20190828150058,
    20190903151524,
    20190913165850,
    20190913175011,
    20190913175543,
    20190925152807,
    20190925161034,
    20190925182253,
    20190928155641,
    20191003130650,
    20191003132415,
    20191007105010,
    20191008191431,
    20191017003836,
    20191020130234,
    20191026144449,
    20191026145925,
    20191026185642,
    20191117042320,
    20191117143038,
    20191117171307,
    20191119162847,
    20191212215130,
    20191212230527,
    20200103073606,
    20200116190926,
    20200120130125,
    20200120142602,
    20200203120311,
    20200203180529,
    20200212001245,
    20200216121330,
    20200302100654,
    20200306130218,
    20200306133847,
    20200318164021,
    20200320140020,
    20200401170940,
    20200401171402,
    20200401171923,
    20200410112005,
    20200502140646,
    20200528163852,
    20200528173223,
    20200528175158,
    20200709165119,
    20210130174838,
    20210812173700,
    20210831153305,
    20211022103654,
    20220123131732,
    20220422132017,
    20220617170400,
    20220718085412,
    20230417225712,
    20240603152807,
    20240627021414,
    20240915193446,
    20240929084639,
    20250407155134,
    20250613133700,
    20250924215353,
    20251207212310,
    20251225150000,
    20260411070212,
    20260715081000,
    20260716110000,
    20260718160000,
    20260807090000,
    20260808090000,
];

fn complete_schema() -> Vec<ObservedColumn<'static>> {
    let mut result = Vec::new();
    for table in SourceTable::ALL {
        for column in projection(table).columns {
            result.push(ObservedColumn {
                table: column.source_table.unwrap_or(table).name(),
                name: column.source_name,
                type_name: column.value_type.canonical_udt(),
                format_type: column.value_type.canonical_udt(),
                nullable: column.nullable,
            });
        }
    }
    for table in PINNED_SOURCE_TABLES {
        for column in table.columns {
            if let Some(existing) = result.iter_mut().find(|existing| {
                existing.table == table.table.name() && existing.name == column.name
            }) {
                existing.type_name = column.value_type.canonical_udt();
                existing.format_type = column.format_type;
                existing.nullable = column.nullable;
            } else {
                result.push(ObservedColumn {
                    table: table.table.name(),
                    name: column.name,
                    type_name: column.value_type.canonical_udt(),
                    format_type: column.format_type,
                    nullable: column.nullable,
                });
            }
        }
    }
    result
}

#[test]
fn all_queries_are_fixed_qualified_and_keyset_paginated() {
    assert!(MIGRATION_VERSION_SQL.contains("\"public\".\"schema_migrations\""));
    assert!(MIGRATION_VERSIONS_SQL.contains("\"public\".\"schema_migrations\""));
    assert!(MIGRATION_VERSIONS_SQL.contains("ORDER BY \"migration\".\"version\" ASC"));
    assert!(SCHEMA_PROBE_SQL.contains("\"pg_catalog\".\"pg_class\""));
    assert!(SCHEMA_PROBE_SQL.contains("\"pg_catalog\".\"pg_attribute\""));
    assert!(SCHEMA_PROBE_SQL.contains("'settings'"));
    assert!(SCHEMA_PROBE_SQL.contains("\"format_type\""));
    assert!(ENUM_PROBE_SQL.contains("\"pg_catalog\".\"pg_enum\""));
    assert!(ENUM_PROBE_SQL.contains("'billing_type'"));
    assert!(SETTINGS_RELATIONSHIP_SQL.contains("\"public\".\"settings\""));

    for table in SourceTable::ALL {
        let descriptor = projection(table);
        assert!(descriptor.sql.contains(&format!(
            "FROM \"public\".\"{}\" AS \"source\"",
            table.name()
        )));
        assert!(descriptor.sql.contains("WHERE \"source\".\"id\" > $1"));
        assert!(descriptor.sql.contains("$3"));
        assert!(descriptor.sql.contains("ORDER BY \"source\".\"id\" ASC"));
        assert!(descriptor.sql.contains("LIMIT $2"));
        assert!(!descriptor.sql.contains("SELECT *"));
        assert!(!descriptor.sql.contains("postgres://"));

        for column in descriptor.columns {
            let source_alias = if column.source_table == Some(SourceTable::CarSettings) {
                "settings"
            } else {
                "source"
            };
            let expected_expression = if column.source_table == Some(SourceTable::CarSettings)
                && column.value_type == ValueType::SmallInt
            {
                format!(
                    "\"{}\".\"{}\"::integer AS \"{}\"",
                    source_alias, column.source_name, column.output_name
                )
            } else if table == SourceTable::Positions {
                let cast_type = if column.source_name == "power" {
                    Some("double precision")
                } else {
                    match column.value_type {
                        ValueType::Integer => {
                            if table == SourceTable::Positions
                                && matches!(column.source_name, "drive_id" | "fan_status")
                            {
                                Some("bigint")
                            } else {
                                Some("integer")
                            }
                        }
                        ValueType::SmallInt => {
                            if table == SourceTable::Positions
                                && matches!(
                                    column.source_name,
                                    "elevation"
                                        | "speed"
                                        | "battery_level"
                                        | "usable_battery_level"
                                )
                            {
                                Some("bigint")
                            } else {
                                Some("smallint")
                            }
                        }
                        ValueType::Numeric => Some("numeric"),
                        ValueType::Floating => Some("double precision"),
                        _ => None,
                    }
                };
                if let Some(cast_type) = cast_type {
                    format!(
                        "\"{}\".\"{}\"::{} AS \"{}\"",
                        source_alias, column.source_name, cast_type, column.output_name
                    )
                } else {
                    format!(
                        "\"{}\".\"{}\" AS \"{}\"",
                        source_alias, column.source_name, column.output_name
                    )
                }
            } else {
                format!(
                    "\"{}\".\"{}\" AS \"{}\"",
                    source_alias, column.source_name, column.output_name
                )
            };
            assert!(descriptor.sql.contains(&expected_expression));
        }
    }
}

#[test]
fn projections_cover_the_current_telemetry_tables() {
    let names: Vec<_> = SourceTable::ALL.iter().map(|table| table.name()).collect();
    assert_eq!(
        names,
        [
            "cars",
            "drives",
            "positions",
            "charging_processes",
            "charges",
            "addresses",
            "geofences",
            "states",
            "updates",
        ]
    );
    assert_eq!(projection(SourceTable::Cars).columns.len(), 22);
    assert_eq!(projection(SourceTable::Drives).columns.len(), 25);
    assert_eq!(projection(SourceTable::Positions).columns.len(), 30);
    assert_eq!(projection(SourceTable::ChargingProcesses).columns.len(), 18);
    assert_eq!(projection(SourceTable::Charges).columns.len(), 22);
    assert_eq!(projection(SourceTable::Addresses).columns.len(), 3);
    assert_eq!(projection(SourceTable::Geofences).columns.len(), 2);
    assert_eq!(projection(SourceTable::States).columns.len(), 5);
    assert_eq!(projection(SourceTable::Updates).columns.len(), 5);
}

#[test]
fn reviewed_migration_pin_requires_the_exact_upstream_set() {
    assert_eq!(
        validate_migration_versions(&PINNED_MIGRATION_VERSIONS),
        Ok(MAX_VALIDATED_MIGRATION)
    );
    assert_eq!(TESLAMATE_V4_SOURCE_REVISION.len(), 40);
    assert_eq!(
        TESLAMATE_V4_SOURCE_REVISION,
        "e8d24886f97f22469c2675f89be843f6d401c76a"
    );
    assert!(is_supported_teslamate_source_revision(
        TESLAMATE_V4_SOURCE_REVISION
    ));
    assert!(is_supported_teslamate_source_revision(
        TESLAMATE_V4_1_1_SOURCE_REVISION
    ));
    assert!(!is_supported_teslamate_source_revision(
        "0000000000000000000000000000000000000000"
    ));
    assert!(matches!(
        validate_migration_versions(
            &PINNED_MIGRATION_VERSIONS[..PINNED_MIGRATION_VERSIONS.len() - 1]
        ),
        Err(SchemaCompatibilityError::LegacyMigration { .. })
    ));
    let mut future = PINNED_MIGRATION_VERSIONS.to_vec();
    future.push(MAX_VALIDATED_MIGRATION + 1);
    assert!(matches!(
        validate_migration_versions(&future),
        Err(SchemaCompatibilityError::UnreviewedMigration { .. })
    ));
    assert_eq!(
        validate_migration_versions(&PINNED_MIGRATION_VERSIONS[1..]),
        Err(SchemaCompatibilityError::MigrationSetMismatch)
    );
    let mut duplicate = PINNED_MIGRATION_VERSIONS.to_vec();
    duplicate[10] = duplicate[9];
    assert_eq!(
        validate_migration_versions(&duplicate),
        Err(SchemaCompatibilityError::InvalidMigrationSet)
    );
    let mut substituted = PINNED_MIGRATION_VERSIONS.to_vec();
    substituted[10] += 1;
    assert_eq!(
        validate_migration_versions(&substituted),
        Err(SchemaCompatibilityError::MigrationSetMismatch)
    );
}

#[test]
fn valid_current_schema_is_accepted() {
    assert_eq!(validate_observed_schema(&complete_schema()), Ok(()));
}

#[test]
fn every_pinned_physical_column_rejects_an_incompatible_type_name() {
    let mut tested = 0;
    for table in PINNED_SOURCE_TABLES {
        for expected in table.columns {
            let mut incompatible = complete_schema();
            incompatible
                .iter_mut()
                .find(|column| column.table == table.table.name() && column.name == expected.name)
                .expect("complete physical field")
                .type_name = "bytea";
            assert!(matches!(
                validate_observed_schema(&incompatible),
                Err(SchemaCompatibilityError::IncompatibleColumnType {
                    table: actual_table,
                    column: actual_column,
                    ..
                }) if actual_table == table.table.name() && actual_column == expected.name
            ));
            tested += 1;
        }
    }
    assert_eq!(tested, 169);
}

#[test]
fn telemetry_timestamps_retain_the_untyped_source_format() {
    let timestamps: Vec<_> = PINNED_SOURCE_TABLES
        .iter()
        .flat_map(|table| {
            table.columns.iter().filter_map(move |column| {
                (column.format_type == "timestamp without time zone")
                    .then_some((table.table.name(), column.name))
            })
        })
        .collect();
    assert_eq!(
        timestamps,
        [
            ("drives", "start_date"),
            ("drives", "end_date"),
            ("positions", "date"),
            ("charging_processes", "start_date"),
            ("charging_processes", "end_date"),
            ("charges", "date"),
            ("states", "start_date"),
            ("states", "end_date"),
            ("updates", "start_date"),
            ("updates", "end_date"),
        ]
    );
}

fn complete_enums() -> Vec<ObservedEnumLabel<'static>> {
    PINNED_ENUM_LABELS
        .iter()
        .flat_map(|(type_name, labels)| {
            labels
                .iter()
                .map(move |label| ObservedEnumLabel { type_name, label })
        })
        .collect()
}

#[test]
fn pinned_settings_and_car_settings_physical_contract_is_enforced() {
    let mut wrong_nullability = complete_schema();
    wrong_nullability
        .iter_mut()
        .find(|column| column.table == "settings" && column.name == "language")
        .expect("settings.language")
        .nullable = true;
    assert_eq!(
        validate_observed_schema(&wrong_nullability),
        Err(SchemaCompatibilityError::IncompatibleColumnNullability {
            table: "settings",
            column: "language",
        })
    );

    let mut wrong_format = complete_schema();
    wrong_format
        .iter_mut()
        .find(|column| column.table == "settings" && column.name == "base_url")
        .expect("settings.base_url")
        .format_type = "text";
    assert_eq!(
        validate_observed_schema(&wrong_format),
        Err(SchemaCompatibilityError::IncompatibleColumnFormat {
            table: "settings",
            column: "base_url",
        })
    );

    let mut wrong_settings_id = complete_schema();
    wrong_settings_id
        .iter_mut()
        .find(|column| column.table == "cars" && column.name == "settings_id")
        .expect("cars.settings_id")
        .type_name = "int4";
    assert_eq!(
        validate_observed_schema(&wrong_settings_id),
        Err(SchemaCompatibilityError::IncompatibleColumnType {
            table: "cars",
            column: "settings_id",
            expected: ValueType::BigInt,
        })
    );
}

#[test]
fn selected_car_physical_columns_fail_closed_on_every_contract_dimension() {
    let tables = [
        (SourceTable::Cars, CARS_SOURCE_COLUMNS),
        (SourceTable::Drives, DRIVES_SOURCE_COLUMNS),
        (SourceTable::Positions, POSITIONS_SOURCE_COLUMNS),
        (
            SourceTable::ChargingProcesses,
            CHARGING_PROCESSES_SOURCE_COLUMNS,
        ),
        (SourceTable::Charges, CHARGES_SOURCE_COLUMNS),
        (SourceTable::States, STATES_SOURCE_COLUMNS),
        (SourceTable::Updates, UPDATES_SOURCE_COLUMNS),
    ];

    assert_eq!(CARS_SOURCE_COLUMNS.len(), 16);
    assert_eq!(DRIVES_SOURCE_COLUMNS.len(), 25);
    assert_eq!(POSITIONS_SOURCE_COLUMNS.len(), 30);
    assert_eq!(CHARGING_PROCESSES_SOURCE_COLUMNS.len(), 18);
    assert_eq!(CHARGES_SOURCE_COLUMNS.len(), 22);
    assert_eq!(STATES_SOURCE_COLUMNS.len(), 5);
    assert_eq!(UPDATES_SOURCE_COLUMNS.len(), 5);
    assert_eq!(
        PINNED_SOURCE_TABLES
            .iter()
            .map(|table| table.columns.len())
            .sum::<usize>(),
        169
    );

    for (table, columns) in tables {
        for expected in columns {
            let mut missing = complete_schema();
            missing
                .retain(|column| !(column.table == table.name() && column.name == expected.name));
            assert_eq!(
                validate_observed_schema(&missing),
                Err(SchemaCompatibilityError::MissingColumn {
                    table: table.name(),
                    column: expected.name,
                })
            );

            let mut wrong_format = complete_schema();
            wrong_format
                .iter_mut()
                .find(|column| column.table == table.name() && column.name == expected.name)
                .expect("complete selected-car physical field")
                .format_type = "unreviewed source format";
            assert_eq!(
                validate_observed_schema(&wrong_format),
                Err(SchemaCompatibilityError::IncompatibleColumnFormat {
                    table: table.name(),
                    column: expected.name,
                })
            );

            let mut wrong_nullability = complete_schema();
            wrong_nullability
                .iter_mut()
                .find(|column| column.table == table.name() && column.name == expected.name)
                .expect("complete selected-car physical field")
                .nullable = !expected.nullable;
            assert_eq!(
                validate_observed_schema(&wrong_nullability),
                Err(SchemaCompatibilityError::IncompatibleColumnNullability {
                    table: table.name(),
                    column: expected.name,
                })
            );
        }
    }
}

#[test]
fn complete_address_and_geofence_physical_columns_fail_closed() {
    assert_eq!(ADDRESS_SOURCE_COLUMNS.len(), 19);
    assert_eq!(GEOFENCE_SOURCE_COLUMNS.len(), 10);

    for (table, columns) in [
        (SourceTable::Addresses, ADDRESS_SOURCE_COLUMNS),
        (SourceTable::Geofences, GEOFENCE_SOURCE_COLUMNS),
    ] {
        for required in columns {
            let mut missing = complete_schema();
            missing
                .retain(|column| !(column.table == table.name() && column.name == required.name));
            assert_eq!(
                validate_observed_schema(&missing),
                Err(SchemaCompatibilityError::MissingColumn {
                    table: table.name(),
                    column: required.name,
                })
            );
        }
    }

    let mut wrong_format = complete_schema();
    wrong_format
        .iter_mut()
        .find(|column| column.table == "geofences" && column.name == "cost_per_unit")
        .expect("geofences.cost_per_unit")
        .format_type = "numeric(6,2)";
    assert_eq!(
        validate_observed_schema(&wrong_format),
        Err(SchemaCompatibilityError::IncompatibleColumnFormat {
            table: "geofences",
            column: "cost_per_unit",
        })
    );

    let mut wrong_nullability = complete_schema();
    wrong_nullability
        .iter_mut()
        .find(|column| column.table == "geofences" && column.name == "latitude")
        .expect("geofences.latitude")
        .nullable = true;
    assert_eq!(
        validate_observed_schema(&wrong_nullability),
        Err(SchemaCompatibilityError::IncompatibleColumnNullability {
            table: "geofences",
            column: "latitude",
        })
    );
}

#[test]
fn unprojected_address_and_geofence_physical_columns_fail_closed() {
    assert!(
        ADDRESS_SOURCE_COLUMNS
            .iter()
            .any(|column| column.name == "raw" && column.value_type == ValueType::Jsonb)
    );
    assert!(
        !projection(SourceTable::Addresses)
            .sql
            .contains("\"source\".\"raw\"")
    );

    let mut raw_as_json = complete_schema();
    raw_as_json
        .iter_mut()
        .find(|column| column.table == "addresses" && column.name == "raw")
        .expect("addresses.raw")
        .type_name = "json";
    assert_eq!(
        validate_observed_schema(&raw_as_json),
        Err(SchemaCompatibilityError::IncompatibleColumnType {
            table: "addresses",
            column: "raw",
            expected: ValueType::Jsonb,
        })
    );

    let mut address_coordinate_precision = complete_schema();
    address_coordinate_precision
        .iter_mut()
        .find(|column| column.table == "addresses" && column.name == "longitude")
        .expect("addresses.longitude")
        .format_type = "numeric(8,6)";
    assert_eq!(
        validate_observed_schema(&address_coordinate_precision),
        Err(SchemaCompatibilityError::IncompatibleColumnFormat {
            table: "addresses",
            column: "longitude",
        })
    );

    let mut address_timestamp_nullability = complete_schema();
    address_timestamp_nullability
        .iter_mut()
        .find(|column| column.table == "addresses" && column.name == "updated_at")
        .expect("addresses.updated_at")
        .nullable = true;
    assert_eq!(
        validate_observed_schema(&address_timestamp_nullability),
        Err(SchemaCompatibilityError::IncompatibleColumnNullability {
            table: "addresses",
            column: "updated_at",
        })
    );

    let mut geofence_timestamp_precision = complete_schema();
    geofence_timestamp_precision
        .iter_mut()
        .find(|column| column.table == "geofences" && column.name == "inserted_at")
        .expect("geofences.inserted_at")
        .format_type = "timestamp without time zone";
    assert_eq!(
        validate_observed_schema(&geofence_timestamp_precision),
        Err(SchemaCompatibilityError::IncompatibleColumnFormat {
            table: "geofences",
            column: "inserted_at",
        })
    );
}

#[test]
fn pinned_enums_and_global_settings_relationship_fail_closed() {
    assert_eq!(validate_observed_enums(&complete_enums()), Ok(()));
    let mut states = complete_enums();
    states
        .iter_mut()
        .find(|value| value.type_name == "states_status" && value.label == "offline")
        .expect("offline state")
        .label = "driving";
    assert_eq!(
        validate_observed_enums(&states),
        Err(SchemaCompatibilityError::EnumLabelsMismatch {
            type_name: "states_status",
        })
    );
    let mut billing = complete_enums();
    billing
        .iter_mut()
        .find(|value| value.type_name == "billing_type" && value.label == "per_minute")
        .expect("per-minute billing")
        .label = "per_session";
    assert_eq!(
        validate_observed_enums(&billing),
        Err(SchemaCompatibilityError::EnumLabelsMismatch {
            type_name: "billing_type",
        })
    );
    assert_eq!(validate_settings_relationship(1, 0), Ok(()));
    assert_eq!(
        validate_settings_relationship(2, 0),
        Err(SchemaCompatibilityError::InvalidSettingsRelationship)
    );
    assert_eq!(
        validate_settings_relationship(1, 1),
        Err(SchemaCompatibilityError::InvalidSettingsRelationship)
    );
}

#[test]
fn joined_car_settings_columns_are_required_at_their_source_table() {
    let mut missing_settings_column = complete_schema();
    missing_settings_column
        .retain(|column| !(column.table == "car_settings" && column.name == "suspend_min"));
    assert_eq!(
        validate_observed_schema(&missing_settings_column),
        Err(SchemaCompatibilityError::MissingColumn {
            table: "car_settings",
            column: "suspend_min",
        })
    );
}

#[test]
fn legacy_car_settings_integer_columns_accept_int4_but_not_unrelated_types() {
    let mut legacy_schema = complete_schema();
    for column in &mut legacy_schema {
        if column.table == "car_settings"
            && matches!(column.name, "suspend_min" | "suspend_after_idle_min")
        {
            column.type_name = "int4";
        }
    }
    assert_eq!(validate_observed_schema(&legacy_schema), Ok(()));

    let mut unrelated_type = legacy_schema;
    unrelated_type
        .iter_mut()
        .find(|column| column.table == "car_settings" && column.name == "suspend_min")
        .expect("suspend_min exists")
        .type_name = "int8";
    assert_eq!(
        validate_observed_schema(&unrelated_type),
        Err(SchemaCompatibilityError::IncompatibleColumnType {
            table: "car_settings",
            column: "suspend_min",
            expected: ValueType::SmallInt,
        })
    );
}

#[test]
fn legacy_position_power_is_normalized_to_the_float_decoder_type() {
    let sql = projection(SourceTable::Positions).sql;
    assert!(sql.contains("\"source\".\"power\"::double precision AS \"power\""));
    assert!(sql.contains("\"source\".\"odometer\"::double precision AS \"odometer\""));
    assert!(sql.contains("\"source\".\"latitude\"::numeric AS \"latitude\""));
    assert!(sql.contains("\"source\".\"fan_status\"::bigint AS \"fan_status\""));
    assert!(sql.contains("\"source\".\"drive_id\"::bigint AS \"drive_id\""));
    assert!(sql.contains("\"source\".\"battery_level\"::bigint AS \"battery_level\""));
}

#[test]
fn missing_table_column_and_type_are_rejected() {
    let mut missing_table = complete_schema();
    missing_table.retain(|column| column.table != "positions");
    assert_eq!(
        validate_observed_schema(&missing_table),
        Err(SchemaCompatibilityError::MissingTable { table: "positions" })
    );

    let mut missing_column = complete_schema();
    missing_column.retain(|column| !(column.table == "charges" && column.name == "charger_power"));
    assert_eq!(
        validate_observed_schema(&missing_column),
        Err(SchemaCompatibilityError::MissingColumn {
            table: "charges",
            column: "charger_power",
        })
    );

    let mut wrong_type = complete_schema();
    let latitude = wrong_type
        .iter_mut()
        .find(|column| column.table == "positions" && column.name == "latitude")
        .expect("latitude exists");
    latitude.type_name = "float8";
    assert_eq!(
        validate_observed_schema(&wrong_type),
        Err(SchemaCompatibilityError::IncompatibleColumnType {
            table: "positions",
            column: "latitude",
            expected: ValueType::Numeric,
        })
    );
}

#[test]
fn text_family_accepts_current_text_and_varchar_columns() {
    assert!(ValueType::Text.accepts_udt("text"));
    assert!(ValueType::Text.accepts_udt("varchar"));
    assert!(!ValueType::Text.accepts_udt("int4"));
}
