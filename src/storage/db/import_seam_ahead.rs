// SPDX-License-Identifier: AGPL-3.0-only

/// What [`HubStore::apply_import_seam_ahead_of_packs`] hands the importer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportSeamAhead {
    /// The part of the plan the packs carry; publication proves it again by
    /// its digest.
    pub(crate) packs: crate::import::source_db::seam::SeamPackPlan,
    /// Packed source drives whose stored row is not yet the planned row (a
    /// drive skipped before and now packed, or one the plan widened): the
    /// packs carry them even when the source did not change them.
    pub(crate) restated_drives: Vec<i64>,
    /// Packed source charges the packs carry even when the source did not
    /// change them: a charge skipped before and now packed, or one the plan
    /// widened. The store holds no source charge to compare with.
    pub(crate) restated_charges: Vec<i64>,
    /// The plan changed the Hub's side of the store, so its sync mutations
    /// must be published before any pack that relies on them.
    pub(crate) changed_the_store: bool,
}

/// What a routing decision for a direct successor rests on (DEC-103): the
/// seam the packs would carry, whether the vehicle had seam history before
/// it, and how many of the vehicle's sync mutations would then wait for
/// publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportSeamDryRun {
    pub(crate) ahead: ImportSeamAhead,
    /// Coverage in the store, a window still open, a Hub-range row, a seam
    /// link or a live delta span: such a vehicle's import base is never
    /// replaced.
    pub(crate) seam_history: bool,
    pub(crate) unpublished: u64,
}

impl HubStore {
    /// Applies the Hub's side of the import seam ahead of the packs (design
    /// M03-SEAM-R3, section 4), for drives and charges alike: Hub sessions
    /// that lost are removed with a tombstone, an earlier import's copy of a skipped or stale source drive
    /// goes with a tombstone, Hub positions outside coverage go with a
    /// tombstone, kept Hub positions follow their drive's winner, kept Hub
    /// drives widen, charges and charge samples follow the same rules, and
    /// the decisions are recorded in `import_seam_links`.
    /// The winning source drives are not written here: publication writes
    /// them in the transaction that publishes the packs carrying them.
    ///
    /// The caller publishes the resulting sync mutations before it writes any
    /// pack, so a client never holds a replacement next to the row it
    /// replaces. The returned plan is the one publication must find again.
    ///
    /// With `decided`, the seam is applied only when the store still gives
    /// the dry run a routing decision rested on (DEC-103): the same seam
    /// history, plan and waiting sync mutations, worked out again in the
    /// transaction that applies it. Otherwise the transaction is rolled back
    /// and `None` comes back: nothing changed, and the caller decides again.
    pub(crate) fn apply_import_seam_ahead_of_packs(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        car_id: i64,
        drives: &[ProjectionDrive],
        charges: &[crate::hub_pack::ProjectionCharge],
        decided: Option<&ImportSeamDryRun>,
    ) -> Result<Option<ImportSeamAhead>, StoreError> {
        let (run, committed) =
            self.import_seam_ahead_of_packs(vehicle_id, car_id, drives, charges, |run| {
                decided.is_none_or(|decided| decided == run)
            })?;
        Ok(committed.then_some(run.ahead))
    }

    /// The seam [`HubStore::apply_import_seam_ahead_of_packs`] would apply,
    /// worked out in a transaction that is rolled back (DEC-103), with what
    /// a routing decision rests on. Nothing in the store changes.
    pub(crate) fn dry_run_import_seam_ahead_of_packs(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        car_id: i64,
        drives: &[ProjectionDrive],
        charges: &[crate::hub_pack::ProjectionCharge],
    ) -> Result<ImportSeamDryRun, StoreError> {
        self.import_seam_ahead_of_packs(vehicle_id, car_id, drives, charges, |_| false)
            .map(|(run, _)| run)
    }

