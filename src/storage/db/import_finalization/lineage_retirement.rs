// SPDX-License-Identifier: AGPL-3.0-only

// The base an import finalisation retires, and the window rebind (DEC-150).

/// The base an import finalisation retires.
#[derive(Clone, Copy)]
enum BaseReplacement<'a> {
    /// A verified import lineage past its pack capacity.
    Import(&'a LineageManifestV2),
    /// The collector's base on a Hub that never imported (DEC-150).
    Window(&'a LineageManifestV2),
}

/// See [`HubStore::window_only_v2_base_is_rebindable`].
fn window_only_v2_base_in_connection(
    connection: &Connection,
    vehicle_key: &str,
) -> Result<bool, StoreError> {
    connection
        .query_row(
            concat!(
                "SELECT EXISTS(SELECT 1 FROM v2_base_bindings AS bindings
                                 JOIN sources ON sources.source_id = bindings.account_id
                                WHERE bindings.vehicle_id = ?1
                                  AND sources.source_kind = 'owner_api_compat')
                    AND NOT EXISTS(SELECT 1 FROM ",
                crate::db::import_table_names::import_projection_state_heads_table!(),
                " WHERE vehicle_id = ?1)
                    AND NOT EXISTS(SELECT 1 FROM ",
                crate::db::import_table_names::import_projection_rows_table!(),
                " WHERE vehicle_id = ?1)
                    AND NOT EXISTS(SELECT 1 FROM import_seam_links WHERE vehicle_id = ?1)"
            ),
            params![vehicle_key],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)
}

/// The materialised tables whose rows a rebind journals (DEC-150), parents
/// before their children: entity, table, id column, JSON column and whether
/// the table has a car id column.
const REBOUND_TABLES: [(&str, &str, &str, &str, bool); 6] = [
    ("drive", "materialised_drives", "drive_id", "drive_json", true),
    ("charge", "materialised_charges", "charge_id", "charge_json", true),
    ("position", "materialised_positions", "position_id", "position_json", true),
    ("charge_sample", "materialised_charge_samples", "sample_id", "sample_json", false),
    ("state", "materialised_states", "state_id", "state_json", true),
    ("update", "materialised_updates", "update_id", "update_json", true),
];

/// Keeps the Hub's rows of a window-only vehicle for the import that rebinds
/// its base (DEC-150): every stored row takes the import's car id.
/// Mutations the retired lineage never published are superseded: the
/// rebind's journal, rebuilt from the final rows once the import is
/// reconciled ([`journal_rebound_rows_in_transaction`]), re-sends them.
/// Returns the revision that journal starts at.
fn rebind_window_rows_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    car_id: i64,
) -> Result<i64, StoreError> {
    let vehicle_key = vehicle_id.to_string();
    transaction
        .execute(
            "UPDATE sync_mutations SET published = 1, claimed_until_ms = 0
              WHERE vehicle_id = ?1 AND published = 0",
            params![vehicle_key.as_str()],
        )
        .map_err(StoreError::LifecycleWrite)?;
    for (_, table, id_column, json_column, has_car_id) in REBOUND_TABLES {
        if !has_car_id {
            continue;
        }
        let rows = {
            let mut statement = transaction
                .prepare(&format!(
                    "SELECT {id_column}, {json_column} FROM {table} WHERE vehicle_id = ?1"
                ))
                .map_err(StoreError::Query)?;
            statement
                .query_map(params![vehicle_key.as_str()], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(StoreError::Query)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::Query)?
        };
        for (id, json) in rows {
            let mut value: serde_json::Value =
                serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?;
            value
                .as_object_mut()
                .ok_or(StoreError::InvalidLifecycleSession)?
                .insert("car_id".to_owned(), serde_json::Value::from(car_id));
            let json = serde_json::to_string(&value).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    &format!(
                        "UPDATE {table} SET car_id = ?3, {json_column} = ?4
                          WHERE vehicle_id = ?1 AND {id_column} = ?2"
                    ),
                    params![vehicle_key.as_str(), id, car_id, json],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
    }
    transaction
        .query_row(
            "SELECT COALESCE(
                 (SELECT next_revision FROM sync_mutation_sequences WHERE vehicle_id = ?1), 1)",
            params![vehicle_key.as_str()],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)
}

/// The rebind's journal (DEC-150), rebuilt from the final reconciled rows
/// so that publication in bounded batches never reads a row a later
/// mutation removes (review finding 1). The mutations the reconciliation
/// journalled from `from_revision` on are replaced, at the same revisions,
/// by one mutation per row, parents first:
/// - every Hub-range row as it is stored, as an upsert;
/// - every source drive of the new base that the seam left out, as a
///   tombstone, and every one whose stored row is no longer the base's row
///   (a source drive that won and widened), as an upsert of the stored row
///   (review finding 2);
/// - every other row the reconciliation journalled, as stored, or as a
///   tombstone once it is gone.
fn journal_rebound_rows_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    car_id: i64,
    drives: &[ProjectionDrive],
    from_revision: i64,
) -> Result<(), StoreError> {
    use crate::db::hub_ids::{HUB_ID_BASE, HUB_ID_MAX};
    use std::collections::BTreeMap;
    let vehicle_key = vehicle_id.to_string();
    // What the reconciliation journalled: its last mutation per row.
    let journalled: Vec<(String, i64, i64, String, String)> = {
        let mut statement = transaction
            .prepare(
                "SELECT entity, entity_id, car_id, operation, payload_json FROM sync_mutations
                  WHERE vehicle_id = ?1 AND published = 0 AND revision >= ?2
                  ORDER BY revision",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map(params![vehicle_key.as_str(), from_revision], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })
            .map_err(StoreError::Query)?
            .collect::<Result<_, _>>()
            .map_err(StoreError::Query)?
    };
    transaction
        .execute(
            "DELETE FROM sync_mutations WHERE vehicle_id = ?1 AND revision >= ?2",
            params![vehicle_key.as_str(), from_revision],
        )
        .map_err(StoreError::LifecycleWrite)?;
    transaction
        .execute(
            "INSERT INTO sync_mutation_sequences(vehicle_id, next_revision) VALUES (?1, ?2)
             ON CONFLICT(vehicle_id) DO UPDATE SET next_revision = excluded.next_revision",
            params![vehicle_key.as_str(), from_revision],
        )
        .map_err(StoreError::LifecycleWrite)?;
    let rank = |entity: &str| match entity {
        "car" | "car_setting" | "car_settings" => 0,
        other => REBOUND_TABLES
            .iter()
            .position(|(name, ..)| *name == other)
            .map_or(REBOUND_TABLES.len() + 1, |index| index + 1),
    };
    // Each row once, in parent order, with the reconciliation's last
    // mutation of it when there is one.
    let mut rows: BTreeMap<(usize, String, i64), Option<(i64, String, String)>> = BTreeMap::new();
    for (entity, id, row_car_id, operation, payload) in journalled {
        rows.insert(
            (rank(&entity), entity, id),
            Some((row_car_id, operation, payload)),
        );
    }
    for (entity, table, id_column, _, _) in REBOUND_TABLES {
        let ids: Vec<i64> = {
            let mut statement = transaction
                .prepare(&format!(
                    "SELECT {id_column} FROM {table}
                      WHERE vehicle_id = ?1 AND {id_column} BETWEEN ?2 AND ?3"
                ))
                .map_err(StoreError::Query)?;
            statement
                .query_map(params![vehicle_key.as_str(), HUB_ID_BASE, HUB_ID_MAX], |row| {
                    row.get(0)
                })
                .map_err(StoreError::Query)?
                .collect::<Result<_, _>>()
                .map_err(StoreError::Query)?
        };
        for id in ids {
            rows.entry((rank(entity), entity.to_owned(), id)).or_insert(None);
        }
    }
    let base_drives: HashMap<i64, &ProjectionDrive> =
        drives.iter().map(|drive| (drive.id, drive)).collect();
    for drive in drives {
        rows.entry((rank("drive"), "drive".to_owned(), drive.id)).or_insert(None);
    }
    for ((_, entity, id), journalled) in rows {
        let Some((_, table, id_column, json_column, _)) =
            REBOUND_TABLES.iter().find(|(name, ..)| *name == entity)
        else {
            // A row without a rebound table (the car): as journalled.
            if let Some((row_car_id, operation, payload)) = journalled {
                record_sync_mutation_in_transaction(
                    transaction,
                    vehicle_id,
                    &entity,
                    id,
                    row_car_id,
                    &operation,
                    &payload,
                )?;
            }
            continue;
        };
        let stored: Option<String> = transaction
            .query_row(
                &format!(
                    "SELECT {json_column} FROM {table} WHERE vehicle_id = ?1 AND {id_column} = ?2"
                ),
                params![vehicle_key.as_str(), id],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)?;
        let base_drive = (entity == "drive").then(|| base_drives.get(&id)).flatten();
        match stored {
            Some(json) => {
                if let Some(base) = base_drive {
                    // A source drive the base carries: only a changed one.
                    let stored: serde_json::Value =
                        serde_json::from_str(&json).map_err(StoreError::DeserializeLifecycleRow)?;
                    let base =
                        serde_json::to_value(base).map_err(StoreError::SerializeLifecycleRow)?;
                    if stored == base {
                        continue;
                    }
                }
                record_sync_mutation_in_transaction(
                    transaction,
                    vehicle_id,
                    &entity,
                    id,
                    car_id,
                    "upsert",
                    &json,
                )?;
            }
            None => {
                let row_car_id = journalled
                    .as_ref()
                    .map(|(row_car_id, ..)| *row_car_id)
                    .or_else(|| base_drive.map(|drive| drive.car_id))
                    .unwrap_or(car_id);
                record_sync_mutation_in_transaction(
                    transaction,
                    vehicle_id,
                    &entity,
                    id,
                    row_car_id,
                    "tombstone",
                    "{}",
                )?;
            }
        }
    }
    Ok(())
}

/// Preserve the exact import checkpoint before changing its active base.
fn retire_import_lineage_in_transaction(
    tx: &Transaction<'_>,
    expected: &LineageManifestV2,
    binding: &ProjectionBinding,
) -> Result<(), StoreError> {
    retire_lineage_in_transaction(tx, expected, Some(binding))
}

/// Retires the lineage `expected` into an expiring checkpoint. With
/// `binding`, an import lineage whose binding it must match and which no
/// collector span may extend; without, the collector's window-only lineage
/// (DEC-150), bound as `expected` says, whose live spans go with it.
fn retire_lineage_in_transaction(
    tx: &Transaction<'_>,
    expected: &LineageManifestV2,
    binding: Option<&ProjectionBinding>,
) -> Result<(), StoreError> {
    let vehicle = expected.vehicle_id.to_string();
    let current: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT base_snapshot_id,head_sequence,head_digest FROM sync_heads WHERE vehicle_id=?1",
            params![&vehicle],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(StoreError::LineageCatalog)?;
    if current
        != Some((
            expected.base.snapshot_id.to_string(),
            i64::try_from(expected.head_sequence).map_err(|_| StoreError::SequenceTooLarge)?,
            expected.head_digest.to_string(),
        ))
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    let base: (i64, String, Vec<u8>) = tx
        .query_row(
            "SELECT base_sequence,base_digest,packs_json FROM sync_bases WHERE vehicle_id=?1",
            params![&vehicle],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(StoreError::LineageCatalog)?;
    if base
        != (
            i64::try_from(expected.base.sequence).map_err(|_| StoreError::SequenceTooLarge)?,
            expected.base.digest.to_string(),
            serde_json::to_vec(&expected.base.packs).map_err(StoreError::SerializeManifest)?,
        )
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    let mut stmt = tx.prepare("SELECT pack_json FROM sync_deltas WHERE vehicle_id=?1 ORDER BY from_sequence,to_sequence")
        .map_err(StoreError::LineageCatalog)?;
    let deltas = stmt
        .query_map(params![&vehicle], |r| r.get::<_, Vec<u8>>(0))
        .map_err(StoreError::LineageCatalog)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::LineageCatalog)?;
    let expected_deltas = expected
        .deltas
        .iter()
        .map(serde_json::to_vec)
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::SerializeManifest)?;
    if deltas != expected_deltas {
        return Err(StoreError::LineageCatalogConflict);
    }
    drop(stmt);
    let stored: (String,String,String,i64,i64) = tx.query_row(
        "SELECT snapshot_id,installation_id,account_id,generation,selected_car_id FROM v2_base_bindings WHERE vehicle_id=?1",
        params![&vehicle], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    ).map_err(StoreError::LineageCatalog)?;
    let Some(binding) = binding else {
        if (&stored.0, &stored.1, &stored.2, stored.3)
            != (
                &expected.base.snapshot_id.to_string(),
                &expected.installation_id.to_string(),
                &expected.account_id.to_string(),
                i64::try_from(expected.generation).map_err(|_| StoreError::SequenceTooLarge)?,
            )
        {
            return Err(StoreError::LineageCatalogConflict);
        }
        return retire_checked_lineage_in_transaction(tx, expected);
    };
    if stored
        != (
            expected.base.snapshot_id.to_string(),
            binding.installation_id.to_string(),
            binding.account_id.to_string(),
            i64::try_from(binding.generation).map_err(|_| StoreError::SequenceTooLarge)?,
            binding.selected_car_id,
        )
    {
        return Err(StoreError::LineageCatalogConflict);
    }
    // Imported rows do not prove ownership of collector spans.
    let live: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_live_delta_spans WHERE vehicle_id=?1)",
            params![&vehicle],
            |r| r.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    if live {
        return Err(StoreError::LineageCatalogConflict);
    }
    retire_checked_lineage_in_transaction(tx, expected)
}

