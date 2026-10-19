// SPDX-License-Identifier: AGPL-3.0-only

// The shared sample-order folds of a charging session (C2 R81 and its
// aggregate extension): the session kind and charger type (C2 R31, R32, R78)
// and the display aggregates (C2 R27).
//
// The live observer and the seed of an imported open session fold the same
// samples in the same order through these one-sample steps, so an open session
// carries the same kind, type and aggregates whether the Hub collected it or
// seeded it. From G2-4 bulk M5 the closed-import projection folds kind and type
// through the same steps and takes `max_charger_power_kw` from the aggregate
// step; it copies the supplied aggregate columns as they stand (see the caller
// contract below). Each fold holds a fixed-size state, so a long session is
// folded page by page, in `(timestamp_ms, id)` order, without holding its
// samples (C2 R81 (b); the import streams and commits per block, Storage
// 2026-10-10).
//
// Caller contract for the closed-import projection (G2-4 bulk M5):
// 1. fold each closed imported session's samples in `(timestamp_ms, id)` order,
//    page by page, through [`fold_charge_session_kind`] (or call
//    [`charge_session_kind`] on an ordered iterator) and write the result to
//    the row's `is_dc` and `fast_charger_type`; read no kind or type from the
//    imported process row (C2 R81 (c));
// 2. copy the process row's supplied aggregate columns as they stand, a null
//    included, and take `max_charger_power_kw`, which the source row does not
//    carry, from [`charge_display_aggregates`] over the same ordered samples
//    (null when no sample's power is finite and non-negative);
// 3. store watermarks through `raise_source_watermarks_in_transaction` and
//    perform the rest of its sequence (see `source_watermarks.rs`);
// 4. add the integration test that a closed imported session's served kind and
//    type equal [`charge_session_kind`] of its samples (C2 T90).
// A change of the projection changes what a later import writes; it rewrites no
// stored row.

/// The two session-level charger fields C2 derives from samples (C2 R9 S2):
/// whether the session is DC and its charger type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ChargeSessionKind {
    pub is_dc: Option<bool>,
    pub fast_charger_type: Option<String>,
}

/// One sample's step of the session kind and type: C2 R31's table for the kind,
/// then C2 R32 for the type (with C2 R78's `Tesla` vocabulary). The one
/// definition of the step: the live and seeded observer
/// ([`observe_charge_sample`]) and [`charge_session_kind`] both call it, so a
/// closed imported session and the same session seeded open carry the same
/// values by construction.
pub(crate) fn fold_charge_session_kind(
    is_dc: &mut Option<bool>,
    fast_charger_type: &mut Option<String>,
    sample: &ProjectionChargeSample,
) {
    *is_dc = session_kind_after(*is_dc, sample_kind_evidence(sample));
    *fast_charger_type = session_charger_type_after(fast_charger_type.take(), sample);
}

/// The session kind and type of the samples, folded from
/// [`ChargeSessionKind::default`] (C2 R81 (b)). Pure; it does not sort and holds
/// only the two fields, so a caller may also feed [`fold_charge_session_kind`]
/// page by page from a read ordered by `(timestamp_ms, id)`. In debug builds it
/// asserts that each sample's `(timestamp_ms, id)` is greater than the
/// previous one's.
///
/// Called by the import projection (G2-4 bulk M5) for each closed imported
/// session, which reads no kind or type from the imported process row (C2 R81
/// (c)); the seed reaches the same step through [`observe_charge_sample`].
#[allow(dead_code)] // C2 R81 (c): the closed-import caller is G2-4 bulk M5; tests call it now.
pub(crate) fn charge_session_kind<'a>(
    samples_in_sample_order: impl IntoIterator<Item = &'a ProjectionChargeSample>,
) -> ChargeSessionKind {
    let mut kind = ChargeSessionKind::default();
    let mut previous: Option<(i64, i64)> = None;
    for sample in samples_in_sample_order {
        let key = (sample.timestamp_ms, sample.id);
        debug_assert!(
            previous.is_none_or(|before| key > before),
            "charge_session_kind needs samples in (timestamp_ms, id) order"
        );
        previous = Some(key);
        fold_charge_session_kind(&mut kind.is_dc, &mut kind.fast_charger_type, sample);
    }
    kind
}