    /// The seam ahead of the packs, with the vehicle's seam history before it
    /// and its unpublished sync mutations after it, in one transaction that
    /// commits only when `commit` accepts what it found.
    fn import_seam_ahead_of_packs(
        &self,
        vehicle_id: Uuid,
        car_id: i64,
        drives: &[ProjectionDrive],
        charges: &[crate::hub_pack::ProjectionCharge],
        commit: impl FnOnce(&ImportSeamDryRun) -> bool,
    ) -> Result<(ImportSeamDryRun, bool), StoreError> {
        if vehicle_id.is_nil() || car_id <= 0 {
            return Err(StoreError::InvalidImportGeneration);
        }
        let mut previous_id = 0_i64;
        for drive in drives {
            if drive.id <= previous_id
                || drive.id >= crate::db::hub_ids::HUB_ID_BASE
                || drive.car_id != car_id
            {
                return Err(StoreError::InvalidImportGeneration);
            }
            previous_id = drive.id;
        }
        validate_import_seam_charges(charges, car_id)?;
        let vehicle_key = vehicle_id.to_string();
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let seam_history = import_seam_history_in_connection(&transaction, &vehicle_key)?;
        let prior = durable_prior_imported_drive_ids_in_transaction(&transaction, &vehicle_key)?;
        let (coverage, before) =
            import_seam_plan_in_transaction(&transaction, vehicle_id, drives, charges, &prior)?;
        let changed_the_store = before.changes_the_store();
        let packs = if changed_the_store {
            apply_import_seam_hub_side_in_transaction(&transaction, vehicle_id, &before)?;
            apply_import_seam_to_charges_in_transaction(
                &transaction,
                vehicle_id,
                &before,
                charges,
                false,
            )?;
            // Planned again, the applied plan must give the packs the same
            // decisions, or publication could never prove them.
            let (coverage_after, after) =
                import_seam_plan_in_transaction(&transaction, vehicle_id, drives, charges, &prior)?;
            let packs = crate::import::source_db::seam::SeamPackPlan::for_plan(&coverage_after, &after);
            if packs.digest()
                != crate::import::source_db::seam::SeamPackPlan::for_plan(&coverage, &before).digest()
            {
                return Err(StoreError::ImportGenerationConflict);
            }
            packs
        } else {
            crate::import::source_db::seam::SeamPackPlan::for_plan(&coverage, &before)
        };
        let restated_drives = restated_import_drives_in_transaction(
            &transaction,
            &vehicle_key,
            drives,
            &packs,
        )?;
        let restated_charges = restated_import_charges(&before, &packs);
        let unpublished: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM sync_mutations WHERE vehicle_id = ?1 AND published = 0",
                params![vehicle_key.as_str()],
                |row| row.get(0),
            )
            .map_err(StoreError::Query)?;
        let unpublished = u64::try_from(unpublished).map_err(|_| StoreError::InvalidStoredSequence)?;
        let run = ImportSeamDryRun {
            ahead: ImportSeamAhead {
                packs,
                restated_drives,
                restated_charges,
                changed_the_store,
            },
            seam_history,
            unpublished,
        };
        let committed = commit(&run);
        if committed {
            transaction.commit().map_err(StoreError::LifecycleWrite)?;
        } else {
            transaction.rollback().map_err(StoreError::LifecycleWrite)?;
        }
        Ok((run, committed))
    }

    /// Refuses ([`StoreError::ImportGenerationConflict`]) a publication path
    /// that does not plan the seam while the vehicle has any of it: coverage,
    /// a Hub-range row or a recorded seam decision.
    pub(crate) fn refuse_unplanned_import_seam(&self, vehicle_id: Uuid) -> Result<(), StoreError> {
        let connection = self.open()?;
        refuse_unplanned_import_seam_in_transaction(&connection, &vehicle_id.to_string())
    }
}

/// The source drive ids the prior import listed, from its durable state.
fn durable_prior_imported_drive_ids_in_transaction(
    connection: &Connection,
    vehicle_key: &str,
) -> Result<HashSet<i64>, StoreError> {
    durable_prior_imported_ids_in_connection(
        connection,
        vehicle_key,
        SourceDbProjectionStateEntity::Drive,
    )
}

/// The source ids of one entity the prior import listed, from its durable
/// state.
fn durable_prior_imported_ids_in_connection(
    connection: &Connection,
    vehicle_key: &str,
    entity: SourceDbProjectionStateEntity,
) -> Result<HashSet<i64>, StoreError> {
    let mut statement = connection
        .prepare(concat!(
            "SELECT entity_id
               FROM ",
            crate::db::import_table_names::import_projection_state_rows_table!(),
            "
              WHERE vehicle_id = ?1 AND entity_ordinal = ?2
              ORDER BY entity_id ASC"
        ))
        .map_err(StoreError::LineageCatalog)?;
    let rows = statement
        .query_map(
            params![vehicle_key, i64::from(entity.ordinal())],
            |row| row.get(0),
        )
        .map_err(StoreError::LineageCatalog)?;
    rows.collect::<Result<HashSet<i64>, _>>()
        .map_err(StoreError::LineageCatalog)
}

