// SPDX-License-Identifier: AGPL-3.0-only

// Values of one activated imported fence row that the store writes (C8 R13),
// and the report of one import upsert (C8 R14, C8 R16, C8 R16a).

/// The tariff of an activated import row as the store holds it: a cost per
/// unit or session fee that is not a finite number is stored NULL, because a
/// NaN is not a usable price, SQLite would store it as NULL anyway and JSON
/// cannot carry it. Finite values are copied as read, negatives included; the
/// billing type is copied as read.
#[derive(Debug, Clone, Copy)]
struct ImportedGeofenceTariff {
    billing_type: Option<crate::hub_pack::GeofenceBillingType>,
    cost_per_unit: Option<f64>,
    session_fee: Option<f64>,
    /// Whether a cost or fee was present but not finite (C8 R13); the import
    /// report lists such rows.
    not_finite: bool,
}

impl ImportedGeofenceTariff {
    fn of(geofence: &crate::teslamate_projection::TeslaMateGeofence) -> Self {
        let finite = |value: Option<f64>| value.filter(|number| number.is_finite());
        Self {
            billing_type: geofence.billing_type,
            cost_per_unit: finite(geofence.cost_per_unit),
            session_fee: finite(geofence.session_fee),
            not_finite: finite(geofence.cost_per_unit) != geofence.cost_per_unit
                || finite(geofence.session_fee) != geofence.session_fee,
        }
    }

    fn billing_type_text(self) -> Option<&'static str> {
        self.billing_type
            .map(crate::hub_pack::GeofenceBillingType::as_str)
    }
}

/// Rows a relabel rewrote, by kind (C8 R26). An import upsert reports zeros
/// when its fence changes leave presence, name, centre and radius as they were
/// (C8 R16, C8 R53).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelabelCounts {
    pub drives: u64,
    pub charges: u64,
}

/// The outcome of one import upsert, by fence id (C8 R14, C8 R16, C8 R16a, C8
/// table 5.4). Every list holds fence ids, ascending, and carries no name,
/// coordinate or value. `source_rows` equals the lengths of `inserted`,
/// `updated`, `unchanged`, `kept_hub_values`, `deleted_in_hub` and every
/// `inactive` list added together; `tariff_not_finite` and `absent_from_source`
/// are not part of that sum.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GeofenceUpsertReport {
    /// Rows in the slice.
    pub source_rows: u64,
    /// Activated rows written as new fences.
    pub inserted: Vec<i64>,
    /// Source-owned fences updated to the values read.
    pub updated: Vec<i64>,
    /// Fences whose stored values equal the values read.
    pub unchanged: Vec<i64>,
    /// Fences the owner edited in the Hub; the values read were not applied.
    pub kept_hub_values: Vec<i64>,
    /// Activated rows for an id the owner deleted in the Hub; nothing written.
    pub deleted_in_hub: Vec<i64>,
    /// Rows that were not activated, by the reason.
    pub inactive: std::collections::BTreeMap<
        crate::geo::geofence_validation::GeofenceInactiveReason,
        Vec<i64>,
    >,
    /// Inserted, updated and unchanged rows whose cost or fee is not finite and is
    /// held as NULL (C8 R13). A row kept for the Hub or deleted in the Hub stored
    /// nothing, so it is not listed.
    pub tariff_not_finite: Vec<i64>,
    /// Stored fences with an imported id and no row in the slice.
    pub absent_from_source: Vec<i64>,
    /// Rows the relabel of this upsert rewrote.
    pub relabel: RelabelCounts,
}
