// SPDX-License-Identifier: AGPL-3.0-only

// Relabel of stored drives and charges after a fence change (C8 R16, C8 R23 to
// C8 R28, C8 R31, C8 R33 and C8 R53). This file is `include!`d into `db.rs`, so
// every item is visible to the fence save, delete and import code and to the
// tests.

/// Where a fence name goes when the fence set changes (C8 R25).
#[derive(Debug, Clone, PartialEq, Eq)]
enum NameSuccessor {
    /// Every fence of that name is still present and carries this one name.
    Name(String),
    /// No fence of that name is present any more.
    Removed,
    /// The fences of that name fared differently.
    Ambiguous,
}

/// The pure labelling rule of one fence change (C8 R25, C8 table 5.3): the
/// fence set before and after the write and the exact-name successor map.
struct RelabelRule<'a> {
    before: &'a [crate::lifecycle::GeofenceFence],
    after: &'a [crate::lifecycle::GeofenceFence],
    successors: std::collections::BTreeMap<&'a str, NameSuccessor>,
}

impl<'a> RelabelRule<'a> {
    fn new(
        before: &'a [crate::lifecycle::GeofenceFence],
        after: &'a [crate::lifecycle::GeofenceFence],
    ) -> Self {
        let after_names: std::collections::BTreeMap<i64, &str> = after
            .iter()
            .map(|fence| (fence.source_geofence_id, fence.name.as_str()))
            .collect();
        let mut groups: std::collections::BTreeMap<&str, Vec<i64>> =
            std::collections::BTreeMap::new();
        for fence in before {
            groups
                .entry(fence.name.as_str())
                .or_default()
                .push(fence.source_geofence_id);
        }
        let successors = groups
            .into_iter()
            .map(|(name, ids)| {
                let kept: Vec<&str> = ids
                    .iter()
                    .filter_map(|id| after_names.get(id).copied())
                    .collect();
                let successor = match kept.first() {
                    None => NameSuccessor::Removed,
                    Some(first)
                        if kept.len() == ids.len() && kept.iter().all(|next| next == first) =>
                    {
                        NameSuccessor::Name((*first).to_owned())
                    }
                    Some(_) => NameSuccessor::Ambiguous,
                };
                (name, successor)
            })
            .collect();
        Self {
            before,
            after,
            successors,
        }
    }

    /// The successor of a stored label: the name itself when no fence of the
    /// set before carried it.
    fn successor(&self, name: &str) -> NameSuccessor {
        self.successors
            .get(name)
            .cloned()
            .unwrap_or_else(|| NameSuccessor::Name(name.to_owned()))
    }

    /// The label an endpoint takes (C8 R25): `point` is the stored point,
    /// `stored` the stored label.
    fn label(&self, point: Option<(f64, f64)>, stored: Option<&str>) -> Option<String> {
        let valid = point.filter(|(latitude, longitude)| {
            crate::geo::location::Wgs84Point::new(*latitude, *longitude).is_ok()
        });
        let Some((latitude, longitude)) = valid else {
            // No usable point: only the name's successor applies.
            return match stored {
                None => None,
                Some(name) => match self.successor(name) {
                    NameSuccessor::Name(next) => Some(next),
                    NameSuccessor::Removed | NameSuccessor::Ambiguous => Some(name.to_owned()),
                },
            };
        };
        let matched_before = crate::lifecycle::match_geofence(latitude, longitude, self.before);
        let matched_after = crate::lifecycle::match_geofence(latitude, longitude, self.after);
        let answer_after = matched_after.map(|fence| fence.name.clone());
        // Rule 1: a change that alters which fence contains the point.
        if matched_before.map(|fence| fence.source_geofence_id)
            != matched_after.map(|fence| fence.source_geofence_id)
        {
            return answer_after;
        }
        // Rule 2: the label equals the answer before the change.
        if stored == matched_before.map(|fence| fence.name.as_str()) {
            return answer_after;
        }
        // Rule 3: the label follows its name's successor.
        match stored {
            None => None,
            Some(name) => match self.successor(name) {
                NameSuccessor::Name(next) => Some(next),
                NameSuccessor::Removed => answer_after,
                NameSuccessor::Ambiguous => Some(name.to_owned()),
            },
        }
    }
}

/// Whether the two fence sets differ in any fence's presence, name, centre or
/// radius, keyed by `source_geofence_id` and compared field by field, never by
/// derived equality (C8 R53): the tariff fields are not read, because no label
/// depends on them (C8 R27).
fn fence_set_changed(
    before: &[crate::lifecycle::GeofenceFence],
    after: &[crate::lifecycle::GeofenceFence],
) -> bool {
    if before.len() != after.len() {
        return true;
    }
    let after_by_id: std::collections::BTreeMap<i64, &crate::lifecycle::GeofenceFence> = after
        .iter()
        .map(|fence| (fence.source_geofence_id, fence))
        .collect();
    before.iter().any(|fence| {
        after_by_id
            .get(&fence.source_geofence_id)
            .is_none_or(|next| {
                next.name != fence.name
                    || next.latitude != fence.latitude
                    || next.longitude != fence.longitude
                    || next.radius_m != fence.radius_m
            })
    })
}