/// Plans the import seam for one vehicle's drives, positions, charges and
/// charge samples from the store, as publication applies it (design
/// M03-SEAM-R3, section 4): the coverage, the stored drives (a Hub-range
/// drive, or a source-id drive the prior import did not list or a Hub-range
/// position belongs to, is the Hub's), the Hub-range positions, the stored
/// charges and Hub-range samples (see `import_seam_hub_charges_in_connection`),
/// the source drives and charges and the recorded links. Reads only.
fn import_seam_plan_in_transaction(
    connection: &Connection,
    vehicle_id: Uuid,
    drives: &[ProjectionDrive],
    charges: &[crate::hub_pack::ProjectionCharge],
    prior_imported_drive_ids: &HashSet<i64>,
) -> Result<
    (
        crate::import::source_db::seam::HubCoverage,
        crate::import::source_db::seam::SeamPlan,
    ),
    StoreError,
> {
    use crate::db::hub_ids::{HUB_ID_BASE, HUB_ID_MAX, is_hub_id};
    use crate::import::source_db::seam::{
        ChildRow, HubCoverage, HubRows, SeamLinks, SessionRow, SourceRows, plan_seam,
    };

    let vehicle_key = vehicle_id.to_string();
    let coverage = HubCoverage::for_store(connection, &vehicle_key)
        .map_err(|_| StoreError::ImportGenerationConflict)?;
    let mut hub = HubRows::default();
    {
        let mut statement = connection
            .prepare(
                "SELECT position_id, drive_id,
                        CAST(json_extract(position_json, '$.date_ms') AS INTEGER)
                   FROM materialised_positions
                  WHERE vehicle_id = ?1 AND position_id BETWEEN ?2 AND ?3
                  ORDER BY position_id ASC",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(
                params![vehicle_key.as_str(), HUB_ID_BASE, HUB_ID_MAX],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .map_err(StoreError::Query)?;
        for row in rows {
            let (id, parent, at_ms) = row.map_err(StoreError::Query)?;
            // A Hub position without an instant cannot be placed; it stays.
            if let Some(at_ms) = at_ms {
                hub.positions.push(ChildRow { id, at_ms, parent });
            }
        }
    }
    let held_by_hub_position: HashSet<i64> =
        hub.positions.iter().filter_map(|row| row.parent).collect();
    {
        let mut statement = connection
            .prepare(
                "SELECT drive_id,
                        CAST(json_extract(drive_json, '$.start_date_ms') AS INTEGER),
                        CAST(json_extract(drive_json, '$.end_date_ms') AS INTEGER)
                   FROM materialised_drives WHERE vehicle_id = ?1 ORDER BY drive_id ASC",
            )
            .map_err(StoreError::Query)?;
        let rows = statement
            .query_map(params![vehicle_key.as_str()], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            })
            .map_err(StoreError::Query)?;
        for row in rows {
            let (id, start_ms, end_ms) = row.map_err(StoreError::Query)?;
            // A stored drive without a start cannot be weighed; it stays.
            let Some(start_ms) = start_ms else { continue };
            let inherited =
                !prior_imported_drive_ids.contains(&id) || held_by_hub_position.contains(&id);
            if is_hub_id(id) || inherited {
                hub.drives.push(SessionRow {
                    id,
                    start_ms,
                    end_ms,
                });
            } else {
                hub.imported_drives.push(id);
            }
        }
    }
    let prior_imported_charge_ids = durable_prior_imported_ids_in_connection(
        connection,
        &vehicle_key,
        SourceDbProjectionStateEntity::Charge,
    )?;
    import_seam_hub_charges_in_connection(
        connection,
        &vehicle_key,
        &prior_imported_charge_ids,
        &mut hub,
    )?;
    let source = SourceRows {
        drives: drives
            .iter()
            .map(|drive| SessionRow {
                id: drive.id,
                start_ms: drive.start_date_ms,
                end_ms: Some(drive.end_date_ms),
            })
            .collect(),
        charges: import_seam_source_charges(charges),
        ..SourceRows::default()
    };
    let links = SeamLinks {
        drives: seam_links_in_connection(connection, &vehicle_key, "drive")?,
        charges: seam_links_in_connection(connection, &vehicle_key, "charge")?,
    };
    let plan = plan_seam(&coverage, &hub, &source, &links);
    Ok((coverage, plan))
}

