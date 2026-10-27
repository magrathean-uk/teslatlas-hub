// SPDX-License-Identifier: AGPL-3.0-only

fn migrate(connection: &Connection) -> Result<(), StoreError> {
    admit_writable_catalogue(connection)?;
    if schema_version(connection)? == 0 {
        let transaction = Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
            .map_err(StoreError::Migrate)?;
        admit_writable_catalogue(&transaction)?;
        if schema_version(&transaction)? == 0 {
            transaction
                .execute_batch(include_str!("schema_floor.sql"))
                .map_err(StoreError::Migrate)?;
            transaction
                .pragma_update(None, "application_id", APPLICATION_ID)
                .map_err(StoreError::Migrate)?;
            transaction
                .pragma_update(None, "user_version", MIN_STORE_VERSION)
                .map_err(StoreError::Migrate)?;
        }
        transaction.commit().map_err(StoreError::Migrate)?;
    }
    let version = schema_version(connection)?;
    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(StoreError::UnsupportedSchema(version))
    }
}
