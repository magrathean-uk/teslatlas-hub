// SPDX-License-Identifier: AGPL-3.0-only

// The import watermarks of a vehicle, one pair per source and domain (C2 R80).
//
// A watermark pair records how far one imported class reaches: the largest
// source row id and the largest time the import supplied for that class. Other
// units read it: the charge import bound (C2 R5, `charging_processes`), the
// terrain fill of imported positions (C9 R6, `positions`), the id cursors (C1
// R84, all six domains) and the seeded version time (C1 R89, `updates`). A
// class's recorded reach can only grow, because imports never delete and a
// later snapshot may reach less far in one class (an absent maximum, fewer
// rows, a partial snapshot): a plain write would move that class back, so rows
// an earlier import supplied would read as Hub rows.
//
// One writer ([`raise_source_watermarks_in_transaction`]) stores
// [`merge_watermark`] of the stored and the incoming pair in every domain, and
// nothing else. Tests and the schema excepted, it holds the only statement in
// the crate that inserts into or updates `lifecycle_source_watermarks`; no
// statement deletes the table's rows outside migrations and tests.
//
// Every store transaction that writes a vehicle's watermark rows performs one
// sequence, in this order, all inside that transaction (C2 R80 (g)):
//
// 1. the pairs through [`raise_source_watermarks_in_transaction`], which
//    returns the domains whose stored pair changed;
// 2. C1 R84 (b)'s raise of the vehicle's id cursors from the rows as they then
//    stand ([`raise_id_cursors_above_watermarks`]), reading every source's
//    rows (C1 R90 for the promotion);
// 3. once C9 Part C has landed, when the returned domains include
//    [`WatermarkDomain::Positions`], C9 R58's boundary re-arm for the vehicle.
//    C9 Part C is not in this tree yet, so no caller performs step 3 today.
//
// A seed that is a no-op by C2 R80 (d) writes nothing and performs none of the
// three. The two seeding paths are `HubStore::seed_imported_open_session` and
// `promote_imported_open_session_in_transaction`; every later import writer
// calls the same three steps in the same order.

/// The six import-watermark domains of `lifecycle_source_watermarks.domain`
/// (C2 R80 (c)): the names of the imported classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatermarkDomain {
    Drives,
    Positions,
    ChargingProcesses,
    Charges,
    States,
    Updates,
}

impl WatermarkDomain {
    /// Every domain, in the canonical order the writer reports changes in.
    pub(crate) const ALL: [WatermarkDomain; 6] = [
        WatermarkDomain::Drives,
        WatermarkDomain::Positions,
        WatermarkDomain::ChargingProcesses,
        WatermarkDomain::Charges,
        WatermarkDomain::States,
        WatermarkDomain::Updates,
    ];

    /// The stored `domain` text.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            WatermarkDomain::Drives => "drives",
            WatermarkDomain::Positions => "positions",
            WatermarkDomain::ChargingProcesses => "charging_processes",
            WatermarkDomain::Charges => "charges",
            WatermarkDomain::States => "states",
            WatermarkDomain::Updates => "updates",
        }
    }
}

// The importer's snapshot types, named once here.
type ImportedWatermark = crate::teslamate_projection::TeslaMateSourceWatermark;
type ImportedWatermarks = crate::teslamate_projection::TeslaMateSourceWatermarks;

/// One domain's pair: the largest source row id and the largest time, each
/// absent when the source supplied none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct WatermarkPair {
    pub max_source_row_id: Option<i64>,
    pub max_timestamp_ms: Option<i64>,
}

impl From<ImportedWatermark> for WatermarkPair {
    fn from(watermark: ImportedWatermark) -> Self {
        Self {
            max_source_row_id: watermark.max_id,
            max_timestamp_ms: watermark.max_timestamp_ms,
        }
    }
}

impl From<WatermarkPair> for ImportedWatermark {
    fn from(pair: WatermarkPair) -> Self {
        Self {
            max_id: pair.max_source_row_id,
            max_timestamp_ms: pair.max_timestamp_ms,
        }
    }
}

/// The six pairs of a snapshot's watermarks, in [`WatermarkDomain::ALL`] order.
fn snapshot_watermark_pairs(
    watermarks: &ImportedWatermarks,
) -> [(WatermarkDomain, WatermarkPair); 6] {
    [
        (WatermarkDomain::Drives, watermarks.drives.into()),
        (WatermarkDomain::Positions, watermarks.positions.into()),
        (
            WatermarkDomain::ChargingProcesses,
            watermarks.charging_processes.into(),
        ),
        (WatermarkDomain::Charges, watermarks.charges.into()),
        (WatermarkDomain::States, watermarks.states.into()),
        (WatermarkDomain::Updates, watermarks.updates.into()),
    ]
}

