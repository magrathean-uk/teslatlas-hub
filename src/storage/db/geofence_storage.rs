// SPDX-License-Identifier: AGPL-3.0-only

// Fence save, delete and import reconciliation (C8 R1 to C8 R3, C8 R9, C8 R10,
// C8 R16, C8 R16a, C8 R17, C8 R28, C8 R47 and C8 R51). This file is `include!`d into
// `db.rs`, so every item is visible to the import finalisation and the tests.

/// A stored fence as the record the validation function reads for an edit: every
/// geometry and tariff field present (C8 R1, C8 table 5.2).
fn stored_fence_record(
    fence: &crate::lifecycle::GeofenceFence,
) -> crate::source_db_projection::SourceDbGeofence {
    crate::source_db_projection::SourceDbGeofence {
        id: fence.source_geofence_id,
        name: fence.name.clone(),
        latitude: Some(fence.latitude),
        longitude: Some(fence.longitude),
        radius_m: Some(fence.radius_m),
        billing_type: fence.billing_type,
        cost_per_unit: fence.cost_per_unit,
        session_fee: fence.session_fee,
    }
}

/// Whether the validated values equal a stored fence in all seven fields, exactly
/// (C8 R9 item 1, C8 R10).
fn validated_equals_stored(
    validated: &crate::geo::geofence_validation::ValidatedGeofence,
    stored: &crate::lifecycle::GeofenceFence,
) -> bool {
    validated.name == stored.name
        && validated.latitude == stored.latitude
        && validated.longitude == stored.longitude
        && validated.radius_m == stored.radius_m
        && validated.billing_type == stored.billing_type
        && validated.cost_per_unit == stored.cost_per_unit
        && validated.session_fee == stored.session_fee
}

/// The smallest id among `others` with the validated radius and a centre within
/// `SAME_PLACE_METRES` of the validated one (C8 R9 item 2). `others` is ordered by
/// id, so the first hit is the smallest.
fn same_place_fence_id<'a>(
    validated: &crate::geo::geofence_validation::ValidatedGeofence,
    others: impl Iterator<Item = &'a crate::lifecycle::GeofenceFence>,
) -> Option<i64> {
    let centre =
        crate::geo::location::Wgs84Point::new(validated.latitude, validated.longitude).ok()?;
    others
        .filter(|other| other.radius_m == validated.radius_m)
        .find(|other| {
            crate::geo::location::Wgs84Point::new(other.latitude, other.longitude).is_ok_and(
                |point| {
                    crate::geo::location::great_circle_distance_m(centre, point)
                        <= crate::geo::location::SAME_PLACE_METRES
                },
            )
        })
        .map(|other| other.source_geofence_id)
}

