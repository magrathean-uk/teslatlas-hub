// SPDX-License-Identifier: AGPL-3.0-only

pub(crate) async fn prepare_read_only_snapshot(
    client: &Client,
    source: &ReadOnlySource,
    limits: TeslaMateReadLimits,
) -> Result<TeslaMateSchemaInfo, TeslaMateReaderError> {
    for statement in source.session_sql() {
        client.batch_execute(statement).await?;
    }
    client
        .batch_execute(&copy_statement_timeout_sql(limits.copy_statement_timeout))
        .await?;

    validate_source_schema(client).await
}

fn copy_statement_timeout_sql(timeout: Duration) -> String {
    format!("SET LOCAL statement_timeout = '{}ms'", timeout.as_millis())
}

async fn validate_source_schema(
    client: &Client,
) -> Result<TeslaMateSchemaInfo, TeslaMateReaderError> {
    let migration_versions = client
        .query(MIGRATION_VERSIONS_SQL, &[])
        .await?
        .iter()
        .map(|row| row.try_get::<_, i64>("version"))
        .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
    if migration_versions.is_empty() {
        return Err(TeslaMateReaderError::MissingMigrationVersion);
    }
    let migration = validate_migration_versions(&migration_versions)?;

    let rows = client.query(SCHEMA_PROBE_SQL, &[]).await?;
    let observed = rows
        .iter()
        .map(|row| {
            Ok(crate::teslamate_schema::ObservedColumn {
                table: row.try_get("table_name")?,
                name: row.try_get("column_name")?,
                type_name: row.try_get("type_name")?,
                format_type: row.try_get("format_type")?,
                nullable: row.try_get("is_nullable")?,
            })
        })
        .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
    validate_observed_schema(&observed)?;

    let enum_rows = client.query(ENUM_PROBE_SQL, &[]).await?;
    let observed_enums = enum_rows
        .iter()
        .map(|row| {
            Ok(crate::teslamate_schema::ObservedEnumLabel {
                type_name: row.try_get("type_name")?,
                label: row.try_get("label")?,
            })
        })
        .collect::<Result<Vec<_>, tokio_postgres::Error>>()?;
    validate_observed_enums(&observed_enums)?;

    let relationship = client.query_one(SETTINGS_RELATIONSHIP_SQL, &[]).await?;
    let settings_count: i64 = relationship.try_get("settings_count")?;
    let cars_without_settings: i64 = relationship.try_get("cars_without_settings")?;
    validate_settings_relationship(settings_count, cars_without_settings)?;

    let mut digest = Sha256::new();
    for version in &migration_versions {
        digest.update(format!("{version:014}\n").as_bytes());
    }
    for column in &observed {
        digest.update(column.table.as_bytes());
        digest.update([0]);
        digest.update(column.name.as_bytes());
        digest.update([0]);
        digest.update(column.type_name.as_bytes());
        digest.update([u8::from(column.nullable)]);
        digest.update(column.format_type.as_bytes());
    }
    for value in &observed_enums {
        digest.update(value.type_name.as_bytes());
        digest.update([0]);
        digest.update(value.label.as_bytes());
    }
    digest.update(settings_count.to_le_bytes());
    digest.update(cars_without_settings.to_le_bytes());
    Ok(TeslaMateSchemaInfo {
        observed_migration_version: migration,
        observed_migration_count: migration_versions.len(),
        minimum_supported_migration_version: MIN_SUPPORTED_MIGRATION,
        maximum_validated_migration_version: MAX_VALIDATED_MIGRATION,
        pinned_source_revision: TESLAMATE_V4_SOURCE_REVISION,
        pinned_migration_set_sha256: TESLAMATE_V4_MIGRATION_SET_SHA256,
        fingerprint: hex::encode(digest.finalize()),
    })
}
