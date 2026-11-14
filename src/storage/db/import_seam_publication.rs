// SPDX-License-Identifier: AGPL-3.0-only

// The import seam staged ahead of the packs and published with them
// (M03-T19, review REV-CX-M03-T16 finding 4). Nothing of the seam is applied
// before publication: the planner works the seam out in a transaction it
// rolls back, with the sync it would journal, and the importer writes that
// sync as live packs ahead of its own. Publication applies the seam, proves
// it journals exactly the staged sync, and makes both visible under one head.

/// One live delta the import seam publishes with its import: a run of the
/// vehicle's waiting sync mutations, in revision order, and the typed rows
/// they publish.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StagedSeamSync {
    pub(crate) from_revision: i64,
    pub(crate) to_revision: i64,
    /// The rows, as live publication builds them; the importer sets the
    /// sequence and parent when it writes the pack.
    pub(crate) rows: ProjectionDelta,
}

impl StagedSeamSync {
    /// The sync mutations it publishes; the sequence advances by as many.
    pub(crate) fn mutation_count(&self) -> u64 {
        u64::try_from(self.to_revision - self.from_revision + 1).unwrap_or(0)
    }
}

/// The seam a direct successor stages ahead of its packs: the part of the
/// plan the packs carry, and the sync published with them.
#[derive(Debug, Clone)]
pub(crate) struct StagedImportSeam {
    pub(crate) ahead: ImportSeamAhead,
    pub(crate) sync: Vec<StagedSeamSync>,
}

/// The seam a publication applies (M03-T16, M03-T19): the pack plan digest it
/// must find again, the source charges, and the staged sync it publishes
/// (`None` when the seam was applied ahead, as the store's own tests do).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImportSeamPublication<'a> {
    pub(crate) digest: [u8; 32],
    pub(crate) charges: &'a [crate::hub_pack::ProjectionCharge],
    pub(crate) staged: Option<&'a [StagedSeamSync]>,
}

/// The stored rows a vehicle's physical 1.4 target takes from the Hub's
/// side once the seam is applied (M03-T21): every stored drive and charge
/// (the importer keeps those the plan does not pack from the source) and
/// every Hub-range position and charge sample.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct ImportSeamStoredRows {
    pub(crate) drives: Vec<ProjectionDrive>,
    pub(crate) charges: Vec<crate::hub_pack::ProjectionCharge>,
    pub(crate) positions: Vec<crate::hub_pack::ProjectionPosition>,
    pub(crate) charge_samples: Vec<crate::hub_pack::ProjectionChargeSample>,
}

/// The most sync mutations one staged live delta publishes, as one live
/// publication turn claims.
const STAGED_SEAM_SYNC_LIMIT: usize = 10_000;

impl HubStore {
    /// Stages the import seam of a direct successor (M03-T19): the seam
    /// [`HubStore::apply_import_seam_ahead_of_packs`] would apply, worked out
    /// in a transaction that is rolled back, with every sync mutation the
    /// vehicle would then have waiting, as the typed rows they publish.
    /// Nothing in the store changes.
    ///
    /// With `decided`, `None` comes back when the store no longer gives the
    /// dry run the routing decision rested on (DEC-103).
    pub(crate) fn stage_import_seam_ahead_of_packs(
        &self,
        _publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        car_id: i64,
        drives: &[ProjectionDrive],
        charges: &[crate::hub_pack::ProjectionCharge],
        decided: Option<&ImportSeamDryRun>,
    ) -> Result<Option<StagedImportSeam>, StoreError> {
        let binding = self.v2_projection_binding(vehicle_id)?;
        let mut staged = None;
        self.import_seam_ahead_of_packs(vehicle_id, car_id, drives, charges, |transaction, run| {
            if decided.is_none_or(|decided| decided == run) {
                staged = Some(StagedImportSeam {
                    ahead: run.ahead.clone(),
                    sync: staged_seam_sync_in_transaction(transaction, vehicle_id, &binding)?,
                });
            }
            Ok(false)
        })?;
        Ok(staged)
    }