#[cfg(test)]
thread_local! {
    /// Relabel pages actually read on this thread (C8 T47): entry to the
    /// relabel does not count, only a page read after the comparison of C8 R53.
    static RELABEL_PAGE_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One page of drives for the relabel, after `after_id`.
fn relabel_drive_page(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    after_id: i64,
) -> Result<Vec<(i64, String)>, StoreError> {
    #[cfg(test)]
    RELABEL_PAGE_READS.with(|reads| reads.set(reads.get() + 1));
    materialised_drive_page(transaction, vehicle_id, after_id)
}

/// One page of charges for the relabel, after `after_id`.
fn relabel_charge_page(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    after_id: i64,
) -> Result<Vec<(i64, String)>, StoreError> {
    #[cfg(test)]
    RELABEL_PAGE_READS.with(|reads| reads.set(reads.get() + 1));
    materialised_charge_page(transaction, vehicle_id, after_id)
}

fn endpoint(latitude: Option<f64>, longitude: Option<f64>) -> Option<(f64, f64)> {
    latitude.zip(longitude)
}

/// Relabel the vehicle's stored drives, then its stored charges, for the fence
/// change from `before` to `after` (C8 R23 to C8 R26). When the two sets do not
/// differ in presence, name, centre or radius, nothing is read (C8 R53). Rows
/// are visited by id ascending in pages of at most 256, drives first, and only
/// a row whose label changes is rewritten, with one `sync_mutations` row each
/// (C8 R26). The three labels are the only fields written (C8 R27). Open
/// sessions are not read (C8 R22). The caller marks the export once for the
/// whole transaction (C8 R28).
fn relabel_materialised_locations_in_transaction(
    transaction: &Transaction<'_>,
    vehicle_id: Uuid,
    before: &[crate::lifecycle::GeofenceFence],
    after: &[crate::lifecycle::GeofenceFence],
) -> Result<RelabelCounts, StoreError> {
    let mut counts = RelabelCounts::default();
    if !fence_set_changed(before, after) {
        return Ok(counts);
    }
    let rule = RelabelRule::new(before, after);
    let mut after_id = 0_i64;
    loop {
        let page = relabel_drive_page(transaction, vehicle_id, after_id)?;
        let Some(last) = page.last().map(|(id, _)| *id) else {
            break;
        };
        after_id = last;
        for (drive_id, drive_json) in page {
            let mut drive: crate::hub_pack::ProjectionDrive =
                serde_json::from_str(&drive_json).map_err(StoreError::DeserializeLifecycleRow)?;
            let start = rule.label(
                endpoint(drive.start_latitude, drive.start_longitude),
                drive.start_geofence.as_deref(),
            );
            let end = rule.label(
                endpoint(drive.end_latitude, drive.end_longitude),
                drive.end_geofence.as_deref(),
            );
            if drive.start_geofence == start && drive.end_geofence == end {
                continue;
            }
            drive.start_geofence = start;
            drive.end_geofence = end;
            let payload =
                serde_json::to_string(&drive).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_drives SET drive_json = ?3
                     WHERE vehicle_id = ?1 AND drive_id = ?2",
                    params![vehicle_id.to_string(), drive_id, payload],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                vehicle_id,
                "drive",
                drive_id,
                drive.car_id,
                "upsert",
                &payload,
            )?;
            counts.drives += 1;
        }
    }

    after_id = 0;
    loop {
        let page = relabel_charge_page(transaction, vehicle_id, after_id)?;
        let Some(last) = page.last().map(|(id, _)| *id) else {
            break;
        };
        after_id = last;
        for (charge_id, charge_json) in page {
            let mut charge: crate::hub_pack::ProjectionCharge =
                serde_json::from_str(&charge_json).map_err(StoreError::DeserializeLifecycleRow)?;
            let geofence = rule.label(
                endpoint(charge.start_latitude, charge.start_longitude),
                charge.geofence.as_deref(),
            );
            if charge.geofence == geofence {
                continue;
            }
            charge.geofence = geofence;
            let payload =
                serde_json::to_string(&charge).map_err(StoreError::SerializeLifecycleRow)?;
            transaction
                .execute(
                    "UPDATE materialised_charges SET charge_json = ?3
                     WHERE vehicle_id = ?1 AND charge_id = ?2",
                    params![vehicle_id.to_string(), charge_id, payload],
                )
                .map_err(StoreError::LifecycleWrite)?;
            record_sync_mutation_in_transaction(
                transaction,
                vehicle_id,
                "charge",
                charge_id,
                charge.car_id,
                "upsert",
                &payload,
            )?;
            counts.charges += 1;
        }
    }
    Ok(counts)
}