/// The display aggregates of one charging session (C2 R27): the last known
/// battery level and ranges, the largest usable charger power and the sum and
/// count of the outside temperatures whose quotient is the mean. The field
/// types equal the six fields of [`OpenCharge`] that hold them. No `Eq`: the
/// fields are `f64`, and [`ChargeSessionKind`]'s `Eq` is not touched.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct ChargeDisplayAggregates {
    pub last_battery_level: Option<i64>,
    pub last_ideal_range_km: Option<f64>,
    pub last_rated_range_km: Option<f64>,
    pub max_charger_power_kw: Option<f64>,
    pub outside_temp_sum: f64,
    pub outside_temp_count: u32,
}

impl ChargeDisplayAggregates {
    /// The mean outside temperature, `sum / count` when `count > 0`, else
    /// `None` (the mean `finalize_charge` takes before it rounds).
    #[allow(dead_code)] // C2 R81 (e): read by the closed-import caller (G2-4 bulk M5) and tests.
    pub(crate) fn outside_temp_avg(&self) -> Option<f64> {
        (self.outside_temp_count > 0)
            .then(|| self.outside_temp_sum / f64::from(self.outside_temp_count))
    }

    /// The six aggregate fields of an open charge.
    fn of_open(open: &OpenCharge) -> Self {
        Self {
            last_battery_level: open.last_battery_level,
            last_ideal_range_km: open.last_ideal_range_km,
            last_rated_range_km: open.last_rated_range_km,
            max_charger_power_kw: open.max_charger_power_kw,
            outside_temp_sum: open.outside_temp_sum,
            outside_temp_count: open.outside_temp_count,
        }
    }

    /// Write the six fields back to an open charge.
    fn store_into(self, open: &mut OpenCharge) {
        open.last_battery_level = self.last_battery_level;
        open.last_ideal_range_km = self.last_ideal_range_km;
        open.last_rated_range_km = self.last_rated_range_km;
        open.max_charger_power_kw = self.max_charger_power_kw;
        open.outside_temp_sum = self.outside_temp_sum;
        open.outside_temp_count = self.outside_temp_count;
    }
}

/// C2 R27 for one sample, with the live step's validity filters. The battery
/// level keeps the last known value and is never filtered; each range keeps the
/// last finite value; the maximum charger power is the largest finite
/// non-negative reading; each finite outside temperature (a negative one
/// included) joins the sum and the count, which saturates. The one definition:
/// the live observer and the seed fold through it, and from G2-4 bulk M5 the
/// closed-import projection takes `max_charger_power_kw` from it.
pub(crate) fn fold_charge_display_aggregates(
    aggregates: &mut ChargeDisplayAggregates,
    sample: &ProjectionChargeSample,
) {
    aggregates.last_battery_level = sample.battery_level.or(aggregates.last_battery_level);
    aggregates.last_ideal_range_km =
        finite_reading(sample.ideal_range_km).or(aggregates.last_ideal_range_km);
    aggregates.last_rated_range_km =
        finite_reading(sample.rated_range_km).or(aggregates.last_rated_range_km);
    if let Some(power) = finite_reading(sample.charger_power_kw).filter(|power| *power >= 0.0) {
        aggregates.max_charger_power_kw = Some(
            aggregates
                .max_charger_power_kw
                .map_or(power, |maximum| maximum.max(power)),
        );
    }
    if let Some(temperature) = finite_reading(sample.outside_temp_c) {
        aggregates.outside_temp_sum += temperature;
        aggregates.outside_temp_count = aggregates.outside_temp_count.saturating_add(1);
    }
}

/// The display aggregates of the samples, folded from
/// [`ChargeDisplayAggregates::default`]. Pure; it does not sort, so a caller
/// feeds it in `(timestamp_ms, id)` order, whole or page by page through
/// [`fold_charge_display_aggregates`]. A closed import copies the supplied
/// process columns as they stand and takes `max_charger_power_kw` from here
/// (G2-4 bulk M5, carried item M5-2).
#[allow(dead_code)] // C2 R81 (e): the closed-import caller is G2-4 bulk M5; tests call it now.
pub(crate) fn charge_display_aggregates<'a>(
    samples_in_sample_order: impl IntoIterator<Item = &'a ProjectionChargeSample>,
) -> ChargeDisplayAggregates {
    let mut aggregates = ChargeDisplayAggregates::default();
    for sample in samples_in_sample_order {
        fold_charge_display_aggregates(&mut aggregates, sample);
    }
    aggregates
}
