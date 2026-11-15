// SPDX-License-Identifier: AGPL-3.0-only

// The imported lifecycle materialisation an import finalisation reconciles.

#[derive(Clone, Copy)]
enum ImportedProjectionRows {
    Attached,
    Durable,
}

#[derive(Clone, Copy)]
enum PriorImportedDriveRows {
    None,
    Durable,
    Legacy,
}

/// Install the direct snapshot's bounded query projection and remove only
/// rows which the prior import projection owned. Rows created solely by live
/// collection are deliberately outside that prior set and survive a reimport.
fn reconcile_imported_materialisation_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    car_id: i64,
    car: &ProjectionCar,
    drives: &[ProjectionDrive],
    current_rows: ImportedProjectionRows,
    prior_rows: PriorImportedDriveRows,
) -> Result<(), StoreError> {
    if vehicle_id.is_nil() || car_id <= 0 || car.id != car_id {
        return Err(StoreError::InvalidImportGeneration);
    }

    let vehicle_key = vehicle_id.to_string();
    let projection_rows = |entity_ordinal: u8| -> Result<Vec<(i64, i64, Vec<u8>)>, StoreError> {
        match current_rows {
            ImportedProjectionRows::Attached => {
                let mut statement = transaction
                    .prepare(concat!(
                        "SELECT entity_id, car_id, projection_sha256
                           FROM ",
                        crate::db::import_table_names::import_state_spool_alias!(),
                        ".current_rows
                          WHERE entity_ordinal = ?1
                          ORDER BY entity_id ASC"
                    ))
                    .map_err(StoreError::LineageCatalog)?;
                let rows = statement
                    .query_map(params![i64::from(entity_ordinal)], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })
                    .map_err(StoreError::LineageCatalog)?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(StoreError::LineageCatalog)
            }
            ImportedProjectionRows::Durable => {
                let mut statement = transaction
                    .prepare(concat!(
                        "SELECT entity_id, car_id, projection_sha256
                           FROM ",
                        crate::db::import_table_names::import_projection_state_rows_table!(),
                        "
                          WHERE vehicle_id = ?1 AND entity_ordinal = ?2
                          ORDER BY entity_id ASC"
                    ))
                    .map_err(StoreError::LineageCatalog)?;
                let rows = statement
                    .query_map(
                        params![vehicle_key.as_str(), i64::from(entity_ordinal)],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .map_err(StoreError::LineageCatalog)?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(StoreError::LineageCatalog)
            }
        }
    };

    let car_digest =
        canonical_payload_and_digest(SourceDbProjectionStateEntity::Car, car_id, car_id, car)
            .map_err(StoreError::SourceDbProjectionState)?
            .1;
    let car_rows = projection_rows(SourceDbProjectionStateEntity::Car.ordinal())?;
    if car_rows.len() != 1
        || car_rows[0].0 != car_id
        || car_rows[0].1 != car_id
        || car_rows[0].2.as_slice() != car_digest.as_bytes().as_slice()
    {
        return Err(StoreError::LineageCatalogConflict);
    }

    let mut previous_id = 0_i64;
    for drive in drives {
        // A source id inside the Hub's range would be taken for a Hub row.
        if drive.id <= previous_id
            || drive.id >= crate::db::hub_ids::HUB_ID_BASE
            || drive.car_id != car_id
        {
            return Err(StoreError::InvalidImportGeneration);
        }
        previous_id = drive.id;
    }
    let projected_drive_rows = projection_rows(SourceDbProjectionStateEntity::Drive.ordinal())?;
    if projected_drive_rows.len() != drives.len() {
        return Err(StoreError::LineageCatalogConflict);
    }
    for ((id, projected_car_id, projected_digest), drive) in projected_drive_rows.iter().zip(drives)
    {
        let digest = canonical_payload_and_digest(
            SourceDbProjectionStateEntity::Drive,
            drive.id,
            drive.car_id,
            drive,
        )
        .map_err(StoreError::SourceDbProjectionState)?
        .1;
        if *id != drive.id
            || *projected_car_id != drive.car_id
            || projected_digest.as_slice() != digest.as_bytes().as_slice()
        {
            return Err(StoreError::LineageCatalogConflict);
        }
    }

    let prior_imported_drive_ids = match prior_rows {
        PriorImportedDriveRows::None => Vec::new(),
        PriorImportedDriveRows::Durable => {
            durable_prior_imported_drive_ids_in_transaction(transaction, &vehicle_key)?
                .into_iter()
                .collect()
        }
        PriorImportedDriveRows::Legacy => {
            let mut statement = transaction
                .prepare(concat!(
                    "SELECT entity_id
                       FROM ",
                    crate::db::import_table_names::import_projection_rows_table!(),
                    "
                      WHERE vehicle_id = ?1 AND entity = 'drive'
                      ORDER BY entity_id ASC"
                ))
                .map_err(StoreError::LineageCatalog)?;
            let rows = statement
                .query_map(params![vehicle_key.as_str()], |row| row.get(0))
                .map_err(StoreError::LineageCatalog)?;
            rows.collect::<Result<Vec<i64>, _>>()
                .map_err(StoreError::LineageCatalog)?
        }
    };
    let prior_imported_drive_ids = prior_imported_drive_ids.into_iter().collect::<HashSet<_>>();

    let car_json = serde_json::to_string(car).map_err(StoreError::SerializeLifecycleRow)?;
    transaction
        .execute(
            "INSERT INTO materialised_cars(vehicle_id, car_id, car_json)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(vehicle_id) DO UPDATE SET
                car_id = excluded.car_id,
                car_json = excluded.car_json
             WHERE materialised_cars.car_id IS NOT excluded.car_id
                OR materialised_cars.car_json IS NOT excluded.car_json",
            params![vehicle_key.as_str(), car_id, car_json],
        )
        .map_err(StoreError::LifecycleWrite)?;

    // Ownership is the id range, not the prior inventory or a digest: the
    // seam plan decides which side keeps each drive and position, and writes
    // its losers to `import_seam_links` with tombstones for the Hub's.
    apply_import_seam_to_drives_in_transaction(
        transaction,
        vehicle_id,
        drives,
        &prior_imported_drive_ids,
    )
}