    /// The rows of [`ImportSeamStoredRows`] for one vehicle, as the store
    /// holds them now, in id order.
    pub(crate) fn import_seam_stored_rows(
        &self,
        vehicle_id: Uuid,
    ) -> Result<ImportSeamStoredRows, StoreError> {
        use crate::db::hub_ids::{HUB_ID_BASE, HUB_ID_MAX};
        fn rows<T: DeserializeOwned>(
            connection: &Connection,
            sql: &str,
            vehicle: &str,
        ) -> Result<Vec<T>, StoreError> {
            let mut statement = connection.prepare(sql).map_err(StoreError::Query)?;
            let payloads = statement
                .query_map(params![vehicle, HUB_ID_BASE, HUB_ID_MAX], |row| row.get::<_, String>(0))
                .map_err(StoreError::Query)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::Query)?;
            payloads
                .iter()
                .map(|json| serde_json::from_str(json).map_err(StoreError::DeserializeLifecycleRow))
                .collect()
        }
        let connection = self.open_read_only_connection()?;
        let vehicle = vehicle_id.to_string();
        Ok(ImportSeamStoredRows {
            drives: rows(
                &connection,
                "SELECT drive_json FROM materialised_drives
                  WHERE vehicle_id = ?1 AND ?2 <= ?3 ORDER BY drive_id",
                &vehicle,
            )?,
            charges: rows(
                &connection,
                "SELECT charge_json FROM materialised_charges
                  WHERE vehicle_id = ?1 AND ?2 <= ?3 ORDER BY charge_id",
                &vehicle,
            )?,
            positions: rows(
                &connection,
                "SELECT position_json FROM materialised_positions
                  WHERE vehicle_id = ?1 AND position_id BETWEEN ?2 AND ?3 ORDER BY position_id",
                &vehicle,
            )?,
            charge_samples: rows(
                &connection,
                "SELECT sample_json FROM materialised_charge_samples
                  WHERE vehicle_id = ?1 AND sample_id BETWEEN ?2 AND ?3 ORDER BY sample_id",
                &vehicle,
            )?,
        })
    }

    /// Whether the vehicle has any of the seam: coverage, a window, a
    /// Hub-range row or a recorded decision. A publication route that does
    /// not plan the seam is refused for such a vehicle.
    pub(crate) fn import_seam_present(&self, vehicle_id: Uuid) -> Result<bool, StoreError> {
        let connection = self.open_read_only_connection()?;
        import_seam_present_in_connection(&connection, &vehicle_id.to_string())
    }

    /// The import seam's lens for a successor capture of one vehicle
    /// (M03-T19), read before the source is: its coverage, its Hub sessions
    /// and its recorded seam decisions. A vehicle whose coverage cannot be
    /// read yet gets an empty lens; its plan refuses later.
    pub(crate) fn import_seam_capture_lens(
        &self,
        vehicle_id: Uuid,
    ) -> Result<crate::import::source_db::seam::SeamCaptureLens, StoreError> {
        use crate::import::source_db::seam::{HubCoverage, SeamCaptureLens, SeamLinks};
        let connection = self.open()?;
        let vehicle_key = vehicle_id.to_string();
        let Ok(coverage) = HubCoverage::for_store(&connection, &vehicle_key) else {
            return Ok(SeamCaptureLens::default());
        };
        let prior_drives = durable_prior_imported_drive_ids_in_transaction(&connection, &vehicle_key)?;
        let mut hub =
            import_seam_hub_drives_in_connection(&connection, &vehicle_key, &coverage, &prior_drives)?
                .rows;
        let prior_charges = durable_prior_imported_ids_in_connection(
            &connection,
            &vehicle_key,
            SourceDbProjectionStateEntity::Charge,
        )?;
        import_seam_hub_charges_in_connection(
            &connection,
            &vehicle_key,
            &coverage,
            &prior_charges,
            &mut hub,
        )?;
        let links = SeamLinks {
            drives: seam_links_in_connection(&connection, &vehicle_key, "drive")?,
            charges: seam_links_in_connection(&connection, &vehicle_key, "charge")?,
        };
        Ok(SeamCaptureLens::new(&coverage, &hub, &links))
    }

    /// Reserves the next full-snapshot sequence above `floor` as well: the
    /// staged seam's live deltas take the sequences up to `floor` before
    /// they are catalogued.
    pub(crate) fn reserve_full_snapshot_sequence_after(
        &self,
        publication_gate: &PublicationGate,
        vehicle_id: Uuid,
        floor: u64,
    ) -> Result<u64, StoreError> {
        let floor_i64 = i64::try_from(floor).map_err(|_| StoreError::SequenceTooLarge)?;
        let next = floor_i64.checked_add(1).ok_or(StoreError::SequenceExhausted)?;
        self.open()?
            .execute(
                "INSERT INTO vehicle_snapshot_sequences (vehicle_id, next_sequence)
                 VALUES (?1, ?2)
                 ON CONFLICT(vehicle_id) DO UPDATE SET
                    next_sequence = MAX(vehicle_snapshot_sequences.next_sequence, excluded.next_sequence)",
                params![vehicle_id.to_string(), next],
            )
            .map_err(StoreError::Query)?;
        let reserved = self.reserve_next_full_snapshot_sequence(publication_gate, vehicle_id)?;
        if reserved <= floor {
            return Err(StoreError::LineageCatalogConflict);
        }
        Ok(reserved)
    }

    /// The importer's successor publication with its staged seam (M03-T19):
    /// as [`HubStore::finalize_import_generation_delta_successors_with_projection_state_and_seam_plan`],
    /// and in the same transaction the seam is applied, its journal proved to
    /// be `staged`, and the staged live deltas (the first of `deltas`) are
    /// catalogued with the import's, under one head.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize_import_generation_delta_successors_with_staged_seam(
        &self,
        run_id: Uuid,
        source_id: Uuid,
        vehicle_id: Uuid,
        car_id: i64,
        updated_at_ms: i64,
        deltas: &[LineageDelta],
        cursor_key: &CursorKey,
        terminal_cursor: &OpaqueCursor,
        fingerprint: Sha256Digest,
        geofences: &[crate::source_db_projection::SourceDbGeofence],
        projection_state: &SourceDbProjectionState,
        materialised_car: &ProjectionCar,
        materialised_drives: &[ProjectionDrive],
        materialised_charges: &[crate::hub_pack::ProjectionCharge],
        seam_digest: [u8; 32],
        staged: &[StagedSeamSync],
    ) -> Result<(), StoreError> {
        if deltas.len() <= staged.len() {
            return Err(StoreError::LineageCatalogConflict);
        }
        self.finalize_import_generation_delta_successors_with_projection_state_inner(
            run_id,
            source_id,
            vehicle_id,
            car_id,
            updated_at_ms,
            deltas,
            cursor_key,
            terminal_cursor,
            fingerprint,
            geofences,
            projection_state,
            Some((materialised_car, materialised_drives)),
            false,
            Some(ImportSeamPublication {
                digest: seam_digest,
                charges: materialised_charges,
                staged: Some(staged),
            }),
        )
    }
}