/// The recorded seam decisions of one vehicle and entity, read outside a
/// publication transaction.
fn seam_links_in_connection(
    connection: &Connection,
    vehicle_key: &str,
    entity: &str,
) -> Result<std::collections::BTreeMap<i64, i64>, StoreError> {
    let mut statement = connection
        .prepare(
            "SELECT loser_id, winner_id FROM import_seam_links
              WHERE vehicle_id = ?1 AND entity = ?2 ORDER BY loser_id ASC",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_key, entity], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(StoreError::Query)?;
    rows.collect::<Result<_, _>>().map_err(StoreError::Query)
}

/// Applies everything of a drive and position plan except writing the
/// winning source drives (see [`HubStore::apply_import_seam_ahead_of_packs`]).
fn apply_import_seam_hub_side_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    plan: &crate::import::source_db::seam::SeamPlan,
) -> Result<(), StoreError> {
    use crate::db::hub_ids::is_hub_id;
    let vehicle_key = vehicle_id.to_string();
    let stored_car = |table: &str, column: &str, id: i64| -> Result<Option<i64>, StoreError> {
        transaction
            .query_row(
                &format!("SELECT car_id FROM {table} WHERE vehicle_id = ?1 AND {column} = ?2"),
                params![vehicle_key.as_str(), id],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::Query)
    };
    let drive_plan = &plan.drives;
    // Hub drives that lost, then an earlier import's copy of a skipped or
    // stale source drive: removed with a tombstone. Their positions follow
    // the instant rule below, never their drive.
    for link in &drive_plan.superseded {
        if let Some(car_id) = stored_car("materialised_drives", "drive_id", link.loser_id)? {
            remove_seam_drive_in_transaction(transaction, vehicle_id, link.loser_id, car_id)?;
        }
    }
    let source_gone = drive_plan
        .skipped
        .iter()
        .map(|link| link.loser_id)
        .chain(drive_plan.stale.iter().copied());
    for id in source_gone {
        if is_hub_id(id) {
            return Err(StoreError::InvalidImportGeneration);
        }
        if let Some(car_id) = stored_car("materialised_drives", "drive_id", id)? {
            remove_seam_drive_in_transaction(transaction, vehicle_id, id, car_id)?;
        }
    }
    // Kept Hub sessions widen to hold the children they gained.
    for bounds in &drive_plan.widened {
        if drive_plan.hub_kept.binary_search(&bounds.id).is_ok() {
            widen_hub_drive_in_transaction(
                transaction,
                vehicle_id,
                bounds.id,
                bounds.start_ms,
                bounds.end_ms,
            )?;
        }
    }
    // Hub positions outside coverage go with a tombstone; kept ones whose
    // drive lost move to the winner.
    for id in &plan.positions.hub_removed {
        let Some(car_id) = stored_car("materialised_positions", "position_id", *id)? else {
            continue;
        };
        transaction
            .execute(
                "DELETE FROM materialised_positions WHERE vehicle_id = ?1 AND position_id = ?2",
                params![vehicle_key.as_str(), id],
            )
            .map_err(StoreError::LifecycleWrite)?;
        record_sync_mutation_in_transaction(
            transaction,
            vehicle_id,
            "position",
            *id,
            car_id,
            "tombstone",
            "{}",
        )?;
    }
    for moved in &plan.positions.moved_hub {
        move_hub_position_in_transaction(transaction, vehicle_id, moved.id, moved.to)?;
    }
    let decided: Vec<_> = drive_plan
        .skipped
        .iter()
        .chain(&drive_plan.superseded)
        .copied()
        .collect();
    record_seam_links_in_transaction(
        transaction,
        &vehicle_key,
        "drive",
        &decided,
        &drive_plan.links_dropped,
    )
}

