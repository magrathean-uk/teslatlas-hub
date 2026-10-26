// SPDX-License-Identifier: AGPL-3.0-only

//! Forward-only transition of Hub-owned import names. Historical payloads stay intact.

use super::*;

pub(super) const VERSION: i32 = TERRAIN_SELECTION_SCHEMA_VERSION + 1;
pub(super) const OLD_KIND: &str = "teslamate";
pub(super) const OLD_DEFAULT_KEY: &str = OLD_KIND;
pub(super) const OLD_POSITION_TYPE: &str = "teslamate_position_v1";
pub(super) const OLD_TABLES: [&str; 6] = [
    "teslamate_import_projection_heads",
    "teslamate_import_projection_rows",
    "teslamate_import_projection_state_heads",
    "teslamate_import_projection_state_rows",
    "teslamate_import_projection_state_bridges",
    "teslamate_legacy_tokens",
];

pub(super) const NEW_TABLES: [&str; 6] = [
    crate::db::import_table_names::import_projection_heads_table!(),
    crate::db::import_table_names::import_projection_rows_table!(),
    crate::db::import_table_names::import_projection_state_heads_table!(),
    crate::db::import_table_names::import_projection_state_rows_table!(),
    crate::db::import_table_names::import_projection_state_bridges_table!(),
    crate::db::import_table_names::legacy_owner_tokens_table!(),
];

pub(super) fn migrate(connection: &Connection) -> Result<(), StoreError> {
    #[cfg(test)]
    {
        migrate_impl(connection, false)
    }
    #[cfg(not(test))]
    {
        migrate_impl(connection)
    }
}

#[cfg(test)]
pub(super) fn migrate_with_fault(
    connection: &Connection,
    fault_after_renames: bool,
) -> Result<(), StoreError> {
    migrate_impl(connection, fault_after_renames)
}

fn migrate_impl(
    connection: &Connection,
    #[cfg(test)] fault_after_renames: bool,
) -> Result<(), StoreError> {
    let transaction =
        rusqlite::Transaction::new_unchecked(connection, rusqlite::TransactionBehavior::Immediate)
            .map_err(StoreError::Migrate)?;
    #[cfg(test)]
    apply(&transaction, fault_after_renames)?;
    #[cfg(not(test))]
    apply(&transaction)?;
    transaction.commit().map_err(StoreError::Migrate)
}