/// The id a created fence takes (C8 R3): the larger of `FIRST_HUB_GEOFENCE_ID`,
/// one above the vehicle's id mark and one above its largest stored id. `largest`
/// is that stored id, read in the same transaction.
fn next_hub_geofence_id(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    largest: Option<i64>,
) -> Result<i64, StoreError> {
    let mark: Option<i64> = transaction
        .query_row(
            "SELECT last_hub_id FROM geofence_id_marks WHERE vehicle_id = ?1",
            params![vehicle_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(StoreError::Query)?;
    let exhausted = || {
        StoreError::GeofenceRejected(
            crate::geo::geofence_validation::GeofenceRejection::IdsExhausted,
        )
    };
    let after_mark = mark.map_or(Ok(0), |value| value.checked_add(1).ok_or_else(exhausted))?;
    let after_largest =
        largest.map_or(Ok(0), |value| value.checked_add(1).ok_or_else(exhausted))?;
    Ok(crate::geo::location::FIRST_HUB_GEOFENCE_ID
        .max(after_mark)
        .max(after_largest))
}

impl HubStore {
    /// Save one fence from the Hub (C8 R1 to C8 R10, C8 table 5.1). Inside one
    /// IMMEDIATE transaction it reads the target row (`Some(id)` must exist, C8
    /// R2) and the vehicle's other fences, validates the record against that
    /// row (omitted radius and tariff values keep the stored ones, C8 R7;
    /// `clear_tariff` drops the stored tariff first), then writes. An edit
    /// whose validated values equal the stored row, and a create whose values
    /// equal an existing fence, write nothing and return that fence. A create
    /// takes an id from `FIRST_HUB_GEOFENCE_ID` upward that is never reused
    /// (C8 R3); a Hub write sets `source_owned` to 0 (C8 R16a). The export is
    /// marked once per committed write (C8 R28). No fence entity mutation is
    /// recorded (C8 R32).
    pub fn save_geofence(
        &self,
        vehicle_id: Uuid,
        source_geofence_id: Option<i64>,
        mut geofence: crate::source_db_projection::SourceDbGeofence,
        clear_tariff: bool,
    ) -> Result<crate::source_db_projection::SourceDbGeofence, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let vehicle_exists = transaction
            .query_row(
                "SELECT 1 FROM vehicles WHERE vehicle_id = ?1",
                params![vehicle_id.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(StoreError::Query)?
            .is_some();
        if !vehicle_exists {
            return Err(StoreError::UnknownVehicle(vehicle_id));
        }
        let fences = load_geofence_fences(&transaction, vehicle_id)?;
        let target = match source_geofence_id {
            Some(id) => Some(
                fences
                    .iter()
                    .find(|fence| fence.source_geofence_id == id)
                    .ok_or(StoreError::UnknownGeofence(id))?,
            ),
            None => None,
        };
        let existing = target.map(stored_fence_record);
        let validated = crate::geo::geofence_validation::validate_geofence_save(
            &geofence,
            existing.as_ref(),
            clear_tariff,
        )
        .map_err(StoreError::GeofenceRejected)?;
        let rejected_place = |existing_id: i64| {
            StoreError::GeofenceRejected(
                crate::geo::geofence_validation::GeofenceRejection::DuplicatePlace { existing_id },
            )
        };
        let id = if let Some(stored) = target {
            if validated_equals_stored(&validated, stored) {
                // C8 R10: nothing to write; the dropped transaction rolls back.
                return Ok(stored_fence_record(stored));
            }
            let moved = validated.latitude != stored.latitude
                || validated.longitude != stored.longitude
                || validated.radius_m != stored.radius_m;
            if moved
                && let Some(existing_id) = same_place_fence_id(
                    &validated,
                    fences
                        .iter()
                        .filter(|fence| fence.source_geofence_id != stored.source_geofence_id),
                )
            {
                return Err(rejected_place(existing_id));
            }
            transaction
                .execute(
                    "UPDATE geofences SET
                        name = ?3, latitude = ?4, longitude = ?5, radius_m = ?6,
                        billing_type = ?7, cost_per_unit = ?8, session_fee = ?9,
                        source_owned = 0
                     WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
                    params![
                        vehicle_id.to_string(),
                        stored.source_geofence_id,
                        &validated.name,
                        validated.latitude,
                        validated.longitude,
                        validated.radius_m,
                        validated
                            .billing_type
                            .map(crate::hub_pack::GeofenceBillingType::as_str),
                        validated.cost_per_unit,
                        validated.session_fee,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            stored.source_geofence_id
        } else {
            if let Some(same) = fences
                .iter()
                .find(|fence| validated_equals_stored(&validated, fence))
            {
                // C8 R9 item 1: a retried create returns the fence it made.
                return Ok(stored_fence_record(same));
            }
            if let Some(existing_id) = same_place_fence_id(&validated, fences.iter()) {
                return Err(rejected_place(existing_id));
            }
            let id = next_hub_geofence_id(
                &transaction,
                vehicle_id,
                fences.last().map(|fence| fence.source_geofence_id),
            )?;
            transaction
                .execute(
                    "INSERT INTO geofence_id_marks(vehicle_id, last_hub_id) VALUES (?1, ?2)
                     ON CONFLICT(vehicle_id) DO UPDATE SET last_hub_id = excluded.last_hub_id",
                    params![vehicle_id.to_string(), id],
                )
                .map_err(StoreError::LifecycleWrite)?;
            transaction
                .execute(
                    "INSERT INTO geofences(
                        vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                        billing_type, cost_per_unit, session_fee, source_owned
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)",
                    params![
                        vehicle_id.to_string(),
                        id,
                        &validated.name,
                        validated.latitude,
                        validated.longitude,
                        validated.radius_m,
                        validated
                            .billing_type
                            .map(crate::hub_pack::GeofenceBillingType::as_str),
                        validated.cost_per_unit,
                        validated.session_fee,
                    ],
                )
                .map_err(StoreError::LifecycleWrite)?;
            id
        };
        let after = load_geofence_fences(&transaction, vehicle_id)?;
        relabel_materialised_locations_in_transaction(&transaction, vehicle_id, &fences, &after)?;
        mark_export_dirty_in_transaction(&transaction, vehicle_id)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        // The returned record is what was stored: the validated values.
        geofence.id = id;
        geofence.name = validated.name;
        geofence.latitude = Some(validated.latitude);
        geofence.longitude = Some(validated.longitude);
        geofence.radius_m = Some(validated.radius_m);
        geofence.billing_type = validated.billing_type;
        geofence.cost_per_unit = validated.cost_per_unit;
        geofence.session_fee = validated.session_fee;
        Ok(geofence)
    }

    /// Delete one fence (C8 R17). An unknown id is `UnknownGeofence` and writes
    /// nothing. Otherwise one IMMEDIATE transaction deletes the row, records the
    /// id in `deleted_imported_geofences` when it is an imported id (below
    /// `FIRST_HUB_GEOFENCE_ID`), relabels and marks the export dirty (C8 R28).
    pub fn delete_geofence(
        &self,
        vehicle_id: Uuid,
        source_geofence_id: i64,
    ) -> Result<(), StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let before = load_geofence_fences(&transaction, vehicle_id)?;
        let deleted = transaction
            .execute(
                "DELETE FROM geofences WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
                params![vehicle_id.to_string(), source_geofence_id],
            )
            .map_err(StoreError::LifecycleWrite)?;
        if deleted == 0 {
            return Err(StoreError::UnknownGeofence(source_geofence_id));
        }
        if source_geofence_id < crate::geo::location::FIRST_HUB_GEOFENCE_ID {
            // The tombstone stops a later import from bringing the fence back
            // (C8 R16a); it commits or rolls back with the delete.
            transaction
                .execute(
                    "INSERT INTO deleted_imported_geofences(vehicle_id, source_geofence_id)
                     VALUES (?1, ?2)
                     ON CONFLICT(vehicle_id, source_geofence_id) DO NOTHING",
                    params![vehicle_id.to_string(), source_geofence_id],
                )
                .map_err(StoreError::LifecycleWrite)?;
        }
        let after = load_geofence_fences(&transaction, vehicle_id)?;
        relabel_materialised_locations_in_transaction(&transaction, vehicle_id, &before, &after)?;
        mark_export_dirty_in_transaction(&transaction, vehicle_id)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(())
    }

    /// Reconcile the vehicle's fences with an import's fence rows (C8 R13, C8
    /// R14, C8 R16a, C8 R47, C8 R51). This opens the IMMEDIATE transaction and
    /// calls [`upsert_geofences_in_transaction`], the one body the import
    /// finalisation uses too. Ids in the slice must be unique.
    pub fn upsert_geofences(
        &self,
        vehicle_id: Uuid,
        geofences: &[crate::source_db_projection::SourceDbGeofence],
    ) -> Result<GeofenceUpsertReport, StoreError> {
        if vehicle_id.is_nil() {
            return Err(StoreError::NilVehicleId);
        }
        let mut connection = self.open()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::Begin)?;
        let report = upsert_geofences_in_transaction(&transaction, vehicle_id, geofences)?;
        transaction.commit().map_err(StoreError::LifecycleWrite)?;
        Ok(report)
    }
}

/// A stored fence with an imported id and whether its values are still those of
/// the last import (C8 R16a).
struct StoredImportedFence {
    fence: crate::lifecycle::GeofenceFence,
    source_owned: bool,
}

/// The vehicle's stored fences with an imported id, by id.
fn stored_imported_fences(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<std::collections::BTreeMap<i64, StoredImportedFence>, StoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT source_geofence_id, name, latitude, longitude, radius_m, billing_type,
                    cost_per_unit, session_fee, source_owned
             FROM geofences WHERE vehicle_id = ?1 AND source_geofence_id < ?2
             ORDER BY source_geofence_id",
        )
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(
            params![
                vehicle_id.to_string(),
                crate::geo::location::FIRST_HUB_GEOFENCE_ID
            ],
            |row| {
                Ok(StoredImportedFence {
                    fence: crate::lifecycle::GeofenceFence {
                        source_geofence_id: row.get(0)?,
                        name: row.get(1)?,
                        latitude: row.get(2)?,
                        longitude: row.get(3)?,
                        radius_m: row.get(4)?,
                        billing_type: row
                            .get::<_, Option<String>>(5)?
                            .and_then(|value| value.parse().ok()),
                        cost_per_unit: row.get(6)?,
                        session_fee: row.get(7)?,
                    },
                    source_owned: row.get::<_, i64>(8)? == 1,
                })
            },
        )
        .map_err(StoreError::Query)?;
    rows.map(|row| {
        row.map(|stored| (stored.fence.source_geofence_id, stored))
            .map_err(StoreError::Query)
    })
    .collect()
}

/// The imported ids the owner deleted in the Hub (C8 R17).
fn deleted_imported_geofence_ids(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
) -> Result<std::collections::BTreeSet<i64>, StoreError> {
    let mut statement = transaction
        .prepare("SELECT source_geofence_id FROM deleted_imported_geofences WHERE vehicle_id = ?1")
        .map_err(StoreError::Query)?;
    let rows = statement
        .query_map(params![vehicle_id.to_string()], |row| row.get(0))
        .map_err(StoreError::Query)?;
    rows.map(|row| row.map_err(StoreError::Query)).collect()
}

/// The one reconciliation body of both import paths (C8 R51): `HubStore::
/// upsert_geofences` and the six import finalisations call it. Duplicate ids in
/// the slice are refused before anything is written (`InvalidGeofence`). The rows
/// are then taken in ascending id order and each ends in one list of the report
/// (C8 table 5.4): not activated (nothing written; an existing row stays), the
/// id deleted in the Hub (nothing written), inserted, unchanged (only
/// `source_owned` may become 1), updated (a source-owned row whose values
/// differ) or kept with the Hub's values (a Hub-edited row whose values
/// differ). Stored fences the slice does not mention are never changed or
/// deleted. When a fence was inserted or updated, the stored drives and charges
/// are relabelled for the change of the fence set (C8 R16, `report.relabel`;
/// rows the caller wrote earlier in the same transaction are relabelled too, a
/// known limit of C8 R51 closed by G2-4), and the export is marked once; a
/// change of `source_owned` alone neither relabels nor marks (C8 R28, C8 R47). No fence
/// entity mutation is recorded (C8 R32). A caller that already marked the export
/// in the same transaction (the promotion of an open session does) raises the
/// mark a second time when a fence is inserted or updated; a counter only needs
/// the one mark that is still due.
fn upsert_geofences_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    geofences: &[crate::source_db_projection::SourceDbGeofence],
) -> Result<GeofenceUpsertReport, StoreError> {
    let mut rows: Vec<&crate::source_db_projection::SourceDbGeofence> = geofences.iter().collect();
    rows.sort_by_key(|row| row.id);
    if rows.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return Err(StoreError::InvalidGeofence);
    }
    let before = load_geofence_fences(transaction, vehicle_id)?;
    let stored = stored_imported_fences(transaction, vehicle_id)?;
    let deleted = deleted_imported_geofence_ids(transaction, vehicle_id)?;
    let mut report = GeofenceUpsertReport {
        source_rows: u64::try_from(rows.len()).unwrap_or(u64::MAX),
        ..GeofenceUpsertReport::default()
    };
    for row in &rows {
        let (latitude, longitude, radius_m) = match row.import_activation() {
            Ok(geometry) => geometry,
            Err(reason) => {
                report.inactive.entry(reason).or_default().push(row.id);
                continue;
            }
        };
        let tariff = ImportedGeofenceTariff::of(row);
        if deleted.contains(&row.id) {
            report.deleted_in_hub.push(row.id);
            continue;
        }
        let note_tariff = |report: &mut GeofenceUpsertReport, id: i64| {
            if tariff.not_finite {
                report.tariff_not_finite.push(id);
            }
        };
        let vehicle_key = vehicle_id.to_string();
        let billing_type = tariff.billing_type_text();
        let values = params![
            vehicle_key,
            row.id,
            row.name,
            latitude,
            longitude,
            radius_m,
            billing_type,
            tariff.cost_per_unit,
            tariff.session_fee,
        ];
        let Some(existing) = stored.get(&row.id) else {
            transaction
                .execute(
                    "INSERT INTO geofences(
                        vehicle_id, source_geofence_id, name, latitude, longitude, radius_m,
                        billing_type, cost_per_unit, session_fee, source_owned
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)",
                    values,
                )
                .map_err(StoreError::LifecycleWrite)?;
            report.inserted.push(row.id);
            note_tariff(&mut report, row.id);
            continue;
        };
        let held = &existing.fence;
        let equal = held.name == row.name
            && held.latitude == latitude
            && held.longitude == longitude
            && held.radius_m == radius_m
            && held.billing_type == tariff.billing_type
            && held.cost_per_unit == tariff.cost_per_unit
            && held.session_fee == tariff.session_fee;
        if equal {
            if !existing.source_owned {
                transaction
                    .execute(
                        "UPDATE geofences SET source_owned = 1
                         WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
                        params![vehicle_key, row.id],
                    )
                    .map_err(StoreError::LifecycleWrite)?;
            }
            report.unchanged.push(row.id);
            note_tariff(&mut report, row.id);
        } else if existing.source_owned {
            transaction
                .execute(
                    "UPDATE geofences SET
                        name = ?3, latitude = ?4, longitude = ?5, radius_m = ?6,
                        billing_type = ?7, cost_per_unit = ?8, session_fee = ?9
                     WHERE vehicle_id = ?1 AND source_geofence_id = ?2",
                    values,
                )
                .map_err(StoreError::LifecycleWrite)?;
            report.updated.push(row.id);
            note_tariff(&mut report, row.id);
        } else {
            report.kept_hub_values.push(row.id);
        }
    }
    report.absent_from_source = stored
        .keys()
        .copied()
        .filter(|id| rows.binary_search_by_key(id, |row| row.id).is_err())
        .collect();
    if !report.inserted.is_empty() || !report.updated.is_empty() {
        // C8 R16: the relabel covers every row stored when it runs, and runs
        // only when a fence's presence, name, centre or radius changed (C8
        // R53). A finalisation site that wrote rows of its own before this
        // call has those relabelled too (C8 R51, a known limit closed by
        // G2-4, C8 T15b).
        let after = load_geofence_fences(transaction, vehicle_id)?;
        report.relabel = relabel_materialised_locations_in_transaction(
            transaction,
            vehicle_id,
            &before,
            &after,
        )?;
        mark_export_dirty_in_transaction(transaction, vehicle_id)?;
    }
    Ok(report)
}