/// The packed source drives whose stored row differs from the row the plan
/// writes (the source copy with any widened bounds), ascending.
fn restated_import_drives_in_transaction(
    connection: &Connection,
    vehicle_key: &str,
    drives: &[ProjectionDrive],
    packs: &crate::import::source_db::seam::SeamPackPlan,
) -> Result<Vec<i64>, StoreError> {
    use crate::import::source_db::seam::PackedSession;
    let mut restated = Vec::new();
    let mut statement = connection
        .prepare("SELECT drive_json FROM materialised_drives WHERE vehicle_id = ?1 AND drive_id = ?2")
        .map_err(StoreError::Query)?;
    for drive in drives {
        let Some(PackedSession::Keep { bounds }) = packs.drive(drive.id) else {
            continue;
        };
        let planned = planned_import_drive(drive, bounds);
        let planned = serde_json::to_string(&planned).map_err(StoreError::SerializeLifecycleRow)?;
        let stored: Option<String> = statement
            .query_row(params![vehicle_key, drive.id], |row| row.get(0))
            .optional()
            .map_err(StoreError::Query)?;
        if stored.as_deref() != Some(planned.as_str()) {
            restated.push(drive.id);
        }
    }
    Ok(restated)
}

/// The packed source charges the packs restate, ascending: those the plan
/// widened, and those readmitted because the link that skipped them went.
fn restated_import_charges(
    plan: &crate::import::source_db::seam::SeamPlan,
    packs: &crate::import::source_db::seam::SeamPackPlan,
) -> Vec<i64> {
    use crate::import::source_db::seam::PackedSession;
    packs
        .imported_charges()
        .filter(|id| {
            matches!(packs.charge(*id), Some(PackedSession::Keep { bounds: Some(_) }))
                || plan.charges.links_dropped.binary_search(id).is_ok()
        })
        .collect()
}

/// The row a packed source drive is written as: its source copy with the
/// bounds the plan widened it to.
pub(crate) fn planned_import_drive(
    drive: &ProjectionDrive,
    bounds: Option<(i64, Option<i64>)>,
) -> ProjectionDrive {
    let mut planned = drive.clone();
    if let Some((start_ms, end_ms)) = bounds {
        planned.start_date_ms = start_ms;
        planned.end_date_ms = end_ms.unwrap_or(planned.end_date_ms);
    }
    planned
}

/// Refuses a publication path that does not plan the seam while the vehicle
/// has any of it (design M03-SEAM-R3, section 4; review finding 9): coverage
/// in the store, a window still open, a Hub-range row, or a recorded seam
/// decision. Before the Hub has collected the vehicle, such a path is exact.
fn refuse_unplanned_import_seam_in_transaction(
    connection: &Connection,
    vehicle_key: &str,
) -> Result<(), StoreError> {
    if import_seam_present_in_connection(connection, vehicle_key)? {
        return Err(StoreError::ImportGenerationConflict);
    }
    Ok(())
}

/// Whether the vehicle has seam history (DEC-103): any of the seam, or a
/// live delta span. Such a vehicle's import base is never replaced.
fn import_seam_history_in_connection(
    connection: &Connection,
    vehicle_key: &str,
) -> Result<bool, StoreError> {
    if import_seam_present_in_connection(connection, vehicle_key)? {
        return Ok(true);
    }
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sync_live_delta_spans WHERE vehicle_id = ?1)",
            params![vehicle_key],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)
}

/// Whether the vehicle has any of the seam: coverage in the store, a window
/// still open, a Hub-range row, or a recorded seam decision.
fn import_seam_present_in_connection(
    connection: &Connection,
    vehicle_key: &str,
) -> Result<bool, StoreError> {
    use crate::db::hub_ids::{HUB_ID_BASE, HUB_ID_MAX};
    let covered = match crate::db::coverage_intervals_for_vehicle(connection, vehicle_key) {
        Ok(intervals) => !intervals.is_empty(),
        Err(_) => true,
    };
    let hub_rows: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM materialised_drives
                            WHERE vehicle_id = ?1 AND drive_id BETWEEN ?2 AND ?3)
                 OR EXISTS(SELECT 1 FROM materialised_positions
                            WHERE vehicle_id = ?1 AND position_id BETWEEN ?2 AND ?3)
                 OR EXISTS(SELECT 1 FROM materialised_charges
                            WHERE vehicle_id = ?1 AND charge_id BETWEEN ?2 AND ?3)
                 OR EXISTS(SELECT 1 FROM materialised_charge_samples
                            WHERE vehicle_id = ?1 AND sample_id BETWEEN ?2 AND ?3)
                 OR EXISTS(SELECT 1 FROM import_seam_links WHERE vehicle_id = ?1)",
            params![vehicle_key, HUB_ID_BASE, HUB_ID_MAX],
            |row| row.get(0),
        )
        .map_err(StoreError::Query)?;
    Ok(covered || hub_rows)
}