fn apply(
    connection: &Connection,
    #[cfg(test)] fault_after_renames: bool,
) -> Result<(), StoreError> {
    let legacy_alter: bool = connection
        .pragma_query_value(None, "legacy_alter_table", |row| row.get(0))
        .map_err(StoreError::Migrate)?;
    if legacy_alter {
        return Err(StoreError::ImportSourceNameMigration(
            "legacy_alter_table must be off before the import-name step",
        ));
    }
    let old_default_present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM source_identities WHERE source_kind=?1 AND source_key=?2)",
            params![OLD_KIND, DEFAULT_IMPORT_SOURCE_KEY],
            |row| row.get(0),
        )
        .map_err(StoreError::Migrate)?;
    let new_kind_present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM source_identities WHERE source_kind=?1)",
            [IMPORT_SOURCE_KIND],
            |row| row.get(0),
        )
        .map_err(StoreError::Migrate)?;
    if old_default_present || new_kind_present {
        return Err(StoreError::ImportSourceNameMigration(
            "the store holds an import source that already uses the key default or a kind this build would create; after the rename a keyless import would match that source. Hub will not choose between sources or merge them. Open it with the previous build, or re-import into a fresh store",
        ));
    }
    for (old, new) in OLD_TABLES.iter().zip(NEW_TABLES) {
        connection
            .execute_batch(&format!("ALTER TABLE {old} RENAME TO {new}"))
            .map_err(StoreError::Migrate)?;
    }
    #[cfg(test)]
    if fault_after_renames {
        return Err(StoreError::ImportSourceNameMigration(
            "injected post-rename failure",
        ));
    }

    // Preserve the actual preceding schema's columns, constraints and type list.
    let current_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name='current_observations'",
            [],
            |row| row.get(0),
        )
        .map_err(StoreError::Migrate)?;
    if current_sql.matches(OLD_POSITION_TYPE).count() != 1 {
        return Err(StoreError::ImportSourceNameMigration(
            "the previous current-row type list is not recognised",
        ));
    }
    let body = current_sql
        .find('(')
        .ok_or(StoreError::ImportSourceNameMigration(
            "the previous current-row schema is not recognised",
        ))?;
    let create_sql = format!(
        "CREATE TABLE current_observations_v78{}",
        &current_sql[body..]
    )
    .replace(
        OLD_POSITION_TYPE,
        crate::collection::source_db_current::SOURCE_DB_POSITION_RECORD_TYPE,
    );
    connection
        .execute_batch(&create_sql)
        .map_err(StoreError::Migrate)?;
    connection
        .execute(
            "INSERT INTO current_observations_v78
         SELECT vehicle_id, CASE record_type WHEN ?1 THEN ?2 ELSE record_type END,
                observation_id, source_id, observed_at_ms, received_at_ms,
                payload_sha256, payload_json FROM current_observations",
            params![
                OLD_POSITION_TYPE,
                crate::collection::source_db_current::SOURCE_DB_POSITION_RECORD_TYPE
            ],
        )
        .map_err(StoreError::Migrate)?;
    let mut after_vehicle: Option<String> = None;
    loop {
        // Drop each read cursor before writes. At most 32 bounded payloads are held.
        let rows = {
            let mut statement = connection
                .prepare(
                    "SELECT vehicle_id, observation_id, payload_json
             FROM current_observations_v78 WHERE record_type=?1
               AND (?2 IS NULL OR vehicle_id>?2) ORDER BY vehicle_id LIMIT 32",
                )
                .map_err(StoreError::Migrate)?;
            let mapped = statement
                .query_map(
                    params![
                        crate::collection::source_db_current::SOURCE_DB_POSITION_RECORD_TYPE,
                        after_vehicle.as_deref()
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(StoreError::Migrate)?;
            mapped
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::Migrate)?
        };
        let Some(last) = rows.last() else {
            break;
        };
        after_vehicle = Some(last.0.clone());
        for row in rows {
            let (vehicle_id, observation_id, original) = row;
            let mut payload: Value =
                serde_json::from_str(&original).map_err(StoreError::SerializeObservation)?;
            if payload.get("record_type").and_then(Value::as_str) != Some(OLD_POSITION_TYPE) {
                return Err(StoreError::ImportSourceNameMigration(
                    "a previous current payload has an unexpected type",
                ));
            }
            payload["record_type"] = Value::String(
                crate::collection::source_db_current::SOURCE_DB_POSITION_RECORD_TYPE.to_owned(),
            );
            let bytes = serde_json::to_vec(&payload).map_err(StoreError::SerializeObservation)?;
            if bytes.len() > MAX_RAW_OBSERVATION_BYTES {
                return Err(StoreError::ObservationTooLarge {
                    actual: bytes.len(),
                    maximum: MAX_RAW_OBSERVATION_BYTES,
                });
            }
            let payload_sha256 = Sha256Digest::of_bytes(&bytes).as_bytes().to_vec();
            let payload_json = String::from_utf8(bytes).expect("writer JSON is UTF-8");
            connection
                .execute(
                    "UPDATE current_observations_v78 SET payload_json=?1, payload_sha256=?2
                 WHERE vehicle_id=?3 AND observation_id=?4 AND record_type=?5",
                    params![
                        payload_json,
                        payload_sha256,
                        vehicle_id,
                        observation_id,
                        crate::collection::source_db_current::SOURCE_DB_POSITION_RECORD_TYPE
                    ],
                )
                .map_err(StoreError::Migrate)?;
        }
    }
    connection
        .execute_batch(
            "DROP TABLE current_observations;
         ALTER TABLE current_observations_v78 RENAME TO current_observations;",
        )
        .map_err(StoreError::Migrate)?;
    connection.execute(
        "UPDATE source_identities SET source_key=CASE source_key WHEN ?1 THEN ?2 ELSE source_key END,
         source_kind=?3 WHERE source_kind=?4",
        params![OLD_DEFAULT_KEY, DEFAULT_IMPORT_SOURCE_KEY, IMPORT_SOURCE_KIND, OLD_KIND],
    ).map_err(StoreError::Migrate)?;
    connection
        .execute(
            "UPDATE sources SET source_kind=?1 WHERE source_kind=?2",
            params![IMPORT_SOURCE_KIND, OLD_KIND],
        )
        .map_err(StoreError::Migrate)?;

    let mut foreign_keys = connection
        .prepare("PRAGMA foreign_key_check(current_observations)")
        .map_err(StoreError::Migrate)?;
    if foreign_keys
        .query([])
        .map_err(StoreError::Migrate)?
        .next()
        .map_err(StoreError::Migrate)?
        .is_some()
    {
        return Err(StoreError::ImportSourceNameMigration(
            "the rebuilt current rows have an invalid foreign key",
        ));
    }
    drop(foreign_keys);
    for old in OLD_TABLES {
        let remains: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE sql IS NOT NULL AND instr(sql,?1)>0)",
            [old], |row| row.get(0),
        ).map_err(StoreError::Migrate)?;
        if remains {
            return Err(StoreError::ImportSourceNameMigration(
                "an old table reference remains in the schema",
            ));
        }
    }
    connection
        .pragma_update(None, "user_version", VERSION)
        .map_err(StoreError::Migrate)?;
    Ok(())
}