/// Every waiting sync mutation of the vehicle, in revision order, as the
/// staged live deltas that publish them (rows read through `connection`).
fn staged_seam_sync_in_transaction(
    connection: &Connection,
    vehicle_id: Uuid,
    binding: &ProjectionBinding,
) -> Result<Vec<StagedSeamSync>, StoreError> {
    let mutations: Vec<SyncMutation> = {
        let mut statement = connection
            .prepare(
                "SELECT revision, entity, entity_id, car_id, operation, payload_json
                   FROM sync_mutations WHERE vehicle_id = ?1 AND published = 0
                  ORDER BY revision",
            )
            .map_err(StoreError::Query)?;
        statement
            .query_map(params![vehicle_id.to_string()], |row| {
                Ok(SyncMutation {
                    vehicle_id,
                    revision: row.get(0)?,
                    entity: row.get(1)?,
                    entity_id: row.get(2)?,
                    car_id: row.get(3)?,
                    operation: row.get(4)?,
                    payload_json: row.get(5)?,
                })
            })
            .map_err(StoreError::Query)?
            .collect::<Result<_, _>>()
            .map_err(StoreError::Query)?
    };
    let mut staged = Vec::new();
    for chunk in mutations.chunks(STAGED_SEAM_SYNC_LIMIT) {
        if chunk
            .windows(2)
            .any(|pair| pair[1].revision != pair[0].revision + 1)
        {
            return Err(StoreError::ImportGenerationConflict);
        }
        let (Some(first), Some(last)) = (chunk.first(), chunk.last()) else {
            continue;
        };
        staged.push(StagedSeamSync {
            from_revision: first.revision,
            to_revision: last.revision,
            rows: projection_delta_for_mutations_in(
                connection,
                chunk,
                binding.clone(),
                SequenceRange {
                    from_exclusive: 0,
                    to_inclusive: 0,
                },
                Sha256Digest::from_bytes([0; 32]),
            )?,
        });
    }
    Ok(staged)
}