/// The pair of one domain in a snapshot's watermarks.
fn snapshot_watermark_of(
    watermarks: &ImportedWatermarks,
    domain: WatermarkDomain,
) -> WatermarkPair {
    match domain {
        WatermarkDomain::Drives => watermarks.drives,
        WatermarkDomain::Positions => watermarks.positions,
        WatermarkDomain::ChargingProcesses => watermarks.charging_processes,
        WatermarkDomain::Charges => watermarks.charges,
        WatermarkDomain::States => watermarks.states,
        WatermarkDomain::Updates => watermarks.updates,
    }
    .into()
}

/// The snapshot's watermarks after each domain's pair is replaced by `merge`
/// of the stored pair and the snapshot's pair: the watermarks a seed would
/// leave (C2 R80 (d)).
fn merged_snapshot_watermarks(
    stored: &ImportedWatermarks,
    snapshot: &ImportedWatermarks,
) -> ImportedWatermarks {
    let merged = |domain| {
        ImportedWatermark::from(merge_watermark(
            snapshot_watermark_of(stored, domain),
            snapshot_watermark_of(snapshot, domain),
        ))
    };
    ImportedWatermarks {
        drives: merged(WatermarkDomain::Drives),
        positions: merged(WatermarkDomain::Positions),
        charging_processes: merged(WatermarkDomain::ChargingProcesses),
        charges: merged(WatermarkDomain::Charges),
        states: merged(WatermarkDomain::States),
        updates: merged(WatermarkDomain::Updates),
    }
}

/// The larger of two known values, the known one when only one is known, `None`
/// when neither is.
fn raise_only_value(stored: Option<i64>, incoming: Option<i64>) -> Option<i64> {
    match (stored, incoming) {
        (Some(stored), Some(incoming)) => Some(stored.max(incoming)),
        (stored, incoming) => stored.or(incoming),
    }
}

/// The pair a watermark write stores (C2 R80 (a)): each field separately, the
/// larger of two known values, the known one when only one is known, `None`
/// when neither is. A source, vehicle and domain without a row has the stored
/// pair `(None, None)`. The merge is commutative and idempotent.
pub(crate) fn merge_watermark(stored: WatermarkPair, incoming: WatermarkPair) -> WatermarkPair {
    WatermarkPair {
        max_source_row_id: raise_only_value(stored.max_source_row_id, incoming.max_source_row_id),
        max_timestamp_ms: raise_only_value(stored.max_timestamp_ms, incoming.max_timestamp_ms),
    }
}

/// The one writer of `lifecycle_source_watermarks` (C2 R80 (c)).
///
/// For each listed pair it writes `merge(stored, incoming)`: a missing row is
/// inserted, also when both merged fields are `None` (a seed still leaves one
/// row per source, vehicle and domain), and an existing row is written only
/// when the merged pair differs from it. A domain listed twice merges both
/// pairs in order. The result lists the domains whose stored pair changed, in
/// [`WatermarkDomain::ALL`] order; a newly inserted row counts as changed only
/// when a field is known, because an all-null row moves no boundary.
///
/// The caller owns the transaction and performs the rest of C2 R80 (g)'s
/// sequence in it (see the module documentation).
pub(crate) fn raise_source_watermarks_in_transaction(
    transaction: &Transaction<'_>,
    source_id: Uuid,
    vehicle_id: Uuid,
    pairs: &[(WatermarkDomain, WatermarkPair)],
) -> Result<Vec<WatermarkDomain>, StoreError> {
    let source = source_id.to_string();
    let vehicle = vehicle_id.to_string();
    let mut changed = [false; 6];
    for (domain, incoming) in pairs {
        let stored: Option<WatermarkPair> = transaction
            .query_row(
                "SELECT max_source_row_id, max_timestamp_ms
                 FROM lifecycle_source_watermarks
                 WHERE source_id = ?1 AND vehicle_id = ?2 AND domain = ?3",
                params![source, vehicle, domain.as_str()],
                |row| {
                    Ok(WatermarkPair {
                        max_source_row_id: row.get(0)?,
                        max_timestamp_ms: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(StoreError::Query)?;
        let merged = merge_watermark(stored.unwrap_or_default(), *incoming);
        if stored == Some(merged) {
            continue;
        }
        transaction
            .execute(
                "INSERT INTO lifecycle_source_watermarks(
                    source_id, vehicle_id, domain, max_source_row_id, max_timestamp_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(source_id, vehicle_id, domain) DO UPDATE SET
                    max_source_row_id = excluded.max_source_row_id,
                    max_timestamp_ms = excluded.max_timestamp_ms",
                params![
                    source,
                    vehicle,
                    domain.as_str(),
                    merged.max_source_row_id,
                    merged.max_timestamp_ms
                ],
            )
            .map_err(StoreError::LifecycleWrite)?;
        if merged != stored.unwrap_or_default() {
            changed[WatermarkDomain::ALL
                .iter()
                .position(|candidate| candidate == domain)
                .expect("every domain is listed in ALL")] = true;
        }
    }
    Ok(WatermarkDomain::ALL
        .into_iter()
        .zip(changed)
        .filter_map(|(domain, was_changed)| was_changed.then_some(domain))
        .collect())
}