/// Moves a lineage the caller has matched against the catalogue into an
/// expiring checkpoint and removes it; its live spans cascade with its deltas.
fn retire_checked_lineage_in_transaction(
    tx: &Transaction<'_>,
    expected: &LineageManifestV2,
) -> Result<(), StoreError> {
    let vehicle = expected.vehicle_id.to_string();
    let now = retired_lineage_clock_ms()?;
    let expires = now
        .checked_add(RETIRED_LINEAGE_PACK_RETENTION_MS)
        .ok_or(StoreError::RetiredLineageClockOverflow)?;
    tx.execute(
        "INSERT INTO sync_retired_lineages(vehicle_id,head_digest,manifest_json,retired_at_ms,expires_at_ms) VALUES(?1,?2,?3,?4,?5)",
        params![&vehicle,expected.head_digest.to_string(),
            serde_json::to_vec(expected).map_err(StoreError::SerializeManifest)?,now,expires],
    ).map_err(StoreError::LineageCatalog)?;
    for pack in expected
        .base
        .packs
        .iter()
        .chain(expected.deltas.iter().map(|d| &d.pack))
    {
        tx.execute(
            "INSERT INTO sync_retired_lineage_packs(vehicle_id,head_digest,pack_digest,relative_path,compressed_bytes) VALUES(?1,?2,?3,?4,?5)",
            params![&vehicle,expected.head_digest.to_string(),pack.sha256.to_string(),
                &pack.relative_path,i64::try_from(pack.compressed_bytes)
                    .map_err(|_|StoreError::PackSizeTooLarge)?],
        ).map_err(StoreError::LineageCatalog)?;
    }
    tx.execute(
        "DELETE FROM sync_deltas WHERE vehicle_id=?1",
        params![&vehicle],
    )
    .map_err(StoreError::LineageCatalog)?;
    tx.execute(
        "DELETE FROM sync_heads WHERE vehicle_id=?1",
        params![&vehicle],
    )
    .map_err(StoreError::LineageCatalog)?;
    // Cascades remove only the superseded binding and import digest/inventory.
    tx.execute(
        "DELETE FROM sync_bases WHERE vehicle_id=?1",
        params![&vehicle],
    )
    .map_err(StoreError::LineageCatalog)?;
    // Retired objects remain reachable only through the expiring checkpoint.
    tx.execute(
        "DELETE FROM sync_manifests WHERE snapshot_id=?1",
        params![expected.base.snapshot_id.to_string()],
    )
    .map_err(StoreError::LineageCatalog)?;
    Ok(())
}