/// Publication's half of the seam before its deltas are catalogued
/// (M03-T19): applies the seam's Hub side, then proves the journal holds
/// exactly the staged sync, run for run and row for row, each run carried by
/// the delta at its index with one sequence per mutation. Any difference
/// refuses ([`StoreError::ImportGenerationConflict`]): the store changed
/// after the seam was staged.
fn apply_staged_import_seam_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    drives: &[ProjectionDrive],
    seam: ImportSeamPublication<'_>,
    deltas: &[LineageDelta],
    binding: &ProjectionBinding,
) -> Result<(), StoreError> {
    let Some(staged) = seam.staged else {
        return Ok(());
    };
    let vehicle_key = vehicle_id.to_string();
    let prior = durable_prior_imported_drive_ids_in_transaction(transaction, &vehicle_key)?;
    let (_, plan) =
        import_seam_plan_in_transaction(transaction, vehicle_id, drives, seam.charges, &prior)?;
    if plan.changes_the_store() {
        apply_import_seam_hub_side_in_transaction(transaction, vehicle_id, &plan)?;
        apply_import_seam_to_charges_in_transaction(
            transaction,
            vehicle_id,
            &plan,
            seam.charges,
            false,
        )?;
    }
    let journal = staged_seam_sync_in_transaction(transaction, vehicle_id, binding)?;
    if journal.len() != staged.len() || deltas.len() < staged.len() {
        return Err(StoreError::ImportGenerationConflict);
    }
    for ((found, expected), delta) in journal.into_iter().zip(staged).zip(deltas) {
        let mut rows = found.rows;
        rows.binding = expected.rows.binding.clone();
        rows.sequence = expected.rows.sequence;
        rows.parent_digest = expected.rows.parent_digest;
        if found.from_revision != expected.from_revision
            || found.to_revision != expected.to_revision
            || rows != expected.rows
        {
            return Err(StoreError::ImportGenerationConflict);
        }
        if delta.to_sequence.checked_sub(delta.from_sequence) != Some(expected.mutation_count()) {
            return Err(StoreError::LineageCatalogConflict);
        }
    }
    Ok(())
}

/// Publication's half of the seam once its deltas are catalogued
/// (M03-T19): the staged sync is published under the staged live deltas.
fn publish_staged_import_seam_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    seam: Option<ImportSeamPublication<'_>>,
    deltas: &[LineageDelta],
) -> Result<(), StoreError> {
    let Some(staged) = seam.and_then(|seam| seam.staged) else {
        return Ok(());
    };
    for (expected, delta) in staged.iter().zip(deltas) {
        transaction
            .execute(
                "UPDATE sync_mutations SET published = 1, claimed_until_ms = 0
                  WHERE vehicle_id = ?1 AND revision BETWEEN ?2 AND ?3",
                params![
                    vehicle_id.to_string(),
                    expected.from_revision,
                    expected.to_revision
                ],
            )
            .map_err(StoreError::LineageCatalog)?;
        insert_live_delta_span_in_transaction(
            transaction,
            &SyncMutationClaim {
                vehicle_id,
                from_revision: expected.from_revision,
                to_revision: expected.to_revision,
                mutations: Vec::new(),
            },
            delta,
        )?;
    }
    Ok(())
}

/// Publication plans the seam again (M03-T16) and refuses unless the pack
/// plan is the one the packs were written from; returns the plan for the
/// charges' publication.
fn replan_import_seam_in_transaction<'a>(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    car_id: i64,
    drives: &[ProjectionDrive],
    seam: Option<ImportSeamPublication<'a>>,
) -> Result<
    Option<(
        crate::import::source_db::seam::SeamPlan,
        &'a [crate::hub_pack::ProjectionCharge],
    )>,
    StoreError,
> {
    let Some(seam) = seam else {
        return Ok(None);
    };
    validate_import_seam_charges(seam.charges, car_id)?;
    let prior = durable_prior_imported_drive_ids_in_transaction(transaction, &vehicle_id.to_string())?;
    let (coverage, plan) =
        import_seam_plan_in_transaction(transaction, vehicle_id, drives, seam.charges, &prior)?;
    let planned = crate::import::source_db::seam::SeamPackPlan::for_plan(&coverage, &plan);
    if planned.digest() != seam.digest {
        return Err(StoreError::ImportGenerationConflict);
    }
    Ok(Some((plan, seam.charges)))
}
